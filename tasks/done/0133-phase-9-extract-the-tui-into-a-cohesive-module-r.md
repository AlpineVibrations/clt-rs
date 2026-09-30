Phase 9: Extract the TUI into a cohesive module (REFACTOR, TUI).

Reference: [CLT Refactor Design/Build Plan](../../refactor-plan.md#phase-9-cohesive-tui-extraction)

Dependency: Phase 8.

Build outcome: All TUI state, panels, input, rendering, terminal management, and event-loop behavior live in the TUI module family; the transitional monolith is removed.

Acceptance: TUI helper/render tests and the complete compatibility suite pass with unchanged keyboard and display behavior.

Completion note:

COMPLETED 2026-09-03: Extracted the remaining application coordination into `src/application.rs` and all TUI state, panels, input, rendering, terminal management, and event-loop behavior into `src/tui.rs`, leaving `src/lib.rs` as launcher/module wiring plus the unchanged compatibility tests; advanced the refactor plan through Phase 9. Checks: `cargo fmt --all`; `cargo fmt --all -- --check`; `cargo check --locked --all-targets --all-features`; `cargo clippy --locked --all-targets --all-features -- -D warnings`; `cargo test --locked --all-targets --all-features` (397 library tests and 7 CLI integration tests passed); `git diff --check`. codex:01a06876-efd6-7e42-a3eb-8a7ef6a40792
