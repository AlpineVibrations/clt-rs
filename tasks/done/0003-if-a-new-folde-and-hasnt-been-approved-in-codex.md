if a new folde and hasnt been approved in codex we get error: Reading additional input from stdin...                                                                                                                │
│Not inside a trusted directory and --skip-git-repo-check was not specified.                                                                           │
│Automated supervisor retains its owned Codex group after the Codex group leader exited: Failed to request Codex process-group termination: Failed to s│
│ignal Codex process group 29315 with 15: Operation not permitted (os error 1)                          not sure how to handle but we might need it to make a new task to get permission by the user. can we ask for it. what needs to happen. also getting error trying to unregister this test project. Error: Failed to unregister project /Users/pro/code/test

Completion note:
COMPLETED 2026-08-26: Treated explicit CLT registration as opt-in for non-Git Codex automation, added `--skip-git-repo-check` to new and resumed exec sessions, and made unregister transactionally remove dependent run history. Checks: `cargo fmt --check`; `cargo test` (267 passed); `cargo clippy --all-targets -- -D warnings`; `git diff --check`.

codex:01a03e9a-65f4-7b13-bfec-d789936617dd
