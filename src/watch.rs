use crate::{
    config::Config,
    git::{COMMIT_JOURNAL, Git, GitError, INTEGRATE_JOURNAL, RepositoryState, snapshot::Snapshot},
    outcome::{Outcome, SyncReport, Trigger},
    process::{CANCELLED, ProcessRunner},
    scheduler::{RunRequest, Scheduler, SchedulerEvent},
    sync::sync_once_guarded,
};
use notify::{Event, EventKind, RecursiveMode, Watcher};
use std::{
    path::{Component, Path},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use thiserror::Error;
#[derive(Debug, Error)]
pub enum WatchError {
    #[error(transparent)]
    Git(#[from] GitError),
    #[error(transparent)]
    Notify(#[from] notify::Error),
    #[error("synchronization worker failed")]
    Worker,
    #[error("active synchronization was interrupted; inspect its report before restarting")]
    Interrupted,
}
pub fn event_relevant(git: &Git, path: &Path) -> Result<bool, GitError> {
    if path.starts_with(&git.git_dir) {
        return Ok(false);
    }
    let Ok(relative) = path.strip_prefix(&git.root) else {
        return Ok(false);
    };
    if relative
        .components()
        .any(|component| matches!(component,Component::Normal(part) if part==".git"))
    {
        return Ok(false);
    }
    if relative.as_os_str().is_empty() {
        return git.is_dirty();
    }
    match git.run([
        std::ffi::OsStr::new("ls-files"),
        "--error-unmatch".as_ref(),
        "--".as_ref(),
        relative.as_os_str(),
    ]) {
        Ok(_) => return Ok(true),
        Err(GitError::Command { ref output, .. }) if output.code == Some(1) => {}
        Err(e) => return Err(e),
    }
    match git.run([
        std::ffi::OsStr::new("check-ignore"),
        "--quiet".as_ref(),
        "--no-index".as_ref(),
        "--".as_ref(),
        relative.as_os_str(),
    ]) {
        Ok(_) => Ok(false),
        Err(GitError::Command { ref output, .. }) if output.code == Some(1) => Ok(true),
        Err(e) => Err(e),
    }
}
pub fn watch_with_reports(config: &Config, emit: impl FnMut(SyncReport)) -> Result<(), WatchError> {
    match watch_loop(config, emit) {
        Err(WatchError::Git(GitError::Process(crate::process::ProcessError::Interrupted(_))))
            if CANCELLED.load(Ordering::Relaxed) =>
        {
            Ok(())
        }
        result => result,
    }
}

fn watch_loop(config: &Config, mut emit: impl FnMut(SyncReport)) -> Result<(), WatchError> {
    let mut git = Git::open(&config.repo, ProcessRunner)?;
    git.timeout = config.git_timeout;
    let (event_tx, event_rx) = mpsc::sync_channel::<notify::Result<Event>>(1024);
    let overflow = Arc::new(AtomicBool::new(false));
    let callback_overflow = overflow.clone();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
        if matches!(&event,Ok(e) if matches!(e.kind,EventKind::Access(_))) {
            return;
        }
        if event_tx.try_send(event).is_err() {
            callback_overflow.store(true, Ordering::Relaxed);
        }
    })?;
    watcher.watch(&git.root, RecursiveMode::Recursive)?;
    let mut scheduler = Scheduler::new(
        config.quiet_period,
        config.interval,
        Instant::now(),
        git.is_dirty()?,
    );
    let (report_tx, report_rx) = mpsc::channel::<SyncReport>();
    let mut worker = WorkerGuard(None);
    let changes = Arc::new(AtomicU64::new(0));
    let launcher = Launcher {
        config,
        git: &git,
        tx: &report_tx,
        changes: &changes,
    };
    eprintln!("watching {}", git.root.display());
    let result = (|| {
        loop {
            if CANCELLED.load(Ordering::Relaxed) {
                return Ok(());
            }
            let finished = if let Ok(report) = report_rx.try_recv() {
                if let Some(handle) = worker.take() {
                    handle.join().map_err(|_| WatchError::Worker)?;
                }
                Some(report)
            } else if worker.as_ref().is_some_and(|handle| handle.is_finished()) {
                // A completed worker must publish a report unless it panicked.
                if worker.take().expect("present worker").join().is_err() {
                    return Err(WatchError::Worker);
                }
                Some(
                    report_rx
                        .recv_timeout(Duration::from_millis(50))
                        .map_err(|_| WatchError::Worker)?,
                )
            } else {
                None
            };
            if let Some(report) = finished {
                let outcome = report.outcome;
                emit(report);
                if outcome == Outcome::ManualIntervention {
                    eprintln!(
                        "automatic synchronization paused; repair the reported state and restart watch"
                    );
                }
                if let Some(request) =
                    scheduler.on_event(SchedulerEvent::RunFinished(outcome), Instant::now())
                {
                    launcher.start(&mut scheduler, &mut worker, request)?;
                }
            }
            if overflow.swap(false, Ordering::Relaxed) {
                changes.fetch_add(1, Ordering::Relaxed);
                if git.is_dirty()?
                    && let Some(request) =
                        scheduler.on_event(SchedulerEvent::LocalChange, Instant::now())
                {
                    launcher.start(&mut scheduler, &mut worker, request)?;
                }
            }
            match event_rx.recv_timeout(Duration::from_millis(20)) {
                Ok(event) => {
                    if reconcile_event(&git, event)? {
                        changes.fetch_add(1, Ordering::Relaxed);
                        if let Some(request) =
                            scheduler.on_event(SchedulerEvent::LocalChange, Instant::now())
                        {
                            launcher.start(&mut scheduler, &mut worker, request)?;
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err(WatchError::Worker),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if let Some(request) = scheduler.on_event(SchedulerEvent::PeriodicTick, Instant::now())
            {
                launcher.start(&mut scheduler, &mut worker, request)?;
            }
        }
    })();
    // Cancellation may arrive while the event loop is inspecting Git, rather
    // than at its explicit cancellation check. Still join/report the worker.
    if CANCELLED.load(Ordering::Relaxed) {
        drop(watcher);
        if let Some(handle) = worker.take() {
            handle.join().map_err(|_| WatchError::Worker)?;
            if let Ok(report) = report_rx.try_recv() {
                emit(report);
            }
            return Err(WatchError::Interrupted);
        }
        return Ok(());
    }
    result
}
struct Launcher<'a> {
    config: &'a Config,
    git: &'a Git,
    tx: &'a mpsc::Sender<SyncReport>,
    changes: &'a Arc<AtomicU64>,
}
impl Launcher<'_> {
    fn start(
        &self,
        scheduler: &mut Scheduler,
        worker: &mut Option<JoinHandle<()>>,
        request: RunRequest,
    ) -> Result<(), WatchError> {
        let git = self.git;
        // Reconcile status before a periodic/initial run: native watcher delivery can lag.
        if request.trigger != Trigger::LocalChange && git.is_dirty()? {
            scheduler.on_event(
                SchedulerEvent::RunFinished(Outcome::Deferred),
                Instant::now(),
            );
            return Ok(());
        }
        let baseline = if request.trigger == Trigger::LocalChange
            && git.state()? == RepositoryState::Ready
            && !git.path(COMMIT_JOURNAL)?.exists()
            && !git.path(INTEGRATE_JOURNAL)?.exists()
        {
            Some(Snapshot::capture(git)?)
        } else {
            None
        };
        let changes = self.changes.clone();
        let generation = changes.load(Ordering::Relaxed);
        let observed_git = git.clone();
        let config = self.config.clone();
        let tx = self.tx.clone();
        debug_assert!(worker.is_none());
        *worker = Some(thread::spawn(move || {
            let report = sync_once_guarded(&config, request.trigger, || {
                changes.load(Ordering::Relaxed) == generation
                    && baseline
                        .as_ref()
                        .is_none_or(|snapshot| snapshot.unchanged(&observed_git).unwrap_or(false))
            });
            let _ = tx.send(report);
        }));
        Ok(())
    }
}

pub fn reconcile_event(git: &Git, event: notify::Result<Event>) -> Result<bool, GitError> {
    match event {
        Err(error) => {
            eprintln!("watch event error; reconciling Git status: {error}");
            git.is_dirty()
        }
        Ok(event) if event.need_rescan() => git.is_dirty(),
        Ok(event) => {
            for path in &event.paths {
                if event_relevant(git, path)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
    }
}
struct WorkerGuard(Option<JoinHandle<()>>);
impl std::ops::Deref for WorkerGuard {
    type Target = Option<JoinHandle<()>>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl std::ops::DerefMut for WorkerGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
impl Drop for WorkerGuard {
    fn drop(&mut self) {
        if let Some(worker) = self.0.take() {
            // Even an event-filter error must not abandon an active Git child.
            CANCELLED.store(true, Ordering::Relaxed);
            let _ = worker.join();
        }
    }
}
