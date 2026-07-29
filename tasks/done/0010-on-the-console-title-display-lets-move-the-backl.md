on the console title display lets move the backlog display to right justified

Completion note:

COMPLETED 2026-07-21: Right-aligned the hidden Backlog count and `[B]` shortcut in the console border while keeping the board title left-aligned; added focused rendering coverage. Checks: `cargo fmt -- --check`, `cargo test tui_console_block_right_aligns_the_backlog_status`, and `cargo test -- --test-threads=1` (137 passed).
