Fix TUI exiting immediately after Models page changes (BUG, TUI, HIGH)

COMPLETED 2026-08-11: Replaced panicking immutable TOML indexing with optional lookups when the user's Codex config omits top-level model defaults, and added regression coverage for that normal configuration. Checks: focused Codex config tests; `cargo test -- --test-threads=1` (175 passed); `cargo clippy --all-targets -- -D warnings`; `git diff --check`; reinstalled with `cargo install --path . --force`; launched and exited the installed `/home/pro/.cargo/bin/clt` successfully against the real Codex config.
