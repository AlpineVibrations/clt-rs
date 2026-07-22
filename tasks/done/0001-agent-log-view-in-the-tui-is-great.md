agent log view in the TUI is great. we should have that available in the kanban view also.

Completion note:

COMPLETED 2026-07-22: Added `l` in the Kanban view to show the active project's live or latest recorded agent output, with project-path selection, shared refresh/close/console behavior, documentation, and regression coverage. Checks: `cargo fmt -- --check`; `cargo test tui_kanban_console_displays_an_open_agent_log -- --test-threads=1`; `cargo test kanban_agent_log_view_uses_the_active_project -- --test-threads=1`; `cargo clippy --all-targets -- -D warnings`; `cargo test -- --test-threads=1` (142 passed); `git diff --check`.
