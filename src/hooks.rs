use crate::{
    config::HookConfig,
    outcome::{Diagnostic, SyncReport, Trigger},
    process::{CommandSpec, ProcessError, ProcessRunner},
};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookPhase {
    PreCommit,
    PostIntegrate,
    OnResult,
}
impl HookPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PreCommit => "pre_commit",
            Self::PostIntegrate => "post_integrate",
            Self::OnResult => "on_result",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookDecision {
    Allow,
    RejectCommit,
    AbortSync,
}
#[derive(Debug)]
pub struct HookReport {
    pub decision: HookDecision,
    pub diagnostics: Vec<Diagnostic>,
    pub timed_out: bool,
}
#[derive(Debug, Clone)]
pub struct HookContext {
    pub repo: PathBuf,
    pub trigger: Trigger,
    pub index: Option<PathBuf>,
    pub report: Option<SyncReport>,
}
impl HookContext {
    pub fn new(repo: &Path, trigger: Trigger) -> Self {
        Self {
            repo: repo.into(),
            trigger,
            index: None,
            report: None,
        }
    }
}
#[derive(Debug, Clone, Copy)]
pub struct HookRunner;
impl HookRunner {
    pub fn run(&self, phase: HookPhase, hooks: &[HookConfig], context: &HookContext) -> HookReport {
        let mut result = HookReport {
            decision: HookDecision::Allow,
            diagnostics: vec![],
            timed_out: false,
        };
        for hook in hooks {
            let mut spec = CommandSpec::new(&hook.command, &context.repo, hook.timeout);
            spec.env
                .insert("RSRS_REPO".into(), context.repo.as_os_str().into());
            spec.env.insert("RSRS_PHASE".into(), phase.as_str().into());
            spec.env
                .insert("RSRS_TRIGGER".into(), context.trigger.as_str().into());
            if let Some(index) = &context.index {
                spec.env
                    .insert("GIT_INDEX_FILE".into(), index.as_os_str().into());
            }
            if let Some(report) = &context.report {
                spec.env
                    .insert("RSRS_OUTCOME".into(), report.outcome.as_str().into());
                // Do not export potentially huge hook diagnostics as environment variables.
                let mut metadata = serde_json::json!({"version":1,"outcome":report.outcome,"trigger":report.trigger,"committed":report.committed,"integrated":report.integrated,"pushed":report.pushed,"recovery":report.recovery});
                if serde_json::to_vec(&metadata)
                    .expect("report metadata")
                    .len()
                    > 16 * 1024
                {
                    metadata = serde_json::json!({"version":1,"outcome":report.outcome,"trigger":report.trigger,"truncated":true});
                }
                spec.env
                    .insert("RSRS_REPORT_JSON".into(), metadata.to_string().into());
            }
            let (code, message, fault) = match ProcessRunner.run(&spec) {
                Ok(output) => {
                    let message = format!(
                        "exit {:?}, signal {:?}: {}{}{}",
                        output.code,
                        output.signal,
                        String::from_utf8_lossy(&output.stdout),
                        String::from_utf8_lossy(&output.stderr),
                        if output.truncated {
                            " [output truncated]"
                        } else {
                            ""
                        }
                    );
                    (output.code, message, false)
                }
                Err(error) => {
                    result.timed_out |= matches!(error, ProcessError::Timeout(_));
                    (None, error.to_string(), true)
                }
            };
            if code == Some(0) && !fault {
                continue;
            }
            result.diagnostics.push(Diagnostic {
                phase: phase.as_str().into(),
                message,
            });
            if phase == HookPhase::PreCommit {
                if !fault && code.is_some_and(|c| hook.reject_exit_codes.contains(&c)) {
                    result.decision = HookDecision::RejectCommit;
                } else {
                    result.decision = HookDecision::AbortSync;
                    break;
                }
            }
        }
        result
    }
}
