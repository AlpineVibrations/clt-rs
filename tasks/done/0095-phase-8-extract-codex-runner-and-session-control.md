Phase 8: Extract Codex runner and session-control modules (REFACTOR, AGENT, CODEX).

Reference: [CLT Refactor Design/Build Plan](../../refactor-plan.md#phase-8-codex-runner-and-sessions)

Dependency: Phase 7.

Build outcome: Codex prompts/commands, gates, supervisors, session linking, logs, handoffs, stop/resume/interrupt, and outcome recording have explicit ownership.

Acceptance: Hidden process protocol, prompt text, session fencing, logs, and interactive/automated recovery tests remain unchanged.

Completion note:

COMPLETED 2026-09-03: Extracted Codex runner and session-control ownership into `src/runner.rs` and `src/session_control.rs`, kept the hidden process/prompt/session protocols behavior-preserving, and advanced the refactor plan through Phase 8. Checks: `cargo fmt --all`; `cargo fmt --all -- --check`; `cargo check --locked --all-targets --all-features`; `cargo clippy --locked --all-targets --all-features -- -D warnings`; `cargo test --locked --all-targets --all-features` (397 unit and 7 CLI tests passed). codex:01a06864-c918-7122-b082-be0f2c77eea9
