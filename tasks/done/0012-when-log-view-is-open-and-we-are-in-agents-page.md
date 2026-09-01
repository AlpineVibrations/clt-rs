when log view is open and we are in agents page it shows the latest run or currnet live. that is cool. but if we are in the task page and no task is selected and the log view is open it shows nothing.. i think it should show the same as the agent page does, the current or latest

Completion note:

COMPLETED 2026-08-26: Made the Task pane fall back to the active project's live or latest recorded output when no task is selected, including when opening the log with `l`, and added regression coverage. Checks: `cargo fmt -- --check`; `cargo test kanban_agent_log -- --test-threads=1` (2 passed); `cargo test -- --test-threads=1` (299 passed); `cargo clippy --all-targets -- -D warnings`; `git diff --check`. codex:01a03fb7-24a3-7912-995f-c82fc53c3173
