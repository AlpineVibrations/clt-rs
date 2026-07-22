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
- **Codex Automation**: Run one Codex task at a time per enabled project, either in the foreground or through a background service.
- **Agent Skills**: Includes installable `clt-task-management` and `git-commit` skill folders for task-board and safe commit workflows.

## Installation

Ensure you have Rust and Cargo installed.

```bash
cargo install clt-rs
```

To install the bundled agent skills, clone this repository and copy the skill folders into the `skills` directory inside your home `.agents` directory. From the repository root, run:

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
Press `Enter` to open a folder task with subtasks, `e` to edit the selected task, `Space` to create a task, `Backspace` to return to the parent board, and `q` to quit.

Press `a` to move the selected task into the archive. Press `A` to open the archive's single-panel scrolling view, and press `A` again to return to the Kanban board.

Backlog is a fourth column for captured work that is not ready to be acted on. It is hidden by default; the task-board console title shows its current task count. Press `b` to move the selected task to Backlog, `B` to show or hide the Backlog column, or `0` to show and focus it. When visible, Backlog appears to the left of To Do and works with the normal Left/Right focus and task-movement controls. Keys `1`, `2`, and `3` continue to focus To Do, Doing, and Done.

Press `Tab` to switch to the full-screen agent projects pane. There, Up/Down selects a registered project, `Enter` opens that project's task board, `Space` toggles the project `ON` or `OFF`, and `g` cycles the `GIT` column through `OFF`, `COM`, and `PUSH`. These modes disable Git automation, ask Codex to commit after a completed task, or ask Codex to commit and push. The currently open project is marked with `*`, and the terminal title updates to the active project.

Press `l` from the Kanban board to open the active project's live agent output, or its latest recorded output when no run is active. The same key opens the selected project's output from the agent projects pane. The console expands and follows new output until `l` or `Esc` closes the log.

Each registered project also has persisted Codex launch settings in the `CODEX` column. Enabled overrides are shown compactly as `model/thinking/fast`; settings that inherit the user's configuration are omitted, and `default` is shown when every setting is inherited. Press `f` to toggle Fast mode, `m` to cycle through the default and supported model choices, and `t` to cycle through the default, low, medium, high, extra-high, max, and ultra reasoning levels. These settings are applied to future automated runs; they do not change an agent process that is already running.

### Codex Agent
`clt agent` can run Codex against enabled registered projects that have pending `todo` tasks. It can also recover a task left in `doing` when a previous agent lease belongs to a crashed process or has expired. Backlog tasks are deliberately ignored until they are promoted to Todo. Each project keeps its own repo-local `tasks/` board, while the agent stores cross-project runtime state in one central state directory.

Before registering a project, initialize its task board and make sure the `codex` CLI is installed and authenticated. With no path, `register` uses the same project root that normal `clt` commands use:
```bash
clt init --folders
clt agent register
```

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

`enable` selects commit-only mode, `push` selects commit-and-push mode, and `disable` turns Git automation off. The scheduler adds matching instructions to the Codex prompt after task completion and verification. Existing enabled registrations migrate to commit-only mode. In the TUI, the modes appear as `COM`, `PUSH`, and `OFF` in the `GIT` column.

Agent-facing workflow skills are included in the repository's `skills/` directory:

- `skills/clt-task-management/`: task-board workflow guidance for using `clt`.
- `skills/git-commit/`: git commit and optional push workflow guidance.

Copy these folders into `~/.agents/skills/` using the commands in [Installation](#installation) before asking Codex to use them.

Run one foreground scheduler pass:
```bash
clt agent run --once
```

The scheduler scans enabled projects, picks projects with pending `todo` tasks, takes an agent lease, and starts one Codex run at a time. Each Codex run is prompted to inspect the board, move one available task to `doing`, complete it, run relevant checks, update the task through `clt`, and stop after that single task. If a crashed run left a stale lease and a task in `doing`, the scheduler reclaims the lease and prompts the replacement run to resume that task before starting new work.

Automated runs start Codex with `--sandbox danger-full-access --ask-for-approval never` so tasks can update Git metadata without pausing for interactive approval. This removes the Codex command sandbox for the entire run. Register only trusted repositories, or run the agent inside an externally isolated container or VM.

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

Run `clt agent start` and `clt agent stop` as your normal user, not with `sudo`; these commands manage per-user services.

Inspect agent state and recent output:
```bash
clt agent status
clt agent logs
clt agent clean
clt agent pause .
clt agent resume .
clt agent unregister .
```

`clt agent clean` resets stored failure state, deletes recorded run history, removes agent run logs, and truncates background service logs. It keeps registered projects and task boards intact, and refuses to run while active Codex leases exist.

By default, agent state is stored at `~/Library/Application Support/clt` on macOS, `$XDG_STATE_HOME/clt` on Linux when `XDG_STATE_HOME` is set, or `~/.local/state/clt` otherwise. The state directory contains `agent.db`, scheduler run logs, and background service logs such as `agent-service.out` and `agent-service.err`. Override it with:
```bash
CLT_AGENT_STATE_DIR=/path/to/state clt agent daemon
```

Useful runtime tuning variables are:

- `CLT_AGENT_MAX_GLOBAL_JOBS`: maximum Codex runs active globally, default `12`.
- `CLT_AGENT_POLL_INTERVAL_SECONDS`: daemon delay between scheduler passes, default `15`.
- `CLT_AGENT_RUN_TIMEOUT_SECONDS`: Codex process timeout, default `2700`.
- `CLT_AGENT_LEASE_TIMEOUT_SECONDS`: active lease expiry, default `3600`.
- `CLT_AGENT_FAILURE_BACKOFF_SECONDS`: delay after a failed project run, default `300`.
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

The folder's `task.md` provides the parent task text. Inside the TUI, selecting that task and pressing `Enter` opens its nested board.


## Development

If you want to contribute or build from source:

```bash
git clone <repository-url>
cd cli-task
cargo build --release
```

Release notes are tracked in [CHANGELOG.md](CHANGELOG.md). New user-facing features, behavior changes, and bug fixes should be added under `Unreleased` first, then moved into a versioned section when publishing a release.
