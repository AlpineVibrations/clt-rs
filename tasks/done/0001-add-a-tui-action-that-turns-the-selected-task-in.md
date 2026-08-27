Add a TUI action that turns the selected task into a subtask board and opens creation, expanding Markdown-backed storage automatically (FEATURE, TUI)

Completion note:

COMPLETED 2026-08-27: Added the `n` TUI flow for creating a Todo subtask under the selected task, with deferred-on-submit Markdown-to-folder expansion, parent task directory conversion, nested-board opening, stale-parent protection, help text, README and changelog documentation, and regression coverage for Markdown-backed, folder-backed, reused, and concurrently changed parents. Checks: `cargo fmt -- --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test -- --test-threads=1` (306 passed), `git diff --check`, and an interactive PTY smoke test of `n` -> description -> Enter with filesystem and `clt list` verification.
