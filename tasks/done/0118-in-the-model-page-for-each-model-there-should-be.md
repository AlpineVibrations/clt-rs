in the model page for each model there should be a reasoning level. it defautls to use system default but you could set it for each one and it wouldbe used as defautl when appropriate.

Completion note:

COMPLETED 2026-08-21: Added persisted per-model reasoning defaults to the Models page with a system fallback and `t` cycling; agent runs inherit the selected model's default unless the project has an explicit reasoning override. Updated documentation and regression coverage. Checks: `cargo check`; `cargo test model -- --test-threads=1` (6 passed); `cargo test codex_runner -- --test-threads=1` (3 passed); `cargo test -- --test-threads=1` (185 passed); `cargo fmt -- --check`; `cargo clippy --all-targets -- -D warnings`; `git diff --check`.
