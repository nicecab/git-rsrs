# git-rsrs

A Rust CLI that commits local changes and synchronizes a Git checkout across
machines. Git performs authentication, transport and history integration;
repository-specific policy runs through external command hooks.

## Build and run

Requires Git and the Rust toolchain pinned in `rust-toolchain.toml` to build.
Runtime requires Git, credentials configured for the selected remote, and any
executables used by hooks. Supported targets are Linux x86_64 and macOS arm64/x86_64.
Linux release binaries use musl static linking so they do not require a matching
glibc version.

```sh
cargo build --locked --release
install -m 755 target/release/git-rsrs "$HOME/.local/bin/git-rsrs"
```

Install the binary outside the checkout being synchronized. Configuration and
repository paths are explicit; they are not inferred from the binary's location.

```sh
git-rsrs --repo /path/to/checkout --config /path/to/config.toml sync
git-rsrs --repo /path/to/checkout --config /path/to/config.toml --json sync
git-rsrs --repo /path/to/checkout --config /path/to/config.toml --json watch
```

`sync` runs once. `watch` includes both local watching and periodic remote
reconciliation; it does not need a second timer invoking `sync`. A manually invoked
`sync` intentionally does not wait for watch's quiet period.

The configured branch is required. A different checked-out branch is skipped;
rsrs never switches branches. Existing merge/rebase/cherry-pick/revert/bisect states,
unborn repositories, detached HEAD, and unmerged index entries require manual intervention. Unresolved
recovery journals and another Git index lock also prevent automatic mutations.

## Homebrew

