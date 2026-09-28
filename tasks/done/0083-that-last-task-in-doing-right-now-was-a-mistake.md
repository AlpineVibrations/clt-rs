that last task in doing right now was a mistake any code that was changed should be removed. also this brings up a apoint that we should be able to press c to jump into codex resume on a task that is stopped in addition to blocked or done.

Blocked note:

BLOCKED 2026-08-26: A separate live Codex session modified `src/main.rs` after this run's clean startup inspection, overlapping the header and stopped-session resume work required here. This run made no implementation edits; `git diff --check` passed on the shared worktree. Continue after the other session finishes and its changes are reconciled.

Completion note:

COMPLETED 2026-09-02: Allowed `c` to reopen an unblocked Doing task when its exact linked Codex session is stopped or otherwise idle, while preserving the active-session refusal and post-reservation task revalidation. Updated the TUI guidance, README, and changelog. Checks: `cargo test --locked -- --test-threads=1` (391 passed); `cargo clippy --locked --all-targets -- -D warnings`; `cargo fmt -- --check`; `git diff --check`. codex:01a03fd4-d782-7e51-86e1-f1904c1b3538
