the codex settings for the agents should for the thinking you are missing some settings like max and ultra.

Completion note:

COMPLETED 2026-07-20: Added max and ultra to the agent TUI's Codex reasoning cycle, documented the full supported sequence, and added a regression assertion. Checks: `git diff --check`, `cargo fmt -- --check`, `cargo test codex_setting_cycles_return_to_project_defaults`, and `cargo test` (126 passed).
