use clap::{Parser, Subcommand};
use std::fs;
use std::io::{Write, stdout};
use std::path::Path;
use anyhow::{Context, Result};

use crossterm::{
    event::{self, Event, KeyCode},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Terminal,
};

#[derive(Parser)]
#[command(name = "lls-cli-task")]
#[command(about = "A simple file-system-backed task management system", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initializes the tasks directory and markdown files
    Init,
    /// Adds a new task to the todo list
    Add {
        /// The description of the task
        description: String,
        /// Optional metadata for the task
        metadata: Option<String>,
    },
    /// Changes the status of a task
    Status {
        /// The transition (e.g., "todo->doing")
        transition: String,
        /// The ID of the task to move
        task_id: String,
    },
    /// Lists all tasks across all states
    List,
    /// Opens the Kanban TUI view
    View,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Init => {
            init_tasks()?;
        }
        Commands::Add { description, metadata } => {
            add_task(&description, metadata)?;
        }
        Commands::Status { transition, task_id } => {
            move_task(&transition, &task_id)?;
        }
        Commands::List => {
            list_tasks()?;
        }
        Commands::View => {
            tui_view()?;
        }
    }

    Ok(())
}

fn get_file_path(status: &str) -> Result<std::path::PathBuf> {
    match status {
        "todo" => Ok(Path::new("tasks/todo.md").to_path_buf()),
        "doing" => Ok(Path::new("tasks/doing.md").to_path_buf()),
        "done" => Ok(Path::new("tasks/done.md").to_path_buf()),
        _ => anyhow::bail!("Invalid status. Use 'todo', 'doing', or 'done'."),
    }
}

fn move_task(transition: &str, task_id: &str) -> Result<()> {
    let parts: Vec<&str> = transition.split("->").collect();
    if parts.len() != 2 {
        anyhow::bail!("Invalid transition format. Use 'source->dest' (e.g., 'todo->doing')");
    }

    let src_path = get_file_path(parts[0])?;
    let dest_path = get_file_path(parts[1])?;

    let content = fs::read_to_string(&src_path).context("Failed to read source file")?;
    let lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();
    
    let mut found_index = None;
    let mut task_line = String::new();

    for (i, line) in lines.iter().enumerate() {
        // Match "- [ ] 1: " or "- [x] 1: "
        if line.contains(&format!(" {}: ", task_id)) && (line.starts_with("- [ ]") || line.starts_with("- [x]")) {
            found_index = Some(i);
            task_line = line.clone();
            break;
        }
    }

    let idx = found_index.context(format!("Task ID {} not found in {:?}", task_id, src_path))?;

    // Remove from source
    let mut new_src_lines = lines.clone();
    new_src_lines.remove(idx);
    fs::write(&src_path, new_src_lines.join("\n") + "\n").context("Failed to update source file")?;

    // Update checkbox for destination
    let final_line = if parts[1] == "done" {
        task_line.replace("- [ ]", "- [x]")
    } else if parts[1] == "todo" {
        task_line.replace("- [x]", "- [ ]")
    } else {
        task_line
    };

    let mut dest_file = fs::OpenOptions::new()
        .append(true)
        .open(&dest_path)
        .context("Failed to open destination file")?;

    dest_file.write_all(format!("{}\n", final_line).as_bytes()).context("Failed to write to destination file")?;

    println!("Task {} moved from {} to {}.", task_id, parts[0], parts[1]);
    Ok(())
}

fn list_tasks() -> Result<()> {
    let statuses = ["todo", "doing", "done"];
    for status in statuses {
        let path = get_file_path(status)?;
        println!("\n--- {} ---", status.to_uppercase());
        let content = fs::read_to_string(&path).context(format!("Failed to read {:?}", path))?;
        for line in content.lines() {
            if line.starts_with("- [") {
                println!("{}", line);
            }
        }
    }
    Ok(())
}

fn add_task(description: &str, metadata: Option<String>) -> Result<()> {
    let path = Path::new("tasks/todo.md");
    if !path.exists() {
        anyhow::bail!("Tasks not initialized. Please run 'init' first.");
    }

    // Find the maximum ID across all task files to ensure uniqueness
    let mut max_id = 0;
    for status in ["todo", "doing", "done"] {
        if let Ok(tasks) = read_tasks(status) {
            for task in tasks {
                // Task format: "- [ ] 1: Description"
                if let Some(id_part) = task.split(':').next() {
                    // The id_part might be "- [ ] 1" or "- [x] 1", so we need to be careful
                    // Better: split by space and take the last part before the colon
                    let parts: Vec<&str> = id_part.split_whitespace().collect();
                    if let Some(last) = parts.last() {
                        if let Ok(id) = last.parse::<usize>() {
                            if id > max_id {
                                max_id = id;
                            }
                        }
                    }
                }
            }
        }
    }
    let task_id = max_id + 1;

    let metadata_str = match metadata {
        Some(m) => format!(" ({})", m),
        None => "".to_string(),
    };

    let task_line = format!("- [ ] {}: {}{}\n", task_id, description, metadata_str);
    
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(path)
        .context("Failed to open todo.md for appending")?;

    file.write_all(task_line.as_bytes()).context("Failed to write task to todo.md")?;

    println!("Task added with ID: {}", task_id);
    Ok(())
}