The [Nice Cabbage tap](https://github.com/nicecab/homebrew-tap) provides Homebrew
distribution. After the first stable release and tap update:

```sh
brew install nicecab/tap/git-rsrs
```

Before that first release, once the initial implementation is merged into main,
the tap can build the default branch from source:

```sh
brew install --HEAD nicecab/tap/git-rsrs
```

The tap verifies all released platform archives and checksums before updating its
formula. It checks for stable releases hourly and supports manual refresh, using
its own repository token rather than a cross-repository personal access token.
Upstream release publication and tap activation happen only after their PRs land;
ordinary builds do not publish a version.

## Configuration

Start with [examples/config.toml](examples/config.toml). Schema version 1 rejects
unknown fields and unsupported versions. Durations are positive strings such as
`30s`, `500ms` and `5m` (up to one year).

```toml
version = 1
branch = "main"
remote = "origin"
quiet_period = "30s"
interval = "5m"
git_timeout = "120s"
lock_timeout = "30s"
lock_name = "git-rsrs.lock"
```

Only `version` and `branch` are required; the other values shown are defaults.
Each invocation manages one checkout. Run separate instances for separate
repositories. Do not select different lock names for processes mutating the same
checkout. The lock name must be a plain, non-reserved `.lock` filename; Git's own
`index.lock`, `HEAD.lock`, `config.lock` and other reserved lock names are rejected.

## Synchronization

1. Acquire the checkout's advisory lock and verify its Git state and branch.
2. Build a candidate snapshot in an alternate index and run validation hooks.
3. Commit allowed local changes, including untracked files that Git does not ignore.
4. Fetch the configured remote branch and rebase with Git autostash when needed.
5. Run post-integration checks, then push if local commits are ahead.
6. Release the lock and invoke result-reporting hooks.

A successful validation authorizes the pinned candidate, not arbitrary later
edits. If the working tree or index changes during validation, the run defers.
Subsequent edits cannot enter the pinned candidate automatically. Watch carries a
change-generation and content token through checkout-lock waits; changed tokens
restart the quiet period. Branch/HEAD identity is rechecked after network waits,
and push uses the expected commit ID rather than a late resolution of HEAD. Native Git hooks
still run; native and external validation hooks must be read-only in v1. A native
hook that changes the candidate index violates that contract and causes recovery
handling before rsrs will push the resulting commit.

Commits happen before network access, so an offline machine can retain local
commits for the next synchronization. Commit signing is disabled for the automated
commit without changing persistent Git settings. A non-fast-forward or recognized
remote-reference race causes one fetch/integration/push retry. Authentication,
server-policy and unknown failures are not blindly retried.

## Watch timing

The last relevant local edit must be followed by 30 quiet seconds before validation
and commit. Every new edit resets the deadline. There is no maximum wait:
continuous editing also postpones periodic integration. The periodic interval
defaults to five minutes and brings in changes made on other machines.

Dirty startup observes the same quiet period; clean startup reconciles the remote.
Ignored untracked files and Git internals do not reset the timer. Tracked ignored
files still count. Events received during sync remain pending, and only one worker
runs per watch process. Independent processes coordinate through the checkout lock.
Watcher errors/overflow cause a Git-status rescan. Changes made by an integration
may cause one subsequent no-op reconciliation.

A manual-intervention result pauses automatic mutations until watch is restarted
after repair. SIGINT/SIGTERM stops new work and terminates active command groups.
An interrupted active sync exits with a recovery result; an idle shutdown exits 0.

## External command hooks

Hooks are executable/argument arrays. There is no implicit shell expansion. All
hooks run at the checkout root with stdin closed; executables resolve through PATH
unless an absolute path is supplied. Each hook defaults to a 30-second deadline.
On timeout, the entire process group receives TERM and then KILL after one second.
stdout and stderr are drained independently and each captured stream is capped at
1 MiB, with truncation reported explicitly on failure.

```toml
[[hooks.pre_commit]]
command = ["/absolute/path/to/pre_commit", "--check"]
timeout = "30s"
reject_exit_codes = [3]

[[hooks.post_integrate]]
command = ["/absolute/path/to/check-home-links", "--check"]
timeout = "10s"

[[hooks.on_result]]
command = ["/absolute/path/to/report-sync-result"]
timeout = "5s"
```

| Phase | Result policy |
| --- | --- |
| `pre_commit` | Exit 0 allows commit; configured rejection codes suppress only new commits. Other failures abort sync. Validation rejections are collected in order; infrastructure failure stops the hook list. |
| `post_integrate` | Failures are recorded but do not prevent pushing existing commits. Runs after each successful integration/reconciliation, including a retry. |
| `on_result` | Runs after lock release. Failures add diagnostics without replacing the finalized sync outcome. |

Validation rejection still permits remote integration and pushing existing
commits. Hook side effects are not rolled back. Mutating hooks are unsupported;
trying to reacquire the same checkout lock from a validation hook would deadlock
until the hook timeout.

Hook context:

| Environment variable | Value |
| --- | --- |
| `RSRS_REPO` | Absolute checkout root |
| `RSRS_PHASE` | `pre_commit`, `post_integrate`, or `on_result` |
| `RSRS_TRIGGER` | `manual`, `initial`, `local_change`, or `periodic` |
| `RSRS_OUTCOME` | Final outcome, only for `on_result` |
| `RSRS_REPORT_JSON` | Final facts and recovery metadata, only for `on_result`; capped at 16 KiB with explicit truncation. Hook diagnostics are excluded. |

An alternate Git index is supplied internally during validation; hooks should use
normal Git commands and not persist its path. Inherited Git repository-location
overrides are cleared so they cannot redirect an invocation to another checkout.

## Lock interoperability

The POSIX flock file lives at `git rev-parse --git-path <lock_name>`. Other tools can
coordinate by locking the same inode with an exclusive `fcntl.flock` in Python or
POSIX flock in another language. Configure the same `lock_name` in all cooperating
tools. Read-only validators do not reacquire it.

Lock-owner JSON contains string fields `pid`, `hostname`, `operation` and
`acquired_at` (Unix seconds). The kernel lock is authoritative; stale metadata does
not imply an active owner. The file remains present after unlock and must not be
unlinked. Linked worktrees use independent checkout locks and indexes; Git itself
protects shared reference updates.

## Results and recovery

`--json sync` writes one versioned JSON report to stdout. Watch writes one report
per completed run. Diagnostics go to stderr. Reports include the primary outcome,
trigger, created commit, integration/push facts, diagnostics and recovery references.
A failed active rebase is retained for manual recovery rather than automatically
aborted: abort can erase concurrent editor changes. Ignored local files are checked
for collisions before integration, including paths touched by intermediate replayed
commits; ignored directories are protected as units, so
incoming tracked content beneath one also requires manual attention. A retained
autostash requires intervention even when the index has no unmerged entries.

A fatal outcome takes precedence over validation rejection; reporting-hook failures
never replace the primary result.

| Exit | Meaning |
| --- | --- |
| 0 | Synced, no-op, branch skipped, or deferred pending quiet/revalidation |
| 1 | Unexpected error or authentication failure (distinct JSON outcomes) |
| 2 | Usage or configuration error |
| 3 | Validation rejection or post-integration check failure |
| 4 | Manual intervention or interrupted active synchronization |
| 5 | Checkout lock timeout |
| 6 | Recognized network unavailability; local commits retained |
| 7 | Command or hook timeout |

Autostash preserves tracked uncommitted content but does not promise to preserve
its original division between staged and unstaged changes. Untracked files are
never removed to make room for remote content. A stash restoration conflict stops
before push and retains recovery references. Inspect the report, `git status` and
`git stash list`; identify the stash contents before applying or dropping anything.

Recovery journals are located with:

```sh
git rev-parse --git-path git-rsrs-commit.json
git rev-parse --git-path git-rsrs-integrate.json
```

Their presence blocks new automatic mutations. Preserve each journal and its
referenced files while comparing HEAD, the candidate tree, the original index,
working-tree content and stash references. After interruption, rsrs conservatively
requires manual inspection rather than guessing that it owns remaining Git state.
Remove recovery markers only after deliberate repair. Never remove `index.lock`
while its owner is running. No recovery procedure should start with a hard reset
or deletion of untracked content.

## Service examples

Watch already performs periodic synchronization. To retain the quiet-period
behavior, run watch as a service instead of also scheduling independent one-shot
sync commands for the same checkout. These examples require manual installation;
the CLI does not modify launchd or systemd configuration.

A macOS LaunchAgent can use:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>local.git-rsrs</string>
  <key>ProgramArguments</key><array>
    <string>/Users/YOU/.local/bin/git-rsrs</string>
    <string>--repo</string><string>/Users/YOU/path/to/checkout</string>
    <string>--config</string><string>/Users/YOU/.config/git-rsrs/config.toml</string>
    <string>watch</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
</dict></plist>
```

Use absolute paths and ensure Git and hook executables are on the service's PATH
(or configure their absolute paths). A Linux systemd user service can use:

```ini
[Unit]
Description=Git repository synchronization

[Service]
ExecStart=%h/.local/bin/git-rsrs --repo %h/path/to/checkout --config %h/.config/git-rsrs/config.toml watch
Restart=on-failure
TimeoutStopSec=10

[Install]
WantedBy=default.target
```

## Development and releases

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo build --locked --release
```

Tests use isolated local bare remotes and multiple clones, not real user repos.
Python 3 is used by interoperability and child-process fixtures. CI separates
formatting, clippy/documentation checks and platform tests, with Rust build caches.
Tests and optimized binary startup run on Linux musl, Apple Silicon macOS and
Intel macOS. RustSec dependency audits run on dependency changes, weekly, and
before release publication. GitHub Actions dependencies are pinned to
full commit SHAs with version comments. The release workflow tests and packages
all three supported targets and publishes archives/checksums only when a maintainer
explicitly pushes a `v<package-version>` tag. No service activation, consumer
migration or tag publication is part of an ordinary build.
