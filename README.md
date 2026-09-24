# clt

A file-backed task manager written in Rust, with a CLI, a Kanban TUI, and optional
Codex automation across projects. Tasks stay in your repository as Markdown files
or folders.

## Installation

With Rust and Cargo installed:

```bash
cargo install clt-rs --locked
clt --version
```

To install from a local checkout, run `cargo install --path . --locked --force`.
After upgrading, run `clt agent start` if you use the background scheduler.

## Quick start

Run these commands in your project:

```bash
clt init
clt add "Write the release plan"
clt list todo
clt status todo 1 doing
clt done doing 1
clt
```

Indexes are scoped to each status; list that status before moving a task. CLT uses
the Git root by default, or the current directory outside Git. Use `--local` to
force the current directory. Prefer individual task files? Use `clt init --folders`
when creating the board.

In the TUI, use arrow keys to navigate, `Space` to add, `e` to edit, `r` to
reorganize, and `q` to quit. `B` shows Backlog; `Tab` opens Agent Projects.
See the [feature guide](features.md) for nested boards, archives, shell integration,
and the full controls.

## Optional Codex automation

With the Codex CLI installed and authenticated, register an initialized project
and run one foreground scheduler pass:

```bash
clt agent register .
clt agent run --once
```

Registration opts the project into automation. Automated runs use full filesystem
access without approval prompts; register only trusted projects or use external
isolation. Each run handles one task per project. `clt agent start` starts the
background scheduler; `clt agent status` shows its state. Pause a project with
`clt agent pause .`.

See [agent setup and operation](features.md#codex-agent) for model selection, Git
commit/push modes, session controls, service setup, and recovery.

## Documentation and development

Keep this README focused on the overview, installation, and quick start. Put new
feature descriptions, detailed behavior, configuration, and troubleshooting in
[features.md](features.md), and release notes in [CHANGELOG.md](CHANGELOG.md).

- [Feature guide](features.md): commands, TUI controls, automation, and troubleshooting.
- [Architecture](ARCHITECTURE.md): module ownership and required verification gates.
- [Release procedure](docs/RELEASING.md): packaging, publishing, and post-release checks.

Build from the repository with `cargo build --release`. Before publishing, run
`python3 scripts/check_release.py` with Python 3.11 or newer; use `--allow-dirty`
only for local release preparation. Add user-facing changes under `Unreleased`
in the changelog before moving them into a versioned release section.
