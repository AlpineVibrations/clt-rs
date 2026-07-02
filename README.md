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

- **File-based Persistence**: Tasks are stored in `tasks/todo.md`, `tasks/doing.md`, and `tasks/done.md`, or in status folders such as `tasks/todo/`.
- **Long Task Files**: In a status folder, each direct file is a task. `clt` displays the first sentence and preserves the full file content.
- **Nested Boards**: A task subfolder can contain its own `todo`, `doing`, and `done` files or folders. The TUI can open those as subtask boards.
- **Kanban TUI**: A visual board view powered by `ratatui`, with nested board navigation and a full-screen registered-projects pane.
- **Simple CLI**: Easy commands to add, move, and list tasks.
- **Smart Root Detection**: Automatically finds the git repository root to keep tasks centralized, or uses the current directory.
- **Agent Registry**: Register many projects, toggle them on or off, inspect `todo`/`doing` counts, and open any registered task board from the TUI.
- **Codex Automation**: Run one Codex task at a time per enabled project, either in the foreground or through a background service.
- **Agent Support Files**: Includes `clt-skill.md` guidance to help AI agents use `clt` safely for task tracking.

## Installation

Ensure you have Rust and Cargo installed.

```bash
cargo install clt-rs
```

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

Press `Tab` to switch to the full-screen agent projects pane. There, Up/Down selects a registered project, `Enter` opens that project's task board, and `Space` toggles the project `ON` or `OFF`. The currently open project is marked with `*`, and the terminal title updates to the active project.

### Codex Agent
`clt agent` can run Codex against enabled registered projects that have pending `todo` tasks. Each project keeps its own repo-local `tasks/` board, while the agent stores cross-project runtime state in one central state directory.

Before registering a project, initialize its task board and make sure the `codex` CLI is installed and authenticated. With no path, `register` uses the same project root that normal `clt` commands use:
```bash
clt init --folders
clt agent register
```

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

Run one foreground scheduler pass:
```bash
clt agent run --once
```

The scheduler scans enabled projects, picks projects with pending `todo` tasks, takes an agent lease, and starts one Codex run at a time. Each Codex run is prompted to inspect the board, move one available task to `doing`, complete it, run relevant checks, update the task through `clt`, and stop after that single task.

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

Inspect agent state and recent output:
```bash
clt agent status
clt agent logs
clt agent pause .
clt agent resume .
clt agent unregister .
```

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
- `CLT_AGENT_HEARTBEAT_TAIL`: print a short stderr tail on still-running heartbeats when set to `1`, `true`, `yes`, or `on`; default `false`.

### Adding Tasks
Add a new task to the To Do list:
```bash
clt add My first task
```

**Metadata:** You can optionally add metadata (tags, priority, or IDs) which will be stored in parentheses:
```bash
clt add "Fix login bug" "BUG, HIGH"
```

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
clt list todo
```

### Folder-Backed Tasks
You can create folder-backed statuses during init or expand an existing markdown list:
```bash
clt init --folders
clt expand todo
clt expand
```

`clt expand todo` migrates only `todo.md`. `clt expand` migrates `todo.md`, `doing.md`, and `done.md`. The original Markdown files are preserved as `.bak` files.

A folder-backed status looks like this:
```text
tasks/
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
