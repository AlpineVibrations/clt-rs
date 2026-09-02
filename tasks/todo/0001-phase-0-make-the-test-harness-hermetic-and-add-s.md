Phase 0: Make the test harness hermetic and add stable CI (REFACTOR, TESTING).

Reference: [CLT Refactor Design/Build Plan](../../refactor-plan.md#phase-0-hermetic-tests-and-ci)

Dependency: none.

Build outcome: Tests cannot resolve the user's live agent state, stable Linux/macOS CI runs the locked quality gates, and Turso is pinned to the WAL-layout version CLT has validated.

Acceptance: Direct `cargo test` and subprocess CLI tests use isolated state; format, strict Clippy, and all tests pass on current stable Rust.

Early work note: 2026-09-02: Before the planning-only boundary was clarified, the unit-test state isolation, exact Turso pin, and stable Ubuntu/macOS CI workflow were added and passed format, strict Clippy, and 392 tests. Review these uncommitted changes when this task is intentionally started; they have not been accepted, committed, or used to activate later phases.
