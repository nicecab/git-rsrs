use crate::{
    config::Config,
    git::{
        COMMIT_JOURNAL, Git, GitError, INTEGRATE_JOURNAL, RepositoryState,
        integrate::{Integration, PushResult},
        snapshot::{CommitResult, Snapshot},
    },
    hooks::{HookContext, HookDecision, HookPhase, HookRunner},
    lock::{CheckoutLock, LockError},
    outcome::{Outcome, SyncReport, Trigger},
    process::{ProcessError, ProcessRunner},
};

#[derive(Debug)]
struct Failure {
    outcome: Outcome,
    message: String,
    recovery: Vec<String>,
}
impl From<GitError> for Failure {
    fn from(error: GitError) -> Self {
        let outcome = match &error {
            GitError::Process(ProcessError::Timeout(_)) => Outcome::TimedOut,
            GitError::Process(ProcessError::Interrupted(_)) => Outcome::ManualIntervention,
            GitError::Command { message, .. } => {
                let lower = message.to_lowercase();
                if [
                    "permission denied (publickey",
                    "authentication failed",
                    "could not read username",
                    "http 401",
                    "http 403",
                    "access denied",
                ]
                .iter()
                .any(|s| lower.contains(s))
                {
                    Outcome::AuthenticationFailed
                } else if [
                    "could not resolve host",
                    "could not resolve hostname",
                    "network is unreachable",
                    "no route to host",
                    "connection timed out",
                    "operation timed out",
                ]
                .iter()
                .any(|s| lower.contains(s))
                {
                    Outcome::Offline
                } else {
                    Outcome::Failed
                }
            }
            _ => Outcome::Failed,
        };
        Self {
            outcome,
            message: error.to_string(),
            recovery: vec![],
        }
    }
}
impl From<LockError> for Failure {
    fn from(error: LockError) -> Self {
        Self {
            outcome: match error {
                LockError::Timeout { .. } => Outcome::LockTimeout,
                LockError::Interrupted => Outcome::ManualIntervention,
                _ => Outcome::Failed,
            },
            message: error.to_string(),
            recovery: vec![],
        }
    }
}
fn manual(message: impl Into<String>) -> Failure {
    Failure {
        outcome: Outcome::ManualIntervention,
        message: message.into(),
        recovery: vec![],
    }
}

pub fn sync_once(config: &Config, trigger: Trigger) -> SyncReport {
    sync_once_guarded(config, trigger, || true)
}

