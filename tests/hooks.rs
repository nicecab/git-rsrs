use git_rsrs::{
    config::HookConfig,
    hooks::{HookContext, HookDecision, HookPhase, HookRunner},
    outcome::Trigger,
};
use std::time::Duration;
fn hook(code: i32, reject: Vec<i32>) -> HookConfig {
    HookConfig {
        command: vec![
            "python3".into(),
            "-c".into(),
            format!("import sys; print('diagnostic'); sys.exit({code})"),
        ],
        timeout: Duration::from_secs(3),
        reject_exit_codes: reject,
    }
}
#[test]
fn rejection_is_explicit_and_infrastructure_failures_abort() {
    let temp = tempfile::tempdir().unwrap();
    let context = HookContext::new(temp.path(), Trigger::Manual);
    let result = HookRunner.run(
        HookPhase::PreCommit,
        &[hook(3, vec![3]), hook(3, vec![3])],
        &context,
    );
    assert_eq!(result.decision, HookDecision::RejectCommit);
    assert_eq!(result.diagnostics.len(), 2);
    let result = HookRunner.run(HookPhase::PreCommit, &[hook(3, vec![])], &context);
    assert_eq!(result.decision, HookDecision::AbortSync);
    let result = HookRunner.run(HookPhase::PostIntegrate, &[hook(1, vec![])], &context);
    assert_eq!(result.decision, HookDecision::Allow);
    assert_eq!(result.diagnostics.len(), 1);
}
#[test]
fn missing_commands_and_timeouts_are_not_validation_rejections() {
    let temp = tempfile::tempdir().unwrap();
    let context = HookContext::new(temp.path(), Trigger::Manual);
    let mut missing = hook(0, vec![3]);
    missing.command = vec!["/no/such/hook".into()];
    assert_eq!(
        HookRunner
            .run(HookPhase::PreCommit, &[missing], &context)
            .decision,
        HookDecision::AbortSync
    );
    let mut slow = hook(0, vec![3]);
    slow.command = vec![
        "python3".into(),
        "-c".into(),
        "import time; time.sleep(20)".into(),
    ];
    slow.timeout = Duration::from_millis(50);
    let result = HookRunner.run(HookPhase::PreCommit, &[slow], &context);
    assert_eq!(result.decision, HookDecision::AbortSync);
    assert!(result.timed_out);
}
#[test]
fn hook_arguments_and_context_are_literal() {
    let temp = tempfile::tempdir().unwrap();
    let context = HookContext::new(temp.path(), Trigger::LocalChange);
    let h=HookConfig { command:vec!["python3".into(),"-c".into(),"import os,sys; assert sys.argv[1]=='a b;$(false)'; assert os.environ['RSRS_TRIGGER']=='local_change'; assert os.environ['RSRS_PHASE']=='pre_commit'".into(),"a b;$(false)".into()],timeout:Duration::from_secs(3),reject_exit_codes:vec![] };
    let result = HookRunner.run(HookPhase::PreCommit, &[h], &context);
    assert_eq!(result.decision, HookDecision::Allow);
    assert!(result.diagnostics.is_empty());
}
