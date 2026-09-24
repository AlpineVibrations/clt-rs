clt agent plan sessions shold work on stopped tasks.. getting a error:: Unable to open a Codex planning session: This task is stopped; press s on it to allow it to start again

Completion note:
COMPLETED 2026-09-24: Enabled planning for stopped Todos while preserving the stop marker and session link through edits and reopening; explicit s restarts the task for automation. Updated feature documentation and added Markdown/folder regression coverage. Checks passed: cargo test --locked --bin clt session_control::planning::tests; rustfmt --edition 2024 --check build.rs src/main.rs src/lib.rs tests/architecture.rs tests/cli.rs; cargo clippy --no-deps --locked --all-targets --all-features -- -D warnings; cargo test --locked --all-targets --all-features (674 passed, 1 optional smoke test ignored); git diff --check.

codex:01a0d3a2-abd7-7922-9947-598a9406f0b4
