the agent was running with codex and the machine crashed. now i restarted adn the agent TUI says its stale... and there is a task left in doing... shouldnt it notice that its stale and try to recover and resume

Completion note: COMPLETED 2026-07-21: Reclaimed dead or expired agent leases and launched a recovery run that resumes the interrupted `doing` task before new TODO work. Documented the behavior. Checks: `cargo fmt -- --check`; `cargo test -- --test-threads=1` (135 passed).
