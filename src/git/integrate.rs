use super::{Git, GitError, nul_paths};
use std::{fs, path::PathBuf};

#[derive(Debug)]
pub enum Integration {
    Integrated { changed: bool },
    DirtyWorktree,
    RebaseConflict { recovery: Vec<String> },
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
    pub fn push(
        &self,
        remote: &str,
        branch: &str,
        expected_head: &str,
    ) -> Result<PushResult, GitError> {
        let refspec = format!("{expected_head}:refs/heads/{branch}");
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
        // Only a clean worktree is rebased; dirty content waits untouched.
        if self.is_dirty()? {
            return Ok(Integration::DirtyWorktree);
        }
        let collisions = self.ignored_collisions(&target)?;
        if !collisions.is_empty() {
            return Ok(Integration::RecoveryRequired {
                recovery: collisions
                    .into_iter()
                    .map(|p| {
                        format!(
                            "ignored local path obstructs incoming tracked content: {}",
                            p.display()
                        )
                    })
                    .collect(),
            });
        }
        let original_head = self.head()?;
        let target_oid = self.text(["rev-parse", &target])?;
        let metadata = serde_json::json!({"version":1,"head":original_head,"target":target,"target_oid":target_oid});
        let journal = &self.paths.integrate_journal;
        self.write_journal(journal, &metadata)?;
        // Override an inherited rebase.autoStash: an edit landing after the dirty
        // check must make the rebase refuse, never be stashed and rebased over.
        let result = self.run([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "rebase.autoStash=false",
            "rebase",
            &target,
        ]);
        let active = self.paths.rebase_merge.exists() || self.paths.rebase_apply.exists();
        if result.is_err() && active {
            // Abort can hard-reset edits made by a concurrent editor. Retain the
            // operation instead of asserting that matching refs prove preservation.
            return Ok(Integration::RebaseConflict {
                recovery: vec![format!(
                    "active rebase retained to preserve concurrent edits; inspect {}",
                    journal.display()
                )],
            });
        }
        let unmerged = match self.has_unmerged() {
            Ok(value) => value,
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
            return Ok(Integration::RecoveryRequired {
                recovery: vec![format!(
                    "integration left unmerged entries; inspect {}",
                    journal.display()
                )],
            });
        }
        fs::remove_file(journal)?;
        // Git refuses to start over an edit that landed after the dirty check.
        if matches!(result, Err(GitError::Command { .. })) && self.is_dirty()? {
            return Ok(Integration::DirtyWorktree);
        }
        result?;
        Ok(Integration::Integrated { changed: true })
    }
}

impl Git {
    fn ignored_collisions(&self, target: &str) -> Result<Vec<PathBuf>, GitError> {
        let ignored = nul_paths(
            &self
                .run([
                    "ls-files",
                    "--others",
                    "--directory",
                    "--ignored",
                    "--exclude-standard",
                    "-z",
                ])?
                .stdout,
        )
        .collect::<Vec<_>>();
        if ignored.is_empty() {
            return Ok(vec![]);
        }
        let mut incoming = nul_paths(
            &self
                .run(["ls-tree", "-r", "--name-only", "-z", target])?
                .stdout,
        )
        .collect::<Vec<_>>();
        // Rebase can create and later remove a path absent from both final trees.
        // Protect ignored content against every local commit it may replay.
        incoming.extend(nul_paths(
            &self
                .run([
                    "log",
                    "--root",
                    "--format=",
                    "--name-only",
                    "--no-renames",
                    "-z",
                    &format!("{target}..HEAD"),
                ])?
                .stdout,
        ));
        incoming.sort();
        incoming.dedup();
        Ok(ignored
            .into_iter()
            .filter(|local| {
                incoming
                    .iter()
                    .any(|remote| local.starts_with(remote) || remote.starts_with(local))
            })
            .collect())
    }
}
