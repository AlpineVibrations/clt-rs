Phase 14: Tighten visibility, relocate tests, and finalize architecture docs (REFACTOR, ARCHITECTURE).

Reference: [CLT Refactor Design/Build Plan](../../refactor-plan.md#phase-14-final-architecture-cleanup)

Dependency: Phase 13.

Build outcome: Tests live with their owners or in true integration suites, visibility is minimal, transitional imports/re-exports are gone, and documentation reflects the final architecture.

Acceptance: `src/main.rs` is launcher-only, `clt_rs::run()` is the sole public API, no wildcard parent imports remain, and final automated/manual gates pass.

Completion note:
COMPLETED 2026-09-04: Relocated the monolithic crate-root unit suite into subsystem-owned test modules, retained black-box CLI coverage and added a cross-module architecture integration suite, replaced production wildcard/root-prelude imports with explicit module dependencies, narrowed CLI and agent visibility, removed repository re-exports, and recorded the final module map in `ARCHITECTURE.md`. Closed the completed refactor entry in `features.md` and marked the staged plan complete. Automated checks: `cargo fmt --all -- --check`; `cargo clippy --locked --all-targets --all-features -- -D warnings`; `cargo test --locked --all-targets --all-features` (418 passed: 407 unit, 4 architecture integration, 7 black-box CLI).
