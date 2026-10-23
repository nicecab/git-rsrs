mod common;
use common::*;
use git_rsrs::{git::Git, outcome::SyncReport, process::ProcessRunner, watch::event_relevant};
use std::{
    fs,
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};
struct WatchChild {
    child: Child,
    reports: Receiver<SyncReport>,
}
impl WatchChild {
    fn start(f: &Fixture, quiet: &str, interval: &str) -> Self {
        let cfg = f.temp.path().join("watch.toml");
        fs::write(
            &cfg,
            format!("version=1\nbranch='main'\nquiet_period='{quiet}'\ninterval='{interval}'\n"),
        )
        .unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_git-rsrs"))
            .arg("--repo")
            .arg(&f.a)
            .arg("--config")
            .arg(cfg)
            .args(["--json", "watch"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let (tx, rx) = mpsc::channel();
        let stdout = child.stdout.take().unwrap();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if let Ok(line) = line
                    && let Ok(report) = serde_json::from_str(&line)
                    && tx.send(report).is_err()
                {
                    break;
                }
            }
        });
        Self { child, reports: rx }
    }
    fn committed(&self) -> SyncReport {
        let deadline = Instant::now() + PHASE_TIMEOUT;
        loop {
            let report = self
                .reports
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap();
            if report.committed.is_some() {
                return report;
            }
        }
    }
}
impl Drop for WatchChild {
    fn drop(&mut self) {
        unsafe {
            libc::kill(self.child.id() as i32, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(4);
        while Instant::now() < deadline {
            if self.child.try_wait().unwrap().is_some() {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
#[test]
fn event_filter_keeps_tracked_ignored_paths_and_excludes_git_and_untracked_ignored() {
    let f = Fixture::new();
    f.commit(&f.a, ".gitignore", "*.ignored\n");
    fs::write(f.a.join("tracked.ignored"), "old").unwrap();
    git(&f.a, ["add", "-f", "tracked.ignored"]);
    git(&f.a, ["commit", "-m", "track"]);
    let g = Git::open(&f.a, ProcessRunner).unwrap();
    assert!(event_relevant(&g, &g.root.join("tracked.ignored")).unwrap());
    assert!(!event_relevant(&g, &g.root.join("new.ignored")).unwrap());
    assert!(!event_relevant(&g, &g.root.join(".git/index")).unwrap());
    assert!(event_relevant(&g, &g.root.join("deleted-path")).unwrap());
}
#[test]
fn watched_edits_wait_for_quiet_even_when_periodic_ticks_are_frequent() {
    let f = Fixture::new();
    let watcher = WatchChild::start(&f, "500ms", "100ms");
    watcher.reports.recv_timeout(PHASE_TIMEOUT).unwrap();
    let head = text(&f.a, &["rev-parse", "HEAD"]);
    fs::write(f.a.join("new"), "first").unwrap();
    thread::sleep(Duration::from_millis(200));
    fs::write(f.a.join("new"), "last").unwrap();
    thread::sleep(Duration::from_millis(100));
    assert_eq!(text(&f.a, &["rev-parse", "HEAD"]), head);
    let report = watcher.committed();
    assert!(report.pushed);
    assert_eq!(text(&f.origin, &["show", "main:new"]), "last");
}
#[test]
fn dirty_startup_waits_instead_of_committing_immediately() {
    let f = Fixture::new();
    let head = text(&f.a, &["rev-parse", "HEAD"]);
    fs::write(f.a.join("new"), "startup").unwrap();
    let watcher = WatchChild::start(&f, "500ms", "100ms");
    thread::sleep(Duration::from_millis(150));
    assert_eq!(text(&f.a, &["rev-parse", "HEAD"]), head);
    watcher.committed();
    assert_eq!(text(&f.origin, &["show", "main:new"]), "startup");
}
#[test]
fn idle_periodic_sync_receives_remote_changes_without_local_events() {
    let f = Fixture::new();
    let watcher = WatchChild::start(&f, "200ms", "200ms");
    watcher.reports.recv_timeout(PHASE_TIMEOUT).unwrap();
    f.commit(&f.b, "remote", "incoming");
    git(&f.b, ["push"]);
    let deadline = Instant::now() + PHASE_TIMEOUT;
    while !f.a.join("remote").exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    assert_eq!(fs::read_to_string(f.a.join("remote")).unwrap(), "incoming");
    let head = text(&f.a, &["rev-parse", "HEAD"]);
    thread::sleep(Duration::from_millis(700));
    assert_eq!(text(&f.a, &["rev-parse", "HEAD"]), head);
}

#[test]
fn watched_rename_and_delete_are_committed() {
    let f = Fixture::new();
    let watcher = WatchChild::start(&f, "200ms", "200ms");
    watcher.reports.recv_timeout(PHASE_TIMEOUT).unwrap();
    fs::rename(f.a.join("shared.txt"), f.a.join("renamed")).unwrap();
    watcher.committed();
    assert_eq!(text(&f.origin, &["show", "main:renamed"]), "initial");
    assert_eq!(
        text(&f.origin, &["ls-tree", "--name-only", "main", "shared.txt"]),
        ""
    );
    fs::remove_file(f.a.join("renamed")).unwrap();
    watcher.committed();
    assert_eq!(
        text(&f.origin, &["ls-tree", "--name-only", "main", "renamed"]),
        ""
    );
}

#[test]
fn watcher_error_reconciles_actual_dirty_status() {
    let f = Fixture::new();
    let g = Git::open(&f.a, ProcessRunner).unwrap();
    let error = || notify::Error::generic("lost events");
    assert!(!git_rsrs::watch::reconcile_event(&g, Err(error())).unwrap());
    fs::write(f.a.join("missed"), "keep").unwrap();
    assert!(git_rsrs::watch::reconcile_event(&g, Err(error())).unwrap());
}

#[test]
fn review_edits_during_lock_wait_must_receive_a_new_quiet_period() {
    use std::os::fd::AsRawFd;
    let f = Fixture::new();
    let lock = fs::File::create(f.a.join(".git/git-rsrs.lock")).unwrap();
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);
    fs::write(f.a.join("new"), "first").unwrap();
    let watcher = WatchChild::start(&f, "700ms", "100ms");
    thread::sleep(Duration::from_millis(1200));
    fs::write(f.a.join("new"), "LAST EDIT").unwrap();
    let last = Instant::now();
    unsafe {
        libc::flock(lock.as_raw_fd(), libc::LOCK_UN);
    }
    watcher.committed();
    assert!(
        last.elapsed() >= Duration::from_millis(700),
        "committed after {:?}",
        last.elapsed()
    );
    assert_eq!(text(&f.origin, &["show", "main:new"]), "LAST EDIT");
}
