Support /goal-prefixed tasks in automated Codex runs (FEATURE)

Completion note:

COMPLETED 2026-08-25: Enabled the Codex goals feature for Rust and shell automated runs; taught the shared task prompt to convert only a leading `/goal` token into a persistent goal with the directive removed; documented the behavior and removed the completed feature idea. Checks: `cargo test` (191 passed), `cargo fmt -- --check`, `git diff --check`, `bash -n scripts/run_codex_tasks.sh`, and `zsh -n scripts/run_codex_tasks.sh`.
