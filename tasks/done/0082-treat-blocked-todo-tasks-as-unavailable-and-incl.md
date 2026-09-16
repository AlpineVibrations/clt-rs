Treat blocked Todo tasks as unavailable and include them in all-blocked recovery (BUG, AGENT)

Completion note:

COMPLETED 2026-08-09: Blocked Todo entries are excluded from normal scheduling and included with Doing tasks in all-blocked recovery, with latest-state note handling, prompts, status visibility, documentation, and regression coverage. Checks: `cargo fmt --check`; `cargo test` (167 passed); `cargo clippy --all-targets -- -D warnings`; `git diff --check`.
