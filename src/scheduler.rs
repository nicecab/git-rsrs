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
}
#[derive(Debug)]
pub struct Scheduler {
    quiet: Duration,
    interval: Duration,
    last_periodic: Instant,
    last_change: Option<Instant>,
    generation: u64,
    in_flight: Option<u64>,
    pending_local: bool,
    pending_periodic: bool,
    initial: bool,
    paused: bool,
    shutdown: bool,
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
            pending_local: dirty,
            pending_periodic: true,
            initial: true,
            paused: false,
            shutdown: false,
        }
    }
    pub fn on_event(&mut self, event: SchedulerEvent, now: Instant) -> Option<RunRequest> {
        match event {
            SchedulerEvent::LocalChange => {
                self.generation = self.generation.wrapping_add(1);
                self.last_change = Some(now);
                self.pending_local = true;
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
                {
                    self.pending_local = false;
                    self.last_change = None;
                }
                if outcome == Outcome::Deferred {
                    self.pending_local = true;
                    self.last_change = Some(now);
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
        let local_ready = self.pending_local
            && self
                .last_change
                .is_some_and(|last| now.duration_since(last) >= self.quiet);
        if local_ready || (!self.pending_local && self.pending_periodic) {
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
            Some(RunRequest { trigger })
        } else {
            None
        }
    }
}
