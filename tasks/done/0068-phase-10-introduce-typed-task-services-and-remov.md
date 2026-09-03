Phase 10: Introduce typed task services and remove task-to-agent coupling (REFACTOR, ARCHITECTURE).

Reference: [CLT Refactor Design/Build Plan](../../refactor-plan.md#phase-10-typed-task-services)

Dependency: all Stage A phases.

Build outcome: Internal `TaskStatus`, `TaskBoard`, and managed-task workflow APIs replace raw status strings and task storage no longer accesses agent persistence.

Acceptance: Serialized status values and behavior remain unchanged; dependency checks confirm `task` does not import `agent` or `tui`.

Completion note:

COMPLETED 2026-09-03: Added typed `TaskStatus` and `TaskBoard` storage APIs, routed CLI mutations through `ManagedTaskWorkflow`, converted task consumers from raw status strings, and removed agent-specific naming from task-storage operations while preserving serialized values and behavior; advanced the refactor plan through Phase 10. Checks: `cargo fmt --all`; `cargo fmt --all -- --check`; `cargo clippy --locked --all-targets --all-features -- -D warnings`; `cargo test --locked --all-targets --all-features` (400 library tests and 7 CLI integration tests passed); `git diff --check`. codex:01a06880-abd4-7c61-a87b-c99021eaa6e5
