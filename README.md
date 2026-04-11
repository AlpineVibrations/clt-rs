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

### Adding Tasks
Add a new task to the To Do list:
```bash
clt add "My first task" "optional metadata"
```

### Moving Tasks
Change the status of a task using the `source->dest` transition format:
```bash
clt status todo->doing 1
clt status doing->done 1
```

### Listing Tasks
Get a quick overview of all tasks:
```bash
clt list
```

### Kanban View
Open the interactive TUI Kanban board:
```bash
clt view
```
*(Press 'q' to quit the TUI view)*

## Development

If you want to contribute or build from source:

```bash
git clone <repository-url>
cd cli-task
cargo build --release
```
