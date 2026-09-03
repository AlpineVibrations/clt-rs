Control viewed Codex sessions from Agent Projects (TUI, CODEX)

Completion note:

COMPLETED 2026-08-30: Agent Projects output views now retain an exact project/session target and support `s`, `i`, and `c` without relying on a visible task marker. Shared handoff helpers preserve the existing lease, fencing, guardian, and cleanup behavior across Agent Projects and task-board controls. Updated TUI guidance and README documentation; checks: `cargo fmt -- --check`, `cargo test` (311 passed), and `cargo clippy --all-targets --all-features -- -D warnings`.
