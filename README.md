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
