when starting clt in a non clt fodler it asks if we want to init the folder. we have to press y or n and then enter. it should not need enter. a y or an n should work without the enter

Completion note:

COMPLETED 2026-07-24: Replaced the line-buffered initialization confirmation with immediate `y`/`n` key handling, safe raw-mode cleanup, and focused key-choice coverage. Checks: `cargo fmt -- --check`; `cargo test` (150 passed); `cargo clippy --all-targets --all-features -- -D warnings`; pseudo-terminal smoke test of `clt --local` with a bare `y` keypress.
