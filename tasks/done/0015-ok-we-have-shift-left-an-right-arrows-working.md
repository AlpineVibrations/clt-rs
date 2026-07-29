ok we have shift left an right arrows working. lets remove the j/l/i/k bindings for the task movement. its confusing.

Completion note: COMPLETED 2026-07-20: Removed the I/K/J/L task-movement bindings and their startup/help text so Shift+Arrow is the single task reorder/move shortcut. Verified with `cargo fmt -- --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test -- --test-threads=1` (132 passed).
