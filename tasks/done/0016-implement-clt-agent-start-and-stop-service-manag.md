Implement clt agent start and stop service management (AGENT, P2).

Design reference: `codex-agent.md` command design.

Acceptance criteria:
- Add `clt agent start` and `clt agent stop` commands.
- Start the platform user service that runs `clt agent daemon`.
- Support macOS launchd first if choosing one platform for the initial implementation.
- Report clear instructions or unsupported-platform errors where service management is not implemented.
- Keep the daemon command itself usable without service installation.
