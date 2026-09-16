when adding a new projet to the agent tui it properly adds it to the alphabetic list of active agents but it fails to move the selected viewed project cursor to taht entry as it should. cause now its confusing

Completion note:

COMPLETED 2026-08-14: Preserved the current-project registration selection by matching its path to the newly registered entry after the alphabetically sorted project list refreshes; added regression and changelog coverage. Checks: `git diff --check`; `cargo fmt -- --check`; `cargo clippy --all-targets --all-features -- -D warnings`; `cargo test` (180 passed).
