Allow both n and + to create a subtask in the TUI (TUI, UX)

Completion note:

COMPLETED 2026-08-27: Added `+` as a subtask-creation alias alongside `n`, handling both traditional unmodified `+` events and enhanced-terminal Shift+`+` events while preserving Ctrl-N task reordering; updated TUI help, README, changelog, and regression coverage. Checks: `cargo fmt -- --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test -- --test-threads=1` (307 passed), and `git diff --check`.
