```
  ██████╗██╗  ████████╗
 ██╔════╝██║  ╚══██╔══╝
 ██║     ██║     ██║   
 ██║     ██║     ██║   
 ╚██████╗███████╗██║   
  ╚═════╝╚══════╝╚═╝   

  ▸ command line tasks
  ▸ file-backed · rust · tui · agents
```

# clt

A file-system-backed task manager written in Rust. `clt` stores work in Markdown files or task folders, gives humans a fast CLI and TUI Kanban board, and can coordinate Codex agent runs across multiple registered projects.

## Features

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

Ensure you have Rust and Cargo installed.

```bash
cargo install clt-rs
```

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

The installed `clt` binary embeds both agent skills. Before an automated Codex run, `clt` looks for each required skill by its frontmatter name in the standard repository, user, and admin skill directories. If a skill is unavailable, `clt` adds its bundled instructions to that run's prompt automatically, so `cargo install clt-rs` is sufficient for agent automation.

To make the skills discoverable to Codex outside `clt` agent runs, clone this repository and copy the skill folders into the `skills` directory inside your home `.agents` directory. From the repository root, run:

```bash
mkdir -p ~/.agents/skills
cp -R skills/clt-task-management skills/git-commit ~/.agents/skills/
```

Each copied folder contains the skill's `SKILL.md` file. Restart your agent after copying the folders so it can discover the new skills.

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

### Kanban View
Open the interactive TUI Kanban board:
```bash
clt
```
Press `Enter` to open a folder task with subtasks, `n` or `+` to create a subtask under the selected task, `e` to edit the selected task, `Space` to create a task, `Backspace` to return to the parent board, and `q` to quit. Creating a subtask automatically expands a Markdown-backed parent status to folder-backed storage, preserving the original status file as a `.bak`, converts the selected task into a nested board, and opens that board after the subtask is saved. Cancelling the prompt leaves storage unchanged.

Press `r` to enter sticky Reorganize mode, then use the arrow keys as many times as needed: Up/Down changes the selected task's position and Left/Right moves it between columns. The task-board borders turn yellow and the selected column shows `REORGANIZE MODE` while the mode is active. Press `r` again or `Esc` to return to normal navigation.

You can also use `Shift+Up` and `Shift+Down` to reorder the selected task, and `Shift+Left` and `Shift+Right` to move it between columns. `Ctrl-P` reorders the selected task up and `Ctrl-N` reorders it down; these portable alternatives work in stock macOS Terminal and through SSH or tmux.

Stock macOS Terminal does not encode Shift in its default Up/Down sequences, so the modifier is lost before `clt` receives it. To keep using Shift+Up/Down there, add these two mappings on the Mac under Terminal > Settings > Profiles > Keyboard:

- Shift+Up: send `\033[1;2A`
- Shift+Down: send `\033[1;2B`

Press `a` to move the selected task into the archive. Press `A` to open the archive's single-panel scrolling view, and press `A` again to return to the Kanban board.

Backlog is a fourth column for captured work that is not ready to be acted on. It is hidden by default; the task-board console title shows its current task count. Press `b` to move the selected task to Backlog, `B` to show or hide the Backlog column, or `0` to show and focus it. When visible, Backlog appears to the left of To Do and works with the normal Left/Right focus and task-movement controls. Keys `1`, `2`, and `3` continue to focus To Do, Doing, and Done.

Press `Tab` to toggle between the task board and the full-screen Agent Projects pane. In Agent Projects, Up/Down selects a registered project, `Enter` opens that project's task board, `Space` toggles the project `ON` or `OFF`, `Delete` removes it from the agent list after a `y`/`n` confirmation, and `g` cycles the `GIT` column through `OFF`, `COM`, and `PUSH`. These modes disable Git automation, ask Codex to commit after a completed task, or ask Codex to commit and push. Removing a project only unregisters it from the agent list; it does not delete the project or its task files. The currently open project is marked with `*`, the pane's top border shows the current local time before the daemon status, and the terminal title updates to the active project.

