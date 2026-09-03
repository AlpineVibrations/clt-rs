Phase 2: Add the launcher library, thin binary, and CLI module (REFACTOR, ARCHITECTURE).

Reference: [CLT Refactor Design/Build Plan](../../refactor-plan.md#phase-2-launcher-library-and-cli-module)

Dependency: Phase 1.

Build outcome: `src/main.rs` delegates to the sole public Rust entry point, `clt_rs::run()`, while Clap definitions and dispatch live in an internal CLI module.

Acceptance: Visible and hidden command parsing, help, stdout/stderr, and process exit behavior remain unchanged; all gates pass.

Completion note:

COMPLETED 2026-09-03: Added the public `clt_rs::run()` launcher, reduced `src/main.rs` to a thin binary, and moved Clap definitions plus command dispatch into the internal `cli` module. Checks: `cargo fmt --all -- --check`; `cargo clippy --locked --all-targets --all-features -- -D warnings`; `cargo test --locked --all-targets --all-features` (396 library tests and 7 CLI integration tests passed).

BLOCKED 2026-09-03: Implementation and all gates pass, but `clt done doing 2` cannot seal because the frozen baseline records Phase 0's unrelated unstaged patch at `src/main.rs`, while this required file split preserves that patch at `src/lib.rs`. Finalization needs external CLT journal recovery after Phase 0 is resolved, or supported baseline path-relocation handling; committing Phase 0's changes in this task would violate the scoped-commit contract.

BLOCKED 2026-09-03: Reverified the durable launcher split and all locked quality gates. Phase 0's older WORKING journal remains unresolved, and CLT's per-path worktree baseline cannot preserve its `src/main.rs` patch after Phase 2 relocates that code to `src/lib.rs`; Phase 0 must be finalized first or CLT must support baseline path relocation before this task can be sealed safely.

BLOCKED 2026-09-03: Interactive recovery confirmed that both Phase 0 and Phase 2 remain in WORKING finalization state and no Phase 2 task commit exists. The verified implementation is preserved, but external journal recovery is still required because no scoped staging layout can retain Phase 0's frozen `src/main.rs` patch after the required move to `src/lib.rs`.

BLOCKED 2026-09-03: Recovery now finds Phases 3–6 also implemented on top of Phase 2's untracked `src/lib.rs`, with all six task journals still WORKING and no Phase 2 commit. Staging Phase 2 alone can no longer produce a buildable tree without absorbing or discarding later task work; resolve the ordered WORKING journals externally before finalization.

COMPLETED 2026-09-03: Manual recovery preserved the verified Phase 2 implementation in combined rescue commit `f881c4f`; the obsolete per-task WORKING journal was cancelled because later extracted modules now depend on this committed launcher/library split.

codex:01a064fb-ac78-7ac0-ba28-963a09cac536
