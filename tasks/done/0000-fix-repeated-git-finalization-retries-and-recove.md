Fix repeated Git finalization retries and recover dnd-friend-www [BUG, HIGH]

COMPLETED 2026-10-04: Automatically retire unverified managed Git attempts after branch switches under the existing board and idle-owner fences. Requeue still-linked tasks for fresh verification; retire orphaned attempts without rewriting current tasks or Git history. Preserve stopped tasks/sessions, verified commits and concurrent owners, and avoid repeatedly launching the obsolete session. Added regression coverage for orphan/replacement links, fresh scheduling, detached HEAD, stop races and existing ownership checks. Updated feature and architecture documentation and patch version to 0.7.9.

Checks: required rustfmt check; strict cargo clippy; cargo test --locked --all-targets --all-features (738 passed, one ignored). An existing damaged-registry fixture failed once during overlapping suite runs, then passed alone and in the clean full-suite rerun. Release build and local Cargo installation passed. Restarted the scheduler and verified that dnd-friend-www automatically retired its stale master attempt and launched next_todo on auth while retaining its master branch.

clt:manual codex:01a107e3-345a-7360-991a-930c47e02231
