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
