use crate::{
    config::Config,
    git::{Git, GitError, RepositoryState, snapshot::Snapshot},
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
        return Ok(!git.status()?.is_empty());
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
pub fn watch(config: &Config) -> Result<(), WatchError> {
    watch_with_reports(config, |report| println!("{}", report.outcome.as_str()))
}

pub fn watch_with_reports(
    config: &Config,
    mut emit: impl FnMut(SyncReport),
) -> Result<(), WatchError> {
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
        !git.status()?.is_empty(),
    );
    let (report_tx, report_rx) = mpsc::channel::<SyncReport>();
    let mut worker = WorkerGuard(None);
    let changes = Arc::new(AtomicU64::new(0));
    eprintln!("watching {}", git.root.display());
    loop {
        if CANCELLED.load(Ordering::Relaxed) {
            scheduler.on_event(SchedulerEvent::Shutdown, Instant::now());
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
        if let Ok(report) = report_rx.try_recv() {
            if let Some(handle) = worker.take() {
                handle.join().map_err(|_| WatchError::Worker)?;
            }
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
                start_run(
                    config,
                    &git,
                    &mut scheduler,
                    &mut worker,
                    &report_tx,
                    &changes,
                    request,
                )?;
            }
        } else if worker.as_ref().is_some_and(|handle| handle.is_finished()) {
            // A completed worker must publish a report unless it panicked.
            if worker.take().expect("present worker").join().is_err() {
                return Err(WatchError::Worker);
            }
            if let Ok(report) = report_rx.recv_timeout(Duration::from_millis(50)) {
                let outcome = report.outcome;
                emit(report);
                if let Some(request) =
                    scheduler.on_event(SchedulerEvent::RunFinished(outcome), Instant::now())
                {
                    start_run(
                        config,
                        &git,
                        &mut scheduler,
                        &mut worker,
                        &report_tx,
                        &changes,
                        request,
                    )?;
                }
            } else {
                return Err(WatchError::Worker);
            }
        }
        if overflow.load(Ordering::Relaxed) {
            changes.fetch_add(1, Ordering::Relaxed);
        }
        if overflow.swap(false, Ordering::Relaxed)
            && !git.status()?.is_empty()
            && let Some(request) = scheduler.on_event(SchedulerEvent::LocalChange, Instant::now())
        {
            start_run(
                config,
                &git,
                &mut scheduler,
                &mut worker,
                &report_tx,
                &changes,
                request,
            )?;
        }
        match event_rx.recv_timeout(Duration::from_millis(20)) {
            Ok(event) => {
                if reconcile_event(&git, event)? {
                    changes.fetch_add(1, Ordering::Relaxed);
                    if let Some(request) =
                        scheduler.on_event(SchedulerEvent::LocalChange, Instant::now())
                    {
                        start_run(
                            config,
                            &git,
                            &mut scheduler,
                            &mut worker,
                            &report_tx,
                            &changes,
                            request,
                        )?;
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err(WatchError::Worker),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if let Some(request) = scheduler.on_event(SchedulerEvent::PeriodicTick, Instant::now()) {
            start_run(
                config,
                &git,
                &mut scheduler,
                &mut worker,
                &report_tx,
                &changes,
                request,
            )?;
        }
    }
}
fn start_run(
    config: &Config,
    git: &Git,
    scheduler: &mut Scheduler,
    worker: &mut Option<JoinHandle<()>>,
    tx: &mpsc::Sender<SyncReport>,
    changes: &Arc<AtomicU64>,
    request: RunRequest,
) -> Result<(), WatchError> {
    // Reconcile status before a periodic/initial run: native watcher delivery can lag.
    if request.trigger != Trigger::LocalChange && !git.status()?.is_empty() {
        scheduler.on_event(
            SchedulerEvent::RunFinished(Outcome::Deferred),
            Instant::now(),
        );
        return Ok(());
    }
    let baseline = if request.trigger == Trigger::LocalChange
        && git.state()? == RepositoryState::Ready
        && !git.path("git-rsrs-commit.json")?.exists()
        && !git.path("git-rsrs-integrate.json")?.exists()
    {
        Some(Snapshot::capture(git)?)
    } else {
        None
    };
    let changes = changes.clone();
    let generation = changes.load(Ordering::Relaxed);
    let observed_git = git.clone();
    let config = config.clone();
    let tx = tx.clone();
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

pub fn reconcile_event(git: &Git, event: notify::Result<Event>) -> Result<bool, GitError> {
    match event {
        Err(error) => {
            eprintln!("watch event error; reconciling Git status: {error}");
            Ok(!git.status()?.is_empty())
        }
        Ok(event) if event.need_rescan() => Ok(!git.status()?.is_empty()),
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
