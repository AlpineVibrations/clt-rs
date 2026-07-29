getting an error on start and top agent clt agent stop
Failed to connect to bus: No medium found
Error: systemctl --user stop clt-agent.service failed with status exit status: 1
pro@8CPU-16GB-SF-A:~/www/agentic-marketing$ clt agent start
Failed to connect to bus: No medium found
Error: systemctl --user daemon-reload failed with status exit status: 1

Completion note:

COMPLETED 2026-07-29: Recovered the Linux systemd user runtime directory for all `systemctl --user` service commands when `XDG_RUNTIME_DIR` is missing, added validation and focused tests, and documented the behavior and recovery guidance. Checks: `cargo fmt --check`; `cargo test systemd_user` (6 passed); `cargo test` (160 passed); `cargo clippy --all-targets -- -D warnings`; `env -u XDG_RUNTIME_DIR target/debug/clt agent status` (`service=running`).