fn read_tasks(status: &str) -> Result<Vec<String>> {
    let path = get_file_path(status)?;
    let content = fs::read_to_string(&path).context(format!("Failed to read {:?}", path))?;
    let tasks = content
        .lines()
        .filter(|l| l.starts_with("- ["))
        .map(|l| l.to_string())
        .collect();
    Ok(tasks)
}

enum Mode {
    View,
    Input,
}

fn tui_view() -> Result<()> {
    // Setup terminal
    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;

    let mut current_mode = Mode::View;
    let mut input_buffer = String::new();

    loop {
        terminal.draw(|f| {
            let size = f.size();
            
            // Main layout: Kanban board on top, input area at bottom if in Input mode
            let main_layout = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(0),
                    if matches!(current_mode, Mode::Input) { Constraint::Length(3) } else { Constraint::Length(0) },
                ])
                .split(size);

            let kanban_area = main_layout[0];
            let chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Percentage(33),
                    Constraint::Percentage(33),
                    Constraint::Percentage(33),
                ])
                .split(kanban_area);

            let statuses = ["todo", "doing", "done"];
            let titles = ["To Do", "In Progress", "Done"];
            let colors = [Color::Yellow, Color::Cyan, Color::Green];

            for (i, status) in statuses.iter().enumerate() {
                let tasks = read_tasks(status).unwrap_or_default();
                let items: Vec<ListItem> = tasks
                    .into_iter()
                    .map(|t| {
                        let cleaned = t.replace("- [ ] ", "").replace("- [x] ", "");
                        ListItem::new(cleaned)
                    })
                    .collect();

                let list = List::new(items)
                    .block(Block::default()
                        .title(titles[i])
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(colors[i])))
                    .style(Style::default().fg(Color::White));

                f.render_widget(list, chunks[i]);
            }

            if matches!(current_mode, Mode::Input) {
                let input_text = format!(" Add Task: {} ", input_buffer);
                let input_paragraph = Paragraph::new(input_text)
                    .block(Block::default().borders(Borders::ALL).title("Input Mode (Enter to save, Esc to cancel)"))
                    .style(Style::default().fg(Color::White));
                f.render_widget(input_paragraph, main_layout[1]);
            }
        })?;

        if event::poll(std::time::Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                match current_mode {
                    Mode::View => {
                        if key.code == KeyCode::Char('q') {
                            break;
                        } else if key.code == KeyCode::Enter {
                            current_mode = Mode::Input;
                            input_buffer.clear();
                        }
                    }
                    Mode::Input => {
                        match key.code {
                            KeyCode::Enter => {
                                if !input_buffer.trim().is_empty() {
                                    add_task(&input_buffer, None)?;
                                }
                                current_mode = Mode::View;
                                input_buffer.clear();
                            }
                            KeyCode::Esc => {
                                current_mode = Mode::View;
                                input_buffer.clear();
                            }
                            KeyCode::Char(c) => {
                                input_buffer.push(c);
                            }
                            KeyCode::Backspace => {
                                input_buffer.pop();
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;

    Ok(())
}

fn init_tasks() -> Result<()> {
    let tasks_dir = Path::new("tasks");
    if !tasks_dir.exists() {
        fs::create_dir(tasks_dir).context("Failed to create tasks directory")?;
        println!("Created directory: tasks/");
    }

    let files = [
        ("todo.md", "# To Do Tasks\n"),
        ("doing.md", "# In Progress\n"),
        ("done.md", "# Completed Tasks\n"),
    ];

    for (filename, content) in files {
        let path = tasks_dir.join(filename);
        if !path.exists() {
            let mut file = fs::File::create(&path).context(format!("Failed to create file {:?}", path))?;
            file.write_all(content.as_bytes()).context(format!("Failed to write to file {:?}", path))?;
            println!("Created file: {:?}", path);
        } else {
            println!("File already exists: {:?}", path);
        }
    }

    println!("Initialization complete.");
    Ok(())
}