we need a small udpate to the skill file for clt it should say something about not creating tasks in stopped mode unless explicity asked for .

Completion note:
COMPLETED 2026-10-06: Required an explicit user request for stopped-task creation in the bundled CLT skill, reconciled missing-session and unavailable-command fallbacks, and updated CHANGELOG.md. Passed: skill quick_validate.py via uv with PyYAML; git diff --check; rustfmt --edition 2024 --check build.rs src/main.rs src/lib.rs tests/architecture.rs tests/cli.rs; cargo clippy --no-deps --locked --all-targets --all-features -- -D warnings; cargo test --locked --all-targets --all-features; cargo test --locked --test cli skills_install_uses_embedded_files_without_a_task_board.

codex:01a11349-90a2-7570-98fc-d682945ef887
