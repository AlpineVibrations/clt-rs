the d key deletes a task. that is bad and accident prone. remove that. the a button archives thats good.

Blocked note:
BLOCKED 2026-09-16: `clt status todo 2 doing` rejected activation because the checkout differs from CLT's frozen launch record at a37a9523cce14871516a17e7631cd74092087e62. Pre-existing board edits added a Todo and renumbered the committed Todo files. Preserved those edits; no implementation or tests ran. Needs scheduler/external recovery of the launch boundary before activation can succeed; this released run cannot refresh it.

BLOCKED 2026-09-16: Recovery retried `clt status todo 1 doing` with a clean checkout at 91a841bc535479e9e633b9bb885fd7123609fb50; CLT still rejected the frozen-launch mismatch. Confirmed this session remains WORKING, its task remains Todo, and no task completion commit exists. No implementation or tests ran. External recovery must reconcile this session's original pre-activation boundary with the intervening committed history; this run cannot reset or replace that boundary.

UNBLOCKED 2026-09-16: Handoff recovery now finds this same session-linked task in Doing with its WORKING journal; continuing implementation from 91a841bc535479e9e633b9bb885fd7123609fb50.

Completion note:
COMPLETED 2026-09-16: Removed task-board d/D/Delete permanent-deletion bindings and their help entry; retained a to archive. Updated README and Unreleased changelog. Passed `rustfmt --edition 2024 --check build.rs src/main.rs src/lib.rs tests/architecture.rs tests/cli.rs`, `cargo clippy --no-deps --locked --all-targets --all-features -- -D warnings`, `cargo test --locked --all-targets --all-features` (621 tests), and `git diff --check`. `python3 /tmp/clt-delete-shortcut-smoke.py` passed isolated PTY checks for Markdown and folder boards: d/D/Delete and Shift encodings preserve task files, and a archives the selection. codex:01a0ab95-efa8-74e3-b76b-62875d2f1c7c
