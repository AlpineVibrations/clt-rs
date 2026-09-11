in the agent log view it should show the codex model and think settings for that run that the log is showing.

Completion note:
COMPLETED 2026-09-04: Added a persistent Model/Thinking log footer from the displayed run's startup header, including completed stdout views; unavailable settings show unknown. Added live refresh, historical selection, header parsing, and rendering coverage; updated README and changelog. Checks passed: `cargo fmt --all`; `cargo fmt --all -- --check`; `cargo test --locked --lib agent_log -- --nocapture` (9 tests); `cargo clippy --locked --all-targets --all-features -- -D warnings`; `cargo test --locked --all-targets --all-features` (481 tests); `git diff --check`.

codex:01a06e89-f7f3-72f1-bb60-a212560d98b6
