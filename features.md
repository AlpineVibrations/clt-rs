# CLT feature guide

Detailed behavior, configuration, and troubleshooting live here. For installation
and a quick start, see [README.md](README.md). Release-by-release changes belong in
[CHANGELOG.md](CHANGELOG.md). Existing proposals and design criteria are preserved
in [Feature Ideas](docs/FEATURE_IDEAS.md); they are not a list of shipped features.

- [Overview](#overview)
- [Installation, shell integration, and skills](#installation)
- [Basic usage](#usage)
- [Kanban view](#kanban-view)
  - [Agent Projects](#agent-projects)
  - [Task sessions and controls](#task-sessions-and-controls)
  - [Direct Codex sessions](#direct-codex-sessions)
  - [Service heartbeat and logs](#service-heartbeat-and-logs)
  - [Models and providers](#models-and-providers)
- [Codex agent](#codex-agent)
  - [Linux sandbox setup](#linux-codex-sandbox-setup)
  - [Project registration and settings](#project-registration-and-settings)
  - [Managed Git](#managed-git)
  - [Missing Git recovery records](#missing-git-recovery-records)
  - [Changing branches with unfinished Git tasks](#changing-branches-with-unfinished-git-tasks)
  - [Agent skills](#agent-skills)
  - [Scheduling and task recovery](#scheduling-and-task-recovery)
  - [Goals and blocked tasks](#goals-and-blocked-tasks)
  - [Blocked-task supervisor](#blocked-task-supervisor)
  - [Background services and workers](#background-services-and-workers)
  - [Database recovery](#database-recovery)
  - [Service environment and state](#service-environment-and-state)
- [Task commands and storage](#task-commands-and-storage)

## Overview

- **File-based Persistence**: Tasks are stored in `tasks/backlog.md`, `tasks/todo.md`, `tasks/doing.md`, and `tasks/done.md`, or in status folders such as `tasks/todo/`.
- **Long Task Files**: In a status folder, each direct file is a task. `clt` displays the first sentence and preserves the full file content.
- **Nested Boards**: A task subfolder can contain its own `backlog`, `todo`, `doing`, and `done` files or folders. The TUI can open those as subtask boards.
- **Kanban TUI**: A visual board view powered by `ratatui`, with nested board navigation and a full-screen registered-projects pane.
- **Simple CLI**: Easy commands to add, move, and list tasks.
- **Smart Root Detection**: Automatically finds the git repository root to keep tasks centralized, or uses the current directory.
- **Agent Registry**: Register many projects, toggle them on or off, inspect `todo`/`doing` counts, choose per-project Git automation, and open any registered task board from the TUI.
- **Model Catalog**: Configure provider presets or custom Responses endpoints, keep a clean enabled/favorite model list, and select CLT-wide or per-project provider/model targets.
- **Codex Automation**: Run one Codex task at a time per enabled project, either in the foreground or through independently managed background workers that survive scheduler restarts and upgrades.
- **Agent Skills**: Includes installable `clt-task-management` and `git-commit` skill folders for task-board and safe commit workflows.

## Installation

Ensure you have Rust and Cargo installed. Starting with CLT 0.6.15, the published package includes the patched database engine:

```bash
cargo install clt-rs --locked
```

Or install from this checkout's repository directory:

```bash
cargo install --path . --locked --force
```

Crates.io builds through 0.6.14 omitted the local Turso fixes because [Cargo removes `[patch]` entries when packaging](https://doc.rust-lang.org/cargo/commands/cargo-package.html). Version 0.6.15 compiles the vendored engine and local Rust API directly as part of the single CLT package. No separate fork packages need publishing. A compile-time check also rejects a core without the required patch marker.

Check the installed version with `clt --version` (or `clt -V`). For older builds without this option, use `cargo install --list`.

After upgrading `clt`, restart the background scheduler so new work uses the newly installed binary:

```bash
clt agent start
```

`start` snapshots that binary into the agent state directory before starting the scheduler. Workers already running continue with their earlier snapshot; newly dispatched work uses the new generation.

### Shell integration

A command cannot directly change the directory of the shell that launched it, so `clt` provides a small shell wrapper for project switching. Add the appropriate line to your shell configuration:

```bash
# ~/.zshrc
eval "$(command clt shell-init zsh)"

# ~/.bashrc
eval "$(command clt shell-init bash)"
```

Restart the shell or reload its configuration. After opening another registered project from the agent projects pane, pressing `q` now exits `clt` and leaves the shell in that project's directory. Other `clt` commands continue to work through the wrapper.

The installed `clt` binary embeds both agent skills. Before an automated Codex run, `clt` looks for each required skill by its frontmatter name in the standard repository, user, and admin skill directories. If a skill is unavailable, `clt` adds its bundled instructions to that run's prompt automatically, so no separate skill installation is required for agent automation.

To make the bundled skills available to Codex outside `clt` agent runs, install them into the `skills` directory inside your user home `.agents` directory:

```bash
clt skills install
```

This works on macOS, Linux, and Windows and does not require a repository checkout or an initialized task board. It creates both skill folders and their `SKILL.md` files from the installed binary. Matching files are left alone. When a file differs, `clt` asks `Overwrite ...? [y/N]`; press `y` to update that skill or Enter to keep it. In a non-interactive session, a changed file stops installation before either skill is written. Use `clt skills install --force` to update changed files without prompts. Other files in the skill folders are preserved. Restart your agent after installing so it can discover the new skills.

## Usage

### Initialization
Initialize the task directory structure:
```bash
clt init
```

Create folder-backed statuses from the start:
```bash
clt init --folders
```

**Note:** By default, `clt` looks for the root of your git repository to store the `tasks/` folder. To force use of the current directory instead, use the `--local` flag:
```bash
clt --local init
```

## Kanban View
Open the interactive TUI Kanban board:
```bash
clt
```
Selected tasks expand to show their text; other tasks occupy one line. When a task collapses or the viewport grows, the list brings earlier tasks back into view to fill available space while keeping the selection visible. In normal navigation, `Esc` clears the task selection.

Press `Enter` to open a folder task with subtasks, `n` or `+` to create a subtask under the selected task, `e` to edit the selected task, `Space` to create a task, `Backspace` to return to the parent board, and `q` to quit. Creating a subtask automatically expands a Markdown-backed parent status to folder-backed storage, preserving the original status file as a `.bak`, converts the selected task into a nested board, and opens that board after the subtask is saved. Cancelling the prompt leaves storage unchanged.

Press `r` to enter sticky Reorganize mode, then use the arrow keys as many times as needed: Up/Down changes the selected task's position and Left/Right moves it between columns. The task-board borders turn yellow and the selected column shows `REORGANIZE MODE` while the mode is active. Press `r` again or `Esc` to return to normal navigation.

You can also use `Shift+Up` and `Shift+Down` to reorder the selected task, and `Shift+Left` and `Shift+Right` to move it between columns. `Ctrl-P` reorders the selected task up and `Ctrl-N` reorders it down; these portable alternatives work in stock macOS Terminal and through SSH or tmux.

Tasks with a managed Git journal still in `WORKING` can be reordered within Todo or Doing, including blocked tasks moved back to Todo. Reordering preserves their content, session link, blocker state, and Git recovery journal. Tasks whose Git finalization has already started remain protected from reordering.

Stock macOS Terminal does not encode Shift in its default Up/Down sequences, so the modifier is lost before `clt` receives it. To keep using Shift+Up/Down there, add these two mappings on the Mac under Terminal > Settings > Profiles > Keyboard:

- Shift+Up: send `\033[1;2A`
- Shift+Down: send `\033[1;2B`

Press `a` to move the selected task into the archive. Press `A` to open the archive's single-panel scrolling view, and press `A` again to return to the Kanban board. Press `Delete` to permanently delete the selected task. The `d` and `D` keys leave tasks in place.

Backlog is a fourth column for captured work that is not ready to be acted on. It is hidden by default; the task-board console title shows its current task count. Press `b` to move the selected task to Backlog, `B` to show or hide the Backlog column, or `0` to show and focus it. When visible, Backlog appears to the left of To Do and works with the normal Left/Right focus and task-movement controls. Keys `1`, `2`, and `3` continue to focus To Do, Doing, and Done.

When the current project is registered, the Kanban console title appends its agent `ON`/`OFF` setting, current runtime status, and configured Codex settings, such as `clt Console | Agent: ON RUNNING | Model: openrouter/gpt-6-astra | Thinking: xhigh | Fast: on`. `Model` uses the project's provider and model and `Thinking` shows the reasoning effort; when the project has no explicit setting, the resolved CLT default is named in parentheses, such as `Model: default (gpt-5.6-sol)` or `Thinking: default (high)`. `Fast: on` appears only when fast mode is enabled. The runtime status follows the same labels as the Agent Projects pane and remains visible when the project's agent is `OFF`. The whole suffix is omitted only when the project is unregistered.

### Agent Projects

Press `Tab` to toggle between the task board and the full-screen Agent Projects pane. In Agent Projects, Up/Down selects a registered project, `Enter` opens that project's task board, `Space` toggles the project `ON` or `OFF`, `Delete` removes it from the agent list after a `y`/`n` confirmation, and `g` cycles the `GIT` column through `OFF`, `COM`, and `PUSH`. These modes disable Git automation, ask Codex to create a task commit, or ask Codex to create that commit and let CLT publish it. Removing a project unregisters it from the agent list and clears its stored agent history, including unfinished Git finalizations and launch boundaries. It leaves the project, task files, and Git checkout unchanged. Removal is refused while an independent worker or agent lease is active. The currently open project is marked with `*`, the pane's top border shows the current local time before the daemon status, and the terminal title updates to the active project.

The daemon persists its own project-scan result separately from the TUI's local task count. If the background service cannot read a project, or a pending project is waiting after a failed run, the `AGENT` column shows `ERROR`, the row turns red, and selecting it shows the full cause and recovery guidance in the console. Retryable failures include their automatic-retry timing; after correcting the cause, press `r` to clear the cooldown and retry immediately. Missing Git starting records instead show `Git recovery available - press r`; `r` opens a [task recovery confirmation](#missing-git-recovery-records). External projects under `/Volumes` specifically direct macOS users to enable Full Disk Access for CLT and restart the agent; missing external projects instead prompt users to check that the drive is mounted. The `AGENT` column shows `INTERACTIVE` in full for a live guarded Codex handoff. This session reserves the project, so queued tasks wait until it releases the reservation. `FENCED` means CLT is preserving a handoff or lease reservation without claiming that an automated agent is running. `STALE` identifies a reservation whose generated owner process has exited; the daemon reclaims it once no matching session or worker still needs the fence. Selecting any of these rows explains the waiting state in the console. Press `s` directly on `INTERACTIVE` or session-backed `FENCED` rows to request a safe stop, or open its output with `l` for exact-session `s`/`i` controls.

The single supervisor line above the project table has its own focus. Press `u` to select it, then `Space` toggles the supervisor, `m` cycles its model, `t` cycles thinking, and `f` toggles fast mode. `Esc`, `Enter`, or Down returns to the project list without changing its selection. These are global supervisor settings, separate from each project's worker settings. See [Blocked-task supervisor](#blocked-task-supervisor).

### Task sessions and controls

Press `s` on an unfinished task to stop it before it starts, including tasks with a saved Codex link but no live or resumable CLT run, and idle Todo planning conversations. CLT saves the stop on the task board and displays `[STOPPED]`, even in an unregistered project. Scheduling and blocked-task recovery skip it until you select it and press `s` again. Its status, details, and blocker notes remain in place; a restarted task still needs to be ready in Todo before automation can pick it up.

On a selected task with a linked, active Codex session, press `s` to stop only that task's current Codex process. The task and its session link remain in place so the work can be resumed later, while its finished worker and project lease are released so another Todo task can run. With that stopped task still selected, press `s` again to queue the exact same session ID for automated `codex exec resume`. Press `i` while a linked session is active to stop its automated process and immediately open that same ID with interactive `codex resume`. CLT transfers the project's scheduler lease only after the old process has exited. When you leave interactive Codex, CLT automatically restarts the same session ID on the same task in `codex exec resume` mode. A second TUI can press `s` on an `INTERACTIVE` or `FENCED` project row even when no output link is available: the waiting parent closes its private lifeline, the guardian stops and reaps its exact Codex process group, and CLT releases a completed-session reservation or preserves an interrupted active session as stopped. CLT recovers a handoff abandoned before the guardian takes ownership without allowing another project task to start in between. If it cannot prove that the prior Codex process group exited, the project stays fenced rather than risking a concurrent resume.

If an automated worker and supervisor disappear while Codex survives, macOS and Linux automatically attach a replacement supervisor to the existing process. CLT checks the saved project/run identity and claims its exact session generation without launching Codex or repeating Git preparation. The same output remains live, and `s`, `i`, and `c` can control the recovered run. Process controls use macOS audit-token identities or Linux pidfds, so reused numeric PIDs cannot target another process. A replacement can itself be replaced after its owner exits. If identity or permissions cannot be verified, CLT retains the fence and reports the cause in the service or `session-supervisors/` log. After the old group exits, CLT honors a stop or interactive request, or queues the same session for automated verification and recovery; it does not invent a successful exit status for a process it could not reap.

Crash-safe exact-session relaunch currently requires Unix process supervision. The outer runner is the only process that polls session controls while connected; the child-owning supervisor watches a database-free lifeline, catches monitor panics, and stops and reaps its Codex process group before emitting shutdown proof. This separation prevents an agent-database failure in the supervisor from stranding a live Codex child. On other platforms, CLT refuses a known-session relaunch before spawning it.

On a Todo with no Codex link, including a `[STOPPED]` task, press `c` to create and open a planning conversation. CLT saves the full task text as context and appends the new session ID without moving the task to Doing or starting an automated turn. Discuss the work, refine its requirements, or ask Codex to update the task. Creation requires a registered project and a Codex CLI with the [app-server thread APIs](https://learn.chatgpt.com/docs/app-server); failures leave the task available for retry. When the project is idle, it remains reserved while you chat. When another task is already running, the new planning conversation opens alongside it and preserves that run and its project reservation. Returning to CLT leaves the planned task in Todo. If its agent is enabled, normal Todo automation starts the first ready task by resuming its exact attached Codex session, preserving the plan and conversation history. CLT explicitly tells Codex to begin implementation of that same task and keeps its session link. An unlinked Todo starts a new session. A stopped Todo stays stopped throughout planning and later visits; press `s` on the task when you want to allow automation. The planning conversation stays idle until the scheduler starts it. For an unstopped Todo, pause the project's agent first if you want to keep planning across visits.

On the task board, select a Todo, Doing, or Done task, then press `c` to open its linked Codex session interactively. For an active automated session, `c` performs the same stop-and-handoff as `i` and automatically resumes exec when you return. For an idle session, it opens with workspace-write access. When the project is otherwise idle, CLT reserves the project until you return. When another Codex task is already using the project, the selected idle session can open alongside it without interrupting the active run, including when automatic commit or push is enabled. This also works while a directly opened Codex session holds a manual task claim: the claim stays in place, and automation remains reserved. The manually claimed session itself must be continued in its original Codex window. Both sessions can modify the same worktree. The automated task retains its process, project lease, and Git finalization records; the interactive session controls only its own saved session ID. Leaving a session opened from idle keeps that exact session linked and stopped, so you can press `c` on the task again later. A session queued for recovery can also open this way once no worker owns it; leaving it keeps it stopped instead of rerunning the completed task. Both modes return to the same board and selection afterward.

When an automated Codex run moves its selected task from Todo to Doing, CLT saves a terminal `codex:<session-id>` marker as part of that board update, including when Git automation is off. The marker uses the exact session registered for that run; activation fails if the task or session already belongs to different work. This internal marker survives task moves and wording changes, is hidden in task lists, the TUI, and the task editor, and is the task-to-session resume link. While a run is active, the database also records that session's exact run generation and log paths so `l`, stop, and interrupt target the correct live process. Completed run history retains the session ID without associating it to mutable task text. A run is reported as failed if CLT cannot persist the marker on its completed or blocked task.

Opening a completed task with `c` moves that same task from Done to Doing before
Codex starts. Continue the follow-up in its existing conversation; CLT supplies
the current task context, so the agent does not need to create or claim another
tracking task. The entry returns to Done when the requested work is marked
complete or when you exit the interactive session. Failed launches and recovered
guardian crashes also restore Done. The original completion notes, conversation,
and completed Git proof are preserved. Todo planning and interactive takeover of
unfinished automated work keep their existing behavior.

### Service heartbeat and logs

The background service refreshes its registry heartbeat every 15 seconds independently of project scans, worker launches and the configured polling interval. `service stale` means that heartbeat has expired after 45 seconds. If registry heartbeats stop completing, the scheduler exits so launchd or systemd restarts it automatically, even with no TUI open; independent workers continue. Temporary heartbeat errors are retried within that window. The agent projects pane also restarts a running service whose check-in is stale and shows `service restarting` while it recovers. Open CLT windows share a restart lock and a 60-second cooldown, allowing the replacement scheduler to check in before another restart. A service explicitly stopped with `clt agent stop` remains stopped.

Press `l` from the Kanban board to open output for the selected task. A task linked to the currently active Codex session shows that session's live agent output even if the task has already moved to Done or become blocked; otherwise completed or blocked tasks with a linked session show that task's recorded output. If a worker exited before saving run history, CLT also checks the exact session's retained log paths. The open console follows the highlighted task as you move through the board. The same key opens the selected project's live or latest output from the Agent Projects pane. On an Agent Projects row, `s` directly controls the one active, interactive, or fenced session; if several sessions are present, open the intended output first. While project output is open, `s` stops or resumes its exact session, `i` takes over a live or stopped session interactively and hands it back to automated exec afterward, and `c` opens the displayed session interactively, taking over its automated run when active. These controls use the session represented by the displayed run rather than searching the task board, so they remain available when a task was moved, nested, deleted, or lost its session marker. CLT refuses to act when the displayed output does not identify one exact session or already has an interactive handoff in progress. The console expands and follows new output until `l` or `Esc` closes the log.

### Direct Codex sessions

When you open Codex directly and start work in a CLT project, create the task and
reserve it for that conversation in one command:

```bash
clt start "Implement the planned feature"
# Uses CODEX_THREAD_ID; alternatively pass --session <exact-current-session-id>.

# To take an existing task from the same conversation or an unlinked task:
clt list todo
clt claim todo 1
```

The task appears directly in Doing as `[MANUAL]`, with its conversation attached.
There is no temporary eligible Todo for the daemon to pick up. The claim reserves
the whole project, including against interrupted Doing recovery, and persists
across Codex exits and daemon restarts. CLT does not infer that an external
session has finished from a missing child process. Opening another CLT session
for that project is also prevented while the manual claim remains.

Finish with `clt done doing <index>`, or explicitly return unfinished work to
automation after recording the plan and remaining steps:

```bash
clt list doing
clt handoff doing 1
```

Handoff moves the task to Todo and releases its claim while preserving the exact
Codex session ID. When eligible, automation resumes that conversation using the
project's normal settings. A disabled project stays disabled. Stop working in
the direct session after handoff; an enabled daemon may pick it up immediately.
Moving the task to Todo in the TUI or with `clt status doing <index> todo` also
hands it off: the manual label disappears and it becomes a regular queued task
with its conversation preserved. Task edits and moves to Backlog preserve the
claim. Deleting or completing the task releases the reservation.
`s` and `c` on a manual task explain its ownership instead of launching or stopping
a duplicate conversation.

Claims require the exact current session UUID and an idle project. Existing
automated conversations, active leases, and unfinished Git finalizations retain
their established CLT controls. A session may belong to only one task. If the
session ID is unavailable, keep any tracking task stopped until it can be linked
and claimed. These commands and scheduler protections require the updated CLT
binary; restart the scheduler after upgrading and refresh the bundled skills
with `clt skills install`.

### Models and providers

Press uppercase `M` from either the task board or Agent Projects to open the Models page; uppercase `M`, `Tab`, or `Esc` returns to the pane you came from. The Models page keeps a catalog of providers and model targets with aligned, labeled columns. `USE` shows live availability, `FAV` marks favorites, and the separate `CLT` and `CODEX` columns identify the effective CLT-wide default and the user's Codex config default; `YES` is shown when a row has that role. `THINK` shows each model's default reasoning level: press `t` to cycle through system, low, medium, high, extra-high, max, and ultra. A model setting is used for agent runs unless the selected project has its own reasoning override. Changing `THINK` on the `CODEX=YES` model also updates Codex's top-level reasoning default immediately; choosing system removes that override. Pressing `c` to choose a new Codex default writes both its model and reasoning setting. When no explicit CLT override exists, CLT follows the Codex default and both columns mark the same model. The provider pane always shows the available presets: press `1` through `4` to add or enable OpenAI, OpenRouter, Ollama, or LM Studio. Ollama and LM Studio query their standard local URLs for models immediately. To remove a provider, select it in the left pane and press `x` or `Delete`; its models and affected CLT/project selections are removed, along with its custom Codex provider configuration. Built-in OpenAI cannot be removed, but `Space` can disable it.

Press `n` to add another local or custom OpenAI-compatible endpoint. CLT asks for a friendly name, the API base URL, and an optional API-key environment-variable name; it creates the internal provider ID automatically. Enter the API root, for example `http://127.0.0.1:9090/v1`. Include `/v1` when that is where the server exposes its compatible API, or omit it when the server exposes endpoints directly at the host root. Do not paste a complete operation URL ending in `/chat`, `/chat/completions`, `/models`, or `/responses`. After saving, CLT requests `<base URL>/models` and shows every returned model in the Models pane. Newly discovered models start `OFF`, so use Right, Up/Down, and `Space` to choose exactly which models appear in project selection. Press `r` to discover again later, or `a` to enter a model ID manually when an endpoint does not expose `/models`. `Space` also enables or hides the selected provider, and `f` toggles model favorite status. Favorites sort first. Use `k` on an endpoint that names no environment variable to store its key in CLT; CLT then records `OPENAI_API_KEY` as that provider's `env_key` so Codex sends the stored credential to the endpoint.

Press `d` on a model to make its provider/model pair the CLT-wide default for new agent runs. Press `c` only when you also want to update the top-level `model_provider` and `model` values in the user's Codex `config.toml`; CLT preserves other TOML content and creates `config.toml.clt.bak` before its first edit. Custom provider definitions use Codex's `model_providers` table with `wire_api = "responses"`, so the selected endpoint must support the Responses API at `<base URL>/responses`.

Press `k` on the Models page to store the selected provider's API key in CLT's local registry. The value is entered in a hidden field, kept in the owner-only agent state directory (`agent.db`, which is also covered by the registry's recovery snapshot), and never rendered again: the `KEY` column and the `Auth` line show only which source will be used, and `x`/`Delete` removes a provider's key with the provider. A stored key has the highest priority — CLT injects it into the Codex child as the provider's `env_key` (for example `OPENROUTER_API_KEY`) — and only the provider a run selected receives it. When no key is stored, CLT falls back to that environment variable, then to whatever Codex itself is configured with, such as the ChatGPT login or `codex login --with-api-key`. Model discovery on the Models page authenticates the same way, so a stored key refreshes `/models` without exporting anything. A background user service picks up stored keys immediately because it reads the registry; environment-only setups must still expose exported variables to the service-manager environment.

Each registered project has persisted Codex launch settings in the `CODEX` column. Overrides are shown compactly as `provider:model/thinking/fast`; `default` means the project follows the CLT-wide default, which in turn falls back to the user's Codex config when unset. Press lowercase `m` to cycle through the CLT default and currently enabled provider/model targets, `f` to toggle Fast mode, and `t` to cycle through the default, low, medium, high, extra-high, max, and ultra reasoning levels. Settings are resolved when a new run launches; an already running process is unchanged.

The console title shows `Log View` while agent output is open, with `LIVE` or `LATEST` identifying the output. Opening a log clears previous console feedback, and the displayed output takes precedence over Agent Projects refresh errors until the log is closed. The agent log footer shows `Model` and `Thinking` from the displayed run's recorded startup header, plus `Fast: on` or `Fast: off` from CLT's recorded launch setting, including when viewing a completed task's final response. These values stay visible as output scrolls and follow the selected task or project. Fast reflects the setting requested at launch and remains unchanged when project settings are edited. Settings that were not recorded or whose log is unavailable appear as `unknown`; older logs without a Fast record show `Fast: unknown`.

## Codex Agent
`clt agent` can run Codex against enabled registered projects that have unblocked `todo` tasks. It can also recover a task left in `doing` when a previous agent lease belongs to a crashed process or has expired. Before starting fresh Todo work, the scheduler starts a blocked-task monitor run when a Todo or Doing task has a current blocker note and its recovery backoff has elapsed. Backlog tasks are deliberately ignored until they are promoted to Todo. Each project keeps its own repo-local `tasks/` board, while the agent stores cross-project runtime state in one central state directory.

An unfinished, unblocked Doing task with a saved Codex session also resumes after a normal timeout releases its worker and lease. Recovery uses that exact session before starting fresh Todo work, respects failure backoff, and leaves explicitly stopped sessions alone. If Codex reports `NO_TASKS_LEFT` while ready Todo work or its linked active task remains, CLT records the discrepancy as a failure and waits for failure backoff instead of treating the run as idle success.

Before registering a project, initialize its task board and make sure the `codex` CLI is installed and authenticated. With no path, `register` uses the same project root that normal `clt` commands use:
```bash
clt init --folders
clt agent register
```

Registering a project is the user's explicit opt-in to automated Codex runs in that directory. Registered projects do not have to be Git repositories; CLT passes Codex's non-Git `exec` override for both new and resumed automated sessions.

### Linux Codex sandbox setup

Codex uses Bubblewrap (`bwrap`) to sandbox commands on Linux. Install the distribution package before starting the agent:

```bash
# Ubuntu or Debian
sudo apt install bubblewrap

# Fedora
sudo dnf install bubblewrap
```

Ubuntu 24.04 may also restrict the unprivileged user namespace that Bubblewrap needs. If Codex reports `bwrap: loopback: Failed RTM_NEWADDR: Operation not permitted`, install and load the Bubblewrap-specific AppArmor profile:

```bash
sudo apt install apparmor-profiles apparmor-utils
sudo install -m 0644 \
  /usr/share/apparmor/extra-profiles/bwrap-userns-restrict \
  /etc/apparmor.d/bwrap-userns-restrict
sudo apparmor_parser -r /etc/apparmor.d/bwrap-userns-restrict
```

Verify the sandbox before starting the background agent:

```bash
codex sandbox -- /bin/true
echo $?
```

The sandbox command should produce no output and exit with status `0`. Prefer the AppArmor profile over disabling `kernel.apparmor_restrict_unprivileged_userns` globally. See the [Codex sandbox documentation](https://learn.chatgpt.com/docs/sandboxing) for platform prerequisites and container-specific guidance.

### Project registration and settings

Register more projects by passing their paths:
```bash
clt agent register ~/code/project-a
clt agent register ~/code/project-b
clt agent projects
```

Turn projects off and on without removing them from the registry:
```bash
clt agent pause ~/code/project-a
clt agent resume ~/code/project-a
```

In the TUI agent pane, the same state appears as `OFF` or `ON`.

### Managed Git

Configure the optional `git-commit` skill instruction per project:
```bash
clt agent git-commit enable ~/code/project-a
clt agent git-commit push ~/code/project-a
clt agent git-commit disable ~/code/project-a
```

Git can be enabled after an unfinished task has been paused and its worker has stopped. On resumption, CLT keeps the same task and session, checkpoints its board, and records a new Git boundary before releasing Codex. Existing implementation and staged work are preserved; CLT does not synchronize the checkout during this transition. The resumed task then uses the selected commit or commit-and-push mode. CLT records session modes durably; older sessions can be recognized from a complete matching launch prompt in their saved log. Missing or ambiguous evidence, completed work, and genuinely lost managed Git journals still require recovery rather than a guessed boundary.

`enable` selects commit-only mode, `push` selects commit-and-push mode, and `disable` turns Git automation off. A fresh Git-enabled run requires an attached branch. Existing staged, unstaged, and untracked changes are accepted and preserved during startup; newly created Todo definitions may remain unstaged. The intended checkout, branch, and upstream must be configured before scheduling. Before spawning or releasing Codex, CLT performs the safe fast-forward-only startup sync only when the checkout is clean and no older `WORKING` journal requires preserving its history; otherwise it keeps the current commit. If the task board differs from `HEAD`, CLT checkpoints the complete `tasks/` tree in a dedicated `CLT Agent` commit while leaving non-board changes and pre-existing staged entries untouched, including partially staged board files. It then captures the resulting `HEAD`, branch, exact index tree, worktree baseline, and upstream configuration and persists that server-owned launch state. CLT releases Codex only after this preparation succeeds. The Todo-to-Doing transition rechecks the frozen snapshot and binds it to the session-specific `WORKING` journal before it changes the board.

Commit-and-push additionally requires one configured upstream and exactly one push URL. At launch, CLT resolves the effective push remote in Git's normal precedence order—`branch.<name>.pushRemote`, then `remote.pushDefault`, then the branch's upstream remote—and freezes that choice, the concrete push URL, and the upstream merge ref. This resolution honors the configured overrides once; later publication does not use implicit `git push` routing, default refspecs, or a newly changed configuration.

A person can add, edit, reorder, or remove other tasks while an agent is starting or completing its task. Unstaged and untracked changes do not invalidate launch verification, Todo-to-Doing activation, or recovery of a terminal worker’s unconsumed launch record. CLT preserves the original launch baseline and still checks HEAD, branch, upstream, and the index; activation also requires the selected task’s identity to exist exactly once in the starting commit. Leave those concurrent board edits unstaged and stage only the selected task's transition and code; CLT verifies the exact staged tree and preserves the other board edits outside the commit.

An unconsumed pre-registration launch boundary is immutable. CLT never overwrites it or recaptures it from a later checkout. Even when Codex exits before announcing a session ID, CLT first proves the exact child reaped and leaves that launch record for the normal recovery check. It is reclaimed automatically only when its exact worker is terminal, no session record owns its run token, and HEAD, branch, upstream, index, and Git mode still match the frozen snapshot; otherwise the project fails closed. `clt agent clean` refuses to erase such a boundary. Explicitly unregistering an idle project abandons its pending Git work and removes the boundary without changing the checkout.

The released agent preserves the recorded launch state. It may inspect Git, implement the task, and create the one sealed task commit, but it never pushes in either Git mode. It must not run a startup pull, fetch or otherwise synchronize, merge, rebase, switch branches, reset history, or reconfigure the upstream. Those operations would move a boundary CLT owns and invalidate proof.

After the task reaches Doing, ordinary user commits such as a patch-version bump may advance the same branch without blocking the agent's commit. CLT keeps the original launch journal and seals the task against the current parent, preserving those commits. It also accepts CLT's board-only checkpoints and other sessions' proven completed commits. Unproven agent implementation commits, unproven task claims, merges, branch switches, and rewritten history remain blockers. The agent must reconcile compatible file changes and rerun affected checks. If a user commit lands after sealing but before the task commit, the agent reviews and reseals the complete staged payload with `clt done done <index>` before committing.

After verification, the agent stages the implementation and active Doing task, including its completion note and session marker. `clt done` projects the Doing-to-Done move and seals the exact full repository tree that the eventual commit must contain, then performs the file-backed move provisionally. The agent stages that board move and creates exactly one normal task commit with one exact `CLT-Task: codex:<session-id>` trailer. A hook that mutates files or rejects the commit invalidates the seal; after fixing and staging the complete result, `clt done done <index>` reseals the provisional Done entry before the same one-commit attempt is retried.

Commit-only mode becomes terminal Done only after CLT proves the exact sealed tree, task identity, commit identity, parent boundary, and trailer. In commit-and-push mode, CLT alone publishes after that local proof: it sends the immutable frozen commit OID to the exact frozen URL and `refs/heads/...` merge ref with an explicit non-force refspec, while honoring normal pre-push and signed-push policy, then independently queries and fetches that destination to prove containment. The scheduler holds a transactionally acquired, renewable finalizer lease and rechecks its exact ownership around every mutation and remote side effect; a live session control prevents acquisition rather than losing an expired guardian lease. The task remains `PUSH-PENDING` until publication and independent remote proof both succeed. A hook rejection, signing failure, non-fast-forward, timeout, or ambiguous publication is retried by the scheduler without resuming Codex, and it blocks every later task in that project until it succeeds or is resolved externally. A failed or interrupted local finalization still resumes the same linked Codex session and rolls forward from existing proof instead of starting another task or making a second completion commit. If completed-task evidence exists but the start journal has been lost, CLT fails closed because it cannot reconstruct the exact-one-commit boundary safely.

Other work may advance the branch after the sealed task commit, including before CLT acknowledges it. Finalization verifies the original task commit and accepts later commits while that commit and its Done identity remain in the branch history. A later commit claiming the same task session is still rejected. In push mode, CLT publishes only the verified task commit or recognizes that the destination already contains it; later commits and staged or unstaged work are preserved.

Unstaged and untracked work can coexist with an automated task, including new files, edits, renames, and deletions made after launch. CLT treats that work as intentional and leaves it outside the sealed commit, even when it shares a path with staged implementation. The launch baseline remains recorded, but sealing and resealing verify the reviewed index rather than requiring the worktree to match that snapshot. The agent must still preserve others' work, stage its complete verified implementation, and resolve actual conflicts. Accepted concurrent user commits can include baseline work or implementation; CLT seals the remaining task changes against the current parent without requiring those edits again. Concurrent task-board edits must also remain unstaged. The Git index is a cooperative boundary: existing staged changes are recorded at launch and rechecked before task activation, but Git cannot identify which actor staged a change during the run. The agent reviews staged changes for task scope. If unrelated work is already staged, it uses a separate index for task staging, sealing, and committing, then reconciles only its task changes into the shared index while preserving unrelated staged hunks. Review the staged diff before sealing and reseal if the intended commit payload changes.

Managed Git task moves preserve folder-backed tasks as paths: Directory-to-Directory Todo/Doing/Done transitions rename only the moving file or folder, preserving every unrelated task's path. New completions appear at the top of Done. Managed completions use a descending filename order prefix to prepend without renumbering existing tasks; ordinary manual reordering can normalize those prefixes. Prelaunch rejects a board layout in which a folder-backed Todo would enter Markdown-backed Doing, or folder-backed Doing would enter Markdown-backed Done; expand and commit the destination layout first. If a crash leaves identical session-linked copies on both sides of a managed move, CLT repairs the duplicate without reordering unrelated tasks. Ambiguous or nonidentical copies fail closed.

Commits from either enabled mode use `CLT Agent <clt-agent@localhost>` as both author and committer so automated work is recognizable without changing repository or global Git configuration. Existing enabled registrations migrate to commit-only mode. In the TUI, the modes appear as `COM`, `PUSH`, and `OFF` in the `GIT` column.

### Missing Git recovery records

When a previous task's Git starting record is missing, Agent Projects shows
`Git recovery available - press r`. Select that project and press `r`, then `y`
to confirm recovery (`n` or `Esc` cancels). The confirmation identifies the task
and explains the action before changing anything.

For unfinished work, CLT preserves the current files, staged changes, commits,
and old conversation, records the previous session ID in the task history, and
queues the same task in Todo for a fresh Codex conversation. That run reviews
existing work and Git history, verifies what is complete, and finishes only what
remains. With Git automation enabled, it receives a new Git starting record
through normal launch preparation.
CLT does not roll back work or invent a record for the old attempt.

For a task already in Done, confirmation accepts its current completion and
clears the obsolete retry request. CLT leaves the task and its conversation link
in place and does not run it again. Project enablement and Git settings stay as
configured; queued work starts when the project's scheduler is active.

The equivalent explicit command is:

```bash
clt agent recover-task /path/to/project
# If the failed run does not identify one task:
clt agent recover-task /path/to/project --session <previous-session-id>
```

Recovery requires an idle project, no manual owner, and no surviving Git journal
for that session or unfinished Git work elsewhere in the project. It rechecks
the saved run and exact task after confirmation, refuses ambiguous links, and
keeps interrupted task moves stopped so recovery can be retried. Stop active
work before retrying a refused recovery. Automated agents cannot invoke this
command to bypass their commit checks.

Press `l` on the project to read the saved diagnostic, even if no log was created.
Before recovery, task-level `l` still shows its earlier output. Old conversations
remain available with `codex resume <previous-session-id>`. Ordinary retries and
restarting CLT cannot recreate the missing record; this action explicitly starts
a new attempt from the current checkout instead.

### Changing branches with unfinished Git tasks

A managed task belongs to the branch recorded when it started. If you switch
branches while an unverified attempt remains, the scheduler automatically retires
that attempt once the project is idle. It preserves the old journal, sealed
manifest, conversation history, files, staging, and commits.

If the old conversation still has a task on the current board, CLT queues that
task in Todo for a fresh conversation to review existing work and finish what
remains. A provisional Done entry returns to Todo for verification. If the old
conversation has no task on the current branch, CLT leaves the board alone and
continues with its queued work. A task linked to a different conversation is
never adopted or rewritten. The next run captures the current branch and Git
settings normally; CLT does not switch branches or certify the old commit.

This happens without a recovery prompt or repeated Codex launches. Paused
projects and deliberately stopped sessions stay paused or stopped. Automatic
recovery requires an attached branch and an idle project, and preserves the
checks for manual owners, other pending journals, active workers, unconsumed
launch records, changed task links, and verified commits. `PUSH-PENDING` commits
retain their publication contract. When one of these checks prevents recovery,
CLT keeps the saved diagnostic available; resolve the reported obstacle first.
The explicit `clt agent recover-task /path/to/project` command and Agent Projects
`r` action remain available, including for orphaned attempts. Interrupted task
moves remain stopped and can be retried through explicit recovery.

### Agent skills

Agent-facing workflow skills are included in the repository's `skills/` directory:

- `skills/clt-task-management/`: task-board workflow guidance for using `clt`.
- `skills/git-commit/`: git commit and optional push workflow guidance.

Automated `clt` agent runs use embedded copies when these skills are not installed. Run `clt skills install` as described in [Installation](#installation) when you also want to invoke them directly in other Codex sessions.

The task-management skill tells standalone Codex sessions to use `clt start` or `clt claim` to reserve their work and attach the current conversation, even when project automation is disabled. See [Direct Codex sessions](#direct-codex-sessions). Existing links are preserved, and independent follow-ups do not reuse the parent's session. If Codex cannot determine its current session ID, it keeps the tracking task stopped and reports the limitation. Refresh an installed skill with `clt skills install` to receive this guidance.

### Scheduling and task recovery

Run one foreground scheduler pass:
```bash
clt agent run --once
```

The scheduler scans enabled projects, picks projects with pending unblocked `todo` tasks, takes an agent lease, and starts one Codex run at a time. A foreground `run --once` owns its run directly through a unique durable inline-worker generation, so its crash and pre-session launch boundaries use the same fencing model. On macOS and Linux, the continuous daemon instead hands each run to a unique launchd job or transient systemd user service. That worker owns lease renewal, the Codex process, task/session finalization, and the run record; the scheduler is free to stop immediately after dispatch. Each normal Codex run is prompted to inspect the board, move one available task to `doing`, complete it, run relevant checks, update the task through `clt`, and stop after that single task.

When a human moves an idle session-linked task to Done while its journal is still `WORKING`, CLT treats that move as explicit acceptance of externally completed work. The session marker identifies the journal even if the user edited the task text or committed the work manually. CLT checks the original journal identity, generation and ownership under a short project fence, cancels the obsolete working journal, and reports external completion. Before checking that fence, it reconciles workers proven to have exited and leases held by dead processes, so stale records do not require waiting for the scheduler. A live worker, session or lease prevents the override. Sealed `FINALIZING` and `PUSH-PENDING` proof must still complete normally; a user move cannot discard it. If the board move is interrupted after cancellation, the scheduler preserves that decision and does not resume the old session. Completed or reaped session logs show `LATEST` even when a project reservation still exists.

If the externally completed task is already in Done, pause the project with `clt agent pause .` and stop any active task run with `s` in the TUI. Once the project is idle, run `clt list done` and then `clt done done <index>` from a normal terminal. CLT accepts the completed task and cancels its stale `WORKING` journal without moving or rewriting the board entry or creating a Git commit. Repeating the command is harmless. Resume scheduling with `clt agent resume .` after acceptance. This recovery requires the task's saved session marker and does not override an active owner or sealed commit proof.

An abandoned `WORKING` journal that never acquired a task identity can outlive its original checkout. Before scheduling or removing a registered project, CLT retires it only when no task marker remains anywhere in the task tree (including nested and archived tasks), no commit proof was sealed, and the project is idle. The cancelled journal retains its original Git boundaries and a recovery reason. To apply this cleanup while a project is paused, run `clt agent reconcile /path/to/project`. This command preserves project settings, files and commits. The TUI's `r` key requests another scheduling attempt; it does not reset Git history or discard pending proof.

`FINALIZING` local-commit work takes priority over all queued work and resumes the exact linked Codex session. `PUSH-PENDING` also blocks every later project task, but it is retried entirely by CLT without launching or resuming Codex. A `WORKING` journal is earlier and more permissive: if its linked task is durably blocked and its blocked-recovery backoff is active, CLT preserves that journal and history while allowing another ready Todo to run. It deliberately skips startup synchronization for the later task so the blocked proof boundary remains reachable; after backoff, the exact blocked session is eligible for recovery again. If a crashed worker instead left an ordinary task in `doing`, the scheduler uses its durable worker record to resume that task. When exactly one interrupted or blocked task carries a session marker, recovery uses `codex exec resume` for that session instead of opening a new one. Explicit stop and interactive-handoff states suppress ordinary scheduling; an interactive `i` handback is prioritized as an exact-session resume before any Todo selection.

### Goals and blocked tasks

Prefix a Todo task with `/goal` when it needs a persistent objective for long-running work. Automated runs enable Codex goals, remove the leading directive from the goal objective, and ask Codex to create the goal before working on the task. The directive must be the task's first non-whitespace token and must be followed by a non-empty objective; `/goal` elsewhere in a task remains ordinary text.

```bash
clt add "/goal Migrate the authentication module and stop when all tests pass"
```

Use this for one durable objective with a verifiable stopping condition; keep quick fixes and unrelated task lists as normal Todo items. See the [official OpenAI goal guide](https://learn.chatgpt.com/use-cases/follow-goals) for goal-writing guidance.

Blocked-task recovery takes priority over fresh Todo work whenever its recovery backoff permits. A monitor run reviews the blocker notes, rechecks whether their conditions still exist, and works on exactly one blocked task from Todo or Doing. It can complete that task, add a newer `UNBLOCKED YYYY-MM-DD:` note and return it to Todo after resolving its blocker, or update its blocked note with the latest attempt. The latest dated `BLOCKED`, `UNBLOCKED`, or `COMPLETED` state note determines whether a task is currently blocked. Returning previously automated work to Todo resumes its existing Codex conversation and Git journal; CLT restores the ready task to Doing before releasing the resumed worker. It does not start a new planning run or capture a replacement Git boundary. Stopped or busy sessions retain their controls, and a missing managed Git journal still requires explicit recovery. If recovery leaves the blocker unresolved, the run is recorded as `blocked`; ready Todo work can proceed during `CLT_AGENT_FAILURE_BACKOFF_SECONDS`, after which the blocker is checked again before another fresh task. Unmarked Doing tasks are left alone because they may belong to a human or another workflow.

Automated runs start Codex with `--sandbox danger-full-access --ask-for-approval never --enable goals` so tasks can update Git metadata without pausing for interactive approval and `/goal` tasks can create persistent goals. This removes the Codex command sandbox for the entire run. Register only trusted repositories, or run the agent inside an externally isolated container or VM.

### Blocked-task supervisor

The optional supervisor reviews execution blockers in Doing before CLT starts another task, and reviews queued blockers when no ready Todo work remains. Blocked Todo tasks waiting on prerequisites do not prevent a ready Todo from running, regardless of its position in the list. A saved review hold from a queued dependency wait is cleared when ready work can proceed. It defaults to off, the CLT default model, high thinking, and fast mode off. Configure it on the single line at the top of Agent Projects (`u` selects the line; `Space`, `m`, `t`, and `f` change its settings). Install the updated binary and restart the scheduler to use the feature.

After the implementation worker exits and saves its blocker evidence, a separate review worker takes the project's lease. It uses a read-only sandbox, the supervisor's model settings, and a three-minute deadline. It examines task notes, relevant project files and the latest available logs for each blocked task’s exact session, then returns one structured decision. Errors from other project sessions are not supplied as current failure evidence:

- **Retry:** give the original task session a concrete new approach. CLT saves the decision before unblocking the task, restores returned Todo work to Doing, and passes the supervisor's direction to the original conversation. Its Git journal and partial work remain intact.
- **Wait:** identify the missing prerequisite and what must change.
- **User:** state the exact decision, permission, or input required.
- **Replan:** propose a task split or reordering that preserves existing work.
- **Repair:** identify an automation or session-state problem and a recovery step.

Retry is the automatic action; the other decisions hold the project and show the reasoning and next action in the selected project's console. Replanning is a proposal, not an automatic rewrite of the task board. Resolve a prerequisite or answer a question in the task notes to provide new evidence, or press `r` on the project to request a fresh review explicitly. Unchanged task evidence reuses the saved decision instead of repeatedly launching a model. The supervisor allows at most two implementation retries before requiring user review; explicit `r` resets that budget. A crashed assessment can be attempted twice on the same evidence. Invalid, stale, failed, or timed-out assessments cannot authorize task changes.

Supervisor settings and decisions survive scheduler restarts and registry recovery. Explicit stops, manual ownership, active leases, and sealed Git finalization retain their protections. Turning the supervisor off restores ordinary scheduling without deleting its saved decisions or changing task sessions. A review already running when it is switched off cannot apply its decision. Supervisor output remains available with the project's usual `l` log control.

### Background services and workers

Run the scheduler continuously in the foreground:
```bash
clt agent daemon
```

Start or stop the background service:
```bash
clt agent start
clt agent stop
```

On macOS, `start` installs a user `launchd` service named `com.alpinevibrations.clt.agent`. On Linux, it installs a user `systemd` service named `clt-agent.service`. Other platforms can still use `clt agent run --once` or `clt agent daemon`, but `start` and `stop` are unsupported.

When replacing the macOS scheduler, `start` waits for the old service to unload and retries bootstrap error 5 while the service is absent, with up to five seconds of combined retry delays. Persistent failures retain the launchd diagnostic. Run the command as your normal user, without `sudo`.

Missing, uninitialized, or unreadable project boards are skipped before task recovery. For example, a project on an unmounted drive does not block ready tasks in other projects. The scheduler logs the skipped project's name, path, and scan status, and checks it again on later passes.

Reconnecting a drive at its registered path requires no re-registration: the next scheduler pass reads the existing board and clears the failed scan result. Permission and filesystem errors retain their underlying cause instead of appearing as a missing folder. If the TUI can read the board while the saved background scan reports a failure, the console identifies that difference and points to `clt agent status`; a terminal and the background service can have different drive permissions. A local scan does not overwrite the daemon's access result.

`clt agent stop` stops only that scheduler. It does not drain, wait for, or terminate independent workers already running. Their leases remain visible to a later scheduler, so `clt agent start` can be run immediately—even after installing a new CLT binary—without duplicating their projects. Task-level stop and interrupt controls continue to reach older workers through the durable session-control records in `agent.db`.

The first upgrade from a CLT release that predates independent workers cannot detach a run that the old scheduler already owns in-process. To prevent accidentally terminating it, `start` and `stop` refuse while a live legacy scheduler lease exists; let that one-time legacy run finish and retry. Runs dispatched after this feature is installed are independent.

Worker startup and heartbeat records are fenced and bounded. If a worker fails before claiming its service or later stops checking in, the scheduler first drains and verifies that worker's exact launchd/systemd service, records one crash outcome, and only then releases its lease for recovery. This prevents a replacement Codex process from overlapping the old process group.

If prelaunch Git validation or persistence fails, the runner explicitly cancels the gated Codex launch and waits for its process group to stop. The runner then records the original error and log paths and releases its lease for the normal failure-backoff retry. This cancellation is distinct from an unexpected runner disconnect, which still triggers supervisor crash recovery.

Worker launch contracts are versioned. A newer scheduler can recover older persisted contracts, while an older scheduler leaves an unknown newer worker untouched. If a future database migration cannot safely coexist with pinned workers, it is deferred: status and task controls remain available, and the scheduler continues crash recovery in compatibility mode until those workers finish.

### Database recovery

If the registry is unavailable, project registration cannot be determined. The TUI reports the registry error and offers registration only after a successful refresh. Recovery accepts both legacy version-1 snapshots and current version-2 snapshots containing session Git modes.

`clt agent stop` does not open the database, so it remains available when Turso is unhealthy. For a shared-WAL ownership or frame-index failure, CLT records a recovery-required state. An idle scheduler attempts exclusive automatic repair using fresh handles before it stops; it never retries queries through an unhealthy handle or interrupts an active run to repair the registry. Once their Codex process groups have been reaped, interactive guardians and disconnected automated supervisors also exit instead of retrying finalization indefinitely; they preserve the session and lease records for recovery.

Registry opens (including TUI refreshes and scheduler heartbeats) check database integrity at most once per minute. Readable but damaged indexes trigger recovery instead of leaving stale worker, failure, or heartbeat rows in circulation. A busy database is retried without being classified as corruption.

Idle registry clients preserve commits made by other processes when opening new connections. A local WAL scan is used for initial reconciliation only, preventing an older client from restoring stale leases or settings after another client updates them. Recognized Turso index-page panics preserve their original cause in the recovery marker and stop further database operations.

On the next registry open, CLT automatically repairs coordination files when it can acquire exclusive database access and prove that recorded workers and session processes have exited. If integrity errors identify only worker indexes, it rebuilds those indexes from the existing table rows and verifies full database integrity and foreign keys. This preserves run history, settings, and Git journals. The original DB/WAL bundle is quarantined before repair. Automatic repair never stops another process or rebuilds the database from a snapshot; while a live worker or client holds the registry, it waits for that owner to exit. An interrupted registry update can also recover automatically: CLT verifies the original database, rechecks worker/session ownership from its committed rows, checkpoints it, and regenerates the external snapshot from those rows. This preserves commits newer than the last snapshot, including project settings and Git finalization state. An unfinished repair or an original database that cannot pass the supported integrity repair still requires explicit recovery. Close other CLT TUIs and foreground sessions, then run:

```bash
clt agent recover
```

Recovery stops the scheduler and verified worker services, checks recorded worker/session processes have exited, and refuses to change database files while any CLT or legacy Turso/SQLite client still holds access. It publishes a durable quarantine directory containing the original `agent.db`, `agent.db-wal`, and coordination files together. It first rebuilds only `agent.db-tshm`/`agent.db-shm` and checks database integrity. If that fails, it restores registered projects and preferences, worker identities, and exact Git launch/finalization journals from the atomically written `registry.json`. Run history, leases, scan timestamps and backoff may be discarded; task boards, Git repositories and filesystem logs remain authoritative. The existing WAL checkpoint pin remains enabled.

A `registry-dirty` marker identifies an interrupted database-to-snapshot update. If the original DB/WAL cannot be repaired, recovery refuses to guess which Git transition completed and retains the quarantine for manual reconciliation. A missing or invalid external snapshot also prevents automatic reconstruction. Recovery does not restart agents; review the result and use `clt agent start` when ready. Never delete live coordination files or separate the database from its WAL.

Transient database lock errors while saving a snapshot retry the snapshot with a fresh connection, up to three attempts. The completed registry mutation is not repeated, and the writer lock and dirty marker remain held until a durable snapshot succeeds. After persistent errors, the next fresh registry open attempts guarded automatic recovery of the original database. The failed operation is not replayed; a committed update is retained even if exporting its snapshot failed. If repair cannot proceed safely, CLT retains the recovery evidence and reports why; this can affect interactive Codex sessions as well as background scheduling.

Recovery also verifies and checkpoints the database write-ahead log (WAL) while holding exclusive access, retaining its original DB/WAL bundle in quarantine. Idle registry opens attempt maintenance once the WAL reaches 64 MiB; live clients or worker sessions defer it. Successful routine maintenance removes its own temporary backup after integrity checks and durable snapshot publication, so repeated cleanup does not accumulate archived WALs. Manual recovery and failed maintenance retain their backups.

If clients keep the WAL pinned until it reaches 128 MiB, CLT refuses new database updates and ordinary database opens before starting a mutation, migration, or dirty-snapshot interval. This guard applies to already-open stores too, leaving substantial headroom below the shared frame-index limit. An operation already in progress may cross the threshold; subsequent updates stop with a visible cleanup instruction. Close other CLT windows and foreground sessions, run `clt agent recover`, then `clt agent start`. The size guard itself does not mark the registry corrupt or discard project settings. Existing processes running an older CLT binary must be restarted to use these safeguards.

Registry writer-lock waits are bounded to five seconds and report contention instead of hanging indefinitely. A failed interactive Codex handoff keeps its terminal output visible until you press Enter, and planning-server startup errors are retained in the CLT console.

### Service environment and state

Run `clt agent start` and `clt agent stop` as your normal user, not with `sudo`; these commands manage per-user services.

On Linux, `clt` recovers the standard `/run/user/<uid>` systemd runtime directory when an SSH or non-interactive shell does not export `XDG_RUNTIME_DIR`. If the user bus is not running at all, log in through a systemd/PAM-managed session or ask an administrator to enable the always-on user manager with `sudo loginctl enable-linger "$USER"`, then start the service again.

Inspect agent state and recent output:
```bash
clt agent status
clt agent logs
clt agent clean
clt agent pause .
clt agent resume .
clt agent unregister .
```

`clt agent clean` resets stored failure and blocked-recovery state, deletes recorded run and terminal-worker history, removes agent run logs, and truncates background service logs. It keeps registered projects and task boards intact, and refuses to run while independent workers, Codex leases, nonterminal Git finalizations, or unconsumed pre-registration launch boundaries remain. Stopping the scheduler does not make an active worker or preserved proof boundary safe to clean.

By default, agent state is stored at `~/Library/Application Support/clt` on macOS, `$XDG_STATE_HOME/clt` on Linux when `XDG_STATE_HOME` is set, or `~/.local/state/clt` otherwise. The state directory contains `agent.db`, scheduler run logs, immutable worker binary generations, per-worker launch metadata, and background service logs such as `agent-service.out` and `agent-service.err`. Terminal worker services and directories are cleaned after completion, and each successful `start` removes binary generations no longer referenced by the new scheduler or an active worker. Override the state directory with:
```bash
CLT_AGENT_STATE_DIR=/path/to/state clt agent daemon
```

Useful runtime tuning variables are:

- `CLT_AGENT_MAX_GLOBAL_JOBS`: maximum Codex runs active globally, default `12`.
- `CLT_AGENT_POLL_INTERVAL_SECONDS`: daemon delay between scheduler passes, default `15`.
- `CLT_AGENT_RUN_TIMEOUT_SECONDS`: optional Codex run deadline in seconds. Unset or `0` (the default) lets a task run until it finishes or is explicitly stopped; elapsed time alone does not end a run. Set a positive value only when you explicitly want a run deadline.
- `CLT_AGENT_LEASE_TIMEOUT_SECONDS`: crash-safety deadline for renewable active leases, default `3600`. Healthy workers and interactive guardians renew before this deadline, so it is not a run-duration limit; known dead orphan reservations are reclaimed earlier.
- `CLT_AGENT_FAILURE_BACKOFF_SECONDS`: delay after a failed project run or an unchanged blocked-task recovery, default `300`.
- `CLT_AGENT_SUCCESS_COOLDOWN_SECONDS`: delay after a successful project run, default `5`.
- `CLT_AGENT_CODEX_PATH`: optional Codex executable override. By default, `clt agent start` verifies that `codex` works and the background service resolves `codex` from the stored `PATH` instead of pinning the executable's absolute location.
- `CLT_AGENT_HEARTBEAT_TAIL`: print a short stderr tail on still-running heartbeats when set to `1`, `true`, `yes`, or `on`; default `false`.

If Codex is installed through a version manager such as NVM, make sure the `PATH` used for `clt agent start` contains a stable bin directory. For example, NVM can maintain `~/.nvm/current`; putting `~/.nvm/current/bin` before version-specific directories lets the service continue finding `codex` after switching Node versions. Run `clt agent start` again after changing the service `PATH`; on Linux this reloads and restarts the existing user service.

## Task commands and storage

### Adding Tasks
Add a new task to the To Do list:
```bash
clt add My first task
```

**Metadata:** You can optionally add metadata (tags, priority, or IDs) which will be stored in parentheses:
```bash
clt add "Fix login bug" "BUG, HIGH"
```

### Managing the Backlog
Backlog is for captured work that is not ready to enter the To Do queue. `clt add` creates To Do tasks; move a task to Backlog when it needs to be deferred, list the Backlog for review, and promote it to To Do when it is ready:
```bash
clt status todo 1 backlog
clt list backlog
clt status backlog 1 todo
```

Automated agent runs ignore Backlog tasks until they are promoted to To Do.

### Moving Tasks
Change the status of a task:
```bash
clt status todo 1 doing
clt status doing 1 done
```

Alternatively, mark a task as done quickly:
```bash
clt done doing 1
```

### Recording an independent follow-up

If a task's implementation and acceptance checks are complete but a separate test harness or environment failure remains, establish that independence with a reproduction on the starting revision. Record the revision, failing command, matching failure, and the remaining work:

```bash
clt list doing
clt follow-up doing 1 "Fix existing lint warnings" --evidence "Same warnings on starting revision abc1234; feature acceptance checks pass"
```

This queues an actionable Todo linked to the parent's session, preserves the parent, and starts no new session. CLT reports that the follow-up is queued and the parent can finish normally. Ordinary code fixes, including pre-existing lint warnings, are ready work; creating them does not make the parent run fail or enter blocked recovery. Repeating the same command does not create another task. Add the follow-up reference and passing validation to the original task's COMPLETED note. In managed Git mode, stage the follow-up with the implementation and original Doing task before `clt done`, then include the Done transition in the same sealed commit. Unrelated board edits remain unstaged. After the parent finishes and the normal success cooldown elapses, the follow-up is eligible for a fresh run with its own session and Git start journal.

Use `--blocked "Unavailable dependency or input; what restores it"` only when an actual obstacle prevents starting the independent work. This explicitly records a blocked Doing follow-up for later recovery. Existing `--blocked` commands remain supported. A failed check that the follow-up itself is meant to fix is not an obstacle to starting it.

For an older follow-up that was marked blocked merely because its own work remains, preserve its notes, append `UNBLOCKED YYYY-MM-DD:` explaining that it is ready, then use `clt status doing <index> todo`. This lets an unstarted follow-up get a fresh run instead of attempting interrupted-task recovery without a Git start journal.

Keep the original task blocked when acceptance is incomplete, a relevant check fails, or the failure's independence is uncertain. A follow-up does not waive task requirements or commit hooks.

### Deleting Tasks
Remove a task from a specific list:
```bash
clt delete todo 1
```

### Listing Tasks
Get an overview of all tasks, or filter by status:
```bash
clt list
clt list backlog
clt list todo
```

Status number `0` is an alias for `backlog`; the existing `1`, `2`, and `3` aliases remain Todo, Doing, and Done.

### Folder-Backed Tasks
You can create folder-backed statuses during init or expand an existing markdown list:
```bash
clt init --folders
clt expand todo
clt expand
```

`clt expand todo` migrates only `todo.md`. `clt expand` migrates `backlog.md`, `todo.md`, `doing.md`, and `done.md`. The original Markdown files are preserved as `.bak` files.

A folder-backed status looks like this:
```text
tasks/
  backlog.md
  todo/
    0001-write-release-plan.md
  doing.md
  done.md
```

Each file in `tasks/todo/` is one task. The CLI and TUI show the first sentence, while the file can hold longer notes, checklists, and links. During an ordinary manual move, if a folder-backed task enters a Markdown-backed status, `clt` expands that destination status to a folder and preserves the old Markdown file as `status.md.bak`. Git-enabled managed automation never performs that conversion: it rejects the mixed Directory-to-Markdown route before launch so the layout can be expanded and committed deliberately.

Task folders become navigable subtask boards when they contain status stores:
```text
tasks/
  doing/
    0001-ship-dashboard/
      task.md
      backlog.md
      todo.md
      doing.md
      done.md
```

The folder's `task.md` provides the parent task text. Inside the TUI, selecting that task and pressing `Enter` opens its nested board. Pressing `n` or `+` on any selected task creates its nested board automatically when needed and prompts for a new Todo subtask.