pub fn sync_once_guarded(
    config: &Config,
    trigger: Trigger,
    fresh: impl Fn() -> bool,
) -> SyncReport {
    let mut report = SyncReport::new(trigger);
    let mut checkout_root = config.repo.clone();
    if let Err(failure) = sync_locked(config, &mut report, &mut checkout_root, &fresh) {
        report.outcome = failure.outcome;
        report.diagnostic("sync", failure.message);
        report.recovery.extend(failure.recovery);
    }
    // sync_locked has returned and dropped its checkout lock before reporting.
    let mut context = HookContext::new(&checkout_root, trigger);
    context.report = Some(report.clone());
    let hooks = HookRunner.run(HookPhase::OnResult, &config.hooks.on_result, &context);
    report.diagnostics.extend(hooks.diagnostics);
    report
}
fn sync_locked(
    config: &Config,
    report: &mut SyncReport,
    checkout_root: &mut std::path::PathBuf,
    fresh: &dyn Fn() -> bool,
) -> Result<(), Failure> {
    let mut git = Git::open(&config.repo, ProcessRunner)?;
    git.timeout = config.git_timeout;
    *checkout_root = git.root.clone();
    let _lock = CheckoutLock::acquire(&git, &config.lock_name, config.lock_timeout)?;
    for marker in [COMMIT_JOURNAL, INTEGRATE_JOURNAL, "index.lock"] {
        let path = git.path(marker)?;
        if path.exists() {
            return Err(Failure {
                outcome: Outcome::ManualIntervention,
                message: format!("pending recovery or Git lock: {}", path.display()),
                recovery: vec![path.display().to_string()],
            });
        }
    }
    let state = git.state()?;
    if state != RepositoryState::Ready {
        return Err(manual(format!(
            "refusing synchronization in Git state {state:?}; finish the existing operation first"
        )));
    }
    if git.branch()? != config.branch {
        report.outcome = Outcome::BranchSkipped;
        return Ok(());
    }
    if !fresh() {
        report.outcome = Outcome::Deferred;
        return Ok(());
    }
    let mut expected_head = git.head()?;
    let context = HookContext::new(&git.root, report.trigger);
    let mut rejected = false;
    if git.is_dirty()? {
        if matches!(report.trigger, Trigger::Initial | Trigger::Periodic) {
            report.outcome = Outcome::Deferred;
            return Ok(());
        }
        let candidate = Snapshot::capture(&git)?;
        let mut validation_context = context.clone();
        validation_context.index = Some(candidate.index.clone());
        let checks = HookRunner.run(
            HookPhase::PreCommit,
            &config.hooks.pre_commit,
            &validation_context,
        );
        report.diagnostics.extend(checks.diagnostics);
        match checks.decision {
            HookDecision::AbortSync => {
                return Err(Failure {
                    outcome: if checks.interrupted {
                        Outcome::ManualIntervention
                    } else if checks.timed_out {
                        Outcome::TimedOut
                    } else {
                        Outcome::Failed
                    },
                    message: "pre-commit hook could not complete".into(),
                    recovery: vec![],
                });
            }
            HookDecision::RejectCommit => rejected = true,
            HookDecision::Allow => {
                if !fresh() {
                    report.outcome = Outcome::Deferred;
                    return Ok(());
                }
                match candidate.commit_validated(&git)? {
                    CommitResult::Created(oid) => {
                        expected_head = oid.clone();
                        report.committed = Some(oid);
                    }
                    CommitResult::NoChanges => {}
                    CommitResult::DeferredConcurrentChange => {
                        report.outcome = Outcome::Deferred;
                        return Ok(());
                    }
                    CommitResult::RecoveryRequired(message) => {
                        return Err(Failure {
                            outcome: Outcome::ManualIntervention,
                            recovery: vec![message.clone()],
                            message,
                        });
                    }
                }
            }
        }
    }
    let mut post_failed = false;
    fetch_and_integrate(
        &git,
        config,
        report,
        &context,
        &mut post_failed,
        &mut expected_head,
    )?;
    if git.ahead(&config.remote, &config.branch)? {
        if git.push(&config.remote, &config.branch, &expected_head)? == PushResult::NonFastForward {
            fetch_and_integrate(
                &git,
                config,
                report,
                &context,
                &mut post_failed,
                &mut expected_head,
            )?;
            if git.push(&config.remote, &config.branch, &expected_head)?
                == PushResult::NonFastForward
            {
                return Err(manual(
                    "push was rejected again after one retry; retry synchronization later",
                ));
            }
        }
        report.pushed = true;
    }
    report.outcome = if rejected {
        Outcome::ValidationRejected
    } else if post_failed {
        Outcome::CheckFailed
    } else if report.committed.is_some() || report.integrated || report.pushed {
        Outcome::Synced
    } else {
        Outcome::Noop
    };
    Ok(())
}
fn fetch_and_integrate(
    git: &Git,
    config: &Config,
    report: &mut SyncReport,
    context: &HookContext,
    post_failed: &mut bool,
    expected_head: &mut String,
) -> Result<(), Failure> {
    require_identity(git, config, expected_head)?;
    git.fetch(&config.remote, &config.branch)?;
    require_identity(git, config, expected_head)?;
    match git.integrate(&config.remote, &config.branch)? {
        Integration::Integrated { changed } => {
            if changed {
                *expected_head = git.head()?;
            }
            require_identity(git, config, expected_head)?;
            report.integrated |= changed;
        }
        Integration::RebaseConflict { recovery }
        | Integration::StashRestoreConflict { recovery }
        | Integration::RecoveryRequired { recovery } => {
            return Err(Failure {
                outcome: Outcome::ManualIntervention,
                message: "integration requires manual intervention".into(),
                recovery,
            });
        }
    }
    let hooks = HookRunner.run(
        HookPhase::PostIntegrate,
        &config.hooks.post_integrate,
        context,
    );
    *post_failed |= !hooks.diagnostics.is_empty();
    report.diagnostics.extend(hooks.diagnostics);
    require_identity(git, config, expected_head)
}

fn require_identity(git: &Git, config: &Config, expected_head: &str) -> Result<(), Failure> {
    if git.branch()? != config.branch
        || git.head()? != expected_head
        || git.state()? != RepositoryState::Ready
    {
        return Err(manual(
            "checkout branch, HEAD or Git state changed during synchronization; no further mutations were attempted",
        ));
    }
    Ok(())
}
