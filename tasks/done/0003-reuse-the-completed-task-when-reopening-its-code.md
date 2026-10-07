Reuse the completed task when reopening its Codex session interactively.

Completion note:
COMPLETED 2026-10-06: Reopen the same completed task in Doing before interactive Codex launches, provide its current task context, and restore Done on explicit completion or interactive exit. Preserve terminal Git proof and other session owners; recover failed launches, interrupted moves, and crashed guardians with exact ownership checks. Update the bundled CLT and Git skills to prevent replacement stopped tracking tasks.

Checks: required rustfmt check; cargo clippy --no-deps --locked --all-targets --all-features -- -D warnings; cargo test --locked --all-targets --all-features. The isolated staged version passed 696 application tests, 47 library tests, 4 architecture checks, and 20 CLI tests; one existing test is ignored. The combined worktree also passed all checks, including the separate pending task-deletion changes.

clt:manual codex:01a1127f-290c-7790-9def-74b3db157350
