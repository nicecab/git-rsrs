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
const WAITING: &str = "remote has new commits; integration waits for a clean worktree";
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
fn rejected_edits_wait_for_a_clean_worktree_when_remote_is_ahead() {
    let f = Fixture::new();
    let mut c = f.config();
    reject(&mut c);
    f.commit(&f.a, "existing", "already committed\n");
    f.commit(&f.b, "shared.txt", "remote edit\n");
    git(&f.b, ["push"]);
    fs::write(f.a.join("shared.txt"), "local rejected edit\n").unwrap();
    let head = text(&f.a, &["rev-parse", "HEAD"]);
    let remote = text(&f.origin, &["rev-parse", "main"]);
    for _ in 0..2 {
        let report = sync_once(&c, Trigger::Manual);
        assert_eq!(report.outcome, Outcome::ValidationRejected, "{report:?}");
        assert!(!report.integrated && !report.pushed);
        assert!(report.diagnostics.iter().any(|d| d.message == WAITING));
        assert_eq!(
            text(&f.a, &["rev-parse", "refs/remotes/origin/main"]),
            remote
        );
        assert_eq!(text(&f.a, &["rev-parse", "HEAD"]), head);
        assert_eq!(
            fs::read_to_string(f.a.join("shared.txt")).unwrap(),
            "local rejected edit\n"
        );
        assert_eq!(text(&f.a, &["stash", "list"]), "");
        assert!(!f.a.join(".git/git-rsrs-integrate.json").exists());
        assert_eq!(text(&f.origin, &["rev-parse", "main"]), remote);
    }
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
fn review_edit_racing_the_commit_defers_integration_without_stashing() {
    let f = Fixture::new();
    f.commit(&f.b, "remote", "remote");
    git(&f.b, ["push"]);
    fs::write(f.a.join("local"), "committed by sync\n").unwrap();
    let barrier = barrier_upload_pack(&f);
    let remote = text(&f.origin, &["rev-parse", "main"]);
    let child = spawn_sync(&f, "version=1\nbranch='main'");
    let ready = barrier.wait();
    fs::write(f.a.join("shared.txt"), "EDIT AFTER COMMIT\n").unwrap();
    barrier.release();
    let report = finish_sync(child);
    assert!(ready);
    assert_eq!(report.outcome, Outcome::Deferred, "{report:?}");
    assert_eq!(
        Some(text(&f.a, &["rev-parse", "HEAD"])),
        report.committed,
        "{report:?}"
    );
    assert!(!report.integrated && !report.pushed);
    assert!(report.diagnostics.iter().any(|d| d.message == WAITING));
    assert_eq!(text(&f.a, &["stash", "list"]), "");
    assert!(!f.a.join(".git/git-rsrs-integrate.json").exists());
    assert_eq!(text(&f.origin, &["rev-parse", "main"]), remote);
    assert_eq!(
        fs::read_to_string(f.a.join("shared.txt")).unwrap(),
        "EDIT AFTER COMMIT\n"
    );
}

#[test]
fn review_edit_before_a_rejected_push_defers_the_retry_integration() {
    let f = Fixture::new();
    f.commit(&f.b, "remote", "remote");
    git(&f.b, ["push"]);
    fs::write(f.a.join("local"), "committed by sync\n").unwrap();
    let hooks = f.temp.path().join("hooks");
    fs::create_dir(&hooks).unwrap();
    let pushed_head = f.temp.path().join("pushed-head");
    let racing_remote = f.temp.path().join("racing-remote");
    let hook = hooks.join("pre-push");
    let script = format!(
        "#!/usr/bin/env python3\nimport os,subprocess,pathlib\nif not pathlib.Path({pushed_head:?}).exists():\n env={{k:v for k,v in os.environ.items() if not k.startswith('GIT_')}}\n def git(repo,*args): return subprocess.run(['git','-C',repo,*args],env=env,check=True,capture_output=True,text=True).stdout.strip()\n pathlib.Path({pushed_head:?}).write_text(git({a:?},'rev-parse','HEAD'))\n pathlib.Path({race:?}).write_text('race')\n git({b:?},'add','-A'); git({b:?},'commit','-m','race'); git({b:?},'push')\n pathlib.Path({racing_remote:?}).write_text(git({b:?},'rev-parse','HEAD'))\n pathlib.Path({edit:?}).write_text('EDIT BEFORE PUSH\\n')\n",
        pushed_head = pushed_head.to_str().unwrap(),
        racing_remote = racing_remote.to_str().unwrap(),
        a = f.a.to_str().unwrap(),
        b = f.b.to_str().unwrap(),
        race = f.b.join("remote-race").to_str().unwrap(),
        edit = f.a.join("shared.txt").to_str().unwrap(),
    );
    fs::write(&hook, script).unwrap();
    fs::set_permissions(hook, fs::Permissions::from_mode(0o755)).unwrap();
    git(
        &f.a,
        [
            std::ffi::OsStr::new("config"),
            "core.hooksPath".as_ref(),
            hooks.as_os_str(),
        ],
    );
    let report = sync_once(&f.config(), Trigger::Manual);
    assert_eq!(report.outcome, Outcome::Deferred, "{report:?}");
    assert!(report.committed.is_some() && report.integrated && !report.pushed);
    assert!(report.diagnostics.iter().any(|d| d.message == WAITING));
    let head = fs::read_to_string(pushed_head).unwrap();
    assert_eq!(text(&f.a, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        text(&f.a, &["rev-parse", "HEAD^"]),
        text(&f.b, &["rev-parse", "HEAD^"])
    );
    let racing = fs::read_to_string(racing_remote).unwrap();
    assert_eq!(text(&f.origin, &["rev-parse", "main"]), racing);
    assert_eq!(
        text(&f.a, &["rev-parse", "refs/remotes/origin/main"]),
        racing
    );
    assert_eq!(text(&f.a, &["stash", "list"]), "");
    assert!(!f.a.join(".git/git-rsrs-integrate.json").exists());
    assert_eq!(
        fs::read_to_string(f.a.join("shared.txt")).unwrap(),
        "EDIT BEFORE PUSH\n"
    );
}

#[test]
fn review_edit_after_the_dirty_check_is_never_autostashed_by_git_config() {
    let f = Fixture::new();
    f.commit(&f.a, "local", "local");
    f.commit(&f.b, "shared.txt", "remote edit\n");
    git(&f.b, ["push"]);
    git(&f.a, ["config", "rebase.autoStash", "true"]);
    // A `git` wrapper on PATH edits a tracked file just before the rebase
    // process starts, i.e. after the dirty check and before Git's own
    // autostash decision (a pre-rebase hook runs too late for that).
    let wrap = f.temp.path().join("wrap");
    fs::create_dir(&wrap).unwrap();
    let marker = f.temp.path().join("edited");
    let script = format!(
        "#!/usr/bin/env python3\nimport os,sys,shutil,pathlib\nif 'rebase' in sys.argv[1:] and not pathlib.Path({marker:?}).exists():\n pathlib.Path({marker:?}).touch()\n pathlib.Path({edit:?}).write_text('EDIT BEFORE REBASE\\n')\nreal=shutil.which('git',path=os.pathsep.join(p for p in os.environ['PATH'].split(os.pathsep) if p!={wrap:?}))\nos.execv(real,[real,*sys.argv[1:]])\n",
        marker = marker.to_str().unwrap(),
        edit = f.a.join("shared.txt").to_str().unwrap(),
        wrap = wrap.to_str().unwrap(),
    );
    fs::write(wrap.join("git"), script).unwrap();
    fs::set_permissions(wrap.join("git"), fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", wrap.display(), std::env::var("PATH").unwrap());
    let child = spawn_sync_env(&f, "version=1\nbranch='main'", &[("PATH", path.as_ref())]);
    let head = text(&f.a, &["rev-parse", "HEAD"]);
    let remote = text(&f.origin, &["rev-parse", "main"]);
    let report = finish_sync(child);
    assert!(marker.exists());
    assert_eq!(report.outcome, Outcome::Deferred, "{report:?}");
    assert!(report.diagnostics.iter().any(|d| d.message == WAITING));
    assert_eq!(text(&f.a, &["stash", "list"]), "", "{report:?}");
    assert_eq!(text(&f.a, &["rev-parse", "HEAD"]), head, "{report:?}");
    assert_eq!(
        fs::read_to_string(f.a.join("shared.txt")).unwrap(),
        "EDIT BEFORE REBASE\n"
    );
    assert!(!report.pushed);
    assert_eq!(text(&f.origin, &["rev-parse", "main"]), remote);
    assert!(!f.a.join(".git/rebase-merge").exists());
    assert!(!f.a.join(".git/git-rsrs-integrate.json").exists());
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
