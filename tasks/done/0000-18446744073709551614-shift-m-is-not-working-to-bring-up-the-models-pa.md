shift m is not working to bring up the models page on some terminals even when shift b does work to bring up backlog view.

Completion note:
COMPLETED 2026-09-04: Recognize uppercase M and lowercase m with Shift when opening or closing Models from Tasks and Agent Projects; preserve plain m model cycling and task input. Added terminal-encoding regression tests and an Unreleased changelog entry. Reproduced the Shift+m failure before the fix. Checks passed: `cargo test --locked --lib tui_models_shortcut -- --nocapture` (2 tests); `cargo fmt --all`; `cargo fmt --all -- --check`; `cargo clippy --locked --all-targets --all-features -- -D warnings`; `cargo test --locked --all-targets --all-features` (490 tests); `git diff --check`.

codex:01a06ebc-eb61-7330-8d53-c46801665f65
