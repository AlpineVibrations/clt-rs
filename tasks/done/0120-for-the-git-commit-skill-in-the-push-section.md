for the git commit skill in the push section. if we are pushing then we should pull first. and merge or rebase as the local user has it configd

Completion note:

COMPLETED 2026-07-22: Updated the bundled git-commit skill and generated agent push prompt to pull first while honoring the user's configured merge/rebase strategy; added prompt coverage and a changelog entry. Checks: `cargo test tests::agent_codex_prompt_follows_git_mode -- --exact --test-threads=1` (1 passed); `cargo fmt -- --check`; `cargo clippy --all-targets -- -D warnings`; `cargo test -- --test-threads=1` (142 passed).
