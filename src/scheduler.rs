use crate::outcome::{Outcome, Trigger};
use std::time::{Duration, Instant};
#[derive(Debug, Clone, Copy)]
pub enum SchedulerEvent {
    LocalChange,
    PeriodicTick,
    RunFinished(Outcome),
    Shutdown,
}
#[derive(Debug, Clone, Copy)]
pub struct RunRequest {
    pub trigger: Trigger,
    pub lock_retry: bool,
}
#[derive(Debug)]
pub struct Scheduler {
    quiet: Duration,
    interval: Duration,
    last_periodic: Instant,
    last_change: Option<Instant>,
    generation: u64,
    in_flight: Option<u64>,
    pending_periodic: bool,
    initial: bool,
    paused: bool,
    shutdown: bool,
    lock_retry_at: Option<Instant>,
    lock_retry_delay: Duration,
}
impl Scheduler {
    pub fn new(quiet: Duration, interval: Duration, now: Instant, dirty: bool) -> Self {
        Self {
            quiet,
            interval,
            last_periodic: now,
            last_change: dirty.then_some(now),
            generation: u64::from(dirty),
            in_flight: None,
            pending_periodic: true,
            initial: true,
            paused: false,
            shutdown: false,
            lock_retry_at: None,
            lock_retry_delay: Duration::from_secs(1),
        }
    }
    /// Clears the in-flight request and keeps local work pending for another quiet
    /// period, whether the run was deferred or never started. The generation is
    /// left alone: deferring is not an edit.
    pub fn defer_local(&mut self, now: Instant) {
        self.in_flight = None;
        self.last_change = Some(now);
    }
    pub fn on_event(&mut self, event: SchedulerEvent, now: Instant) -> Option<RunRequest> {
        match event {
            SchedulerEvent::LocalChange => {
                self.generation = self.generation.wrapping_add(1);
                self.last_change = Some(now);
            }
            SchedulerEvent::PeriodicTick => {
                if now.duration_since(self.last_periodic) >= self.interval {
                    self.pending_periodic = true;
                    self.last_periodic = now;
                }
            }
            SchedulerEvent::RunFinished(outcome) => {
                if let Some(generation) = self.in_flight.take()
                    && generation == self.generation
                    && outcome != Outcome::LockTimeout
                {
                    self.last_change = None;
                }
                if outcome == Outcome::LockTimeout {
                    self.lock_retry_at = Some(now + self.lock_retry_delay);
                    self.lock_retry_delay = (self.lock_retry_delay * 2).min(Duration::from_secs(5));
                } else {
                    self.lock_retry_at = None;
                    self.lock_retry_delay = Duration::from_secs(1);
                }
                if outcome == Outcome::Deferred {
                    self.defer_local(now);
                }
                if outcome == Outcome::ManualIntervention {
                    self.paused = true;
                }
            }
            SchedulerEvent::Shutdown => self.shutdown = true,
        }
        if self.shutdown || self.paused || self.in_flight.is_some() {
            return None;
        }
        if self.lock_retry_at.is_some_and(|at| now < at) {
            return None;
        }
        let local_ready = self
            .last_change
            .is_some_and(|last| now.duration_since(last) >= self.quiet);
        let lock_retry = self.lock_retry_at.is_some_and(|at| now >= at);
        if lock_retry || local_ready || (self.last_change.is_none() && self.pending_periodic) {
            let trigger = if local_ready {
                Trigger::LocalChange
            } else if self.initial {
                Trigger::Initial
            } else {
                Trigger::Periodic
            };
            self.initial = false;
            self.pending_periodic = false;
            self.last_periodic = now;
            self.in_flight = Some(self.generation);
            self.lock_retry_at = None;
            Some(RunRequest {
                trigger,
                lock_retry,
            })
        } else {
            None
        }
    }
}
