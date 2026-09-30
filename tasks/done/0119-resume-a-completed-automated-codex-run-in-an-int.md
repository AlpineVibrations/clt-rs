Resume a completed automated Codex run in an interactive session (FEATURE, AGENT, TUI)

Blocked note:

BLOCKED 2026-08-09: A separate live interactive Codex session in this repository began making overlapping changes to the same session-resume feature after the initial worktree inspection. Preserved the shared in-flight edits and stopped instead of overwriting or committing them. Checks attempted on the combined tree: `cargo check` passed; `git diff --check` passed; `cargo fmt -- --check` reported formatting changes; `cargo clippy --all-targets -- -D warnings` failed because the concurrent implementation still has seven unused/dead-code warnings. Continue after the other session finishes or its changes are explicitly coordinated.

Completion note:

COMPLETED 2026-08-09: Reconciled the overlapping work, captured automated Codex session IDs, associated them with the exact task moved to Done, and added `c` on a selected Done task to suspend the CLT TUI, resume that Codex session interactively with workspace-write/on-request permissions, and restore the board afterward. Documented the shortcut and compatibility behavior. Verification: `cargo fmt --check`, `cargo test` (171 passed), `cargo clippy --all-targets -- -D warnings`, and `git diff --check` all passed.
