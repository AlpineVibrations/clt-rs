the git-commit needs work. first in the TUI we need a 3rd option for git. it could be off, commit, or commit & push. abbreviated for the tui inteligently. the skill needs to be updated to reflect that sometimes we will push. and the agent needs to follow the tui setting.

Completion note:

COMPLETED 2026-07-22: Added persisted `OFF`/`COM`/`PUSH` Git modes with migration of existing commit-enabled projects, matching TUI/CLI controls and agent commit/push prompts, plus updated changelog, README, and `git-commit` skill guidance. Checks: `cargo fmt -- --check`; `cargo clippy --all-targets -- -D warnings`; `cargo test -- --test-threads=1` (140 passed).
