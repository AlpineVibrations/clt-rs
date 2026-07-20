Add hidden Backlog status and TUI column (FEATURE, TUI)

Completion note:

COMPLETED 2026-07-20: Added Backlog storage and CLI transitions, a hidden-by-default leftmost TUI column with `b`/`B`/`0` controls, backlog counts, compatibility repair for existing and nested boards, agent exclusion for backlog-only work, documentation, and focused coverage. Checks: `cargo fmt -- --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test -- --test-threads=1` (131 passed), CLI initialization/move/list smoke test, and interactive TUI show/hide smoke test.
