when moving a task to done it should go to the top of the list so its visible.

Completion note:
COMPLETED 2026-09-04: TUI moves now select the newest task at the top of Done and scroll it into view. Added rendered regression coverage for repeated completions in Markdown and folder boards; documented in CHANGELOG.md. Ran cargo fmt --all; cargo fmt --all -- --check; cargo clippy --locked --all-targets --all-features -- -D warnings; cargo test --locked --lib tui_done_move_selects_and_shows_the_newest_completion (failed before fix, passed after); cargo test --locked --all-targets --all-features (469 passed, five independent recovery failures; all 118 task/TUI tests passed); cargo test --locked --test cli --test architecture (17 passed); git diff --check. The same five failures reproduced on archived starting revision f80f825534c731d82b5358711db5b3e02aaaae52 (468 passed, five failed). Evidence and unblock requirement are in linked blocked follow-up tasks/doing/0002-repair-orphan-journal-recovery-failures-for-maco.md, created through clt follow-up; no follow-up implementation attempted.

codex:01a06eca-03ea-7621-b210-c1aee2b2e8f9
