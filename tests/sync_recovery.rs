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
    time::Duration,
};
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
    let mut c = f.config();
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
    let mut c = f.config();
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
    let mut c = f.config();
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
fn rebase_conflict_retains_operation_and_local_commits_for_manual_recovery() {
    let f = Fixture::new();
    let c = f.config();
    f.commit(&f.a, "shared.txt", "local committed\n");
    let head = text(&f.a, &["rev-parse", "HEAD"]);
    f.commit(&f.b, "shared.txt", "remote committed\n");
    git(&f.b, ["push"]);
    let report = sync_once(&c, Trigger::Manual);
    assert_eq!(report.outcome, Outcome::ManualIntervention, "{report:?}");
    assert_eq!(text(&f.a, &["rev-parse", "refs/heads/main"]), head);
    assert_eq!(
        text(&f.a, &["show", &format!("{head}:shared.txt")]),
        "local committed"
    );
    let conflict = fs::read_to_string(f.a.join("shared.txt")).unwrap();
    assert!(conflict.contains("local committed") && conflict.contains("remote committed"));
    assert!(f.a.join(".git/rebase-merge").exists());
    assert!(f.a.join(".git/git-rsrs-integrate.json").exists());
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
    wait_for(&ready);
    terminate(&mut child, Duration::from_secs(4));
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
    let report = sync_once(&f.config(), Trigger::Manual);
    assert_eq!(report.outcome, Outcome::ManualIntervention);
}

