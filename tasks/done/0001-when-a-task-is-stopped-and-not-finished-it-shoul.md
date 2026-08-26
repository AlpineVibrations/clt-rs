when a task is stopped and not finished it should have a STOPPED flag at the start of its title. and should be have a flag at the start of a title for a running task handled by CLT. could say CLT for those

Completion note:

COMPLETED 2026-08-26: Added dynamic `[STOPPED]` and `[CLT]` prefixes to CLI and TUI task titles using live CLT session-control state without changing stored task text. Checks: `cargo fmt -- --check`; focused flag test; `cargo clippy --all-targets -- -D warnings`; live `target/debug/clt list doing`; `git diff --check`. Full `cargo test -- --test-threads=1` ran twice with 300/301 passing; the unrelated concurrent virgin-database-open race failed in both full runs and passed on isolated rerun.

codex:01a03fdc-9ffc-7252-9c8b-568ec7c1c6a7
