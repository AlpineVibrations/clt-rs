Add cross-platform clt agent state directory resolution (AGENT, P1).

Design reference: `codex-agent.md` storage model.

Acceptance criteria:
- Add a helper that resolves the agent state directory for macOS, Linux, and tests.
- Store central agent state outside project repositories.
- Ensure the directory is created when agent commands need it.
- Keep platform-specific path logic isolated from scheduler and registry code.
- Make tests able to override the state directory without touching real user state.
