mod common;
use common::*;
use git_rsrs::{
    git::{
        Git,
        snapshot::{CommitResult, Snapshot},
    },
    process::ProcessRunner,
};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
};

#[test]
fn candidate_commits_all_validated_changes_and_preserves_later_work() {
    let f = Fixture::new();
    fs::write(f.a.join("partial"), "staged\n").unwrap();
    git(&f.a, ["add", "partial"]);
    fs::write(f.a.join("partial"), "validated\n").unwrap();
    fs::remove_file(f.a.join("shared.txt")).unwrap();
    symlink("partial", f.a.join("link")).unwrap();
    fs::set_permissions(f.a.join("partial"), fs::Permissions::from_mode(0o755)).unwrap();
    let g = Git::open(&f.a, ProcessRunner).unwrap();
    let candidate = Snapshot::capture(&g).unwrap();
    let before = fs::read(g.path("index").unwrap()).unwrap();
    assert!(matches!(
        candidate.commit_validated(&g).unwrap(),
        CommitResult::Created(_)
    ));
    assert_ne!(fs::read(g.path("index").unwrap()).unwrap(), before);
    assert_eq!(text(&f.a, &["show", "HEAD:partial"]), "validated");
    assert_eq!(text(&f.a, &["show", "HEAD:link"]), "partial");
    assert!(text(&f.a, &["ls-tree", "HEAD", "partial"]).starts_with("100755"));
    assert!(g.status().unwrap().is_empty());
    assert!(!g.path("git-rsrs-commit.json").unwrap().exists());
}

#[test]
fn changed_content_or_real_index_defers_without_overwriting_it() {
    let f = Fixture::new();
    let g = Git::open(&f.a, ProcessRunner).unwrap();
    fs::write(f.a.join("shared.txt"), "candidate").unwrap();
    let head = g.head().unwrap();
    let before = fs::read(g.path("index").unwrap()).unwrap();
    let snapshot = Snapshot::capture(&g).unwrap();
    fs::write(f.a.join("shared.txt"), "later").unwrap();
    assert!(matches!(
        snapshot.commit_validated(&g).unwrap(),
        CommitResult::DeferredConcurrentChange
    ));
    assert_eq!(g.head().unwrap(), head);
    assert_eq!(fs::read(g.path("index").unwrap()).unwrap(), before);
    let snapshot = Snapshot::capture(&g).unwrap();
    fs::write(f.a.join("another"), "staged elsewhere").unwrap();
    git(&f.a, ["add", "another"]);
    let staged = fs::read(g.path("index").unwrap()).unwrap();
    assert!(matches!(
        snapshot.commit_validated(&g).unwrap(),
        CommitResult::DeferredConcurrentChange
    ));
    assert_eq!(fs::read(g.path("index").unwrap()).unwrap(), staged);
}

#[test]
fn native_git_hooks_run_and_signing_is_disabled_only_for_this_commit() {
    let f = Fixture::new();
    let g = Git::open(&f.a, ProcessRunner).unwrap();
    let hooks = f.temp.path().join("hooks");
    fs::create_dir(&hooks).unwrap();
    let marker = f.temp.path().join("native-hook-ran");
    let script = hooks.join("pre-commit");
    fs::write(
        &script,
        format!("#!/bin/sh\nprintf ran > '{}'\n", marker.display()),
    )
    .unwrap();
    fs::set_permissions(script, fs::Permissions::from_mode(0o755)).unwrap();
    git(
        &f.a,
        [
            std::ffi::OsStr::new("config"),
            "core.hooksPath".as_ref(),
            hooks.as_os_str(),
        ],
    );
    git(&f.a, ["config", "commit.gpgsign", "true"]);
    fs::write(f.a.join("new"), "content").unwrap();
    assert!(matches!(
        Snapshot::capture(&g).unwrap().commit_validated(&g).unwrap(),
        CommitResult::Created(_)
    ));
    assert!(marker.exists());
    assert_eq!(text(&f.a, &["config", "commit.gpgsign"]), "true");
}

#[test]
fn recovery_journal_blocks_new_snapshots_without_touching_staged_work() {
    let f = Fixture::new();
    let g = Git::open(&f.a, ProcessRunner).unwrap();
    fs::write(f.a.join("partial"), "staged").unwrap();
    git(&f.a, ["add", "partial"]);
    let before = fs::read(g.path("index").unwrap()).unwrap();
    fs::write(
        g.path("git-rsrs-commit.json").unwrap(),
        "{\"recovery\":true}",
    )
    .unwrap();
    assert!(Snapshot::capture(&g).is_err());
    assert_eq!(fs::read(g.path("index").unwrap()).unwrap(), before);
}

