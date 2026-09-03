# Changelog

All notable changes to this project are documented in this file.

This project follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) style sections and uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html) for releases.

## [Unreleased]

Use this section while developing the next release.

### Added

- Added persisted daemon project-scan errors to the Agent Projects pane, with red `ERROR` rows and actionable macOS Full Disk Access guidance for inaccessible external drives.
- Added `n` and `+` TUI shortcuts that create a Todo subtask under the selected task, automatically expand Markdown-backed parent storage, and open the resulting nested board.
- Added durable per-run agent workers: macOS uses one-shot launchd jobs and Linux uses transient user services, with persisted launch contracts, fenced worker records, heartbeats, crash recovery, and idempotent run finalization.
- Added `/goal`-prefixed task support for automated Codex runs, including explicit goals feature enablement and prompt guidance that removes the directive from the persistent goal objective.
- Added confirmed `Delete`-key removal for registered projects in the Agent Projects pane without deleting project files.
- Added a TUI Models page with provider presets, custom Responses-compatible endpoints, enabled model targets, favorites, a CLT-wide default, and per-project provider/model overrides.
- Added `x`/`Delete` removal for non-built-in providers on the Models page, including dependent model, selection, and Codex configuration cleanup.
- Added explicit, backup-protected Codex `config.toml` actions for custom provider definitions and the user's top-level default while keeping API keys exclusively in environment variables.
- Added an idle Done-or-blocked-task `c` shortcut to resume that task's Codex session interactively with workspace-write access, including while another Codex task is using the project, and return to the same board after Codex exits.
- Added task-level `s` controls to stop a selected task's linked active Codex session and later queue that exact session ID for automated `codex exec resume`, without stopping the agent service.
- Added task-level `i` interruption to stop a selected task's linked active Codex process, open the same ID in interactive Codex, and automatically restart that session in `codex exec resume` mode after exit.
- Added blocked-task monitoring that revisits one existing blocker at a time and backs off unresolved recovery attempts.
- Added the current local time to the agent projects pane's top border before the daemon status.
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

- Task titles now use only the actionable `[STOPPED]` session prefix; active CLT work remains visible in the Agent Projects runtime column without a redundant `[CLT]` task prefix.
- `clt agent start` now snapshots the current executable into an immutable generation, while `clt agent stop` stops only the scheduler and leaves already-dispatched workers running on their original binary generation.
- Codex session IDs are now attached while automated work is active, stored with generation-safe live process and log metadata, and reused by unambiguous stop, interactive handoff, and interrupted/blocked recovery through `codex exec resume`.
- Automated commit and commit-and-push runs now use the isolated `CLT Agent <clt-agent@localhost>` Git author and committer identity without modifying Git configuration.
- Made embedded `codex:<session-id>` task markers the sole interactive-resume link, removed mutable task-text associations from the agent database, and made marker persistence failures visible as failed runs.
- Made the TUI task-board console help show task controls instead of Agent Projects controls.
- Changed the portable `r` task-reorganization shortcut into a sticky mode with yellow task-board borders and a visible mode title; arrows keep reorganizing tasks until `r` or `Esc` exits.
- Made local OpenAI-compatible endpoint setup preset-led and self-discovering: Ollama and LM Studio load their model catalogs automatically, custom endpoints generate their provider IDs, the API-root prompt explains `/v1` and rejects complete operation URLs, `/models` results are presented as explicit opt-in choices, and model discovery can be refreshed from the Models page.
- Replaced the plain GPT-5.6 entry in the built-in OpenAI catalog with the explicit GPT-5.6 Sol model while preserving existing selections and defaults during migration.
- Added aligned, labeled provider/model columns to the Models page, replacing unexplained favorite stars with `FAV` values and marking CLT and Codex defaults independently.
- Restored `Tab` as a direct Kanban/Agent Projects toggle; lowercase `m` retains quick per-project target cycling, while uppercase `M` opens the Models page from either Tasks or Agent Projects and returns to the originating pane.
- Git commit-and-push automation now pulls with the user's configured merge or rebase strategy instead of forcing a rebase.
- Moving a folder-backed task into a Markdown-backed status now expands the destination status to a folder and preserves the old Markdown file as `status.md.bak`.
- Right-aligned the hidden Backlog count and shortcut in the task console title.
- Updated the terminal title to show the active project when using the TUI.
- Renamed the agent task workflow guide from `clt-skill.md` to `skill-clt.md`.
- Automated Codex agent runs now use `danger-full-access` with approvals disabled so non-interactive tasks can update Git metadata.

