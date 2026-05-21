```
  ██████╗██╗  ████████╗
 ██╔════╝██║  ╚══██╔══╝
 ██║     ██║     ██║   
 ██║     ██║     ██║   
 ╚██████╗███████╗██║   
  ╚═════╝╚══════╝╚═╝   

  ▸ command line tasks
  ▸ file-backed · rust · tui
```

# clt

A simple, file-system-backed CLI task management app written in Rust. It uses Markdown files or task folders for persistence and provides both a command-line interface and a TUI Kanban board.

## Features

- **File-based Persistence**: Tasks are stored in `tasks/todo.md`, `tasks/doing.md`, and `tasks/done.md`, or in status folders such as `tasks/todo/`.
- **Long Task Files**: In a status folder, each direct file is a task. `clt` displays the first sentence and preserves the full file content.
- **Nested Boards**: A task subfolder can contain its own `todo`, `doing`, and `done` files or folders. The TUI can open those as subtask boards.
- **Kanban TUI**: A visual board view powered by `ratatui`.
- **Simple CLI**: Easy commands to add, move, and list tasks.
- **Smart Root Detection**: Automatically finds the git repository root to keep tasks centralized, or uses the current directory.
- **Agent Support**: Includes a `clt-skill.md` file to help AI agents use `clt` for task tracking.

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
Press `Enter` to open a folder task with subtasks, `e` to edit the selected task, `Backspace` to return to the parent board, and `q` to quit.

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