#[test]
fn review_existing_stash_apply_conflict_is_never_committed() {
    let f = Fixture::new();
    let c = f.config();
    fs::write(f.a.join("shared.txt"), "stashed local\n").unwrap();
    git(&f.a, ["stash", "push"]);
    f.commit(&f.a, "shared.txt", "new committed\n");
    let g = git_rsrs::git::Git::open(&f.a, git_rsrs::process::ProcessRunner).unwrap();
    assert!(g.run(["stash", "apply"]).is_err());
    let head = text(&f.a, &["rev-parse", "HEAD"]);
    let bytes = fs::read(f.a.join("shared.txt")).unwrap();
    let index = fs::read(f.a.join(".git/index")).unwrap();
    let remote = text(&f.origin, &["rev-parse", "main"]);
    let report = sync_once(&c, Trigger::Manual);
    assert_eq!(report.outcome, Outcome::ManualIntervention, "{report:?}");
    assert_eq!(text(&f.a, &["rev-parse", "HEAD"]), head);
    assert_eq!(text(&f.origin, &["rev-parse", "main"]), remote);
    assert_eq!(fs::read(f.a.join("shared.txt")).unwrap(), bytes);
    assert_eq!(fs::read(f.a.join(".git/index")).unwrap(), index);
}
#[test]
fn review_failed_integration_never_aborts_over_concurrent_editor_content() {
    let f = Fixture::new();
    f.commit(&f.a, "local", "local");
    f.commit(&f.b, "remote", "remote");
    git(&f.b, ["push"]);
    let barrier = barrier_hook(&f, "pre-rebase");
    let child = spawn_sync(&f, "version=1\nbranch='main'");
    let ready = barrier.wait();
    fs::write(f.a.join("shared.txt"), "EDITOR DURING INTEGRATION\n").unwrap();
    barrier.release();
    let report = finish_sync(child);
    assert!(ready);
    assert_eq!(report.outcome, Outcome::ManualIntervention, "{report:?}");
    assert_eq!(
        fs::read_to_string(f.a.join("shared.txt")).unwrap(),
        "EDITOR DURING INTEGRATION\n"
    );
    assert!(f.a.join(".git/git-rsrs-integrate.json").exists());
}
#[test]
fn review_ignored_files_and_directory_collisions_are_preserved() {
    for (local, incoming) in [
        ("ignored-file", "ignored-file"),
        ("ignored-dir/secret", "ignored-dir"),
        ("ignored-parent", "ignored-parent/remote"),
    ] {
        let f = Fixture::new();
        let c = f.config();
        fs::write(
            f.a.join(".git/info/exclude"),
            "ignored-file\nignored-dir/\nignored-parent\n",
        )
        .unwrap();
        let local_path = f.a.join(local);
        fs::create_dir_all(local_path.parent().unwrap()).unwrap();
        fs::write(&local_path, "LOCAL SECRET\n").unwrap();
        fs::create_dir_all(f.b.join(incoming).parent().unwrap()).unwrap();
        f.commit(&f.b, incoming, "remote value\n");
        git(&f.b, ["push"]);
        let head = text(&f.a, &["rev-parse", "HEAD"]);
        let report = sync_once(&c, Trigger::Manual);
        assert_eq!(
            report.outcome,
            Outcome::ManualIntervention,
            "{local}: {report:?}"
        );
        assert_eq!(fs::read_to_string(local_path).unwrap(), "LOCAL SECRET\n");
        assert_eq!(text(&f.a, &["rev-parse", "HEAD"]), head);
        assert!(!report.pushed);
    }
}
#[test]
fn review_retained_autostash_without_unmerged_entries_stops_before_push() {
    let f = Fixture::new();
    f.commit(&f.a, "local", "local");
    f.commit(&f.b, "remote", "remote");
    git(&f.b, ["push"]);
    fs::write(f.a.join("shared.txt"), "REJECTED BEFORE REBASE\n").unwrap();
    let barrier = barrier_hook(&f, "post-rewrite");
    let child = spawn_sync(
        &f,
        "version=1\nbranch='main'\n[[hooks.pre_commit]]\ncommand=['python3','-c','import sys;sys.exit(3)']\nreject_exit_codes=[3]",
    );
    let ready = barrier.wait();
    fs::write(f.a.join("shared.txt"), "NEW EDIT DURING REBASE\n").unwrap();
    barrier.release();
    let report = finish_sync(child);
    assert!(ready);
    assert_eq!(text(&f.a, &["ls-files", "--unmerged"]), "");
    assert_eq!(report.outcome, Outcome::ManualIntervention, "{report:?}");
    assert!(!report.pushed);
    assert!(!report.recovery.is_empty());
    assert!(f.a.join(".git/git-rsrs-integrate.json").exists());
    assert_eq!(
        fs::read_to_string(f.a.join("shared.txt")).unwrap(),
        "NEW EDIT DURING REBASE\n"
    );
    let stash = text(&f.a, &["stash", "list", "--format=%H"]);
    assert!(!stash.is_empty());
    assert_eq!(
        text(
            &f.a,
            &[
                "show",
                &format!("{}:shared.txt", stash.lines().next().unwrap())
            ]
        ),
        "REJECTED BEFORE REBASE"
    );
}

#[test]
fn review_ignored_content_is_protected_from_intermediate_replayed_commits() {
    let f = Fixture::new();
    let c = f.config();
    f.commit(&f.a, "temporary", "committed temporarily");
    fs::remove_file(f.a.join("temporary")).unwrap();
    git(&f.a, ["add", "-A"]);
    git(&f.a, ["commit", "-m", "remove temporary"]);
    f.commit(&f.b, "remote", "incoming");
    git(&f.b, ["push"]);
    fs::write(f.a.join(".git/info/exclude"), "temporary\n").unwrap();
    fs::write(f.a.join("temporary"), "LOCAL SECRET\n").unwrap();
    let head = text(&f.a, &["rev-parse", "HEAD"]);
    let report = sync_once(&c, Trigger::Manual);
    assert_eq!(report.outcome, Outcome::ManualIntervention, "{report:?}");
    assert_eq!(
        fs::read_to_string(f.a.join("temporary")).unwrap(),
        "LOCAL SECRET\n"
    );
    assert_eq!(text(&f.a, &["rev-parse", "HEAD"]), head);
    assert!(!report.pushed);
}
