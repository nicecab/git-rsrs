mod common;
use common::*;
use git_rsrs::{
    git::Git,
    lock::{CheckoutLock, LockError},
    process::ProcessRunner,
};
use std::{
    fs,
    io::{BufRead, BufReader},
    process::{Command, Stdio},
    time::Duration,
};

#[test]
fn rust_lock_excludes_python_and_releases_without_unlinking() {
    let f = Fixture::new();
    let git = Git::open(&f.a, ProcessRunner).unwrap();
    let lock =
        CheckoutLock::acquire(&git, "docs-maintenance.lock", Duration::from_secs(1)).unwrap();
    let path = git.path("docs-maintenance.lock").unwrap();
    let probe = || {
        Command::new("python3").arg("-c").arg("import fcntl,sys; f=open(sys.argv[1],'a+');\ntry: fcntl.flock(f,fcntl.LOCK_EX|fcntl.LOCK_NB)\nexcept BlockingIOError: sys.exit(73)").arg(&path).status().unwrap().code()
    };
    assert_eq!(probe(), Some(73));
    let metadata: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    for key in ["pid", "hostname", "operation", "acquired_at"] {
        assert!(metadata[key].is_string());
    }
    drop(lock);
    assert!(path.exists());
    assert_eq!(probe(), Some(0));
}

#[test]
fn python_lock_times_out_then_killed_owner_releases_kernel_lock() {
    let f = Fixture::new();
    let git = Git::open(&f.a, ProcessRunner).unwrap();
    let path = git.path("docs-maintenance.lock").unwrap();
    let mut python=Command::new("python3").args(["-c","import fcntl,sys; f=open(sys.argv[1],'a+'); fcntl.flock(f,fcntl.LOCK_EX); print('ready',flush=True); sys.stdin.read()"])
        .arg(&path).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    let mut ready = String::new();
    BufReader::new(python.stdout.take().unwrap())
        .read_line(&mut ready)
        .unwrap();
    assert_eq!(ready, "ready\n");
    let result = CheckoutLock::acquire(&git, "docs-maintenance.lock", Duration::from_millis(50));
    python.kill().unwrap();
    python.wait().unwrap();
    assert!(matches!(result, Err(LockError::Timeout { .. })));
    let _lock =
        CheckoutLock::acquire(&git, "docs-maintenance.lock", Duration::from_secs(1)).unwrap();
}

#[test]
fn linked_worktrees_have_distinct_lock_paths() {
    let f = Fixture::new();
    let linked = f.temp.path().join("linked");
    git(
        &f.a,
        [
            std::ffi::OsStr::new("worktree"),
            "add".as_ref(),
            "-b".as_ref(),
            "feature".as_ref(),
            linked.as_os_str(),
        ],
    );
    let a = Git::open(&f.a, ProcessRunner).unwrap();
    let b = Git::open(&linked, ProcessRunner).unwrap();
    assert_ne!(
        a.path("git-rsrs.lock").unwrap(),
        b.path("git-rsrs.lock").unwrap()
    );
    let _one = CheckoutLock::acquire(&a, "git-rsrs.lock", Duration::from_secs(1)).unwrap();
    let _two = CheckoutLock::acquire(&b, "git-rsrs.lock", Duration::from_secs(1)).unwrap();
}
