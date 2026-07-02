Implement clt agent register, unregister, and projects commands (AGENT, P1).

Design reference: `codex-agent.md` command design.

Acceptance criteria:
- `clt agent register [path]` stores a canonical absolute project path.
- Registration is idempotent for already-registered projects.
- Registration verifies that a `tasks/` board exists or reports a useful error.
- `clt agent unregister [path]` removes the registry entry without deleting project files.
- `clt agent projects` lists registered projects with enabled state and recent run fields.
