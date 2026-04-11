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
