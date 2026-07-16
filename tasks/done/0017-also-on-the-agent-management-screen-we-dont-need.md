also on the agent management screen we dont need a refresh button this panel should be auto refreshing like the other panel

Completed: Removed the agent projects pane manual refresh affordance and key binding, kept the existing timed auto-refresh behavior, and added a focused unit test for the auto-refresh instructions/interval.

Checks: cargo fmt --check; cargo test tui_agent_panel_uses_auto_refresh_instructions; cargo test.
