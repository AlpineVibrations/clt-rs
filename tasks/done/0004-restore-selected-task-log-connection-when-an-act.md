Restore selected task log connection when an active agent task loses its session marker (BUG)

Completion note:
COMPLETED 2026-09-11: Git-off automated Todo-to-Doing activation now saves the exact run session marker under the board lock and rejects conflicting ownership, including nested tasks. Restored FISHDOME's missing marker without interrupting its agent and verified selected-task Log View [LIVE] in the installed TUI after the task moved to Done. Checks: cargo fmt --all -- --check; cargo clippy --locked --all-targets --all-features -- -D warnings; cargo test --locked --all-targets --all-features (547 unit, 4 architecture, 14 CLI tests passed).

Release note: Included in patch release 0.6.11; manifest, lockfile, and changelog updated together.
