Allow a new concurrent Todo while sealing a managed Git task (BUG, GIT, HIGH)

Completion note: COMPLETED 2026-09-02: Made managed Git baseline checks ignore unstaged task-board-only drift while retaining exact staged-tree proof and non-task safeguards; added an end-to-end concurrent-Todo finalization regression, updated workflow/design documentation and changelog, and recorded the separate refactor UX follow-up. Checks: `cargo test --locked -- --test-threads=1` (396 passed), `cargo clippy --locked --all-targets -- -D warnings`, `cargo fmt --all -- --check`, and `git diff --check`.
