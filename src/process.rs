use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    io::{self, Read},
    os::{
        fd::AsRawFd,
        unix::process::{CommandExt, ExitStatusExt},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};
use thiserror::Error;

pub static CANCELLED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone)]
pub struct CommandSpec {
    pub argv: Vec<OsString>,
    pub cwd: PathBuf,
    pub env: BTreeMap<OsString, OsString>,
    pub timeout: Duration,
    pub capture_limit: usize,
}
impl CommandSpec {
    pub fn new<I, S>(argv: I, cwd: impl AsRef<Path>, timeout: Duration) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        Self {
            argv: argv
                .into_iter()
                .map(|s| s.as_ref().to_os_string())
                .collect(),
            cwd: cwd.as_ref().into(),
            env: BTreeMap::new(),
            timeout,
            capture_limit: 1024 * 1024,
        }
    }
}
#[derive(Debug, Default, Clone)]
pub struct CommandOutput {
    pub code: Option<i32>,
    pub signal: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub truncated: bool,
}
#[derive(Debug, Error)]
pub enum ProcessError {
    #[error("cannot execute command: {0}")]
    Io(#[from] io::Error),
    #[error("command exceeded its deadline")]
    Timeout(CommandOutput),
    #[error("command was interrupted")]
    Interrupted(CommandOutput),
}
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessRunner;

struct ChildGroup(Child);
impl Drop for ChildGroup {
    fn drop(&mut self) {
        // The child is always placed in its own process group before exec.
        unsafe {
            libc::kill(-(self.0.id() as i32), libc::SIGKILL);
        }
        let _ = self.0.wait();
    }
}

fn nonblocking(pipe: &impl AsRawFd) -> io::Result<()> {
    // The descriptor belongs to the pipe and remains open throughout these calls.
    let flags = unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
fn drain(
    pipe: &mut impl Read,
    buffer: &mut Vec<u8>,
    limit: usize,
    truncated: &mut bool,
) -> io::Result<bool> {
    let mut chunk = [0; 8192];
    // Bound each drain pass so a continuously writing pipe cannot starve the deadline.
    for _ in 0..16 {
        match pipe.read(&mut chunk) {
            Ok(0) => return Ok(true),
            Ok(n) => {
                let keep = n.min(limit.saturating_sub(buffer.len()));
                buffer.extend_from_slice(&chunk[..keep]);
                *truncated |= keep < n;
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(false)
}

impl ProcessRunner {
    pub fn run(&self, spec: &CommandSpec) -> Result<CommandOutput, ProcessError> {
        if CANCELLED.load(Ordering::Relaxed) {
            return Err(ProcessError::Interrupted(CommandOutput::default()));
        }
        let executable = spec
            .argv
            .first()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty argv"))?;
        let mut command = Command::new(executable);
        command
            .args(&spec.argv[1..])
            .current_dir(&spec.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        for key in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_COMMON_DIR",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_PREFIX",
            "GIT_INTERNAL_SUPER_PREFIX",
            "GIT_QUARANTINE_PATH",
            "GIT_NAMESPACE",
        ] {
            command.env_remove(key);
        }
        command
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_EDITOR", "true")
            .env("GIT_SEQUENCE_EDITOR", "true")
            .env("LC_ALL", "C")
            .envs(&spec.env);
        let mut child = ChildGroup(command.spawn()?);
        let mut stdout = child.0.stdout.take().expect("piped stdout");
        let mut stderr = child.0.stderr.take().expect("piped stderr");
        nonblocking(&stdout)?;
        nonblocking(&stderr)?;
        let deadline = Instant::now() + spec.timeout;
        let mut output = CommandOutput::default();
        let mut status = None;
        let (mut out_done, mut err_done) = (false, false);
        let mut terminating: Option<(Instant, bool)> = None;
        loop {
            if !out_done {
                out_done = drain(
                    &mut stdout,
                    &mut output.stdout,
                    spec.capture_limit,
                    &mut output.truncated,
                )?;
            }
            if !err_done {
                err_done = drain(
                    &mut stderr,
                    &mut output.stderr,
                    spec.capture_limit,
                    &mut output.truncated,
                )?;
            }
            if status.is_none() {
                status = child.0.try_wait()?;
            }
            if terminating.is_none()
                && (CANCELLED.load(Ordering::Relaxed) || Instant::now() >= deadline)
            {
                let interrupted = CANCELLED.load(Ordering::Relaxed);
                unsafe {
                    libc::kill(-(child.0.id() as i32), libc::SIGTERM);
                }
                terminating = Some((Instant::now(), interrupted));
            }
            if let Some((started, interrupted)) = terminating {
                // Do not stop cleanup merely because the group leader has exited.
                if started.elapsed() >= Duration::from_secs(1) {
                    unsafe {
                        libc::kill(-(child.0.id() as i32), libc::SIGKILL);
                    }
                    if status.is_none() {
                        status = Some(child.0.wait()?);
                    }
                    output.code = status.and_then(|s| s.code());
                    output.signal = status.and_then(|s| s.signal());
                    return Err(if interrupted {
                        ProcessError::Interrupted(output)
                    } else {
                        ProcessError::Timeout(output)
                    });
                }
            } else if out_done && err_done && status.is_some() {
                output.code = status.and_then(|s| s.code());
                output.signal = status.and_then(|s| s.signal());
                return Ok(output);
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
}

extern "C" fn request_cancellation(_: libc::c_int) {
    CANCELLED.store(true, Ordering::Relaxed);
}

pub fn install_signal_handlers() -> io::Result<()> {
    // AtomicBool uses lock-free operations on supported macOS/Linux targets.
    for signal in [libc::SIGINT, libc::SIGTERM] {
        if unsafe {
            libc::signal(
                signal,
                request_cancellation as *const () as libc::sighandler_t,
            )
        } == libc::SIG_ERR
        {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}
