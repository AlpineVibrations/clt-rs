i have had problems jumping into codex interactive resume on finsihed tasks. one error i got was : Error: Project is reserved by a directly opened Codex session; use clt handoff to release it │
└. we should be able to go in even while some other codex session is running.

Completion note:
COMPLETED 2026-10-06: Allow finished sessions to resume alongside a manual owner while preserving its claim, scheduler fence, and selected-session controls. Added open/exit/reopen and project-routing regressions; updated feature, architecture, and changelog docs. Checks passed: rustfmt --edition 2024 --check build.rs src/main.rs src/lib.rs tests/architecture.rs tests/cli.rs; cargo clippy --no-deps --locked --all-targets --all-features -- -D warnings; cargo test --locked --all-targets --all-features (756 passed, 1 ignored).

codex:01a111df-a7e0-7e01-98c5-b2d2a9852fe1
