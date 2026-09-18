# Feature Ideas

## Quality of Life

- Task search/filter on the CLI (`clt list --search "bug"`) and a `/` filter in the TUI.
- Due-date metadata (`(due:2026-09-01, HIGH)`) with TUI color-coding for overdue items.
- Multiple independent task boards per project for parallel workstreams.

## Agent Improvements

- Parallel agent runs per project (N concurrent agents on disjoint tasks).
- Agent run history & stats: `clt agent stats` showing runs/day, avg completion time, failure rates per project.
- Reusable per-project agent prompt templates (e.g., "always run `cargo test` after changes").

## Secure Provider Credentials

- Background services must not depend on shell startup files such as `~/.zshenv`; `launchd` and `systemd` do not automatically receive variables exported by an interactive terminal.
- Store a provider's key in the CLT registry from the Models page (`k`), reading it through a hidden prompt and identifying it by provider ID. The registry lives in an owner-only state directory and is retained by recovery snapshots, so a background service picks the key up without a shell or service-manager environment change.
- Never write secrets to launchd plists, systemd units, logs, task files, or error messages, and never render a stored value after entry.
- Resolve credentials in this order: the key stored in CLT, the provider's environment variable, then whatever Codex itself is configured with (ChatGPT login or `codex login --with-api-key`). This keeps explicit stored keys highest priority while preserving environment-variable overrides for development and CI.
- Resolve the selected provider before starting Codex and inject only its configured `env_key` into the Codex child process. Do not add retrieved credentials to the daemon's global environment or source a login shell.
- Background access must not open an interactive credential prompt. Report missing credentials safely and allow the scheduler to retry after the user stores or updates a key.
- Add `clt agent doctor` diagnostics that report credential presence and source without exposing values, including the common case where a key is visible in the terminal but missing from the service environment.
- Keep `launchctl setenv KEY "$KEY"` and the equivalent systemd environment import as documented temporary-session workarounds, with an explicit warning that service-manager environment changes do not survive every reboot or login lifecycle.
- Acceptance criteria: credentials work after reboot and agent restart, rotation and removal take effect without reinstalling the service, only the selected provider receives its key, and tests verify that generated service definitions and logs never contain secret values.

## Integrations

- Bi-directional Linear/GitHub Issues sync with file-backed task boards.
- Slack notifications on agent task completion.
