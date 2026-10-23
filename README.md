# git-rsrs

Git repository synchronization with external command hooks, written in Rust.

Under development. Supported platforms: macOS and Linux. Git handles transport
and authentication. Configuration is explicit and versioned; see
[examples/config.toml](examples/config.toml).

Development uses the pinned Rust toolchain:

```sh
cargo test --all-targets
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
```

One-shot synchronization:

```sh
cargo run -- --repo /path/to/checkout --config /path/to/config.toml sync
cargo run -- --repo /path/to/checkout --config /path/to/config.toml --json sync
```

The configured branch is required. A different checked-out branch is skipped;
rsrs never switches branches. Local changes are validated and committed before
network access. Credentials are provided by your normal Git configuration.

Safety and recovery:

- Rebase uses Git autostash for tracked uncommitted edits. Untracked paths are
  never deleted to make room for remote content. Autostash does not promise to
  preserve the original division between staged and unstaged changes.
- A stash restoration conflict stops before push and retains recovery references.
  Inspect the report, `git status`, and `git stash list`; resolve the conflicts
  manually. Do not pop or drop a stash without identifying its contents.
- `git rev-parse --git-path git-rsrs-commit.json` and
  `git rev-parse --git-path git-rsrs-integrate.json` locate recovery journals.
  Their presence blocks further automatic mutations. Preserve the journal and
  referenced files while comparing HEAD, the candidate tree, original index,
  working tree and stash. Remove recovery markers only after a deliberate repair.
- Never remove a Git `index.lock` while its owning process is alive. The rsrs
  advisory lock file itself is persistent and must not be unlinked.
