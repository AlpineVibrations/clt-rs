Make embedded Codex session markers the sole task resume link (FEATURE, AGENT, TUI)

Completion note: COMPLETED 2026-08-25: Made terminal `codex:<session-id>` markers the sole task-to-session resume association, removed exact task-text database lookup and storage with a schema migration, retained session IDs as structured run history, and report marker persistence failures as failed runs. Updated documentation and regression coverage. Checks: `cargo fmt -- --check`; `cargo clippy --all-targets -- -D warnings`; `cargo test -- --test-threads=1` (195 passed); `git diff --check`.
