the agent service got stale somehow... cant we have something check that and restart it.

Completion note: COMPLETED 2026-07-23: Added TUI health recovery that restarts an OS-reported running service when its service check-in expires, clears the stale check-in after recovery, preserves explicitly stopped services, and makes Linux restart after unexpected clean exits; documented the behavior and added regression coverage. Checks: `cargo fmt -- --check`; `cargo test -- --test-threads=1` (143 passed); `cargo clippy --all-targets --all-features -- -D warnings`; `git diff --check`.