#[test]
fn edits_arriving_after_validation_stay_out_of_the_pinned_commit() {
    let f = Fixture::new();
    let g = Git::open(&f.a, ProcessRunner).unwrap();
    fs::write(f.a.join("shared.txt"), "validated\n").unwrap();
    let snapshot = Snapshot::capture(&g).unwrap();
    let hooks = f.temp.path().join("hooks");
    fs::create_dir(&hooks).unwrap();
    let script = hooks.join("pre-commit");
    fs::write(&script, "#!/bin/sh\nprintf 'later edit\n' > shared.txt\n").unwrap();
    fs::set_permissions(script, fs::Permissions::from_mode(0o755)).unwrap();
    git(
        &f.a,
        [
            std::ffi::OsStr::new("config"),
            "core.hooksPath".as_ref(),
            hooks.as_os_str(),
        ],
    );
    assert!(matches!(
        snapshot.commit_validated(&g).unwrap(),
        CommitResult::Created(_)
    ));
    assert_eq!(text(&f.a, &["show", "HEAD:shared.txt"]), "validated");
    assert_eq!(
        fs::read_to_string(f.a.join("shared.txt")).unwrap(),
        "later edit\n"
    );
    assert!(!g.status().unwrap().is_empty());
}

#[test]
fn failed_native_hook_keeps_live_index_and_cleans_owned_publication_files() {
    let f = Fixture::new();
    let g = Git::open(&f.a, ProcessRunner).unwrap();
    fs::write(f.a.join("shared.txt"), "candidate").unwrap();
    let before = fs::read(g.path("index").unwrap()).unwrap();
    let head = g.head().unwrap();
    let hooks = f.temp.path().join("hooks");
    fs::create_dir(&hooks).unwrap();
    let script = hooks.join("pre-commit");
    fs::write(&script, "#!/bin/sh\nexit 1\n").unwrap();
    fs::set_permissions(script, fs::Permissions::from_mode(0o755)).unwrap();
    git(
        &f.a,
        [
            std::ffi::OsStr::new("config"),
            "core.hooksPath".as_ref(),
            hooks.as_os_str(),
        ],
    );
    assert!(Snapshot::capture(&g).unwrap().commit_validated(&g).is_err());
    assert_eq!(g.head().unwrap(), head);
    assert_eq!(fs::read(g.path("index").unwrap()).unwrap(), before);
    assert!(!g.path("index.lock").unwrap().exists());
    assert!(!g.path("git-rsrs-commit.json").unwrap().exists());
}

#[test]
fn commit_worker() {
    if let Some(repo) = std::env::var_os("RSRS_TEST_COMMIT_REPO") {
        let g = Git::open(std::path::Path::new(&repo), ProcessRunner).unwrap();
        Snapshot::capture(&g).unwrap().commit_validated(&g).unwrap();
    }
}

#[test]
fn killed_commit_retains_a_journal_and_original_index_for_manual_recovery() {
    use std::{
        process::Command,
        time::{Duration, Instant},
    };
    let f = Fixture::new();
    let g = Git::open(&f.a, ProcessRunner).unwrap();
    fs::write(f.a.join("shared.txt"), "candidate").unwrap();
    let before = fs::read(g.path("index").unwrap()).unwrap();
    let head = g.head().unwrap();
    let hooks = f.temp.path().join("hooks");
    fs::create_dir(&hooks).unwrap();
    let ready = f.temp.path().join("ready");
    let script = hooks.join("pre-commit");
    fs::write(&script,format!("#!/usr/bin/env python3\nimport os,time\nopen({:?},'w').write(str(os.getpgrp()))\ntime.sleep(20)\n",ready.to_str().unwrap())).unwrap();
    fs::set_permissions(script, fs::Permissions::from_mode(0o755)).unwrap();
    git(
        &f.a,
        [
            std::ffi::OsStr::new("config"),
            "core.hooksPath".as_ref(),
            hooks.as_os_str(),
        ],
    );
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "commit_worker", "--nocapture"])
        .env("RSRS_TEST_COMMIT_REPO", &f.a)
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + PHASE_TIMEOUT;
    while !ready.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    child.kill().unwrap();
    child.wait().unwrap();
    if let Ok(pid) = fs::read_to_string(&ready)
        .unwrap_or_default()
        .parse::<i32>()
    {
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    assert!(ready.exists());
    assert_eq!(g.head().unwrap(), head);
    assert_eq!(fs::read(g.path("index").unwrap()).unwrap(), before);
    assert!(g.path("git-rsrs-commit.json").unwrap().exists());
    assert!(Snapshot::capture(&g).is_err());
}
