now the log view shows no logs. it should show the latest out put or the currnet running if there is one running.

Blocked note: BLOCKED 2026-07-16: Updated the TUI log view to stream the active Codex stderr log and fall back to recorded stderr when completed stdout is empty. `cargo test` passed all 117 tests, `cargo fmt -- --check` and `git diff --check` passed, but `cargo clippy --all-targets -- -D warnings` failed on 10 pre-existing unrelated lints in `src/main.rs`; left uncommitted and in doing per the no-commit-on-failed-checks rule.

Completion note:
- COMPLETED 2026-07-16: Verified the committed live stderr streaming and recorded-run stderr fallback, and cleared the strict-Clippy gate as part of the cleanup.
