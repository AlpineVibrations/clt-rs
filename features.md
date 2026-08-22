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


## Goal Toggle per Task
• Use /goal when the work is:

  - Too large for one normal turn.
  - Focused on one durable objective.
  - Safe for Codex to advance independently.
  - Equipped with a test, artifact, or other verifiable stopping condition.

  Good examples include migrations, large refactors, repeated deployment fixes, benchmark-driven experiments, and substantial
  prototypes.

  A strong goal looks like:

  /goal Migrate the authentication module to OAuth 2.1 without changing its public API. Work in checkpoints and stop when all
  existing tests and the new OAuth integration tests pass.

  Avoid /goal for quick fixes, exploratory questions, vague requests such as “improve the codebase,” unrelated backlogs, or work
  requiring frequent product decisions. In those cases, use a regular prompt or create a plan first.

  Rule of thumb: if Codex can objectively determine “done” and keep making useful progress without you, /goal is a good fit. You
  can inspect it with /goal and control it using /goal pause, /goal resume, and /goal clear. Official OpenAI /goal guide
  (https://learn.chatgpt.com/use-cases/follow-goals)
