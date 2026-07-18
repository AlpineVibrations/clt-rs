Make agent Codex lookup portable across Node installations (BUG, LINUX, AGENT)

Completion note: COMPLETED 2026-07-15: Agent services now resolve `codex` from `PATH` by default, preserve explicit `CLT_AGENT_CODEX_PATH` overrides, and restart existing Linux systemd services after rewriting their unit. Updated service-generation tests and documentation; verified with `cargo fmt -- --check`, `git diff --check`, and `cargo test -- --test-threads=1` (111 passed).
