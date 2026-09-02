Phase 7: Extract scheduler and worker lifecycle modules (REFACTOR, AGENT).

Reference: [CLT Refactor Design/Build Plan](../../refactor-plan.md#phase-7-scheduler-and-worker-lifecycle)

Dependency: Phase 6.

Build outcome: Project scanning, selection, cooldown, reconciliation, capacity, leases, dispatch, heartbeats, finalization, and daemon loops have focused modules.

Acceptance: Scheduler ordering, one-worker-per-project, global capacity, recovery, and service-worker tests pass unchanged.
