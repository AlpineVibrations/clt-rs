our log viewer is not updating for the currnet task that is selected. BUG

Completion note:

COMPLETED 2026-08-25: Made Kanban agent output follow the highlighted task through live Doing logs or session-linked run history, refresh on selection changes, and updated documentation. Checks: `cargo fmt -- --check`; `cargo test kanban_agent_log -- --test-threads=1` (2 passed); `cargo test -- --test-threads=1` (197 passed); `cargo clippy --all-targets -- -D warnings`; `git diff --check`. codex:01a03b59-2c7d-7f82-b026-2bf7e1685fc3
