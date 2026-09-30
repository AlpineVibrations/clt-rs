add something minimal in the clt skill about not creating tasks with circular dependencies

Completion note:
COMPLETED 2026-09-29: Added two sentences to the bundled clt skill requiring dependency-chain checks and avoiding direct or indirect cycles; recorded the change under Unreleased. Checks passed: `uv run --no-project --with pyyaml python /Users/pro/.codex/skills/.system/skill-creator/scripts/quick_validate.py skills/clt-task-management`, `git diff --check`, `rustfmt --edition 2024 --check build.rs src/main.rs src/lib.rs tests/architecture.rs tests/cli.rs`, `cargo clippy --no-deps --locked --all-targets --all-features -- -D warnings`, and `cargo test --locked --all-targets --all-features` (702 passed, 1 ignored). The initial CLI installation check used a stale packaged binary; `cargo clean --package clt-rs` followed by the full test command rebuilt it and passed, including the embedded-skill installation test.

codex:01a0edb2-bd41-7760-9660-f46b9cb2d795
