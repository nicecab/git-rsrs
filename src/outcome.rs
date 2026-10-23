use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    Manual,
    Initial,
    LocalChange,
    Periodic,
}
impl Trigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Initial => "initial",
            Self::LocalChange => "local_change",
            Self::Periodic => "periodic",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Synced,
    Noop,
    BranchSkipped,
    Deferred,
    ValidationRejected,
    CheckFailed,
    ManualIntervention,
    LockTimeout,
    Offline,
    TimedOut,
    AuthenticationFailed,
    Failed,
}
impl Outcome {
    pub fn exit_code(self) -> i32 {
        match self {
            Self::Synced | Self::Noop | Self::BranchSkipped | Self::Deferred => 0,
            Self::ValidationRejected | Self::CheckFailed => 3,
            Self::ManualIntervention => 4,
            Self::LockTimeout => 5,
            Self::Offline => 6,
            Self::TimedOut => 7,
            Self::AuthenticationFailed | Self::Failed => 1,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Synced => "synced",
            Self::Noop => "noop",
            Self::BranchSkipped => "branch_skipped",
            Self::Deferred => "deferred",
            Self::ValidationRejected => "validation_rejected",
            Self::CheckFailed => "check_failed",
            Self::ManualIntervention => "manual_intervention",
            Self::LockTimeout => "lock_timeout",
            Self::Offline => "offline",
            Self::TimedOut => "timed_out",
            Self::AuthenticationFailed => "authentication_failed",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    pub phase: String,
    pub message: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncReport {
    pub version: u32,
    pub outcome: Outcome,
    pub trigger: Trigger,
    pub committed: Option<String>,
    pub integrated: bool,
    pub pushed: bool,
    pub diagnostics: Vec<Diagnostic>,
    pub recovery: Vec<String>,
}
impl SyncReport {
    pub fn new(trigger: Trigger) -> Self {
        Self {
            version: 1,
            outcome: Outcome::Noop,
            trigger,
            committed: None,
            integrated: false,
            pushed: false,
            diagnostics: vec![],
            recovery: vec![],
        }
    }
    pub fn diagnostic(&mut self, phase: &str, message: impl Into<String>) {
        self.diagnostics.push(Diagnostic {
            phase: phase.into(),
            message: message.into(),
        });
    }
}
