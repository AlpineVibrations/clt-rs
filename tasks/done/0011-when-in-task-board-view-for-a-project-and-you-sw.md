when in task board view for a project and you switch to agent panel view the agent panel should auto select the project that you were just looking at.

Completion note: COMPLETED 2026-07-16: Added path-based selection so switching from a project task board to the agent pane highlights that project, including the current unregistered-project row. Checks: `cargo fmt -- --check`; `cargo test tui_agent_panel_selects_ -- --test-threads=1` (3 passed); `cargo test -- --test-threads=1` (121 passed).
