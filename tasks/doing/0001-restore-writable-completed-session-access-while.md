Restore writable completed-session access while another Git-enabled task runs (BUG, TUI)

Completion note:
COMPLETED 2026-09-13: Restored writable shared access to completed, stopped, and unowned queued sessions while preserving the other task's process, lease, and Git launch/finalization records. Updated README and regression coverage for commit/push boundaries, exit/reopen, worker ownership, and queued-session availability. Preserved the frozen pre-existing log-output edits unstaged. Checks: `cargo fmt --all -- --check`; `cargo clippy --locked --all-targets --all-features -- -D warnings`; `cargo test --locked --all-targets --all-features` (552 unit + 4 architecture + 14 CLI tests passed); `git diff --check`. Clippy passed with six vendored turso_core warnings.

codex:01a09bac-956e-7972-8f66-c4b3b30e5bcf
