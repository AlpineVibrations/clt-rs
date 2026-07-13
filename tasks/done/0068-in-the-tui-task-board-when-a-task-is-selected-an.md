in the tui task board when a task is selected and a new task is added the new task should be placed above the selected task not below it.

Completion note:
- COMPLETED 2026-07-13: Changed TUI task creation to insert at the selected task's index, placing the new task above it, with regression coverage for Markdown- and folder-backed boards. Checks: `cargo test tui_add_inserts_above_selected`, `cargo test` (105 passed), `cargo fmt -- --check`, and `git diff --check`.
