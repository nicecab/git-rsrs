pub mod integrate;
pub mod snapshot;
pub mod status;
use crate::process::{CommandOutput, CommandSpec, ProcessError, ProcessRunner};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs::{File, OpenOptions},
    io::Write,
    os::unix::ffi::OsStringExt,
    path::{Path, PathBuf},
    time::Duration,
};
use thiserror::Error;

pub const COMMIT_JOURNAL: &str = "git-rsrs-commit.json";
pub const INTEGRATE_JOURNAL: &str = "git-rsrs-integrate.json";

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
        self.text_env(args, &BTreeMap::new())
    }
    pub fn text_env<I, S>(
        &self,
        args: I,
        env: &BTreeMap<OsString, OsString>,
    ) -> Result<String, GitError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        String::from_utf8(self.run_env(args, env)?.stdout)
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
    pub fn is_dirty(&self) -> Result<bool, GitError> {
        Ok(!self
            .run(["status", "--porcelain=v1", "-z", "--untracked-files=normal"])?
            .stdout
            .is_empty())
    }
    /// Durably creates a recovery journal, failing if one already exists.
    pub fn write_journal(
        &self,
        name: &str,
        contents: &serde_json::Value,
    ) -> Result<PathBuf, GitError> {
        let path = self.path(name)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        file.write_all(
            serde_json::to_string_pretty(contents)
                .expect("journal metadata")
                .as_bytes(),
        )?;
        file.sync_all()?;
        self.sync_git_dir()?;
        Ok(path)
    }
    pub fn sync_git_dir(&self) -> Result<(), GitError> {
        Ok(File::open(&self.git_dir)?.sync_all()?)
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
/// Splits NUL-terminated Git path output, skipping empty records.
pub(crate) fn nul_paths(raw: &[u8]) -> impl Iterator<Item = PathBuf> + '_ {
    raw.split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| PathBuf::from(OsString::from_vec(s.to_vec())))
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
    Unmerged,
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
        if self.has_unmerged()? {
            return Ok(RepositoryState::Unmerged);
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

impl Git {
    pub fn has_unmerged(&self) -> Result<bool, GitError> {
        Ok(!self
            .run(["ls-files", "--unmerged", "-z"])?
            .stdout
            .is_empty())
    }
}
