Recover blocked doing tasks when the automated agent exhausts the todo queue (FEATURE, AGENT)

Completion note:
- COMPLETED 2026-08-09: Added blocked-note detection for Markdown and folder boards, a one-task blocked recovery scheduler mode, unchanged-recovery backoff state, agent status visibility, prompts, documentation, and regression coverage. Checks: `cargo test -- --test-threads=1` (166 passed); `cargo clippy --all-targets -- -D warnings`; `cargo fmt -- --check`; `git diff --check`.
