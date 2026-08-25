# Feature Ideas

## Codebase Architecture

- Split `src/main.rs` (~18K LOC) into proper modules: `src/agent.rs`, `src/tui.rs`, `src/cli.rs`, `src/tasks.rs`, etc.
- Introduce integration and unit tests for each module.

## Quality of Life

- Task search/filter on the CLI (`clt list --search "bug"`) and a `/` filter in the TUI.
- Due-date metadata (`(due:2026-09-01, HIGH)`) with TUI color-coding for overdue items.
- Multiple independent task boards per project for parallel workstreams.

## Agent Improvements

- Parallel agent runs per project (N concurrent agents on disjoint tasks).
- Agent run history & stats: `clt agent stats` showing runs/day, avg completion time, failure rates per project.
- Reusable per-project agent prompt templates (e.g., "always run `cargo test` after changes").

## Integrations

- Bi-directional Linear/GitHub Issues sync with file-backed task boards.
- Slack notifications on agent task completion.
