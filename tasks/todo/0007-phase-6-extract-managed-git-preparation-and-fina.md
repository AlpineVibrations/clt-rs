Phase 6: Extract managed Git preparation and finalization (REFACTOR, AGENT, GIT).

Reference: [CLT Refactor Design/Build Plan](../../refactor-plan.md#phase-6-managed-git-lifecycle)

Dependency: Phase 5.

Build outcome: Git preflight, frozen state, tree projection, manifest proof, publication, leases, journals, and recovery live behind the managed-Git module.

Acceptance: Existing Git lifecycle and crash-recovery tests pass without changing fail-closed semantics or repository command behavior.
