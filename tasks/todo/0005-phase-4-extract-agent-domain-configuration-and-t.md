Phase 4: Extract agent domain, configuration, and Turso persistence (REFACTOR, AGENT, DATABASE).

Reference: [CLT Refactor Design/Build Plan](../../refactor-plan.md#phase-4-agent-domain-configuration-and-persistence)

Dependency: Phase 3.

Build outcome: Agent domain records, provider/Codex configuration, Turso migrations, WAL recovery, and the existing persistence facade have explicit module ownership.

Acceptance: Migration SQL, schema version, transaction/fencing behavior, configuration output, and all database tests remain unchanged.
