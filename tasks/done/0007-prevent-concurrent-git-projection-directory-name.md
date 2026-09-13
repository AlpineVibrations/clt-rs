Prevent concurrent Git projection directory name collisions BUG; Parallel managed Git tests reproduced File exists (os error 17) in create_agent_git_tree_projection: process ID plus SystemTime nanoseconds can collide across threads. Use collision-safe temporary directory creation and add concurrent coverage.

Completion note:
COMPLETED 2026-09-10: Replaced timestamp-derived projection directories with tempfile's atomic creation and collision retries, retaining Unix 0700 permissions and automatic cleanup. Added coverage for 32 concurrent projections, independent indexes/worktrees and cleanup, and preservation of the live checkout. Updated the Unreleased changelog. Checks passed: `cargo test --offline --lib managed_git::tests::projection:: -- --nocapture`; `cargo fmt --all`; `cargo fmt --all -- --check`; `cargo clippy --locked --all-targets --all-features -- -D warnings`; `cargo test --locked --all-targets --all-features` (525 unit, 4 architecture, 14 CLI tests); `git diff --check`.

codex:01a08c27-c924-7541-8af8-1153a2ef3f42
