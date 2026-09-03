Phase 4: Extract agent domain, configuration, and Turso persistence (REFACTOR, AGENT, DATABASE).

Reference: [CLT Refactor Design/Build Plan](../../refactor-plan.md#phase-4-agent-domain-configuration-and-persistence)

Dependency: Phase 3.

Build outcome: Agent domain records, provider/Codex configuration, Turso migrations, WAL recovery, and the existing persistence facade have explicit module ownership.

Acceptance: Migration SQL, schema version, transaction/fencing behavior, configuration output, and all database tests remain unchanged.

Completion note:

COMPLETED 2026-09-03: Added the internal `agent` module for agent domain records and persisted state enums, provider/Codex configuration, state-directory and store opening, Turso migrations and WAL recovery, and the existing persistence facade. Checks: `cargo fmt --all -- --check`; `cargo clippy --locked --all-targets --all-features -- -D warnings`; `cargo test --locked --all-targets --all-features` (396 library tests and 7 CLI integration tests passed); `git diff --check`.

BLOCKED 2026-09-03: Implementation and all gates pass, but the frozen baseline has Phase 2/3's `src/lib.rs` as an untracked file and Phase 4 necessarily changes that file to declare and consume the extracted `agent` module. Git cannot stage only Phase 4's delta against a path absent from `HEAD`; staging it would also commit the older Phase 0-3 payload and violate the task-scoped exact-one-commit contract. Finalization requires external CLT recovery that resolves the older WORKING journals first, or supported baseline/path-relocation handling.

codex:01a0651a-1fd8-77e1-b899-25316ac10e50