The daemon persists its own project-scan result separately from the TUI's local task count. If the background service cannot read a project, the `AGENT` column shows `ERROR`, the row turns red, and selecting it shows the full scan error and recovery guidance in the console. External projects under `/Volumes` specifically direct macOS users to enable Full Disk Access for CLT and restart the agent; missing external projects instead prompt users to check that the drive is mounted. `INTERACTIVE` identifies a live guarded Codex handoff, while `FENCED` means a session control survived without a live project lease, usually while CLT is proving that a crashed run's process group exited. Press `s` directly on either project row to request a safe stop, or open its output with `l` for exact-session `s`/`i` controls.

On a selected task with a linked, active Codex session, press `s` to stop only that task's current Codex process. The task and its session link remain in place so the work can be resumed later, while its finished worker and project lease are released so another Todo task can run. With that stopped task still selected, press `s` again to queue the exact same session ID for automated `codex exec resume`. Press `i` while a linked session is active to stop its automated process and immediately open that same ID with interactive `codex resume`. CLT transfers the project's scheduler lease only after the old process has exited. When you leave interactive Codex, CLT automatically restarts the same session ID on the same task in `codex exec resume` mode. A second TUI can press `s` on an `INTERACTIVE` or `FENCED` project row even when no output link is available: the waiting parent closes its private lifeline, the guardian stops and reaps its exact Codex process group, and CLT releases a completed-session reservation or preserves an interrupted active session as stopped. CLT recovers a handoff abandoned before the guardian takes ownership without allowing another project task to start in between. If it cannot prove that the prior Codex process group exited, the project stays fenced rather than risking a concurrent resume.

Crash-safe exact-session relaunch currently requires Unix process supervision. The outer runner is the only process that polls session controls while connected; the child-owning supervisor watches a database-free lifeline, catches monitor panics, and stops and reaps its Codex process group before emitting shutdown proof. This separation prevents an agent-database failure in the supervisor from stranding a live Codex child. On other platforms, CLT refuses a known-session relaunch before spawning it.

On the task board, the `c` shortcut remains deliberately narrower: select a Done task or a currently blocked Todo/Doing task whose linked Codex session is idle, then press `c` to open it interactively with workspace-write access. When the project is otherwise idle, CLT reserves the project until you return. When another Codex task is already using the project, the selected idle session opens alongside it without interrupting the active run; both sessions can modify the same worktree. CLT still prevents the exact selected session from opening twice; use `i` to take over that session's active automated run. Both modes return to the same board and selection afterward.

As soon as Codex announces its session ID and the selected task enters Doing, CLT appends a terminal `codex:<session-id>` marker to the task. This internal marker survives task moves and wording changes, is hidden in task lists, the TUI, and the task editor, and is the task-to-session resume link. While a run is active, the database also records that session's exact run generation and log paths so `l`, stop, and interrupt target the correct live process. Completed run history retains the session ID without associating it to mutable task text. A run is reported as failed if CLT cannot persist the marker on its completed or blocked task.

The agent projects pane also monitors the installed background service. If the operating system still reports the service as running but its daemon check-in becomes stale, the pane restarts the service automatically and shows `service restarting` while it recovers. A service explicitly stopped with `clt agent stop` remains stopped.

Press `l` from the Kanban board to open output for the selected task. A task linked to the currently active Codex session shows that session's live agent output even if the task has already moved to Done or become blocked; otherwise completed or blocked tasks with a linked session show that task's recorded output. The open console follows the highlighted task as you move through the board. The same key opens the selected project's live or latest output from the Agent Projects pane. On an Agent Projects row, `s` directly controls the one active, interactive, or fenced session; if several sessions are present, open the intended output first. While project output is open, `s` stops or resumes its exact session, `i` takes over a live or stopped session interactively and hands it back to automated exec afterward, and `c` continues an idle/latest session interactively. These controls use the session represented by the displayed run rather than searching the task board, so they remain available when a task was moved, nested, deleted, or lost its session marker. Pressing `c` on a live session directs you to `i`, and CLT refuses to act when the displayed output does not identify one exact session. The console expands and follows new output until `l` or `Esc` closes the log.

