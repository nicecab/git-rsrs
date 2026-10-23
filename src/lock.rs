use crate::{
    config::valid_lock_name,
    git::{Git, GitError},
    process::CANCELLED,
};
use serde_json::json;
use std::{
    fs::{File, OpenOptions},
    io::{self, Seek, SeekFrom, Write},
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    path::PathBuf,
    sync::atomic::Ordering,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum LockError {
    #[error("checkout lock timed out: {path}")]
    Timeout { path: PathBuf },
    #[error("lock acquisition interrupted")]
    Interrupted,
    #[error(transparent)]
    Git(#[from] GitError),
    #[error(transparent)]
    Io(#[from] io::Error),
}
#[derive(Debug)]
pub struct CheckoutLock {
    file: File,
}
impl CheckoutLock {
    pub fn acquire(git: &Git, name: &str, timeout: Duration) -> Result<Self, LockError> {
        if !valid_lock_name(name) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsafe checkout lock filename",
            )
            .into());
        }
        let path = git.path(name)?;
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)?;
        let deadline = Instant::now() + timeout;
        loop {
            // flock operates on the live owned descriptor, never on a pid file.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                break;
            }
            let error = io::Error::last_os_error();
            if !matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) {
                return Err(error.into());
            }
            if CANCELLED.load(Ordering::Relaxed) {
                return Err(LockError::Interrupted);
            }
            if Instant::now() >= deadline {
                return Err(LockError::Timeout { path });
            }
            thread::sleep(Duration::from_millis(10));
        }
        let mut lock = Self {
            file: file.try_clone()?,
        };
        file.seek(SeekFrom::Start(0))?;
        file.set_len(0)?;
        let metadata = json!({"pid":std::process::id().to_string(),"hostname":std::env::var("HOSTNAME").unwrap_or_else(|_|"localhost".into()),"operation":"git-rsrs sync","acquired_at":SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs().to_string()});
        file.write_all(
            serde_json::to_string(&metadata)
                .expect("metadata is serializable")
                .as_bytes(),
        )?;
        file.sync_all()?;
        // Keep one descriptor so RAII release always refers to the same lock inode.
        lock.file = file;
        Ok(lock)
    }
}
impl Drop for CheckoutLock {
    fn drop(&mut self) {
        let _ = self.file.set_len(0);
        let _ = self.file.sync_all();
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}
