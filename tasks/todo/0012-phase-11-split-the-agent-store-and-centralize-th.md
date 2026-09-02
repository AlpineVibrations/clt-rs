Phase 11: Split the agent store and centralize the sync-async boundary (REFACTOR, AGENT, DATABASE).

Reference: [CLT Refactor Design/Build Plan](../../refactor-plan.md#phase-11-cohesive-agent-repositories-and-runtime-ownership)

Dependency: Phase 10.

Build outcome: Focused repositories sit behind one agent-store facade, and one blocking adapter owns synchronous access to the asynchronous store.

Acceptance: Per-operation runtime construction is removed without changing SQL, transactions, daemon behavior, or store results.
