when a project is registed with clt agents and the user is in the kanban tui they see the agent status in the console title, add the on/off state after the word agent and before the current running state.

Completion note:
COMPLETED 2026-09-23: Added ON/OFF before the Kanban console runtime state and updated tests and README. Checks: cargo fmt --all; cargo test --bin clt kanban_console_title; cargo test --bin clt kanban_render_shows_registered_project_codex_settings_in_the_console_title; git diff --check. codex:01a0ceac-22ad-7b72-a09f-4c30deb48b38
