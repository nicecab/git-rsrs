pub mod integrate;
pub mod snapshot;
pub mod status;
use crate::process::{CommandOutput, CommandSpec, ProcessError, ProcessRunner};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    os::unix::ffi::OsStringExt,
    path::{Path, PathBuf},
    time::Duration,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum GitError {
    #[error(transparent)]
    Process(#[from] ProcessError),
    #[error("Git {command} failed: {message}")]
    Command {
        command: String,
        message: String,
        output: Box<CommandOutput>,
    },
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
#[derive(Debug, Clone)]
pub struct Git {
    pub root: PathBuf,
    pub git_dir: PathBuf,
    pub runner: ProcessRunner,
    pub timeout: Duration,
}
impl Git {
    pub fn open(repo: &Path, runner: ProcessRunner) -> Result<Self, GitError> {
        let repo = repo.canonicalize()?;
        let mut git = Self {
            root: repo,
            git_dir: PathBuf::new(),
            runner,
            timeout: Duration::from_secs(120),
        };
        git.root = path_output(git.run(["rev-parse", "--show-toplevel"])?.stdout);
        git.git_dir = git.path(".")?;
        Ok(git)
    }
    pub fn spec<I, S>(&self, args: I) -> CommandSpec
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut argv = vec![OsString::from("git")];
        argv.extend(args.into_iter().map(|s| s.as_ref().to_os_string()));
        CommandSpec::new(argv, &self.root, self.timeout)
    }
    pub fn run<I, S>(&self, args: I) -> Result<CommandOutput, GitError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.run_env(args, &BTreeMap::new())
    }
    pub fn run_env<I, S>(
        &self,
        args: I,
        env: &BTreeMap<OsString, OsString>,
    ) -> Result<CommandOutput, GitError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut spec = self.spec(args);
        spec.env.extend(env.clone());
        let output = self.runner.run(&spec)?;
        if output.code != Some(0) {
            return Err(GitError::Command {
                command: spec
                    .argv
                    .iter()
                    .skip(1)
                    .map(|s| s.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(" "),
                message: format!(
                    "{}{}",
                    String::from_utf8_lossy(&output.stderr),
                    String::from_utf8_lossy(&output.stdout)
                ),
                output: Box::new(output),
            });
        }
        if output.truncated {
            return Err(GitError::Invalid(
                "Git output exceeded the capture limit".into(),
            ));
        }
        Ok(output)
    }
    pub fn text<I, S>(&self, args: I) -> Result<String, GitError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        String::from_utf8(self.run(args)?.stdout)
            .map(|s| s.trim_end_matches('\n').into())
            .map_err(|_| GitError::Invalid("expected textual Git output".into()))
    }
    pub fn path(&self, name: &str) -> Result<PathBuf, GitError> {
        Ok(path_output(
            self.run(["rev-parse", "--path-format=absolute", "--git-path", name])?
                .stdout,
        ))
    }
    pub fn status(&self) -> Result<Vec<status::StatusEntry>, GitError> {
        status::parse(
            &self
                .run(["status", "--porcelain=v1", "-z", "--untracked-files=all"])?
                .stdout,
        )
    }
    pub fn head(&self) -> Result<String, GitError> {
        self.text(["rev-parse", "--verify", "HEAD"])
    }
    pub fn branch(&self) -> Result<String, GitError> {
        self.text(["symbolic-ref", "--quiet", "--short", "HEAD"])
    }
}
fn path_output(mut raw: Vec<u8>) -> PathBuf {
    if raw.last() == Some(&b'\n') {
        raw.pop();
    }
    PathBuf::from(OsString::from_vec(raw))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepositoryState {
    Ready,
    Unborn,
    DetachedHead,
    Merging,
    Rebasing,
    ApplyingPatches,
    CherryPicking,
    Reverting,
    Bisecting,
    Sequencing,
}
impl Git {
    pub fn state(&self) -> Result<RepositoryState, GitError> {
        for (marker, state) in [
            ("MERGE_HEAD", RepositoryState::Merging),
            ("rebase-merge", RepositoryState::Rebasing),
            ("CHERRY_PICK_HEAD", RepositoryState::CherryPicking),
            ("REVERT_HEAD", RepositoryState::Reverting),
            ("BISECT_START", RepositoryState::Bisecting),
            ("sequencer", RepositoryState::Sequencing),
        ] {
            if self.path(marker)?.exists() {
                return Ok(state);
            }
        }
        let apply = self.path("rebase-apply")?;
        if apply.exists() {
            return Ok(if apply.join("rebasing").exists() {
                RepositoryState::Rebasing
            } else {
                RepositoryState::ApplyingPatches
            });
        }
        if self.branch().is_err() {
            return Ok(RepositoryState::DetachedHead);
        }
        match self.head() {
            Ok(_) => Ok(RepositoryState::Ready),
            Err(GitError::Command { .. }) => Ok(RepositoryState::Unborn),
            Err(e) => Err(e),
        }
    }
}
