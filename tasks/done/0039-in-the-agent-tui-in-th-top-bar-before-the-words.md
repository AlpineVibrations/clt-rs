in the agent tui in th top bar before the words agetn status it should show the current time of day

Completion note:

COMPLETED 2026-08-03: Added the current local time in `HH:MM` format before the daemon status in the agent pane, with documentation and regression coverage. Checks: `cargo fmt --check`; `cargo test` (160 passed).
