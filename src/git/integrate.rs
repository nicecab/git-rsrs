use super::{Git, GitError};
use std::{
    fs::{self, OpenOptions},
    io::Write,
};

#[derive(Debug)]
pub enum Integration {
    Integrated { changed: bool },
    RebaseConflict { recovery: Vec<String> },
    StashRestoreConflict { recovery: Vec<String> },
    RecoveryRequired { recovery: Vec<String> },
}
#[derive(Debug, PartialEq, Eq)]
pub enum PushResult {
    Accepted,
    NonFastForward,
}

impl Git {
    pub fn fetch(&self, remote: &str, branch: &str) -> Result<(), GitError> {
        self.run(["check-ref-format", "--branch", branch])?;
        self.run(["remote", "get-url", remote])?;
        let refspec = format!("+refs/heads/{branch}:refs/remotes/{remote}/{branch}");
        self.run(["fetch", "--no-tags", "--", remote, &refspec])?;
        Ok(())
    }
    pub fn ahead(&self, remote: &str, branch: &str) -> Result<bool, GitError> {
        Ok(self.text([
            "rev-list",
            "--count",
            &format!("refs/remotes/{remote}/{branch}..HEAD"),
        ])? != "0")
    }
    pub fn push(&self, remote: &str, branch: &str) -> Result<PushResult, GitError> {
        let refspec = format!("HEAD:refs/heads/{branch}");
        match self.run(["push", "--porcelain", "--", remote, &refspec]) {
            Ok(_) => Ok(PushResult::Accepted),
            Err(GitError::Command { ref output, .. })
                if String::from_utf8_lossy(&output.stdout).lines().any(|line| {
                    line.starts_with('!')
                        && (line.contains("[rejected] (fetch first)")
                            || line.contains("[rejected] (non-fast-forward)")
                            || line.contains("[remote rejected] (incorrect old value provided)"))
                }) =>
            {
                Ok(PushResult::NonFastForward)
            }
            Err(e) => Err(e),
        }
    }
    pub fn integrate(&self, remote: &str, branch: &str) -> Result<Integration, GitError> {
        let target = format!("refs/remotes/{remote}/{branch}");
        if self.text(["rev-list", "--count", &format!("HEAD..{target}")])? == "0" {
            return Ok(Integration::Integrated { changed: false });
        }
        if self.state()? != super::RepositoryState::Ready {
            return Ok(Integration::RecoveryRequired { recovery: vec!["Git changed state before integration; refusing to abort or overwrite another operation".into()] });
        }
        let original_head = self.head()?;
        let target_oid = self.text(["rev-parse", &target])?;
        let journal = self.path("git-rsrs-integrate.json")?;
        let before = self.text(["stash", "list", "--format=%H"])?;
        let metadata = serde_json::json!({"version":1,"head":original_head,"target":target,"target_oid":target_oid,"stash_before":before.lines().collect::<Vec<_>>()});
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&journal)?;
        file.write_all(metadata.to_string().as_bytes())?;
        file.sync_all()?;
        fs::File::open(&self.git_dir)?.sync_all()?;
        let result = self.run([
            "-c",
            "commit.gpgsign=false",
            "rebase",
            "--autostash",
            &target,
        ]);
        let mut conflicted = false;
        if result.is_err() {
            let active = self.path("rebase-merge")?.exists() || self.path("rebase-apply")?.exists();
            if active {
                let directory = if self.path("rebase-merge")?.exists() {
                    self.path("rebase-merge")?
                } else {
                    self.path("rebase-apply")?
                };
                let belongs_to_run = fs::read_to_string(directory.join("orig-head"))
                    .is_ok_and(|s| s.trim() == original_head)
                    && fs::read_to_string(directory.join("onto"))
                        .is_ok_and(|s| s.trim() == target_oid);
                if !belongs_to_run {
                    return Ok(Integration::RecoveryRequired {
                        recovery: vec![format!(
                            "rebase ownership is ambiguous; inspect {}",
                            journal.display()
                        )],
                    });
                }
                // Only abort the matching rebase started by this run.
                if self
                    .run(["-c", "commit.gpgsign=false", "rebase", "--abort"])
                    .is_err()
                {
                    return Ok(Integration::RecoveryRequired {
                        recovery: vec![format!(
                            "owned rebase could not be aborted; inspect {}",
                            journal.display()
                        )],
                    });
                }
                conflicted = matches!(&result, Err(GitError::Command { .. }));
            }
        }
        let unmerged = match self.run(["ls-files", "--unmerged", "-z"]) {
            Ok(o) => !o.stdout.is_empty(),
            Err(_) => {
                return Ok(Integration::RecoveryRequired {
                    recovery: vec![format!(
                        "cannot verify integration state; inspect {}",
                        journal.display()
                    )],
                });
            }
        };
        if unmerged {
            let after = self.text(["stash", "list", "--format=%H"])?;
            let mut recovery = after
                .lines()
                .filter(|id| !before.lines().any(|old| old == *id))
                .map(|id| format!("retained autostash {id}"))
                .collect::<Vec<_>>();
            recovery.push(format!(
                "resolve restored working-tree conflicts; inspect {}",
                journal.display()
            ));
            return Ok(Integration::StashRestoreConflict { recovery });
        }
        fs::remove_file(journal)?;
        if conflicted {
            return Ok(Integration::RebaseConflict {
                recovery: vec![
                    "rebase was aborted; original local commits and edits were retained".into(),
                ],
            });
        }
        result?;
        Ok(Integration::Integrated { changed: true })
    }
}
