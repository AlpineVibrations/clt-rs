Phase 3: Extract the task domain and file-backed board storage (REFACTOR, TASKS).

Reference: [CLT Refactor Design/Build Plan](../../refactor-plan.md#phase-3-task-domain-and-storage)

Dependency: Phase 2.

Build outcome: Task models, marker parsing, Markdown/folder stores, nested boards, ordering, locks, and atomic mutations live in the task module family.

Acceptance: Task storage has no TUI dependency; every Markdown/folder/nested/archive/concurrency test passes with unchanged on-disk behavior.

Completion note:

COMPLETED 2026-09-03: Added the internal `task` module for task models, marker and outcome parsing, Markdown/folder/archive storage, nested-board traversal and creation, ordering, locking, and atomic mutations while retaining managed-Git authorization in the application layer. Checks: `cargo fmt --all -- --check`; `cargo clippy --locked --all-targets --all-features -- -D warnings`; `cargo test --locked --all-targets --all-features` (396 library tests and 7 CLI integration tests passed); `git diff --check`.

BLOCKED 2026-09-03: Implementation and all gates pass, but the frozen baseline has Phase 2's `src/lib.rs` as an untracked file and Phase 3 necessarily changes that file. Git cannot stage only Phase 3's delta against a path absent from `HEAD`; staging it would also commit the older Phase 0/2 payload and violate the task-scoped exact-one-commit contract. Finalization requires external CLT recovery that first resolves the older Phase 0/2 WORKING journals, or supported baseline/path-relocation handling.

COMPLETED 2026-09-03: Manual recovery preserved the verified Phase 3 implementation in combined rescue commit `f881c4f`; the obsolete per-task WORKING journal was cancelled after the extracted task module became part of that recovery boundary.

codex:01a06510-0f91-7d21-86cf-3f131faa1fd5
