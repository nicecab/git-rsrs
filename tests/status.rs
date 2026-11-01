use git_rsrs::{
    outcome::{HookFailure, Outcome, SyncReport, Trigger},
    status::{RECENT_LEN, RecentAttempt, Status, next, record},
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn now() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_793_500_000)
}
fn report(outcome: Outcome) -> SyncReport {
    let mut report = SyncReport::new(Trigger::Periodic);
    report.outcome = outcome;
    report
}
fn failing_hook() -> HookFailure {
    HookFailure {
        phase: "on_result".into(),
        timed_out: true,
        message: "command exceeded its deadline".into(),
    }
}
fn run(outcomes: &[Outcome]) -> Status {
    let mut status: Option<Status> = None;
    for outcome in outcomes {
        status = Some(next(status.as_ref(), &report(*outcome), now(), 1));
    }
    status.unwrap()
}

#[test]
fn healthy_attempt_counts_and_sets_success() {
    let status = next(None, &report(Outcome::Synced), now(), 42);
    assert_eq!(status.version, 1);
    assert_eq!(status.attempt_seq, 1);
    assert_eq!(status.consecutive_successes, 1);
    assert_eq!(status.consecutive_failures, 0);
    assert_eq!(
        status.recent,
        vec![RecentAttempt {
            seq: 1,
            healthy: true
        }]
    );
    let stamp = humantime::format_rfc3339_seconds(now()).to_string();
    assert_eq!(status.last_success_at, Some(stamp.clone()));
    assert_eq!(status.last_attempt_at, stamp);
    assert_eq!(status.pid, 42);
    assert_eq!(status.git_rsrs_version, env!("CARGO_PKG_VERSION"));
}
#[test]
fn hook_failure_makes_noop_unhealthy_but_still_a_success_time() {
    let mut noop = report(Outcome::Noop);
    noop.hook_failures.push(failing_hook());
    let status = next(None, &noop, now(), 1);
    assert_eq!(status.consecutive_failures, 1);
    assert_eq!(status.consecutive_successes, 0);
    assert!(!status.recent[0].healthy);
    assert!(status.last_success_at.is_some());
    assert_eq!(status.hook_failures, vec![failing_hook()]);
}
#[test]
fn uncounted_outcomes_leave_counters_alone() {
    let previous = run(&[
        Outcome::Synced,
        Outcome::Synced,
        Outcome::Failed,
        Outcome::Failed,
    ]);
    assert_eq!(previous.attempt_seq, 4);
    assert_eq!(previous.consecutive_failures, 2);
    for outcome in [
        Outcome::BranchSkipped,
        Outcome::Offline,
        Outcome::Deferred,
        Outcome::LockTimeout,
        Outcome::ValidationRejected,
    ] {
        let later = now() + Duration::from_secs(60);
        let status = next(Some(&previous), &report(outcome), later, 1);
        assert_eq!(status.attempt_seq, 4, "{outcome:?}");
        assert_eq!(status.consecutive_failures, 2, "{outcome:?}");
        assert_eq!(status.consecutive_successes, 0, "{outcome:?}");
        assert_eq!(status.recent, previous.recent, "{outcome:?}");
        assert_eq!(status.last_outcome, outcome);
        assert_eq!(
            status.last_attempt_at,
            humantime::format_rfc3339_seconds(later).to_string()
        );
        assert_eq!(status.last_success_at, previous.last_success_at);
    }
}
#[test]
fn success_resets_failures_and_vice_versa() {
    let status = run(&[Outcome::Failed, Outcome::TimedOut, Outcome::Synced]);
    assert_eq!(status.consecutive_failures, 0);
    assert_eq!(status.consecutive_successes, 1);
    let status = next(Some(&status), &report(Outcome::CheckFailed), now(), 1);
    assert_eq!(status.consecutive_successes, 0);
    assert_eq!(status.consecutive_failures, 1);
}
#[test]
fn failure_keeps_previous_success_time() {
    let first = next(None, &report(Outcome::Synced), now(), 1);
    let later = now() + Duration::from_secs(600);
    let status = next(
        Some(&first),
        &report(Outcome::AuthenticationFailed),
        later,
        1,
    );
    assert_eq!(status.last_success_at, first.last_success_at);
}
#[test]
fn recent_is_capped_newest_first() {
    let status = run(&[Outcome::Noop; 12]);
    assert_eq!(status.recent.len(), RECENT_LEN);
    assert_eq!(RECENT_LEN, 10);
    assert_eq!(status.recent[0].seq, 12);
    assert_eq!(status.recent[9].seq, 3);
}
#[test]
fn message_is_first_sync_diagnostic() {
    let mut failed = report(Outcome::Failed);
    failed.diagnostic("on_result", "hook noise");
    failed.diagnostic("sync", "boom\nmore");
    assert_eq!(next(None, &failed, now(), 1).message, Some("boom".into()));
    assert_eq!(next(None, &report(Outcome::Noop), now(), 1).message, None);
}

#[test]
fn record_persists_across_calls() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("status.json");
    record(&path, &report(Outcome::Synced)).unwrap();
    let written = record(&path, &report(Outcome::Synced)).unwrap();
    let parsed: Status = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(parsed.attempt_seq, 2);
    assert_eq!(parsed, written);
}
#[test]
fn corrupt_file_is_replaced() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("status.json");
    std::fs::write(&path, "{not json").unwrap();
    let status = record(&path, &report(Outcome::Synced)).unwrap();
    assert_eq!(status.attempt_seq, 1);
}
#[test]
fn concurrent_records_lose_no_increment() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("status.json");
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let path = path.clone();
            std::thread::spawn(move || {
                for _ in 0..25 {
                    record(&path, &report(Outcome::Synced)).unwrap();
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    let parsed: Status = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(parsed.attempt_seq, 200);
}

#[test]
fn counters_since_marks_each_counter_epoch() {
    let first = next(None, &report(Outcome::Synced), now(), 1);
    assert_eq!(
        first.counters_since,
        humantime::format_rfc3339_seconds(now()).to_string()
    );
    let later = next(
        Some(&first),
        &report(Outcome::Synced),
        now() + Duration::from_secs(600),
        1,
    );
    assert_eq!(later.counters_since, first.counters_since);
}
#[test]
fn unreadable_previous_file_is_an_error_not_a_reset() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("status.json");
    use std::os::unix::fs::PermissionsExt;
    record(&path, &report(Outcome::Synced)).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    let result = record(&path, &report(Outcome::Synced));
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(result.is_err());
    let kept: Status = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(kept.attempt_seq, 1);
}
