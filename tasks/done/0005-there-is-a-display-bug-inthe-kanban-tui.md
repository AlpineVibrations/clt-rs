there is a display bug inthe kanban tui. lets say i have 5 tasks and i scroll down the the 4th task and its very long so it pushes the frist three up above the visible scroll area. then i go to task 5 and its short and i hit esc so im not selected on any tasks. the task list will still only show the bottom 1 or 2 single line tasks. even though there is room for all above and more below. it should show the full list up above if there is room for it

Completion note:
COMPLETED 2026-10-06: Backfill earlier tasks after expanded selections collapse, Escape clears selection, or the viewport grows; preserve selection visibility. Added helper and rendered Kanban regressions, updated features and Unreleased changelog. Checks passed: cargo test --locked --bin clt keep_selected_task_visible; cargo test --locked --bin clt tui_kanban_reveals_earlier_tasks; rustfmt --edition 2024 --check build.rs src/main.rs src/lib.rs tests/architecture.rs tests/cli.rs; cargo clippy --no-deps --locked --all-targets --all-features -- -D warnings; cargo test --locked --all-targets --all-features (754 passed, 1 ignored across targets).

codex:01a111d8-c655-78b2-b65e-95df75f3f050
