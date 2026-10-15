Allow active task finalization while an older Done conversation is open interactively

Completion note:
COMPLETED 2026-10-15: Shared interactive conversations without unfinished Git journals no longer block the active task's commit verification, publication, or terminal Done state. Finalizer acquisition, renewal, and ownership use the same guard; exclusive owners and unfinished Git proof remain protected. Regression coverage keeps the older conversation open across completion in Markdown and folder boards, in commit and commit-and-push modes, while preserving its task, controls, and journal. Verified formatting, Clippy, and the full test suite (806 passed, 1 existing ignored): `rustfmt --edition 2024 --check build.rs src/main.rs src/lib.rs tests/architecture.rs tests/cli.rs`; `cargo clippy --no-deps --locked --all-targets --all-features -- -D warnings`; `cargo test --locked --all-targets --all-features`.

clt:manual codex:01a14010-2041-78a0-ab3e-9d7eaa6470b6
