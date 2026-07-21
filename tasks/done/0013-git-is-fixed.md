git is fixed. clean up and finish any lingering tasks

Completion note:
- COMPLETED 2026-07-16: Reconciled the three stale doing entries and fixed ten strict-Clippy findings with behavior-preserving Rust cleanups. Verified with `cargo fmt -- --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test -- --test-threads=1` (117 passed), and `git diff --check`.
