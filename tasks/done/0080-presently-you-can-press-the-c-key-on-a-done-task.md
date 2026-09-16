presently you can press the c key on a done task to enter into a live console view with codex. that should also work for a blocked task aas those are not being used or updated anymore.

Completion note:

COMPLETED 2026-08-13: Enabled `c` to resume recorded Codex sessions from currently blocked Todo/Doing tasks as well as Done tasks, persisted session IDs for blocked agent runs, and updated the TUI guidance and documentation. Checks: `cargo fmt -- --check`; `cargo test interactive_codex_resume_accepts_done_and_currently_blocked_tasks -- --test-threads=1`; `cargo test blocked_agent_run_records_its_codex_session_for_the_blocked_task -- --test-threads=1`; `cargo clippy --all-targets -- -D warnings`; `cargo test -- --test-threads=1` (179 passed); `git diff --check`.