Press uppercase `M` from either the task board or Agent Projects to open the Models page; uppercase `M`, `Tab`, or `Esc` returns to the pane you came from. The Models page keeps a catalog of providers and model targets with aligned, labeled columns. `USE` shows live availability, `FAV` marks favorites, and the separate `CLT` and `CODEX` columns identify the effective CLT-wide default and the user's Codex config default; `YES` is shown when a row has that role. `THINK` shows each model's default reasoning level: press `t` to cycle through system, low, medium, high, extra-high, max, and ultra. A model setting is used for agent runs unless the selected project has its own reasoning override. Changing `THINK` on the `CODEX=YES` model also updates Codex's top-level reasoning default immediately; choosing system removes that override. Pressing `c` to choose a new Codex default writes both its model and reasoning setting. When no explicit CLT override exists, CLT follows the Codex default and both columns mark the same model. The provider pane always shows the available presets: press `1` through `4` to add or enable OpenAI, OpenRouter, Ollama, or LM Studio. Ollama and LM Studio query their standard local URLs for models immediately. To remove a provider, select it in the left pane and press `x` or `Delete`; its models and affected CLT/project selections are removed, along with its custom Codex provider configuration. Built-in OpenAI cannot be removed, but `Space` can disable it.

Press `n` to add another local or custom OpenAI-compatible endpoint. CLT asks for a friendly name, the API base URL, and an optional API-key environment-variable name; it creates the internal provider ID automatically. Enter the API root, for example `http://127.0.0.1:9090/v1`. Include `/v1` when that is where the server exposes its compatible API, or omit it when the server exposes endpoints directly at the host root. Do not paste a complete operation URL ending in `/chat`, `/chat/completions`, `/models`, or `/responses`. After saving, CLT requests `<base URL>/models` and shows every returned model in the Models pane. Newly discovered models start `OFF`, so use Right, Up/Down, and `Space` to choose exactly which models appear in project selection. Press `r` to discover again later, or `a` to enter a model ID manually when an endpoint does not expose `/models`. `Space` also enables or hides the selected provider, and `f` toggles model favorite status. Favorites sort first.

Press `d` on a model to make its provider/model pair the CLT-wide default for new agent runs. Press `c` only when you also want to update the top-level `model_provider` and `model` values in the user's Codex `config.toml`; CLT preserves other TOML content and creates `config.toml.clt.bak` before its first edit. Custom provider definitions use Codex's `model_providers` table with `wire_api = "responses"`, so the selected endpoint must support the Responses API at `<base URL>/responses`.

Provider API keys remain ordinary environment variables. The Models page shows whether the configured variable, such as `OPENROUTER_API_KEY`, is visible to the current CLT process, but it never accepts, stores, or writes secret values or `.env` files. OpenAI can also use the normal Codex login. A foreground daemon inherits its launch environment; a background user service must have the same variables in its service-manager environment.

Each registered project has persisted Codex launch settings in the `CODEX` column. Overrides are shown compactly as `provider:model/thinking/fast`; `default` means the project follows the CLT-wide default, which in turn falls back to the user's Codex config when unset. Press lowercase `m` to cycle through the CLT default and currently enabled provider/model targets, `f` to toggle Fast mode, and `t` to cycle through the default, low, medium, high, extra-high, max, and ultra reasoning levels. Settings are resolved when a new run launches; an already running process is unchanged.

### Codex Agent
`clt agent` can run Codex against enabled registered projects that have unblocked `todo` tasks. It can also recover a task left in `doing` when a previous agent lease belongs to a crashed process or has expired. Before starting fresh Todo work, the scheduler starts a blocked-task monitor run when a Todo or Doing task has a current blocker note and its recovery backoff has elapsed. Backlog tasks are deliberately ignored until they are promoted to Todo. Each project keeps its own repo-local `tasks/` board, while the agent stores cross-project runtime state in one central state directory.

Before registering a project, initialize its task board and make sure the `codex` CLI is installed and authenticated. With no path, `register` uses the same project root that normal `clt` commands use:
```bash
clt init --folders
clt agent register
```

