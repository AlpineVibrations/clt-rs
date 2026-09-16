Phase 7: Extract scheduler and worker lifecycle modules (REFACTOR, AGENT).

Reference: [CLT Refactor Design/Build Plan](../../refactor-plan.md#phase-7-scheduler-and-worker-lifecycle)

Dependency: Phase 6.

Build outcome: Project scanning, selection, cooldown, reconciliation, capacity, leases, dispatch, heartbeats, finalization, and daemon loops have focused modules.

Acceptance: Scheduler ordering, one-worker-per-project, global capacity, recovery, and service-worker tests pass unchanged.

Completion note:

COMPLETED 2026-09-03: Extracted daemon scheduling, project scanning and selection, cooldown/capacity/lease policy, session reconciliation, durable worker reservation/dispatch/recovery, run heartbeats, and worker finalization into focused `scheduler` and `worker` modules; advanced the Phase 7 refactor-plan status. Checks: `cargo fmt --all -- --check`; `cargo clippy --locked --all-targets --all-features -- -D warnings`; `cargo test --locked --all-targets --all-features` (397 library tests and 7 CLI integration tests passed); `git diff --check`.

codex:01a0679a-1748-72f0-a28e-b29ad82cec93
