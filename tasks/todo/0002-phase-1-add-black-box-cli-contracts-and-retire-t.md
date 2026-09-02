Phase 1: Add black-box CLI contracts and retire the unsafe flow script (REFACTOR, TESTING).

Reference: [CLT Refactor Design/Build Plan](../../refactor-plan.md#phase-1-black-box-cli-contracts)

Dependency: Phase 0.

Build outcome: The built binary is exercised in temporary workspaces for its public CLI contract, and the unsafe repository-mutating flow script is retired.

Acceptance: Tests assert help, shell integration, init/add/list/status/done/delete/expand, failures, output streams, and exit codes without touching a real board or agent database.
