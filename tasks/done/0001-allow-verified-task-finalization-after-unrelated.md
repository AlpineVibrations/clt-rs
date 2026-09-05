Allow verified task finalization after unrelated commits advance the branch (BUG, GIT, RECOVERY)

Completion note:
COMPLETED 2026-09-04: Finalization now accepts the sealed task commit after unrelated commits advance the branch, while retaining exact parent, tree, identity, and unique-session-trailer checks. Added regressions for commit-only recovery, publication with later local or already-pushed commits, preservation of staged and unstaged work, duplicate session claims, and scheduler recovery without reopening Codex. Reproduced the original stuck CommitPending state before the fix. Checks: cargo fmt --all -- --check; cargo clippy --locked --offline --all-targets --all-features -- -D warnings; cargo test --locked --offline --all-targets --all-features (477 unit tests and 17 integration tests passed); git diff --check.

Installed the verified release locally and restarted the existing scheduler; confirmed its executable matches the tested release. Meshdock's paused setting was preserved.
