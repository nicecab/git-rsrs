mod common;
use common::*;
use git_rsrs::{
    config::HookConfig,
    outcome::{Outcome, SyncReport, Trigger},
    sync::sync_once,
};
use std::{fs, time::Duration};
// Runs through `spawn_sync` so the fake `core.sshCommand` from `setup` is used.
fn ssh_sync(f: &Fixture, extra: &str) -> SyncReport {
    finish_sync(spawn_sync(f, &format!("version=1\nbranch='main'\n{extra}")))
}
fn setup(f: &Fixture, message: &str, sleep: bool) {
    git(
        &f.a,
        ["remote", "set-url", "origin", "ssh://test.invalid/repo"],
    );
    let script = f.temp.path().join("ssh.py");
    fs::write(
        &script,
        format!(
            "import sys,time\nprint({message:?},file=sys.stderr,flush=True)\n{}\nsys.exit(255)\n",
            if sleep { "time.sleep(20)" } else { "" }
        ),
    )
    .unwrap();
    git(
        &f.a,
        [
            "config",
            "core.sshCommand",
            &format!("python3 '{}'", script.display()),
        ],
    );
}
#[test]
fn offline_failure_keeps_the_new_local_commit() {
    let f = Fixture::new();
    setup(&f, "Network is unreachable", false);
    fs::write(f.a.join("new"), "offline work").unwrap();
    let report = ssh_sync(&f, "");
    assert_eq!(report.outcome, Outcome::Offline, "{report:?}");
    assert!(report.committed.is_some());
    assert!(!report.pushed);
    assert_eq!(text(&f.a, &["show", "HEAD:new"]), "offline work");
}
#[test]
fn authentication_and_timeout_have_distinct_outcomes() {
    let f = Fixture::new();
    setup(&f, "Permission denied (publickey).", false);
    assert_eq!(ssh_sync(&f, "").outcome, Outcome::AuthenticationFailed);
    setup(&f, "waiting", true);
    assert_eq!(
        ssh_sync(&f, "git_timeout='100ms'").outcome,
        Outcome::TimedOut
    );
}
#[test]
fn result_hook_failure_does_not_replace_success() {
    let f = Fixture::new();
    let mut c = f.config();
    c.hooks.on_result.push(HookConfig {
        command: vec![
            "python3".into(),
            "-c".into(),
            "import sys; sys.exit(1)".into(),
        ],
        timeout: Duration::from_secs(2),
        reject_exit_codes: vec![],
    });
    let report = sync_once(&c, Trigger::Manual);
    assert_eq!(report.outcome, Outcome::Noop);
    assert_eq!(report.diagnostics.len(), 1);
}
