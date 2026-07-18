in the agent TUI each project should have a codex model setting With fast or not, and which model and which thinking

Completion note: COMPLETED 2026-07-18: Added persisted per-project Codex model, reasoning, and fast-mode settings to the agent TUI and applied them to automated Codex launches. Checks: `cargo fmt -- --check`; `cargo test -- --test-threads=1` (123 passed); `cargo clippy --all-targets -- -D warnings`.
