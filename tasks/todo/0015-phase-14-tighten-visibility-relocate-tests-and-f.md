Phase 14: Tighten visibility, relocate tests, and finalize architecture docs (REFACTOR, ARCHITECTURE).

Reference: [CLT Refactor Design/Build Plan](../../refactor-plan.md#phase-14-final-architecture-cleanup)

Dependency: Phase 13.

Build outcome: Tests live with their owners or in true integration suites, visibility is minimal, transitional imports/re-exports are gone, and documentation reflects the final architecture.

Acceptance: `src/main.rs` is launcher-only, `clt_rs::run()` is the sole public API, no wildcard parent imports remain, and final automated/manual gates pass.
