the console should show when log view is active. when opening log view the console should clear its current text also. update patch version.

Completion note:
COMPLETED 2026-09-10: Added an explicit Log View title, cleared previous feedback on opening logs in both panes, and kept registry refresh errors from covering open output. Bumped the patch version to 0.6.9 and updated documentation. Checks passed: `cargo fmt --all -- --check`; `cargo clippy --locked --all-targets --all-features -- -D warnings`; `cargo test --locked --all-targets --all-features` (539 tests, including console rendering regression); `git diff --check`.

codex:01a08bd2-4322-7692-8425-74d477e42bcf
