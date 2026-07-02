Add Turso-backed agent registry initialization and migrations (AGENT, P1).

Design reference: `codex-agent.md` Turso SQL registry.

Acceptance criteria:
- Add Turso database support to the Rust project.
- Initialize an agent database at the resolved state path without requiring a C compiler.
- Create tables for registered projects, runs, and leases.
- Add a `schema_migrations` table and explicit SQL migration path.
- Keep database access behind an internal `AgentStore` module or trait.
- Cover database initialization with a temp-directory test.
