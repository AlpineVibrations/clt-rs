in the agents tui page we need a way to remove a project from the agent list. delete key shold work , it should ask for confirm

Completion note:

COMPLETED 2026-08-21: Added Delete-key removal for the selected registered Agent Project with explicit y/n/Esc confirmation, nearest-row selection, help and documentation updates, and coverage proving project files remain untouched; checks: `cargo fmt -- --check`, `cargo test` (187 passed), and `cargo clippy --all-targets --all-features -- -D warnings`.
