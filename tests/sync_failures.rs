mod common;
use common::*;
use git_rsrs::{
    config::{Config, HookConfig},
    outcome::{Outcome, Trigger},
    sync::sync_once,
};
use std::{fs, time::Duration};
fn setup(f: &Fixture, message: &str, sleep: bool) -> Config {
    let mut c = Config::parse("version=1\nbranch='main'").unwrap();
    c.repo = f.a.clone();
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
    c
}
#[test]
fn offline_failure_keeps_the_new_local_commit() {
    let f = Fixture::new();
    let c = setup(&f, "Network is unreachable", false);
    fs::write(f.a.join("new"), "offline work").unwrap();
    let report = sync_once(&c, Trigger::Manual);
    assert_eq!(report.outcome, Outcome::Offline, "{report:?}");
    assert!(report.committed.is_some());
    assert!(!report.pushed);
    assert_eq!(text(&f.a, &["show", "HEAD:new"]), "offline work");
}
#[test]
fn authentication_and_timeout_have_distinct_outcomes() {
    let f = Fixture::new();
    let c = setup(&f, "Permission denied (publickey).", false);
    assert_eq!(
        sync_once(&c, Trigger::Manual).outcome,
        Outcome::AuthenticationFailed
    );
    let mut c = setup(&f, "waiting", true);
    c.git_timeout = Duration::from_millis(100);
    assert_eq!(sync_once(&c, Trigger::Manual).outcome, Outcome::TimedOut);
}
#[test]
fn result_hook_failure_does_not_replace_success() {
    let f = Fixture::new();
    let mut c = Config::parse("version=1\nbranch='main'").unwrap();
    c.repo = f.a.clone();
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
