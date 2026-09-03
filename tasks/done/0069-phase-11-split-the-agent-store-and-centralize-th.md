Phase 11: Split the agent store and centralize the sync-async boundary (REFACTOR, AGENT, DATABASE).

Reference: [CLT Refactor Design/Build Plan](../../refactor-plan.md#phase-11-cohesive-agent-repositories-and-runtime-ownership)

Dependency: Phase 10.

Build outcome: Focused repositories sit behind one agent-store facade, and one blocking adapter owns synchronous access to the asynchronous store.

Acceptance: Per-operation runtime construction is removed without changing SQL, transactions, daemon behavior, or store results.

Completion note:
COMPLETED 2026-09-03: Split agent persistence into project/model, worker/lease, session/run, and Git-journal modules behind the existing store facade; added one store-owned blocking runtime adapter and an architecture regression test. Checks: `cargo fmt --all -- --check`; `cargo clippy --locked --all-targets --all-features -- -D warnings`; `cargo test --locked --all-targets --all-features` (408 passed).

codex:01a0689a-950c-7772-8300-71edc82fbe7d
