//! Per-checkout status file for external health monitoring.
//!
//! The file records the latest attempt plus counters that survive restarts, so
//! a watchdog can judge health without reading logs or keeping history.
use crate::outcome::{HookFailure, Outcome, SyncReport};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    fs::{self, OpenOptions},
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::SystemTime,
};

pub const STATUS_VERSION: u32 = 1;
/// Number of counted attempts kept in `Status::recent`.
pub const RECENT_LEN: usize = 10;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecentAttempt {
    pub seq: u64,
    pub healthy: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Status {
    pub version: u32,
    pub last_attempt_at: String,
    pub last_outcome: Outcome,
    pub last_success_at: Option<String>,
    pub message: Option<String>,
    pub pid: u32,
    pub git_rsrs_version: String,
    pub hook_failures: Vec<HookFailure>,
    /// When the counters (and `attempt_seq`) started. A change means they were
    /// reset, so a consumer must discard anything keyed on an earlier `attempt_seq`.
    pub counters_since: String,
    pub attempt_seq: u64,
    pub consecutive_failures: u32,
    pub consecutive_successes: u32,
    /// Newest first, at most `RECENT_LEN` entries.
    pub recent: Vec<RecentAttempt>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Healthy,
    Unhealthy,
    /// Expected or transient states that a watchdog judges by time instead.
    Uncounted,
}

pub fn health(report: &SyncReport) -> Health {
    match report.outcome {
        Outcome::BranchSkipped
        | Outcome::Offline
        | Outcome::Deferred
        | Outcome::LockTimeout
        | Outcome::ValidationRejected => Health::Uncounted,
        Outcome::Synced | Outcome::Noop if report.hook_failures.is_empty() => Health::Healthy,
        _ => Health::Unhealthy,
    }
}

pub fn next(previous: Option<&Status>, report: &SyncReport, now: SystemTime, pid: u32) -> Status {
    let stamp = humantime::format_rfc3339_seconds(now).to_string();
    let succeeded = matches!(report.outcome, Outcome::Synced | Outcome::Noop);
    let mut attempt_seq = previous.map_or(0, |p| p.attempt_seq);
    let mut consecutive_failures = previous.map_or(0, |p| p.consecutive_failures);
    let mut consecutive_successes = previous.map_or(0, |p| p.consecutive_successes);
    let mut recent = previous.map_or_else(Vec::new, |p| p.recent.clone());
    let health = health(report);
    if health != Health::Uncounted {
        let healthy = health == Health::Healthy;
        attempt_seq += 1;
        if healthy {
            consecutive_successes += 1;
            consecutive_failures = 0;
        } else {
            consecutive_failures += 1;
            consecutive_successes = 0;
        }
        recent.insert(
            0,
            RecentAttempt {
                seq: attempt_seq,
                healthy,
            },
        );
        recent.truncate(RECENT_LEN);
    }
    let counters_since = previous.map_or_else(|| stamp.clone(), |p| p.counters_since.clone());
    Status {
        version: STATUS_VERSION,
        last_attempt_at: stamp.clone(),
        last_outcome: report.outcome,
        last_success_at: if succeeded {
            Some(stamp)
        } else {
            previous.and_then(|p| p.last_success_at.clone())
        },
        message: report
            .diagnostics
            .iter()
            .find(|d| d.phase == "sync")
            .and_then(|d| d.message.lines().next())
            .map(str::to_owned),
        pid,
        git_rsrs_version: env!("CARGO_PKG_VERSION").into(),
        hook_failures: report.hook_failures.clone(),
        counters_since,
        attempt_seq,
        consecutive_failures,
        consecutive_successes,
        recent,
    }
}

fn lock_path(path: &Path) -> PathBuf {
    let mut name = OsString::from(path.as_os_str());
    name.push(".lock");
    PathBuf::from(name)
}

/// Read the previous status, compute the next one and replace the file atomically.
///
/// `sync` and `watch` may finish attempts concurrently, so the read-modify-write
/// runs under an exclusive lock on a sibling `.lock` file. A missing or
/// unparsable previous file starts the counters afresh.
pub fn record(path: &Path, report: &SyncReport) -> io::Result<Status> {
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(lock_path(path))?;
    lock.lock()?;
    // Only a missing or unparsable file starts afresh; other read errors must
    // not silently reset the counters.
    let previous = match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str::<Status>(&text).ok(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let status = next(
        previous.as_ref(),
        report,
        SystemTime::now(),
        std::process::id(),
    );
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temp = tempfile::NamedTempFile::new_in(dir)?;
    temp.write_all(&serde_json::to_vec_pretty(&status).map_err(io::Error::other)?)?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|e| e.error)?;
    lock.unlock()?;
    Ok(status)
}
