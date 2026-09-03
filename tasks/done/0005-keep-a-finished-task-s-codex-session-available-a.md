Keep a finished task's Codex session available after leaving an interactive resume (BUG)

Completion note:

COMPLETED 2026-09-01: A completed or blocked task opened with `c` now retains its exact Codex session as stopped after interactive exit, including shared-project and stale-guardian cleanup paths, so the same task can reopen it repeatedly. Updated the TUI return message and README guidance. Checks: `cargo fmt -- --check`; `cargo clippy --all-targets --all-features -- -D warnings`; `cargo test --all-features -- --test-threads=1` (326 passed); `git diff --check`.
