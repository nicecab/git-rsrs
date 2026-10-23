use git_rsrs::{
    outcome::{Outcome, Trigger},
    scheduler::{Scheduler, SchedulerEvent},
};
use std::time::{Duration, Instant};
fn seconds(n: u64) -> Duration {
    Duration::from_secs(n)
}
#[test]
fn thirty_seconds_of_quiet_reset_on_each_edit_and_ticks_do_not_bypass() {
    let t = Instant::now();
    let mut s = Scheduler::new(seconds(30), seconds(5), t, true);
    assert!(
        s.on_event(SchedulerEvent::LocalChange, t + seconds(20))
            .is_none()
    );
    assert!(
        s.on_event(SchedulerEvent::LocalChange, t + seconds(35))
            .is_none()
    );
    assert!(
        s.on_event(SchedulerEvent::PeriodicTick, t + seconds(40))
            .is_none()
    );
    assert!(
        s.on_event(SchedulerEvent::PeriodicTick, t + seconds(64))
            .is_none()
    );
    assert_eq!(
        s.on_event(SchedulerEvent::PeriodicTick, t + seconds(65))
            .unwrap()
            .trigger,
        Trigger::LocalChange
    );
    assert!(
        s.on_event(SchedulerEvent::PeriodicTick, t + seconds(100))
            .is_none()
    );
}
#[test]
fn events_during_a_run_remain_pending_and_only_one_run_is_active() {
    let t = Instant::now();
    let mut s = Scheduler::new(seconds(30), seconds(300), t, false);
    assert_eq!(
        s.on_event(SchedulerEvent::PeriodicTick, t).unwrap().trigger,
        Trigger::Initial
    );
    assert!(
        s.on_event(SchedulerEvent::LocalChange, t + seconds(1))
            .is_none()
    );
    assert!(
        s.on_event(SchedulerEvent::PeriodicTick, t + seconds(40))
            .is_none()
    );
    assert_eq!(
        s.on_event(
            SchedulerEvent::RunFinished(Outcome::Synced),
            t + seconds(41)
        )
        .unwrap()
        .trigger,
        Trigger::LocalChange
    );
    assert!(
        s.on_event(
            SchedulerEvent::RunFinished(Outcome::Synced),
            t + seconds(42)
        )
        .is_none()
    );
}
#[test]
fn remote_ticks_run_when_idle_but_continuous_edits_can_wait_forever() {
    let t = Instant::now();
    let mut s = Scheduler::new(seconds(30), seconds(300), t, false);
    s.on_event(SchedulerEvent::PeriodicTick, t).unwrap();
    assert!(
        s.on_event(SchedulerEvent::RunFinished(Outcome::Noop), t + seconds(1))
            .is_none()
    );
    assert_eq!(
        s.on_event(SchedulerEvent::PeriodicTick, t + seconds(300))
            .unwrap()
            .trigger,
        Trigger::Periodic
    );
    s.on_event(SchedulerEvent::RunFinished(Outcome::Noop), t + seconds(301));
    for n in (310..2000).step_by(10) {
        assert!(
            s.on_event(SchedulerEvent::LocalChange, t + seconds(n))
                .is_none()
        );
        assert!(
            s.on_event(SchedulerEvent::PeriodicTick, t + seconds(n + 1))
                .is_none()
        );
    }
}
#[test]
fn recovery_pause_shutdown_and_deferred_runs_do_not_start_early() {
    let t = Instant::now();
    let mut s = Scheduler::new(seconds(30), seconds(5), t, false);
    s.on_event(SchedulerEvent::PeriodicTick, t).unwrap();
    assert!(
        s.on_event(
            SchedulerEvent::RunFinished(Outcome::Deferred),
            t + seconds(1)
        )
        .is_none()
    );
    assert!(
        s.on_event(SchedulerEvent::PeriodicTick, t + seconds(30))
            .is_none()
    );
    s.on_event(SchedulerEvent::PeriodicTick, t + seconds(31))
        .unwrap();
    s.on_event(
        SchedulerEvent::RunFinished(Outcome::ManualIntervention),
        t + seconds(32),
    );
    assert!(
        s.on_event(SchedulerEvent::LocalChange, t + seconds(100))
            .is_none()
    );
    assert!(
        s.on_event(SchedulerEvent::PeriodicTick, t + seconds(200))
            .is_none()
    );
    let mut s = Scheduler::new(seconds(30), seconds(5), t, true);
    s.on_event(SchedulerEvent::Shutdown, t);
    assert!(
        s.on_event(SchedulerEvent::PeriodicTick, t + seconds(100))
            .is_none()
    );
}
