the log view is working good in the agent pane of the TUI. it would be better if we could see the current log of a running live agent. is that made available for us to watch or do we need to wait till it finished...?

Blocked note:
- BLOCKED 2026-07-16: Implemented explicit `LIVE`/`LATEST` agent-output labels, clarified the `l` key help, and added a passing live-refresh test. `cargo fmt --check`, focused tests, `git diff --check`, and `cargo test -- --test-threads=1` all pass (116 tests), but the required commit cannot be created because the workspace mounts `.git` read-only and Git cannot create `.git/index.lock`. Continue by making `.git` writable, then commit the source and task-board changes together.
