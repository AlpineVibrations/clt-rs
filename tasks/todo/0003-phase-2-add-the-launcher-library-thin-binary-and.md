Phase 2: Add the launcher library, thin binary, and CLI module (REFACTOR, ARCHITECTURE).

Reference: [CLT Refactor Design/Build Plan](../../refactor-plan.md#phase-2-launcher-library-and-cli-module)

Dependency: Phase 1.

Build outcome: `src/main.rs` delegates to the sole public Rust entry point, `clt_rs::run()`, while Clap definitions and dispatch live in an internal CLI module.

Acceptance: Visible and hidden command parsing, help, stdout/stderr, and process exit behavior remain unchanged; all gates pass.
