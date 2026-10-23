mod common;
use common::*;
use git_rsrs::{
    config::{Config, HookConfig},
    outcome::{Outcome, Trigger},
    sync::sync_once,
};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
fn config(f: &Fixture) -> Config {
    let mut c = Config::parse("version=1\nbranch='main'").unwrap();
    c.repo = f.a.clone();
    c
}
fn reject(c: &mut Config) {
    c.hooks.pre_commit.push(HookConfig {
        command: vec![
            "python3".into(),
            "-c".into(),
            "import sys; sys.exit(3)".into(),
        ],
        timeout: Duration::from_secs(3),
        reject_exit_codes: vec![3],
    });
}

#[test]
fn rejected_edits_survive_integration_while_existing_commits_push() {
    let f = Fixture::new();
    let mut c = config(&f);
    reject(&mut c);
    fs::write(f.a.join("shared.txt"), "old stash\n").unwrap();
    git(&f.a, ["stash", "push", "-m", "existing user stash"]);
    let stash = text(&f.a, &["rev-parse", "refs/stash"]);
    f.commit(&f.a, "existing", "already committed\n");
    f.commit(&f.b, "incoming", "remote\n");
    git(&f.b, ["push"]);
    fs::write(f.a.join("shared.txt"), "rejected edits\n").unwrap();
    let report = sync_once(&c, Trigger::Manual);
    assert_eq!(report.outcome, Outcome::ValidationRejected, "{report:?}");
    assert!(report.committed.is_none());
    assert!(report.pushed);
    assert_eq!(
        fs::read_to_string(f.a.join("shared.txt")).unwrap(),
        "rejected edits\n"
    );
    assert_eq!(text(&f.origin, &["show", "main:shared.txt"]), "initial");
    assert_eq!(
        text(&f.origin, &["show", "main:existing"]),
        "already committed"
    );
    assert_eq!(text(&f.a, &["rev-parse", "refs/stash"]), stash);
}
#[test]
fn stash_restore_conflict_retains_stash_and_blocks_future_mutations() {
    let f = Fixture::new();
    let mut c = config(&f);
    reject(&mut c);
    f.commit(&f.b, "shared.txt", "remote edit\n");
    git(&f.b, ["push"]);
    fs::write(f.a.join("shared.txt"), "local rejected edit\n").unwrap();
    let report = sync_once(&c, Trigger::Manual);
    assert_eq!(report.outcome, Outcome::ManualIntervention, "{report:?}");
    assert!(!report.pushed);
    assert!(!report.recovery.is_empty());
    let stash = text(&f.a, &["stash", "list", "--format=%H"]);
    assert!(!stash.is_empty());
    let conflicted = fs::read(f.a.join("shared.txt")).unwrap();
    let report = sync_once(&c, Trigger::Manual);
    assert_eq!(report.outcome, Outcome::ManualIntervention);
    assert_eq!(fs::read(f.a.join("shared.txt")).unwrap(), conflicted);
    assert_eq!(text(&f.a, &["stash", "list", "--format=%H"]), stash);
}
#[test]
fn untracked_collision_does_not_overwrite_or_remove_local_content() {
    let f = Fixture::new();
    let mut c = config(&f);
    reject(&mut c);
    f.commit(&f.b, "collision", "remote\n");
    git(&f.b, ["push"]);
    fs::write(f.a.join("collision"), "local untracked\n").unwrap();
    let head = text(&f.a, &["rev-parse", "HEAD"]);
    let report = sync_once(&c, Trigger::Manual);
    assert_ne!(report.outcome, Outcome::Synced);
    assert_eq!(
        fs::read_to_string(f.a.join("collision")).unwrap(),
        "local untracked\n"
    );
    assert_eq!(text(&f.a, &["rev-parse", "HEAD"]), head);
}
#[test]
fn rebase_conflict_is_aborted_without_losing_local_commits() {
    let f = Fixture::new();
    let c = config(&f);
    f.commit(&f.a, "shared.txt", "local committed\n");
    let head = text(&f.a, &["rev-parse", "HEAD"]);
    f.commit(&f.b, "shared.txt", "remote committed\n");
    git(&f.b, ["push"]);
    let report = sync_once(&c, Trigger::Manual);
    assert_eq!(report.outcome, Outcome::ManualIntervention, "{report:?}");
    assert_eq!(text(&f.a, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        fs::read_to_string(f.a.join("shared.txt")).unwrap(),
        "local committed\n"
    );
    assert!(!f.a.join(".git/rebase-merge").exists());
}
#[test]
fn sigterm_during_integration_stops_children_and_retains_recovery_state() {
    let f = Fixture::new();
    f.commit(&f.a, "local", "local\n");
    f.commit(&f.b, "remote", "remote\n");
    git(&f.b, ["push"]);
    let hooks = f.temp.path().join("hooks");
    fs::create_dir(&hooks).unwrap();
    let ready = f.temp.path().join("ready");
    let hook = hooks.join("pre-rebase");
    fs::write(&hook,format!("#!/usr/bin/env python3\nimport os,time\nopen({:?},'w').write(str(os.getpgrp()))\ntime.sleep(20)\n",ready.to_str().unwrap())).unwrap();
    fs::set_permissions(hook, fs::Permissions::from_mode(0o755)).unwrap();
    git(
        &f.a,
        [
            std::ffi::OsStr::new("config"),
            "core.hooksPath".as_ref(),
            hooks.as_os_str(),
        ],
    );
    let cfg = f.temp.path().join("config.toml");
    fs::write(&cfg, "version=1\nbranch='main'").unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_git-rsrs"))
        .arg("--repo")
        .arg(&f.a)
        .arg("--config")
        .arg(cfg)
        .arg("sync")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut status = None;
    while Instant::now() < deadline {
        status = child.try_wait().unwrap();
        if status.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    if status.is_none() {
        child.kill().unwrap();
    }
    let status = child.wait().unwrap();
    if let Ok(pid) = fs::read_to_string(&ready)
        .unwrap_or_default()
        .parse::<i32>()
    {
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    assert!(ready.exists());
    assert_eq!(status.code(), Some(4));
    assert!(f.a.join(".git/git-rsrs-integrate.json").exists());
    let report = sync_once(&config(&f), Trigger::Manual);
    assert_eq!(report.outcome, Outcome::ManualIntervention);
}
