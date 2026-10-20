Keep blocked and stopped tasks in Todo without reclaiming Doing [BUG]

Completion note:
COMPLETED 2026-10-20: Git link repair now recognizes the session before clt:stopped and preserves queued blockers and stops during missing-marker and duplicate-move recovery. Idle queued resumes honor stopped task markers. Blocked workers return their unsealed task to Todo, and independent manual claims can coexist with uniquely linked idle blocked/stopped Todo WORKING journals. Preserved live-owner and sealed-proof fences, updated automated prompts and task-management guidance, and added regressions for both board layouts plus CLI claims and completion.

Checks: rustfmt --edition 2024 --check build.rs src/main.rs src/lib.rs tests/architecture.rs tests/cli.rs; cargo clippy --no-deps --locked --offline --all-targets --all-features -- -D warnings; cargo test --locked --offline --all-targets --all-features (810 passed, 1 ignored); git diff --check. Built and installed the release binary, restarted the scheduler, and returned Multigen's original stopped Blender task to Todo while preserving its notes, session, WORKING journal and open interactive owner.

Release follow-up 2026-10-20: Prepared patch version 0.8.3 with matching manifest, lockfile, dated changelog, comparison links and installation examples. Rechecked formatting and release metadata. CARGO_NET_OFFLINE=true python3 scripts/check_release.py --allow-dirty passed archive compilation, source/provenance audit, and packaged version, task and registry persistence smoke checks.

clt:manual codex:01a15a9b-9089-7662-bd65-1e665dfc5661
