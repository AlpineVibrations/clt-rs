when entering a new task or editing a task adn they press ctrl c it shold esc out of the prompt editing like the esc key does. also update the minor version with this fix.

Completion note:

COMPLETED 2026-09-01: Made Ctrl-C cancel add, subtask, and edit prompts like Escape; added regression coverage, changelog documentation, and bumped the crate to 0.5.0. Checks: `cargo test tui_task_prompt_cancel_shortcuts_support_escape_and_control_c` (1 passed), `cargo test` (315 passed), `cargo fmt -- --check`, and `git diff --check`.

codex:01a05d9c-adee-7ad0-a468-3d8e094e511a
