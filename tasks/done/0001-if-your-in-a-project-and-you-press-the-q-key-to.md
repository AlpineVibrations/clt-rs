if your in a project and you press the q key to quit you should exit the app an change directory to that project folder

Completion note:

COMPLETED 2026-07-25: Added Bash/Zsh shell integration that hands the active TUI project back to the calling shell on quit, plus setup documentation, changelog coverage, and focused parsing/handoff tests. Checks: `cargo fmt -- --check`; `cargo clippy --all-targets --all-features -- -D warnings`; `cargo test --all-features -- --test-threads=1` (153 passed); `git diff --check`.
