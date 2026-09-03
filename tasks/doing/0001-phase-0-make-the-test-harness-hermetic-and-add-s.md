Phase 0: Make the test harness hermetic and add stable CI (REFACTOR, TESTING).

Reference: [CLT Refactor Design/Build Plan](../../refactor-plan.md#phase-0-hermetic-tests-and-ci)

Dependency: none.

Build outcome: Tests cannot resolve the user's live agent state, stable Linux/macOS CI runs the locked quality gates, and Turso is pinned to the WAL-layout version CLT has validated.

Acceptance: Direct `cargo test` and subprocess CLI tests use isolated state; format, strict Clippy, and all tests pass on current stable Rust.

Early work note: 2026-09-02: Before the planning-only boundary was clarified, the unit-test state isolation, exact Turso pin, and stable Ubuntu/macOS CI workflow were added and passed format, strict Clippy, and 392 tests. Review these uncommitted changes when this task is intentionally started; they have not been accepted, committed, or used to activate later phases.

Blocked note:

BLOCKED 2026-09-03: Corrected test isolation so parallel unit tests ignore the parent automated-run context and use per-test temporary state; `cargo fmt --all -- --check` and strict locked Clippy pass, but the full locked suite still has two pre-existing Linux process-supervision failures (`codex_runner_renews_its_automated_project_lease_while_running` times out and `stale_guardian_keeps_a_live_registered_group_fenced_then_recovers_when_gone` hits a broken launch-gate pipe). These need process-supervision fixture or implementation repair before Phase 0 can be completed; full run: 393 passed, 2 failed.

UNBLOCKED 2026-09-03: Stabilized the Linux process-supervision fixtures by exercising the real Rust interactive gate helper and allowing the intentional process-group shutdown proof to finish.

Completion note:

COMPLETED 2026-09-03: Isolated unit-test agent state per test, ignored inherited automated-run context in test builds, retained the exact Turso 0.7.0 pin and stable Ubuntu/macOS CI, and stabilized subprocess supervision coverage. Checks: `cargo fmt --all -- --check`; `cargo clippy --locked --all-targets --all-features -- -D warnings`; `cargo test --locked --all-targets --all-features` (396 unit and 7 integration tests passed).

BLOCKED 2026-09-03: Implementation and all checks pass, but `clt done doing 1` refused to seal because commit `409dd93` is an unproven intervening CLT task-board checkpoint in this session's older WORKING history. External CLT journal recovery is required; the frozen-boundary contract forbids resetting, rebasing, amending, or committing an unsealed payload.

UNBLOCKED 2026-09-03: CLT subsequently proved the intervening Phase 1 finalization at `c11aaa1`; the current full quality run passes against that non-overlapping HEAD.

BLOCKED 2026-09-03: Retried `clt done doing 1` after CLT proved the intervening Phase 1 commit, but sealing still fails because this older WORKING journal contains an unproven intervening commit. The implementation and full quality run pass; CLT journal recovery is required before this task can be sealed and committed safely.

codex:01a064d8-62d4-73e0-af01-9e6a39dd624e
