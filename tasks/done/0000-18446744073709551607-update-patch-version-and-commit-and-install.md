update patch version and commit and install

Completion note:
COMPLETED 2026-10-21: Bumped clt-rs from 0.8.4 to 0.8.5 in Cargo.toml and Cargo.lock without changing dependencies, dated the existing changelog fix, and updated release install examples. Installed with `cargo +1.97.1 install --path . --locked --force`; `/Users/pro/.cargo/bin/clt --version` and `-V` both report 0.8.5.

Checks passed: `cargo +1.97.1 fmt --all -- --check`; `cargo +1.97.1 clippy --package clt-rs --no-deps --locked --all-targets --all-features -- -D warnings`; `cargo +1.97.1 test --locked --all-targets --all-features` (841 passed, 1 ignored); `RUSTUP_TOOLCHAIN=1.97.1 python3 scripts/check_release.py --allow-dirty` (archive audit and version/task/registry smoke checks); `git diff --check`. Used installed Rust 1.97.1 because the stable alias is incomplete. After the initial test build exhausted disk space, removed inactive repository incremental cache and reran successfully with `CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0` for tests and packaging.

codex:01a15f05-e2a6-70d2-ab1a-eb3de24827de
