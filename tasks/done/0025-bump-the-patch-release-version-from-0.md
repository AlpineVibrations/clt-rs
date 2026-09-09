Bump the patch release version from 0.5.2 to 0.5.3 (RELEASE)

Completion note:

COMPLETED 2026-09-01: Updated the crate and lockfile patch version from 0.5.2 to 0.5.3 for the fenced-session crash recovery and interactive TUI stop release. Checks: `cargo test --locked` (325 passed), `cargo clippy --locked --all-targets -- -D warnings`, `cargo fmt --all -- --check`, and `git diff --check`.
