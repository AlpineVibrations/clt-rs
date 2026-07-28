when we are entering a new task in the tui and we paste in content with line returns it breaks and enters in tons of new entrires like oone per line. i nnoticed that in openai codex which is also a tui when you paste in. it shows a blue colored [Pasted Content line count] and then you can keep typeing and when your done it expands that out. we need that.

Completion note:

COMPLETED 2026-07-28: Enabled bracketed paste for TUI task input, collapsed multiline clipboard text into a blue `[Pasted Content N lines]` placeholder until submission, and added focused placeholder expansion, deletion, and single-line paste coverage. Checks: `cargo test task_input -- --nocapture`, `cargo test`, `cargo fmt -- --check`, `cargo clippy --all-targets -- -D warnings`, and `git diff --check`.