### Fixed

- Rebuild the active-worker project index and retry worker reservation when a SQLite-restored legacy index quotes worker states as identifiers and Turso reports `no such column: dispatching`.
- Moving an idle, session-linked managed Git task to Done now explicitly accepts external completion: CLT safely cancels its stale `WORKING` journal and queued resume state under a short project fence, while continuing to protect live sessions and sealed commit/push proof.
- Fixed managed Git sealing so a Todo or other task-board edit added during an agent run can remain unstaged and survive outside the exact task commit, while staged unrelated board changes and non-task baseline drift are still rejected.

- Git-enabled scheduling now checkpoints dirty task-board definitions in a dedicated prelaunch commit while preserving unrelated unstaged work, so tasks created in the CLI or TUI no longer fail before Codex starts. Failed pending projects also render as red `ERROR` rows with the stored cause, automatic-retry timing, and an `r` immediate-retry action instead of appearing unexplained as `IDLE`.
- Reclaim orphaned interactive reservations as soon as their generated holder process exits, release the exact lease when a guarded session disappears after reap, and show interactive lease-only states as `FENCED` or `STALE` instead of a false `RUNNING` agent.
- Allowed `c` to reopen a linked Doing task when its exact Codex session is stopped or otherwise idle, while continuing to reject sessions that are still running.
- Rebuild the derived active-worker project index after Turso reports a missing index entry, retry the scheduler pass once, and keep the daemon alive for later retries if a pass still fails.
- Kept TUI startup and keyboard input responsive while agent-panel refreshes or stale-service recovery are slow.
- Made Ctrl-C cancel TUI task creation and editing prompts, matching Escape without saving changes.
- Recheck blocked Todo and Doing tasks before fresh Todo work whenever recovery backoff permits, while allowing ready work to proceed during an unresolved blocker's backoff.
- Distinguished independent scheduler dispatch leases from legacy in-process runs so `clt agent stop` no longer reports a false legacy-run fence during post-reboot worker handoff.
- Moved independent-worker dispatch off the daemon's async runtime so blocking agent-store operations cannot panic and restart the scheduler before the worker service is launched.
- Reclaimed dead or expired agent leases for disabled projects and during unregister, while continuing to protect live, unknown, and independent-worker leases from deletion.
- Prevented scheduler restarts and binary upgrades from duplicating old-worker projects, made worker run recording/project counter updates/lease release one transaction, bounded failed startup and stale-heartbeat recovery behind verified service draining, serialized global worker capacity, treated newer worker protocols as opaque, deferred incompatible migrations without disabling controls, and kept stop and interrupt requests compatible across binary generations through the existing session-control protocol.
- Exact-session recovery now continues from the next unfinished step and requires requested code, file, configuration, or task-board changes to exist and pass relevant checks before marking the linked task done, while still allowing response-only tasks to finish with a response.
- Recovered Codex session markers displaced by completion notes so interactive handback can finish cleanly and the scheduler can continue with ready Todo work.
- Allowed explicitly registered non-Git folders to start and resume automated Codex runs, and allowed projects with run history to be unregistered cleanly.
- Fixed interactive Codex handoff on macOS by preserving the inherited terminal through the guardian and launch gate, handling zombie-only process groups safely, and showing stop, entry, and return-to-exec progress in the TUI.
- Prevented `c` from opening a Codex session that is still occupied by its automated run, and reserved the project while the interactive handoff is active so the scheduler cannot resume it concurrently.
- Prevented stop and interactive handoff races by having the owning runner terminate its own Codex process group, fencing the scheduler with persisted session state, and recovering stale TUI handoffs.
- Made the Kanban agent-output viewer follow the selected task, using live output for the active Doing task and session-linked run history for completed or blocked tasks.
- Fixed prompt construction in the bundled task-runner script under the macOS-provided Bash 3.2.
- Registering the current project from the Agent Projects pane now keeps the cursor on that project after it moves into the alphabetically sorted project list.
- Added portable task-reorganization shortcuts for terminals that do not distinguish Shift+Arrow: `Ctrl-P`/`Ctrl-N` reorder vertically, and `r` toggles a keyboard-driven reorganization mode.
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
