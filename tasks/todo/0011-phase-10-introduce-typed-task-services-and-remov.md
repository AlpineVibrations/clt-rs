Phase 10: Introduce typed task services and remove task-to-agent coupling (REFACTOR, ARCHITECTURE).

Reference: [CLT Refactor Design/Build Plan](../../refactor-plan.md#phase-10-typed-task-services)

Dependency: all Stage A phases.

Build outcome: Internal `TaskStatus`, `TaskBoard`, and managed-task workflow APIs replace raw status strings and task storage no longer accesses agent persistence.

Acceptance: Serialized status values and behavior remain unchanged; dependency checks confirm `task` does not import `agent` or `tui`.
