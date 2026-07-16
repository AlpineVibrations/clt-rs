clean up the doing tasks and fix and commit

Blocked note:
- BLOCKED 2026-07-16: Reconciled the existing log-view work and verified all three focused log tests plus `cargo fmt -- --check`, but the required full `cargo test -- --test-threads=1` check failed in unrelated scheduler test `agent_daemon_loop_repeats_passes_and_respects_success_cooldown` (expected one recorded run, observed two). Per the no-commit-on-failed-checks rule, left the cleanup task in doing and did not mark tasks done or commit.

Completion note:
- COMPLETED 2026-07-16: The scheduler test now passes; formatting, strict Clippy, diff integrity, and all 117 serialized tests are clean, so the lingering log-view tasks were reconciled.
