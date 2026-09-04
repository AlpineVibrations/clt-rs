Phase 12: Decompose agent coordinators into explicit testable stages (REFACTOR, AGENT).

Reference: [CLT Refactor Design/Build Plan](../../refactor-plan.md#phase-12-explicit-agent-orchestration-stages)

Dependency: Phase 11.

Build outcome: Scheduler decisions, reconciliation effects, runner launch, supervision, session linking, and result recording are explicit stages with request/result types.

Acceptance: Oversized coordinators are decomposed and deterministic dependencies are injectable while all scheduling and recovery behavior remains unchanged. codex:01a068b1-2a2a-7ff3-b8a8-c0338cdcb607
