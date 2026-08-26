when the agent commits can it commit with a agent specific name? maybe include some other info about it?

Completion note:

COMPLETED 2026-08-25: Automated COM/PUSH Codex runs now use the process-scoped Git author and committer `CLT Agent <clt-agent@localhost>` without changing Git configuration; added prompt and scoping coverage plus README and changelog documentation. Checks: `cargo fmt -- --check`; `cargo clippy --all-targets -- -D warnings`; `cargo test -- --test-threads=1` (196 passed). codex:01a03a9c-6d34-71e1-a27e-a1b77982be2a
