when a project is registed with aclt agent then its status should show on the console titlebar. only omit the status when the project is not registerd with agents

Completion note:
COMPLETED 2026-09-16: Show the current registered project's runtime status in the Kanban console title even when its agent is OFF; updated README and changelog. Passed `rustfmt --edition 2024 --check build.rs src/main.rs src/lib.rs tests/architecture.rs tests/cli.rs`, `cargo clippy --no-deps --locked --all-targets --all-features -- -D warnings`, `cargo test --locked --all-targets --all-features` (621 tests), and `git diff --check`. Terminal smoke check with an isolated registry confirmed status for registered OFF and ON projects and omission after unregistering. Preserved unrelated concurrent Git-workflow edits and task.

codex:01a0aba7-6c2b-7df3-9067-ca820e6cac40
