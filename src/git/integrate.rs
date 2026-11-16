use super::{Git, GitError, nul_paths};
use std::{fs, path::PathBuf};

#[derive(Debug)]
pub enum Integration {
    Integrated { changed: bool },
    DirtyWorktree,
    IndexLocked,
    RebaseConflict { recovery: Vec<String> },
    RecoveryRequired { recovery: Vec<String> },
}
#[derive(Debug, PartialEq, Eq)]
pub enum PushResult {
    Accepted,
    NonFastForward,
}

impl Git {
    /// Clear only a completed rebase whose branch reflog still identifies the
    /// recorded starting commit and whose result contains the recorded target.
    pub fn clear_completed_integration(&self, branch: &str) -> Result<bool, GitError> {
        let journal = &self.paths.integrate_journal;
        let Ok(bytes) = fs::read(journal) else {
            return Ok(false);
        };
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            return Ok(false);
        };
        let Some(head) = value.get("head").and_then(|v| v.as_str()) else {
            return Ok(false);
        };
        let Some(target) = value.get("target_oid").and_then(|v| v.as_str()) else {
            return Ok(false);
        };
        if value.get("version").and_then(|v| v.as_u64()) != Some(1)
            || ![head, target]
                .iter()
                .all(|s| matches!(s.len(), 40 | 64) && s.bytes().all(|c| c.is_ascii_hexdigit()))
            || self.paths.index_lock.exists()
            || self.state()? != super::RepositoryState::Ready
            || self.branch()? != branch
        {
            return Ok(false);
        }
        let branch_ref = format!("refs/heads/{branch}");
        let previous = format!("{branch_ref}@{{1}}");
        let Ok(previous_head) = self.text(["rev-parse", "--verify", &previous]) else {
            return Ok(false);
        };
        let Ok(message) = self.text(["reflog", "show", "-1", "--format=%gs", &branch_ref]) else {
            return Ok(false);
        };
        if previous_head != head
            || !message.starts_with("rebase (finish): ")
            || self
                .run(["merge-base", "--is-ancestor", target, "HEAD"])
                .is_err()
        {
            return Ok(false);
        }
        // Refuse to remove a journal replaced during verification.
        if fs::read(journal)? != bytes {
            return Ok(false);
        }
        fs::remove_file(journal)?;
        Ok(true)
    }
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
            let mut recovery = vec![format!(
                "active rebase retained to preserve concurrent edits; inspect {}",
                journal.display()
            )];
            match self.run(["diff", "--name-only", "--diff-filter=U", "-z"]) {
                Ok(output) => recovery.extend(
                    nul_paths(&output.stdout).map(|path| format!("conflicted path: {:?}", path)),
                ),
                Err(_) => recovery.push("could not list conflicted paths".into()),
            }
            return Ok(Integration::RebaseConflict { recovery });
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
        if let Err(GitError::Command { message, .. }) = &result
            && message.contains("Unable to create")
            && message.contains(self.paths.index_lock.to_string_lossy().as_ref())
            && message.contains("File exists")
        {
            return Ok(Integration::IndexLocked);
        }
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
