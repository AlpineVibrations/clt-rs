Default new queued tasks to Todo and use Backlog only when the user asks.

Completion note:
COMPLETED 2026-10-08: Updated the bundled task-management skill to default queued tasks to Todo and use Backlog only when the user asks, removed Backlog from the default queued-work pipeline, and recorded the change under Unreleased. Checks: manual skill review, git diff --check, required rustfmt check, cargo clippy --no-deps --locked --all-targets --all-features -- -D warnings, and cargo test --locked --all-targets --all-features passed; quick_validate.py could not run because PyYAML is not installed.

clt:manual codex:01a11c3a-dfc0-77b0-9a6b-887a960bed22
