use super::{COMMIT_JOURNAL, Git, GitError};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};
use tempfile::TempDir;

#[derive(Debug)]
pub enum CommitResult {
    Created(String),
    NoChanges,
    DeferredConcurrentChange,
    RecoveryRequired(String),
}
#[derive(Debug)]
pub struct Snapshot {
    directory: TempDir,
    pub index: PathBuf,
    pub tree: String,
    head: String,
    branch: String,
    original_index: Option<Vec<u8>>,
}
impl Snapshot {
    pub fn capture(git: &Git) -> Result<Self, GitError> {
        if git.has_unmerged()? {
            return Err(GitError::Invalid(
                "unmerged index entries require manual resolution".into(),
            ));
        }
        if git.path(COMMIT_JOURNAL)?.exists() {
            return Err(GitError::Invalid(
                "unfinished commit publication; inspect git-rsrs-commit.json before retrying"
                    .into(),
            ));
        }
        let directory = tempfile::Builder::new()
            .prefix("git-rsrs-candidate-")
            .tempdir_in(&git.git_dir)?;
        let index = directory.path().join("index");
        let original_index = read_optional(&git.path("index")?)?;
        let head = git.head()?;
        let branch = git.branch()?;
        let env = index_env(&index);
        if let Some(bytes) = &original_index {
            fs::write(&index, bytes)?;
        } else {
            git.run_env(["read-tree", &head], &env)?;
        }
        git.run_env(["add", "-A", "--", "."], &env)?;
        let tree = git.text_env(["write-tree"], &env)?;
        Ok(Self {
            directory,
            index,
            tree,
            head,
            branch,
            original_index,
        })
    }
    fn base_unchanged(&self, git: &Git) -> Result<bool, GitError> {
        Ok(git.head()? == self.head
            && git.branch()? == self.branch
            && read_optional(&git.path("index")?)? == self.original_index)
    }
    pub fn unchanged(&self, git: &Git) -> Result<bool, GitError> {
        if !self.base_unchanged(git)? {
            return Ok(false);
        }
        let fresh = Self::capture(git)?;
        if fresh.tree != self.tree {
            return Ok(false);
        }
        Ok(git.text_env(["write-tree"], &index_env(&self.index))? == self.tree)
    }
    pub fn commit_validated(self, git: &Git) -> Result<CommitResult, GitError> {
        if !self.unchanged(git)? {
            return Ok(CommitResult::DeferredConcurrentChange);
        }
        if git.text(["rev-parse", "HEAD^{tree}"])? == self.tree {
            return Ok(CommitResult::NoChanges);
        }
        let lock_path = git.path("index.lock")?;
        let mut index_lock = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)?;
        if !self.base_unchanged(git)? {
            drop(index_lock);
            fs::remove_file(lock_path)?;
            return Ok(CommitResult::DeferredConcurrentChange);
        }
        // Pin all recovery data before the command is allowed to update HEAD.
        let directory = self.directory.keep();
        let backup = directory.join("original-index");
        if let Some(bytes) = &self.original_index {
            fs::write(&backup, bytes)?;
            fs::File::open(&backup)?.sync_all()?;
        }
        let metadata = serde_json::json!({"version":1,"head":self.head,"branch":self.branch,"tree":self.tree,"candidate_index":self.index,"original_index":backup,"original_index_existed":self.original_index.is_some(),"directory":directory,"index_lock":lock_path});
        let journal = git.write_journal(COMMIT_JOURNAL, &metadata)?;
        let result = git.run_env(
            [
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-m",
                "chore: synchronize local changes",
            ],
            &index_env(&self.index),
        );
        let current_head = match git.head() {
            Ok(head) => head,
            Err(_) => {
                return Ok(CommitResult::RecoveryRequired(format!(
                    "inspect {} and {}",
                    journal.display(),
                    directory.display()
                )));
            }
        };
        if let Err(error) = result {
            if current_head == self.head {
                // A native hook may have changed its alternate index; retain the live original.
                drop(index_lock);
                fs::remove_file(&lock_path)?;
                fs::remove_file(&journal)?;
                fs::remove_dir_all(directory)?;
                return Err(error);
            }
            return Ok(CommitResult::RecoveryRequired(format!(
                "commit changed HEAD before failure; inspect {}",
                journal.display()
            )));
        }
        if git.text(["rev-parse", "HEAD^{tree}"])? != self.tree {
            return Ok(CommitResult::RecoveryRequired(format!(
                "native Git hook changed the validated tree; inspect {}",
                journal.display()
            )));
        }
        index_lock.write_all(&fs::read(&self.index)?)?;
        index_lock.sync_all()?;
        drop(index_lock);
        fs::rename(&lock_path, git.path("index")?)?;
        git.sync_git_dir()?;
        fs::remove_file(journal)?;
        fs::remove_dir_all(directory)?;
        Ok(CommitResult::Created(current_head))
    }
}
pub fn index_env(index: &Path) -> BTreeMap<OsString, OsString> {
    BTreeMap::from([("GIT_INDEX_FILE".into(), index.as_os_str().into())])
}
fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, std::io::Error> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}
