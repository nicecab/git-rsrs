use crate::{
    config::Config,
    git::{
        Git, GitError, RepositoryState,
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
fn manual(message: impl Into<String>) -> Failure {
    Failure {
        outcome: Outcome::ManualIntervention,
        message: message.into(),
        recovery: vec![],
    }
}

pub fn sync_once(config: &Config, trigger: Trigger) -> SyncReport {
    let mut report = SyncReport::new(trigger);
    if let Err(failure) = sync_locked(config, &mut report) {
        report.outcome = failure.outcome;
        report.diagnostic("sync", failure.message);
        report.recovery.extend(failure.recovery);
    }
    // sync_locked has returned and dropped its checkout lock before reporting.
    let mut context = HookContext::new(&config.repo, trigger);
    context.report = Some(report.clone());
    let hooks = HookRunner.run(HookPhase::OnResult, &config.hooks.on_result, &context);
    report.diagnostics.extend(hooks.diagnostics);
    report
}
fn sync_locked(config: &Config, report: &mut SyncReport) -> Result<(), Failure> {
    let mut git = Git::open(&config.repo, ProcessRunner)?;
    git.timeout = config.git_timeout;
    let _lock =
        CheckoutLock::acquire(&git, &config.lock_name, config.lock_timeout).map_err(|error| {
            Failure {
                outcome: if matches!(error, LockError::Timeout { .. }) {
                    Outcome::LockTimeout
                } else {
                    Outcome::Failed
                },
                message: error.to_string(),
                recovery: vec![],
            }
        })?;
    for marker in [
        "git-rsrs-commit.json",
        "git-rsrs-integrate.json",
        "index.lock",
    ] {
        if git.path(marker)?.exists() {
            let path = git.path(marker)?;
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
    let context = HookContext::new(&git.root, report.trigger);
    let mut rejected = false;
    if !git.status()?.is_empty() {
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
                    outcome: if checks.timed_out {
                        Outcome::TimedOut
                    } else {
                        Outcome::Failed
                    },
                    message: "pre-commit hook could not complete".into(),
                    recovery: vec![],
                });
            }
            HookDecision::RejectCommit => rejected = true,
            HookDecision::Allow => match candidate.commit_validated(&git)? {
                CommitResult::Created(oid) => report.committed = Some(oid),
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
            },
        }
    }
    git.fetch(&config.remote, &config.branch)?;
    let mut post_failed = false;
    integrate(&git, config, report, &context, &mut post_failed)?;
    if git.ahead(&config.remote, &config.branch)? {
        if git.push(&config.remote, &config.branch)? == PushResult::NonFastForward {
            git.fetch(&config.remote, &config.branch)?;
            integrate(&git, config, report, &context, &mut post_failed)?;
            if git.push(&config.remote, &config.branch)? == PushResult::NonFastForward {
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
fn integrate(
    git: &Git,
    config: &Config,
    report: &mut SyncReport,
    context: &HookContext,
    post_failed: &mut bool,
) -> Result<(), Failure> {
    match git.integrate(&config.remote, &config.branch)? {
        Integration::Integrated { changed } => report.integrated |= changed,
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
    Ok(())
}
