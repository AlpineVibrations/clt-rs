Phase 3: Extract the task domain and file-backed board storage (REFACTOR, TASKS).

Reference: [CLT Refactor Design/Build Plan](../../refactor-plan.md#phase-3-task-domain-and-storage)

Dependency: Phase 2.

Build outcome: Task models, marker parsing, Markdown/folder stores, nested boards, ordering, locks, and atomic mutations live in the task module family.

Acceptance: Task storage has no TUI dependency; every Markdown/folder/nested/archive/concurrency test passes with unchanged on-disk behavior.
