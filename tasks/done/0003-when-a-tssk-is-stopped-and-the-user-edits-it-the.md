when a tssk is stopped and the user edits it, the clt task edit input should not show the user the clt:stopped, like it omits the codex task from the user input.

Completion note:
COMPLETED 2026-09-23: Hid the stop marker in both task edit entry points and preserved stopped state after save. Checks: cargo test --bin clt editing_stopped_task_hides_and_preserves_stop_marker; cargo test --bin clt task_edit_hides_and_preserves_terminal_codex_session_marker; cargo test --bin clt unlinked_stop_persists_and_skips_automation_until_restarted; rustfmt --edition 2024 --check on changed source files; git diff --check. Linked follow-up: tasks/todo/0001-resolve-projectwide-rustfmt-drift-in-vendored-tu.md for baseline vendored rustfmt drift at 063671637397573ae61ca6a95aca10d381073dda.
codex:01a0cecb-9ef4-7b71-a495-1b7e0ee2e6dd
