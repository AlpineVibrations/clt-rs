# Changelog

All notable changes to this project are documented in this file.

This project follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) style sections and uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html) for releases.

## [Unreleased]

Use this section while developing the next release.

### Added

- Added `clt shell-init bash|zsh` integration so quitting after opening a registered project can change the calling shell to that project's directory.
- Added a first-class Backlog status for Markdown- and folder-backed boards, including CLI listing and status transitions.
- Added a hidden-by-default Backlog TUI column: `b` sends the selected task to Backlog, `B` toggles the column, and `0` reveals and focuses it.
- Added folder-backed status support: `tasks/backlog/`, `tasks/todo/`, `tasks/doing/`, and `tasks/done/` can now contain one task per file or subfolder.
- Added `clt init --folders` for fresh folder-backed task stores.
- Added `clt expand [status]` to migrate Markdown status files into folder-backed task files.
- Added nested subtask board navigation in the TUI for folder tasks that contain their own `backlog`, `todo`, `doing`, and `done` stores.
- Added first-sentence summaries for long task files while preserving full task content on moves.
- Added a multi-project Codex agent registry with `register`, `unregister`, `pause`, `resume`, scheduler, daemon, service, status, and log commands.
- Added a full-screen TUI agent projects pane for switching between registered project boards, toggling projects `ON` or `OFF`, and seeing `todo`/`doing` counts.
- Added per-project `git-commit` skill toggles for Codex agent runs through `clt agent git-commit enable|disable` and the TUI agent projects pane.
- Added per-project commit-and-push automation through `clt agent git-commit push`; the TUI now cycles Git automation through `OFF`, `COM`, and `PUSH` modes.
- Added live/latest agent output viewing directly from the active Kanban board with `l`.
- Added `skill-git-commit.md` guidance for safe agent-driven Git commits.

### Changed

- Git commit-and-push automation now pulls with the user's configured merge or rebase strategy instead of forcing a rebase.
- Moving a folder-backed task into a Markdown-backed status now expands the destination status to a folder and preserves the old Markdown file as `status.md.bak`.
- Right-aligned the hidden Backlog count and shortcut in the task console title.
- Updated the terminal title to show the active project when using the TUI.
- Renamed the agent task workflow guide from `clt-skill.md` to `skill-clt.md`.
- Automated Codex agent runs now use `danger-full-access` with approvals disabled so non-interactive tasks can update Git metadata.

### Fixed

- Added portable task-reorganization shortcuts for terminals that do not distinguish Shift+Arrow: `Ctrl-P`/`Ctrl-N` reorder vertically, and tapping `r` before any arrow performs one reorganization move.
- Documented the Terminal.app profile mappings required to preserve Shift+Up and Shift+Down task reordering through SSH and tmux.
- The agent projects pane now detects and restarts a stale background service while leaving explicitly stopped services alone; Linux services also restart after unexpected clean exits.
- Agent scheduling now reclaims crashed or expired leases and resumes the interrupted `doing` task instead of leaving it stranded.
- Existing folder- or Markdown-backed boards with one or more missing empty status stores are now detected and repaired instead of prompting for initialization.
- Declining the no-board initialization prompt now opens the TUI in the agent projects pane without creating an active task board.
- Agent services now resolve `codex` from `PATH` by default instead of pinning a version-manager-specific executable path, while preserving explicit `CLT_AGENT_CODEX_PATH` overrides.
- `clt agent start` now restarts an existing Linux user service after rewriting its systemd unit so updated environment settings take effect.
- Linux agent service commands now recover the standard user runtime directory when `XDG_RUNTIME_DIR` is missing, avoiding user-bus connection failures in SSH and non-interactive shells.

## [0.1.10] - 2026-05-11

### Added

- Added scroll handling in TUI task columns so keyboard navigation keeps the selected task visible.
- Added TUI task editing, deletion, help popover, console feedback, cursor-aware text input, and wrapping for selected long tasks.
- Added multiline-aware TUI input navigation for wrapped add/edit prompts, including Up/Down row movement, word jumps/deletes, and Ctrl-A/E/U/K/W shortcuts.
- Added CLI deletion support and single-status task listing.
- Added support for unquoted multi-word task descriptions in `clt add`.
- Added agent task workflow guidance, now published as `skill-clt.md`.

### Changed

- Made the TUI Kanban board the default when running `clt` with no subcommand.
- Made completed tasks appear at the top of the Done column.
- Updated README usage examples to match the current CLI behavior.

### Fixed

- Fixed stale or empty TUI selections causing add, edit, navigation, and move panics.
- Fixed terminal cleanup so raw mode and alternate screen are restored on TUI error paths.
- Fixed task moves so destination write failures do not remove the source task.
- Fixed TUI navigation on empty boards.

[Unreleased]: https://github.com/AlpineVibrations/clt-rs/compare/v0.1.10...HEAD
[0.1.10]: https://github.com/AlpineVibrations/clt-rs/releases/tag/v0.1.10
