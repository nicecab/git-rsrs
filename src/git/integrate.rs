use super::{Git, GitError, nul_paths};
use std::{fs, path::PathBuf};

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
        let before = self.text(["stash", "list", "--format=%H"])?;
        let metadata = serde_json::json!({"version":1,"head":original_head,"target":target,"target_oid":target_oid,"stash_before":before.lines().collect::<Vec<_>>()});
        let journal = &self.paths.integrate_journal;
        self.write_journal(journal, &metadata)?;
        let result = self.run([
            "-c",
            "commit.gpgsign=false",
            "rebase",
            "--autostash",
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
            for dir in [&self.paths.rebase_merge, &self.paths.rebase_apply] {
                if let Ok(id) = fs::read_to_string(dir.join("autostash")) {
                    recovery.push(format!("retained autostash object {}", id.trim()));
                }
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
        let after = match self.text(["stash", "list", "--format=%H"]) {
            Ok(value) => value,
            Err(_) => {
                return Ok(Integration::RecoveryRequired {
                    recovery: vec![format!(
                        "cannot verify autostash restoration; inspect {}",
                        journal.display()
                    )],
                });
            }
        };
        let added = after
            .lines()
            .filter(|id| !before.lines().any(|old| old == *id))
            .collect::<Vec<_>>();
        if unmerged || !added.is_empty() {
            let mut recovery = added
                .into_iter()
                .map(|id| {
                    format!("stash added during integration; verify ownership and contents: {id}")
                })
                .collect::<Vec<_>>();
            recovery.push(format!(
                "autostash restoration is incomplete or ambiguous; inspect {}",
                journal.display()
            ));
            return Ok(Integration::StashRestoreConflict { recovery });
        }
        fs::remove_file(journal)?;
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
