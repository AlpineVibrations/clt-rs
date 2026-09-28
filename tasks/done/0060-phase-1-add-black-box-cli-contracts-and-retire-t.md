Phase 1: Add black-box CLI contracts and retire the unsafe flow script (REFACTOR, TESTING).

Reference: [CLT Refactor Design/Build Plan](../../refactor-plan.md#phase-1-black-box-cli-contracts)

Dependency: Phase 0.

Build outcome: The built binary is exercised in temporary workspaces for its public CLI contract, and the unsafe repository-mutating flow script is retired.

Acceptance: Tests assert help, shell integration, init/add/list/status/done/delete/expand, failures, output streams, and exit codes without touching a real board or agent database.

Completion note:

COMPLETED 2026-09-03: Added hermetic black-box CLI contracts for help, shell integration, Markdown and folder initialization, the full task lifecycle, expansion, and invalid-input output/exit behavior; removed `test_flow.sh`. Checks: `cargo fmt --all -- --check`; `cargo clippy --locked --all-targets --all-features -- -D warnings`; `cargo test --locked --all-targets --all-features` (396 unit and 7 integration tests passed).

codex:01a064ee-289b-7080-8ed5-faba847e4614
