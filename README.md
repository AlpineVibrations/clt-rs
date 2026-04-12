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

A simple, file-system-backed CLI task management app written in Rust. It uses Markdown files for persistence and provides both a command-line interface and a TUI Kanban board.

## Features

- **File-based Persistence**: Tasks are stored in `tasks/todo.md`, `tasks/doing.md`, and `tasks/done.md`.
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

**Note:** By default, `clt` looks for the root of your git repository to store the `tasks/` folder. To force use of the current directory instead, use the `--local` flag:
```bash
clt --local init
```

### Kanban View
Open the interactive TUI Kanban board:
```bash
clt view
```
*(Press 'q' to quit the TUI view)*

### Adding Tasks
Add a new task to the To Do list:
```bash
clt add "My first task"
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
clt done 1
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


## Development

If you want to contribute or build from source:

```bash
git clone <repository-url>
cd cli-task
cargo build --release
```
