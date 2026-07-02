Implement agent project scanning and pending-task detection (AGENT, P1).

Design reference: `codex-agent.md` scheduling rules.

Acceptance criteria:
- Add logic to inspect a registered project's `tasks/` board.
- Detect whether the project has available `todo` tasks for an agent run.
- Support the existing Markdown-backed and folder-backed task stores.
- Skip missing or uninitialized projects with clear status information.
- Update `last_scan_at` or equivalent project metadata after scans.