Registering a project is the user's explicit opt-in to automated Codex runs in that directory. Registered projects do not have to be Git repositories; CLT passes Codex's non-Git `exec` override for both new and resumed automated sessions.

#### Linux Codex sandbox setup

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

Configure the optional `git-commit` skill instruction per project:
```bash
clt agent git-commit enable ~/code/project-a
clt agent git-commit push ~/code/project-a
clt agent git-commit disable ~/code/project-a
```

`enable` selects commit-only mode, `push` selects commit-and-push mode, and `disable` turns Git automation off. The scheduler adds matching instructions to the Codex prompt after task completion and verification. Commits from either enabled mode use `CLT Agent <clt-agent@localhost>` as both author and committer so automated work is recognizable without changing repository or global Git configuration. Existing enabled registrations migrate to commit-only mode. In the TUI, the modes appear as `COM`, `PUSH`, and `OFF` in the `GIT` column.

Agent-facing workflow skills are included in the repository's `skills/` directory:

- `skills/clt-task-management/`: task-board workflow guidance for using `clt`.
- `skills/git-commit/`: git commit and optional push workflow guidance.

Automated `clt` agent runs use embedded copies when these skills are not installed. Copy the folders into `~/.agents/skills/` using the commands in [Installation](#installation) only when you also want to invoke them directly in other Codex sessions.

Run one foreground scheduler pass:
```bash
clt agent run --once
```

The scheduler scans enabled projects, picks projects with pending unblocked `todo` tasks, takes an agent lease, and starts one Codex run at a time. A foreground `run --once` owns its run directly. On macOS and Linux, the continuous daemon instead hands each run to a unique launchd job or transient systemd user service. That worker owns lease renewal, the Codex process, task/session finalization, and the run record; the scheduler is free to stop immediately after dispatch. Each normal Codex run is prompted to inspect the board, move one available task to `doing`, complete it, run relevant checks, update the task through `clt`, and stop after that single task. If a crashed worker left a task in `doing`, the scheduler uses its durable worker record to resume that task before starting new work. When exactly one interrupted or blocked task carries a session marker, recovery uses `codex exec resume` for that session instead of opening a new one. Explicit stop and interactive-handoff states suppress ordinary scheduling; an interactive `i` handback is prioritized as an exact-session resume before any Todo selection.

Prefix a Todo task with `/goal` when it needs a persistent objective for long-running work. Automated runs enable Codex goals, remove the leading directive from the goal objective, and ask Codex to create the goal before working on the task. The directive must be the task's first non-whitespace token and must be followed by a non-empty objective; `/goal` elsewhere in a task remains ordinary text.

```bash
clt add "/goal Migrate the authentication module and stop when all tests pass"
```

Use this for one durable objective with a verifiable stopping condition; keep quick fixes and unrelated task lists as normal Todo items. See the [official OpenAI goal guide](https://learn.chatgpt.com/use-cases/follow-goals) for goal-writing guidance.

Blocked-task recovery takes priority over fresh Todo work whenever its recovery backoff permits. A monitor run reviews the blocker notes, rechecks whether their conditions still exist, and works on exactly one blocked task from Todo or Doing. It can complete that task, add a newer `UNBLOCKED YYYY-MM-DD:` note and return it to Todo after resolving its blocker, or update its blocked note with the latest attempt. The latest dated `BLOCKED`, `UNBLOCKED`, or `COMPLETED` state note determines whether a task is currently blocked. If recovery leaves the blocker unresolved, the run is recorded as `blocked`; ready Todo work can proceed during `CLT_AGENT_FAILURE_BACKOFF_SECONDS`, after which the blocker is checked again before another fresh task. Unmarked Doing tasks are left alone because they may belong to a human or another workflow.

Automated runs start Codex with `--sandbox danger-full-access --ask-for-approval never --enable goals` so tasks can update Git metadata without pausing for interactive approval and `/goal` tasks can create persistent goals. This removes the Codex command sandbox for the entire run. Register only trusted repositories, or run the agent inside an externally isolated container or VM.

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

`clt agent stop` stops only that scheduler. It does not drain, wait for, or terminate independent workers already running. Their leases remain visible to a later scheduler, so `clt agent start` can be run immediately—even after installing a new CLT binary—without duplicating their projects. Task-level stop and interrupt controls continue to reach older workers through the durable session-control records in `agent.db`.

The first upgrade from a CLT release that predates independent workers cannot detach a run that the old scheduler already owns in-process. To prevent accidentally terminating it, `start` and `stop` refuse while a live legacy scheduler lease exists; let that one-time legacy run finish and retry. Runs dispatched after this feature is installed are independent.

Worker startup and heartbeat records are fenced and bounded. If a worker fails before claiming its service or later stops checking in, the scheduler first drains and verifies that worker's exact launchd/systemd service, records one crash outcome, and only then releases its lease for recovery. This prevents a replacement Codex process from overlapping the old process group.

Worker launch contracts are versioned. A newer scheduler can recover older persisted contracts, while an older scheduler leaves an unknown newer worker untouched. If a future database migration cannot safely coexist with pinned workers, it is deferred: status and task controls remain available, and the scheduler continues crash recovery in compatibility mode until those workers finish.

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

`clt agent clean` resets stored failure and blocked-recovery state, deletes recorded run and terminal-worker history, removes agent run logs, and truncates background service logs. It keeps registered projects and task boards intact, and refuses to run while independent workers or other Codex leases are active. Stopping the scheduler does not make an active worker safe to clean.

By default, agent state is stored at `~/Library/Application Support/clt` on macOS, `$XDG_STATE_HOME/clt` on Linux when `XDG_STATE_HOME` is set, or `~/.local/state/clt` otherwise. The state directory contains `agent.db`, scheduler run logs, immutable worker binary generations, per-worker launch metadata, and background service logs such as `agent-service.out` and `agent-service.err`. Terminal worker services and directories are cleaned after completion, and each successful `start` removes binary generations no longer referenced by the new scheduler or an active worker. Override the state directory with:
```bash
CLT_AGENT_STATE_DIR=/path/to/state clt agent daemon
```

Useful runtime tuning variables are:

- `CLT_AGENT_MAX_GLOBAL_JOBS`: maximum Codex runs active globally, default `12`.
- `CLT_AGENT_POLL_INTERVAL_SECONDS`: daemon delay between scheduler passes, default `15`.
- `CLT_AGENT_RUN_TIMEOUT_SECONDS`: Codex process timeout, default `2700`.
- `CLT_AGENT_LEASE_TIMEOUT_SECONDS`: active lease expiry, default `3600`.
- `CLT_AGENT_FAILURE_BACKOFF_SECONDS`: delay after a failed project run or an unchanged blocked-task recovery, default `300`.
- `CLT_AGENT_SUCCESS_COOLDOWN_SECONDS`: delay after a successful project run, default `5`.
- `CLT_AGENT_CODEX_PATH`: optional Codex executable override. By default, `clt agent start` verifies that `codex` works and the background service resolves `codex` from the stored `PATH` instead of pinning the executable's absolute location.
- `CLT_AGENT_HEARTBEAT_TAIL`: print a short stderr tail on still-running heartbeats when set to `1`, `true`, `yes`, or `on`; default `false`.

If Codex is installed through a version manager such as NVM, make sure the `PATH` used for `clt agent start` contains a stable bin directory. For example, NVM can maintain `~/.nvm/current`; putting `~/.nvm/current/bin` before version-specific directories lets the service continue finding `codex` after switching Node versions. Run `clt agent start` again after changing the service `PATH`; on Linux this reloads and restarts the existing user service.

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

Each file in `tasks/todo/` is one task. The CLI and TUI show the first sentence, while the file can hold longer notes, checklists, and links. If a folder-backed task moves into a Markdown-backed status, `clt` expands that destination status to a folder and preserves the old Markdown file as `status.md.bak`.

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


## Development

If you want to contribute or build from source:

```bash
git clone <repository-url>
cd cli-task
cargo build --release
```

Release notes are tracked in [CHANGELOG.md](CHANGELOG.md). New user-facing features, behavior changes, and bug fixes should be added under `Unreleased` first, then moved into a versioned section when publishing a release.
