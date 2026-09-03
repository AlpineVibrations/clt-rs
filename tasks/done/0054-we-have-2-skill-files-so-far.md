we have 2 skill files so far. our clt skill and the git-commit skill for our agent features. we document how to copy them but many people might not read the readme. we should have those files come with the install somehow thats standard maybe embedded or.. whats best. and then if they are not found by name in the agents folder we use our own embedded version for the codex agent work

Completion note:

COMPLETED 2026-07-23: Embedded both bundled `SKILL.md` files in the binary, detect installed skills by frontmatter name across standard Codex skill directories, inject only required missing skills into automated-run prompts, and documented the fallback. Checks: `cargo fmt -- --check`; `cargo test` (145 passed); `cargo clippy --all-targets -- -D warnings`; `cargo package --allow-dirty --list` confirmed both skill files are packaged.
