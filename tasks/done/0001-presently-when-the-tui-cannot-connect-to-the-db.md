presently when the tui cannot connect to the db it blanks the screen and shows a red message error only. we should keep the screen and show the red error message inteh console.

Completion note: COMPLETED 2026-07-16: Preserved the last successful agent-panel snapshot when registry refreshes fail and show the error in the red console. Verified with `cargo fmt -- --check`, `cargo test tui_agent_panel_refresh_error -- --test-threads=1`, `cargo test -- --test-threads=1` (121 passed), and `git diff --check`.
