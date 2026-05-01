use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use ratatui::layout::{Alignment, Position};
use std::fs;
use std::io::{self, Write, stdout};
use std::path::Path;

use crossterm::{
    ExecutableCommand,
    event::{self, Event, KeyCode, KeyModifiers},
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, ListItem, ListState, Paragraph},
};

const TASK_FILES: [(&str, &str); 3] = [
    ("todo.md", "# To Do Tasks\n"),
    ("doing.md", "# Doing Tasks\n"),
    ("done.md", "# Done Tasks\n"),
];

#[derive(Parser)]
#[command(name = "lls-cli-task")]
#[command(about = "A simple file-system-backed task management system", long_about = None)]
struct Cli {
    /// Force use of current directory instead of git root
    #[arg(long, default_value_t = false)]
    local: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Initializes the tasks directory and markdown files
    Init,
    /// Adds a new task to the todo list
    Add {
        /// The description of the task, optionally followed by tag-like metadata
        #[arg(required = true, num_args = 1.., trailing_var_arg = true)]
        task: Vec<String>,
    },
    /// Changes the status of a task
    Status {
        /// The source status (e.g., "todo")
        from: String,
        /// The index of the task to move
        task_index: String,
        /// The destination status (e.g., "doing")
        to: String,
    },
    /// Marks a task as done
    Done {
        /// The status the task is currently in (todo, doing)
        status: String,
        /// The index of the task to mark as done
        task_index: String,
    },
    /// Deletes a task
    Delete {
        /// The status the task is currently in (todo, doing, done)
        status: String,
        /// The index of the task to delete
        task_index: String,
    },
    /// Lists tasks. Optional status to filter by (todo, doing, done)
    List { status: Option<String> },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let root = get_task_root(cli.local)?;
    let cwd = std::env::current_dir()?;

    if root != cwd {
        println!("Using tasks at: {:?}", root);
    }

    match cli.command {
        Some(Commands::Init) => {
            init_tasks(&root)?;
        }
        Some(Commands::Add { task }) => {
            let (description, metadata) = parse_add_task_args(task)?;
            let msg = add_task(&root, &description, metadata)?;
            println!("{}", msg);
        }
        Some(Commands::Status {
            from,
            task_index,
            to,
        }) => {
            move_task(&root, &from, &to, &task_index)?;
        }
        Some(Commands::Done { status, task_index }) => {
            if status == "done" {
                println!("Task is already done.");
            } else {
                move_task(&root, &status, "done", &task_index)?;
                println!("Task {} from {} marked as done.", task_index, status);
            }
        }
        Some(Commands::Delete { status, task_index }) => {
            delete_task(&root, &status, &task_index)?;
            println!("Task {} from {} deleted successfully.", task_index, status);
        }
        Some(Commands::List { status }) => {
            list_tasks(&root, status)?;
        }
        None => {
            if !is_initialized(&root) {
                print!("Tasks not initialized. Would you like to initialize now? (y/n): ");
                io::stdout().flush()?;

                let mut response = String::new();
                io::stdin().read_line(&mut response)?;

                if response.trim().to_lowercase() == "y" {
                    init_tasks(&root)?;
                } else {
                    println!(
                        "Initialization skipped. Please run 'init' to set up your task lists."
                    );
                    return Ok(());
                }
            }
            tui_view(&root)?;
        }
    }

    Ok(())
}

fn get_task_root(local: bool) -> Result<std::path::PathBuf> {
    if local {
        return Ok(std::env::current_dir()?);
    }

    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let path_str = String::from_utf8_lossy(&out.stdout).trim().to_string();
            Ok(Path::new(&path_str).to_path_buf())
        }
        _ => Ok(std::env::current_dir()?),
    }
}

fn get_tasks_dir(root: &Path) -> std::path::PathBuf {
    root.join("tasks")
}

fn is_initialized(root: &Path) -> bool {
    let tasks_dir = get_tasks_dir(root);
    if !tasks_dir.exists() {
        return false;
    }
    let files = ["todo.md", "doing.md", "done.md"];
    files.iter().all(|f| tasks_dir.join(f).exists())
}

fn get_file_path(root: &Path, status: &str) -> Result<std::path::PathBuf> {
    let tasks_dir = get_tasks_dir(root);
    let filename = match status {
        "todo" => "todo.md",
        "doing" => "doing.md",
        "done" => "done.md",
        _ => anyhow::bail!("Invalid status. Use 'todo', 'doing', or 'done'."),
    };
    ensure_task_store(root)?;
    Ok(tasks_dir.join(filename))
}

fn ensure_task_store(root: &Path) -> Result<()> {
    let tasks_dir = get_tasks_dir(root);
    fs::create_dir_all(&tasks_dir).context("Failed to create tasks directory")?;

    for (filename, content) in TASK_FILES {
        let path = tasks_dir.join(filename);
        if !path.exists() {
            fs::write(&path, content).context(format!("Failed to create file {:?}", path))?;
        }
    }

    Ok(())
}

// find_task_status is no longer needed for index-based referencing
// as the user must specify the source list.

fn delete_task(root: &Path, status: &str, task_index_str: &str) -> Result<()> {
    let path = get_file_path(root, status)?;
    let task_index = task_index_str
        .parse::<usize>()
        .context("Invalid task index. Please provide a number.")?;
    if task_index == 0 {
        anyhow::bail!("Task index must be 1 or greater.");
    }

    let content = fs::read_to_string(&path).context("Failed to read file")?;
    let lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();

    let task_lines: Vec<(usize, &String)> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.starts_with("- "))
        .collect();

    if task_index > task_lines.len() {
        anyhow::bail!(
            "Task index {} out of range. Only {} tasks found in {}.",
            task_index,
            task_lines.len(),
            status
        );
    }

    let (actual_line_idx, _) = task_lines[task_index - 1];
    let mut new_lines = lines.clone();
    new_lines.remove(actual_line_idx);

    let updated_content = new_lines.join("\n");
    let final_content = if updated_content.is_empty() {
        updated_content
    } else {
        format!("{}\n", updated_content)
    };

    fs::write(&path, final_content).context("Failed to update file")?;
    Ok(())
}

fn move_task(root: &Path, from: &str, to: &str, task_index_str: &str) -> Result<()> {
    let src_path = get_file_path(root, from)?;
    let dest_path = get_file_path(root, to)?;

    let task_index = task_index_str
        .parse::<usize>()
        .context("Invalid task index. Please provide a number.")?;
    if task_index == 0 {
        anyhow::bail!("Task index must be 1 or greater.");
    }

    let content = fs::read_to_string(&src_path).context("Failed to read source file")?;
    let lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();

    // Filter for task lines only to find the Nth task
    let task_lines: Vec<(usize, &String)> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.starts_with("- "))
        .collect();

    if task_index > task_lines.len() {
        anyhow::bail!(
            "Task index {} out of range. Only {} tasks found in {}.",
            task_index,
            task_lines.len(),
            from
        );
    }

    let (actual_line_idx, task_line) = task_lines[task_index - 1];
    let task_line_content = task_line.clone();

    // Remove from source
    let mut new_src_lines = lines.clone();
    new_src_lines.remove(actual_line_idx);
    let updated_src_content = new_src_lines.join("\n");
    // Add trailing newline if the file wasn't empty
    let final_src_content = if updated_src_content.is_empty() {
        updated_src_content
    } else {
        format!("{}\n", updated_src_content)
    };
    fs::write(&src_path, final_src_content).context("Failed to update source file")?;

    // Ensure the destination file ends with a newline before appending to prevent line merging
    let mut dest_content =
        fs::read_to_string(&dest_path).context("Failed to read destination file")?;
    if !dest_content.is_empty() && !dest_content.ends_with('\n') {
        dest_content.push('\n');
    }
    dest_content.push_str(&task_line_content);
    // task_line_content already includes \n from the add_task function or the original file
    fs::write(&dest_path, dest_content).context("Failed to update destination file")?;

    Ok(())
}

fn update_task(root: &Path, status: &str, task_index: usize, new_description: &str) -> Result<()> {
    let path = get_file_path(root, status)?;
    let content = fs::read_to_string(&path).context("Failed to read file")?;
    let lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();

    let mut task_count = 0;
    let mut updated_lines = Vec::new();

    for line in lines {
        if line.starts_with("- ") {
            task_count += 1;
            if task_count == task_index {
                updated_lines.push(format!("- {}", new_description));
            } else {
                updated_lines.push(line);
            }
        } else {
            updated_lines.push(line);
        }
    }

    if task_count < task_index {
        anyhow::bail!("Task index {} out of range", task_index);
    }

    fs::write(&path, updated_lines.join("\n") + "\n").context("Failed to write file")?;
    Ok(())
}

fn reorder_task(root: &Path, status: &str, from_idx: usize, to_idx: usize) -> Result<()> {
    let path = get_file_path(root, status)?;
    let content = fs::read_to_string(&path).context("Failed to read file")?;
    let lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();

    let task_indices: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.starts_with("- "))
        .map(|(i, _)| i)
        .collect();

    if from_idx >= task_indices.len() {
        anyhow::bail!("Task index out of range");
    }

    let actual_from_idx = task_indices[from_idx];
    let task_line = lines[actual_from_idx].clone();

    let mut new_lines = lines.clone();
    new_lines.remove(actual_from_idx);

    // Find the new position for the task line
    let new_task_indices: Vec<usize> = new_lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.starts_with("- "))
        .map(|(i, _)| i)
        .collect();

    let insert_at_idx = if to_idx < new_task_indices.len() {
        new_task_indices[to_idx]
    } else {
        new_lines.len()
    };

    new_lines.insert(insert_at_idx, task_line);
    fs::write(&path, new_lines.join("\n") + "\n").context("Failed to write file")?;
    Ok(())
}

fn list_tasks(root: &Path, filter_status: Option<String>) -> Result<()> {
    if let Some(ref s) = filter_status {
        let status = match s.as_str() {
            "1" => "todo",
            "2" => "doing",
            "3" => "done",
            _ => s.as_str(),
        };

        let path = get_file_path(root, status)?;
        println!("\n--- {} ---", status.to_uppercase());
        let content = fs::read_to_string(&path).context(format!("Failed to read {:?}", path))?;
        let mut index = 1;
        for line in content.lines() {
            if line.starts_with("- ") {
                println!("{}. {}", index, &line[2..]);
                index += 1;
            }
        }
    } else {
        let statuses = ["todo", "doing", "done"];
        for status in statuses {
            let path = get_file_path(root, status)?;
            println!("\n--- {} ---", status.to_uppercase());
            let content =
                fs::read_to_string(&path).context(format!("Failed to read {:?}", path))?;
            let mut index = 1;
            for line in content.lines() {
                if line.starts_with("- ") {
                    println!("{}. {}", index, &line[2..]);
                    index += 1;
                }
            }
        }
    }
    Ok(())
}

fn parse_add_task_args(args: Vec<String>) -> Result<(String, Option<String>)> {
    if args.is_empty() {
        anyhow::bail!("Task description cannot be empty.");
    }

    let mut args = args;
    let metadata = if args.len() > 1 && looks_like_metadata(args.last().unwrap()) {
        args.pop()
    } else {
        None
    };
    let description = args.join(" ");

    if description.trim().is_empty() {
        anyhow::bail!("Task description cannot be empty.");
    }

    Ok((description, metadata))
}

fn looks_like_metadata(value: &str) -> bool {
    value.contains(',')
        || (value.chars().any(|c| c.is_ascii_alphabetic())
            && value
                .chars()
                .all(|c| !c.is_ascii_lowercase() && !matches!(c, '"' | '\'')))
}

fn add_task(root: &Path, description: &str, metadata: Option<String>) -> Result<String> {
    insert_task(root, "todo", None, description, metadata)
        .map(|_| "Task added successfully.".to_string())
}

fn insert_task(
    root: &Path,
    status: &str,
    index: Option<usize>,
    description: &str,
    metadata: Option<String>,
) -> Result<()> {
    let path = get_file_path(root, status)?;

    let metadata_str = match metadata {
        Some(m) => format!(" ({})", m),
        None => "".to_string(),
    };

    let task_line = format!("- {}{}", description, metadata_str);
    let content = fs::read_to_string(&path).context("Failed to read file")?;
    let mut lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();

    if let Some(idx) = index {
        // Find the actual line index for the Nth task
        let task_lines: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, line)| line.starts_with("- "))
            .map(|(i, _)| i)
            .collect();

        if idx < task_lines.len() {
            let actual_idx = task_lines[idx] + 1;
            lines.insert(actual_idx, task_line);
        } else {
            lines.push(task_line);
        }
    } else {
        lines.push(task_line);
    }

    let updated_content = lines.join("\n");
    let final_content = if updated_content.is_empty() {
        updated_content
    } else {
        format!("{}\n", updated_content)
    };

    fs::write(&path, final_content).context("Failed to write task to file")?;
    Ok(())
}

fn read_tasks(root: &Path, status: &str) -> Result<Vec<String>> {
    let path = get_file_path(root, status)?;
    let content = fs::read_to_string(&path).context(format!("Failed to read {:?}", path))?;
    let tasks = content
        .lines()
        .filter(|l| l.starts_with("- "))
        .map(|l| l.to_string())
        .collect();
    Ok(tasks)
}

fn select_first_task_if_present(root: &Path, status: &str, state: &mut ListState) {
    let has_tasks = read_tasks(root, status)
        .map(|tasks| !tasks.is_empty())
        .unwrap_or(false);

    state.select(if has_tasks { Some(0) } else { None });
}

enum Mode {
    View,
    Input,
    Edit,
    Help,
}

struct TerminalSession;

impl TerminalSession {
    fn enter() -> Result<Self> {
        enable_raw_mode()?;
        if let Err(err) = stdout().execute(EnterAlternateScreen) {
            let _ = disable_raw_mode();
            return Err(err.into());
        }

        Ok(Self)
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = stdout().execute(LeaveAlternateScreen);
    }
}

fn wrap_text(text: &str, width: usize) -> String {
    if width == 0 {
        return text.to_string();
    }
    let mut result = String::new();
    let mut current_line = String::new();

    for word in text.split_whitespace() {
        // Handle words longer than the width by breaking them
        let mut word_to_add = word;
        while word_to_add.len() > width {
            if !current_line.is_empty() {
                result.push_str(&current_line);
                result.push('\n');
                current_line.clear();
            }
            let (head, tail) = word_to_add.split_at(width);
            result.push_str(head);
            result.push('\n');
            word_to_add = tail;
        }

        if current_line.is_empty() {
            current_line.push_str(word_to_add);
        } else if current_line.len() + 1 + word_to_add.len() <= width {
            current_line.push(' ');
            current_line.push_str(word_to_add);
        } else {
            result.push_str(&current_line);
            result.push('\n');
            current_line.clear();
            current_line.push_str(word_to_add);
        }
    }
    result.push_str(&current_line);
    result
}

fn input_cursor_offset(wrapped_input: &str, width: usize) -> (u16, u16) {
    if width == 0 {
        return (0, 0);
    }

    let row = wrapped_input.lines().count().saturating_sub(1);
    let col = wrapped_input
        .lines()
        .last()
        .map(|line| line.len())
        .unwrap_or(0);

    if col >= width {
        (0, (row + 1) as u16)
    } else {
        (col as u16, row as u16)
    }
}

fn tui_view(root: &Path) -> Result<()> {
    // Setup terminal
    let _terminal_session = TerminalSession::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;

    let mut current_mode = Mode::View;
    let mut input_buffer = String::new();
    let mut feedback_buffer = String::from(
        "Kanban View! Spacebar to create new Task. Arrows to navigate/focus boards, Shift+Arrows or I/K to reorder, Shift+Arrows or J/L to move tasks, Numbers to reorder, Space to add, Enter to edit, 'd' or Delete to delete, 'q' to quit.",
    );

    let mut selected_board = 0; // 0: todo, 1: doing, 2: done
    let mut editing_task_idx: Option<usize> = None;
    let mut board_states = [
        ListState::default(),
        ListState::default(),
        ListState::default(),
    ];

    let statuses = ["todo", "doing", "done"];
    let titles = ["To Do", "Doing", "Done"];
    // let c_1 = Color::LightCyan;
    // let c_2 = Color::LightGreen;
    // let c_3 = Color::LightMagenta;
    let c_1 = Color::Indexed(110);
    let c_2 = Color::Indexed(108);
    let c_3 = Color::Indexed(139);
    let text_color = Color::Indexed(248); //Color::DarkGray;
    let c_highlight = Color::Indexed(221);
    let colors = [c_1, c_2, c_3];

    loop {
        terminal.draw(|f| {
            let size = f.area();

            // Calculate input height if in Input or Edit mode
            let input_height =
                if matches!(current_mode, Mode::Input) || matches!(current_mode, Mode::Edit) {
                    let label = if matches!(current_mode, Mode::Input) {
                        " Add Task: "
                    } else {
                        " Edit Task: "
                    };
                    let full_text = format!("{}{}", label, input_buffer);
                    // Subtract 2 for the borders of the block
                    let available_width = size.width.saturating_sub(2) as usize;
                    let wrapped = wrap_text(&full_text, available_width);
                    let lines = wrapped.lines().count();
                    let cursor_row = input_cursor_offset(&wrapped, available_width).1 as usize;
                    // Height = content rows + 2 (for top and bottom borders)
                    (lines.max(cursor_row + 1) + 2).max(3) as u16
                } else {
                    0
                };

            // Main layout: Kanban board, input area (if active), and feedback console
            let main_layout = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(0),
                    Constraint::Length(input_height),
                    Constraint::Length(3),
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

            for (i, status) in statuses.iter().enumerate() {
                let selected_idx = board_states[i].selected();
                let col_width = (size.width / 3) as usize;
                let tasks = read_tasks(root, status).unwrap_or_default();
                let _items: Vec<ListItem> = tasks
                    .clone()
                    .into_iter()
                    .enumerate()
                    .map(|(idx, t)| {
                        let cleaned = t.replace("- ", "");

                        let (desc, meta) = if let Some(start) = cleaned.rfind(" (") {
                            if cleaned.ends_with(')') {
                                (
                                    &cleaned[..start],
                                    Some(&cleaned[start + 2..cleaned.len() - 1]),
                                )
                            } else {
                                (&cleaned[..], None)
                            }
                        } else {
                            (&cleaned[..], None)
                        };

                        let mut line = Line::from(vec![
                            Span::raw(format!("{}. ", idx + 1)),
                            Span::raw(if Some(idx) == selected_idx {
                                wrap_text(desc, col_width.saturating_sub(5))
                            } else {
                                desc.to_string()
                            }),
                        ]);

                        if let Some(m) = meta {
                            line.spans.push(
                                Span::raw(format!(" ({})", m))
                                    .style(Style::default().bg(Color::DarkGray).fg(Color::White)),
                            );
                        }

                        ListItem::new(line)
                    })
                    .collect();

                let highlight_style = if matches!(current_mode, Mode::View) {
                    Style::default().fg(Color::Black).bg(c_highlight)
                } else {
                    // Use a more subtle highlight when in Input/Edit mode
                    Style::default().fg(Color::White).bg(Color::DarkGray)
                };

                let block = Block::default()
                    .title(format!(
                        "{} {}",
                        titles[i],
                        if selected_board == i {
                            "  <<<<<< * >>>>>>     "
                        } else {
                            ""
                        }
                    ))
                    .title(
                        Line::from(vec![Span::raw(format!(" {} ", tasks.len()))])
                            .alignment(Alignment::Right),
                    )
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(colors[i]));

                let inner_area = block.inner(chunks[i]);

                let mut current_y = 0;
                for (idx, t) in tasks.iter().enumerate() {
                    let cleaned = t.replace("- ", "");
                    let is_selected = Some(idx) == selected_idx;

                    let (desc, _meta) = if let Some(start) = cleaned.rfind(" (") {
                        if cleaned.ends_with(')') {
                            (
                                &cleaned[..start],
                                Some(&cleaned[start + 2..cleaned.len() - 1]),
                            )
                        } else {
                            (&cleaned[..], None)
                        }
                    } else {
                        (&cleaned[..], None)
                    };

                    let text = if is_selected {
                        wrap_text(desc, col_width.saturating_sub(5))
                    } else {
                        desc.to_string()
                    };

                    let style = if is_selected {
                        highlight_style
                    } else {
                        Style::default().fg(text_color)
                    };

                    let content = format!("{}. {}", idx + 1, text);
                    let _paragraph = Paragraph::new(content).style(style);

                    let _area = ratatui::layout::Rect {
                        x: inner_area.x,
                        y: inner_area.y + current_y as u16,
                        width: inner_area.width,
                        height: 1, // This is a simplification; we should calculate height based on wrap_text
                    };

                    // To actually support multi-line expansion in a manual loop,
                    // we need to render the wrapped text as a Paragraph and increment current_y
                    // by the number of lines it actually takes.

                    let wrapped_content = if is_selected {
                        wrap_text(desc, col_width.saturating_sub(5))
                    } else {
                        desc.to_string()
                    };

                    let line_count = wrapped_content.lines().count();
                    let item_area = ratatui::layout::Rect {
                        x: inner_area.x,
                        y: inner_area.y + current_y as u16,
                        width: inner_area.width,
                        height: line_count as u16,
                    };

                    let item_text = format!("{}. {}", idx + 1, wrapped_content);
                    f.render_widget(Paragraph::new(item_text).style(style), item_area);

                    current_y += line_count;
                    if inner_area.y + current_y as u16 >= chunks[i].height {
                        break;
                    }
                }
                f.render_widget(block, chunks[i]);
            }

            if matches!(current_mode, Mode::Input) || matches!(current_mode, Mode::Edit) {
                let label = if matches!(current_mode, Mode::Input) {
                    " Add Task: "
                } else {
                    " Edit Task: "
                };
                let input_text = format!("{}{}", label, input_buffer);
                // Subtract 2 for the borders of the block
                let available_width = size.width.saturating_sub(2) as usize;
                let wrapped_input = wrap_text(&input_text, available_width);
                let input_paragraph = Paragraph::new(wrapped_input.as_str())
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title("Input Mode (Enter to save, Esc to cancel)"),
                    )
                    .style(Style::default().fg(Color::White));
                f.render_widget(input_paragraph, main_layout[1]);

                let (cursor_x, cursor_y) = input_cursor_offset(&wrapped_input, available_width);
                let input_inner = main_layout[1].inner(ratatui::layout::Margin {
                    horizontal: 1,
                    vertical: 1,
                });
                f.set_cursor_position(Position::new(
                    input_inner.x + cursor_x.min(input_inner.width.saturating_sub(1)),
                    input_inner.y + cursor_y.min(input_inner.height.saturating_sub(1)),
                ));
            }

            let feedback_paragraph = Paragraph::new(feedback_buffer.as_str())
                .block(Block::default().borders(Borders::ALL).title("Console"))
                .style(Style::default().fg(Color::Gray));

            // The feedback area is always the last element of main_layout
            let feedback_area = *main_layout.last().unwrap();
            f.render_widget(feedback_paragraph, feedback_area);

            if matches!(current_mode, Mode::Help) {
                let help_text = "TUI Commands:\n\n\
                                 [Space]  - Create new task\n\
                                 [Enter]  - Edit selected task / Save input\n\
                                 [d/Del]  - Delete selected task\n\
                                 [Arrows] - Navigate boards and tasks\n\
                                 [Shift+Arrows] - Reorder/Move tasks\n\
                                 [I, K]   - Move task Up/Down\n\
                                 [J, L]   - Move task Left/Right\n\
                                 [1, 2, 3]- Switch board focus\n\
                                 [h / ?]  - Toggle Help\n\
                                 [q]      - Quit";

                let area = f.area();
                let popover_width = 50;
                let popover_height = 15;
                let x = (area.width as isize - popover_width as isize) / 2;
                let y = (area.height as isize - popover_height as isize) / 2;

                let popover_area = ratatui::layout::Rect {
                    x: x as u16,
                    y: y as u16,
                    width: popover_width as u16,
                    height: popover_height as u16,
                };

                let help_paragraph = Paragraph::new(help_text)
                    .block(
                        Block::default()
                            .title(" Help ")
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(Color::Yellow)),
                    )
                    .style(Style::default().fg(Color::White));

                f.render_widget(help_paragraph, popover_area);
            }
        })?;

        if event::poll(std::time::Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                match current_mode {
                    Mode::View => {
                        if key.modifiers.contains(KeyModifiers::SHIFT) {
                            match key.code {
                                KeyCode::Up => {
                                    let state = &mut board_states[selected_board];
                                    if let Some(idx) = state.selected() {
                                        if idx > 0 {
                                            match reorder_task(
                                                root,
                                                statuses[selected_board],
                                                idx,
                                                idx - 1,
                                            ) {
                                                Ok(_) => {
                                                    feedback_buffer =
                                                        format!("Moved task up to position {}", idx)
                                                }
                                                Err(e) => feedback_buffer = format!("Error: {}", e),
                                            }
                                            state.select(Some(idx - 1));
                                        } else {
                                            feedback_buffer = "Already at the top".to_string();
                                        }
                                    }
                                }
                                KeyCode::Down => {
                                    let state = &mut board_states[selected_board];
                                    if let Some(idx) = state.selected() {
                                        let tasks = read_tasks(root, statuses[selected_board])
                                            .unwrap_or_default();
                                        if tasks.is_empty() {
                                            state.select(None);
                                            feedback_buffer = "No task selected".to_string();
                                        } else if idx < tasks.len() - 1 {
                                            match reorder_task(
                                                root,
                                                statuses[selected_board],
                                                idx,
                                                idx + 1,
                                            ) {
                                                Ok(_) => {
                                                    feedback_buffer = format!(
                                                        "Moved task down to position {}",
                                                        idx + 2
                                                    )
                                                }
                                                Err(e) => feedback_buffer = format!("Error: {}", e),
                                            }
                                            state.select(Some(idx + 1));
                                        } else {
                                            feedback_buffer = "Already at the bottom".to_string();
                                        }
                                    }
                                }
                                KeyCode::Left => {
                                    let state = &mut board_states[selected_board];
                                    if let Some(idx) = state.selected() {
                                        if selected_board > 0 {
                                            let from = statuses[selected_board];
                                            let to = statuses[selected_board - 1];
                                            match move_task(
                                                root,
                                                &from,
                                                &to,
                                                &(idx + 1).to_string(),
                                            ) {
                                                Ok(_) => {
                                                    feedback_buffer =
                                                        format!("Moved task to {}", to)
                                                }
                                                Err(e) => feedback_buffer = format!("Error: {}", e),
                                            }
                                        } else {
                                            feedback_buffer =
                                                "Already at the first board".to_string();
                                        }
                                    }
                                }
                                KeyCode::Right => {
                                    let state = &mut board_states[selected_board];
                                    if let Some(idx) = state.selected() {
                                        if selected_board < 2 {
                                            let from = statuses[selected_board];
                                            let to = statuses[selected_board + 1];
                                            match move_task(
                                                root,
                                                &from,
                                                &to,
                                                &(idx + 1).to_string(),
                                            ) {
                                                Ok(_) => {
                                                    feedback_buffer =
                                                        format!("Moved task to {}", to)
                                                }
                                                Err(e) => feedback_buffer = format!("Error: {}", e),
                                            }
                                        } else {
                                            feedback_buffer =
                                                "Already at the last board".to_string();
                                        }
                                    }
                                }
                                _ => {}
                            }
                        } else if key.modifiers.contains(KeyModifiers::CONTROL)
                            || key.modifiers.contains(KeyModifiers::ALT)
                        {
                            // Alt/Ctrl modifiers no longer used for moving tasks
                            _ = ();
                        } else {
                            match key.code {
                                KeyCode::Esc => {
                                    let state = &mut board_states[selected_board];
                                    state.select(None);
                                    feedback_buffer = "Task unselected".to_string();
                                }
                                KeyCode::Char('q') => break,
                                KeyCode::Enter => {
                                    let state = &board_states[selected_board];
                                    if let Some(idx) = state.selected() {
                                        current_mode = Mode::Edit;
                                        editing_task_idx = Some(idx + 1);
                                        let tasks = read_tasks(root, statuses[selected_board])
                                            .unwrap_or_default();
                                        input_buffer = tasks[idx].replace("- ", "");
                                    } else {
                                        current_mode = Mode::Input;
                                        input_buffer.clear();
                                    }
                                }
                                KeyCode::Char(' ') => {
                                    current_mode = Mode::Input;
                                    input_buffer.clear();
                                }
                                KeyCode::Char('1') => {
                                    selected_board = 0;
                                    for state in board_states.iter_mut() {
                                        state.select(None);
                                    }
                                    select_first_task_if_present(
                                        root,
                                        statuses[selected_board],
                                        &mut board_states[selected_board],
                                    );
                                }
                                KeyCode::Char('2') => {
                                    selected_board = 1;
                                    for state in board_states.iter_mut() {
                                        state.select(None);
                                    }
                                    select_first_task_if_present(
                                        root,
                                        statuses[selected_board],
                                        &mut board_states[selected_board],
                                    );
                                }
                                KeyCode::Char('3') => {
                                    selected_board = 2;
                                    for state in board_states.iter_mut() {
                                        state.select(None);
                                    }
                                    select_first_task_if_present(
                                        root,
                                        statuses[selected_board],
                                        &mut board_states[selected_board],
                                    );
                                }
                                KeyCode::Char('i') | KeyCode::Char('I') => {
                                    let state = &mut board_states[selected_board];
                                    if let Some(idx) = state.selected() {
                                        if idx > 0 {
                                            match reorder_task(
                                                root,
                                                statuses[selected_board],
                                                idx,
                                                idx - 1,
                                            ) {
                                                Ok(_) => {
                                                    feedback_buffer =
                                                        format!("Moved task up to position {}", idx)
                                                }
                                                Err(e) => feedback_buffer = format!("Error: {}", e),
                                            }
                                            state.select(Some(idx - 1));
                                        } else {
                                            feedback_buffer = "Already at the top".to_string();
                                        }
                                    }
                                }
                                KeyCode::Char('k') | KeyCode::Char('K') => {
                                    let state = &mut board_states[selected_board];
                                    if let Some(idx) = state.selected() {
                                        let tasks = read_tasks(root, statuses[selected_board])
                                            .unwrap_or_default();
                                        if tasks.is_empty() {
                                            state.select(None);
                                            feedback_buffer = "No task selected".to_string();
                                        } else if idx < tasks.len() - 1 {
                                            match reorder_task(
                                                root,
                                                statuses[selected_board],
                                                idx,
                                                idx + 1,
                                            ) {
                                                Ok(_) => {
                                                    feedback_buffer = format!(
                                                        "Moved task down to position {}",
                                                        idx + 2
                                                    )
                                                }
                                                Err(e) => feedback_buffer = format!("Error: {}", e),
                                            }
                                            state.select(Some(idx + 1));
                                        } else {
                                            feedback_buffer = "Already at the bottom".to_string();
                                        }
                                    }
                                }
                                KeyCode::Char('j') | KeyCode::Char('J') => {
                                    let state = &mut board_states[selected_board];
                                    if let Some(idx) = state.selected() {
                                        if selected_board > 0 {
                                            let from = statuses[selected_board];
                                            let to = statuses[selected_board - 1];
                                            match move_task(
                                                root,
                                                &from,
                                                &to,
                                                &(idx + 1).to_string(),
                                            ) {
                                                Ok(_) => {
                                                    feedback_buffer =
                                                        format!("Moved task to {}", to)
                                                }
                                                Err(e) => feedback_buffer = format!("Error: {}", e),
                                            }
                                        } else {
                                            feedback_buffer =
                                                "Already at the first board".to_string();
                                        }
                                    }
                                }
                                KeyCode::Char('l') | KeyCode::Char('L') => {
                                    let state = &mut board_states[selected_board];
                                    if let Some(idx) = state.selected() {
                                        if selected_board < 2 {
                                            let from = statuses[selected_board];
                                            let to = statuses[selected_board + 1];
                                            match move_task(
                                                root,
                                                &from,
                                                &to,
                                                &(idx + 1).to_string(),
                                            ) {
                                                Ok(_) => {
                                                    feedback_buffer =
                                                        format!("Moved task to {}", to)
                                                }
                                                Err(e) => feedback_buffer = format!("Error: {}", e),
                                            }
                                        } else {
                                            feedback_buffer =
                                                "Already at the last board".to_string();
                                        }
                                    }
                                }
                                KeyCode::Char('d') | KeyCode::Char('D') | KeyCode::Delete => {
                                    let state = &mut board_states[selected_board];
                                    if let Some(idx) = state.selected() {
                                        let status = statuses[selected_board];
                                        match delete_task(root, status, &(idx + 1).to_string()) {
                                            Ok(_) => {
                                                feedback_buffer = format!(
                                                    "Deleted task {} from {}",
                                                    idx + 1,
                                                    status
                                                );
                                                state.select(if idx > 0 {
                                                    Some(idx - 1)
                                                } else {
                                                    None
                                                });
                                            }
                                            Err(e) => feedback_buffer = format!("Error: {}", e),
                                        }
                                    } else {
                                        feedback_buffer = "No task selected to delete".to_string();
                                    }
                                }
                                KeyCode::Char('h') | KeyCode::Char('H') | KeyCode::Char('?') => {
                                    current_mode = Mode::Help;
                                }
                                KeyCode::Up => {
                                    let state = &mut board_states[selected_board];
                                    let tasks = read_tasks(root, statuses[selected_board])
                                        .unwrap_or_default();
                                    if !tasks.is_empty() {
                                        let i = state.selected().unwrap_or(0);
                                        if i > 0 {
                                            state.select(Some(i - 1));
                                        } else {
                                            state.select(Some(tasks.len() - 1));
                                        }
                                    }
                                }
                                KeyCode::Down => {
                                    let state = &mut board_states[selected_board];
                                    let tasks = read_tasks(root, statuses[selected_board])
                                        .unwrap_or_default();
                                    if !tasks.is_empty() {
                                        let i = state.selected().unwrap_or(0);
                                        if i < tasks.len() - 1 {
                                            state.select(Some(i + 1));
                                        } else {
                                            state.select(Some(0));
                                        }
                                    }
                                }
                                KeyCode::Left => {
                                    if selected_board > 0 {
                                        selected_board -= 1;
                                    } else {
                                        selected_board = 2;
                                    }
                                    for state in board_states.iter_mut() {
                                        state.select(None);
                                    }
                                    select_first_task_if_present(
                                        root,
                                        statuses[selected_board],
                                        &mut board_states[selected_board],
                                    );
                                }
                                KeyCode::Right => {
                                    if selected_board < 2 {
                                        selected_board += 1;
                                    } else {
                                        selected_board = 0;
                                    }
                                    for state in board_states.iter_mut() {
                                        state.select(None);
                                    }
                                    select_first_task_if_present(
                                        root,
                                        statuses[selected_board],
                                        &mut board_states[selected_board],
                                    );
                                }
                                KeyCode::Char(c) if c.is_ascii_digit() => {
                                    let new_pos = (c as u8 - b'0') as usize;
                                    let state = &mut board_states[selected_board];
                                    if let Some(idx) = state.selected() {
                                        if new_pos > 0 {
                                            match reorder_task(
                                                root,
                                                statuses[selected_board],
                                                idx,
                                                new_pos - 1,
                                            ) {
                                                Ok(_) => {
                                                    feedback_buffer = format!(
                                                        "Reordered task to position {}",
                                                        new_pos
                                                    )
                                                }
                                                Err(e) => feedback_buffer = format!("Error: {}", e),
                                            }
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    Mode::Help => match key.code {
                        KeyCode::Enter
                        | KeyCode::Esc
                        | KeyCode::Char('h')
                        | KeyCode::Char('H')
                        | KeyCode::Char('?') => {
                            current_mode = Mode::View;
                        }
                        _ => {}
                    },
                    Mode::Input => match key.code {
                        KeyCode::Enter => {
                            if !input_buffer.trim().is_empty() {
                                let state = &board_states[selected_board];
                                let index = state.selected();
                                match insert_task(
                                    root,
                                    statuses[selected_board],
                                    index,
                                    &input_buffer,
                                    None,
                                ) {
                                    Ok(_) => {
                                        feedback_buffer = "Task added successfully.".to_string()
                                    }
                                    Err(e) => feedback_buffer = format!("Error: {}", e),
                                }
                            } else {
                                feedback_buffer = "Task description cannot be empty.".to_string();
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
                    },
                    Mode::Edit => match key.code {
                        KeyCode::Enter => {
                            if !input_buffer.trim().is_empty() {
                                if let Some(idx) = editing_task_idx {
                                    match update_task(
                                        root,
                                        statuses[selected_board],
                                        idx,
                                        &input_buffer,
                                    ) {
                                        Ok(_) => {
                                            feedback_buffer =
                                                format!("Task {} updated successfully.", idx)
                                        }
                                        Err(e) => feedback_buffer = format!("Error: {}", e),
                                    }
                                }
                            } else {
                                feedback_buffer = "Task description cannot be empty.".to_string();
                            }
                            current_mode = Mode::View;
                            input_buffer.clear();
                            editing_task_idx = None;
                        }
                        KeyCode::Esc => {
                            current_mode = Mode::View;
                            input_buffer.clear();
                            editing_task_idx = None;
                        }
                        KeyCode::Char(c) => {
                            input_buffer.push(c);
                        }
                        KeyCode::Backspace => {
                            input_buffer.pop();
                        }
                        _ => {}
                    },
                }
            }
        }
    }

    Ok(())
}

fn init_tasks(root: &Path) -> Result<()> {
    let tasks_dir = get_tasks_dir(root);
    if !tasks_dir.exists() {
        fs::create_dir_all(&tasks_dir).context("Failed to create tasks directory")?;
        println!("Created directory: {:?}", tasks_dir);
    }

    for (filename, content) in TASK_FILES {
        let path = tasks_dir.join(filename);
        if !path.exists() {
            let mut file =
                fs::File::create(&path).context(format!("Failed to create file {:?}", path))?;
            file.write_all(content.as_bytes())
                .context(format!("Failed to write to file {:?}", path))?;
            println!("Created file: {:?}", path);
        } else {
            println!("File already exists: {:?}", path);
        }
    }

    println!("Initialization complete.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(name: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("clt-{}-{}", name, nonce))
    }

    #[test]
    fn add_task_creates_missing_task_store() {
        let root = temp_root("auto-init");

        let result = add_task(&root, "write from a fresh directory", None);

        assert!(result.is_ok());
        let todo = fs::read_to_string(root.join("tasks/todo.md")).unwrap();
        let doing = fs::read_to_string(root.join("tasks/doing.md")).unwrap();
        let done = fs::read_to_string(root.join("tasks/done.md")).unwrap();

        assert!(todo.contains("# To Do Tasks"));
        assert!(todo.contains("- write from a fresh directory"));
        assert_eq!(doing, "# Doing Tasks\n");
        assert_eq!(done, "# Done Tasks\n");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ensure_task_store_preserves_existing_files() {
        let root = temp_root("preserve");
        let tasks_dir = root.join("tasks");
        fs::create_dir_all(&tasks_dir).unwrap();
        fs::write(tasks_dir.join("todo.md"), "# Custom Todo\n- keep me\n").unwrap();

        ensure_task_store(&root).unwrap();

        let todo = fs::read_to_string(tasks_dir.join("todo.md")).unwrap();
        let doing = fs::read_to_string(tasks_dir.join("doing.md")).unwrap();
        let done = fs::read_to_string(tasks_dir.join("done.md")).unwrap();

        assert_eq!(todo, "# Custom Todo\n- keep me\n");
        assert_eq!(doing, "# Doing Tasks\n");
        assert_eq!(done, "# Done Tasks\n");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn parse_add_task_args_joins_unquoted_description_words() {
        let (description, metadata) = parse_add_task_args(vec![
            "write".to_string(),
            "release".to_string(),
            "notes".to_string(),
        ])
        .unwrap();

        assert_eq!(description, "write release notes");
        assert_eq!(metadata, None);
    }

    #[test]
    fn parse_add_task_args_keeps_tag_like_metadata() {
        let (description, metadata) =
            parse_add_task_args(vec!["Fix login bug".to_string(), "BUG, HIGH".to_string()])
                .unwrap();

        assert_eq!(description, "Fix login bug");
        assert_eq!(metadata, Some("BUG, HIGH".to_string()));
    }

    #[test]
    fn add_command_accepts_multiple_description_words() {
        let cli = Cli::try_parse_from(["clt", "add", "write", "release", "notes"]).unwrap();

        match cli.command {
            Some(Commands::Add { task }) => {
                assert_eq!(task, vec!["write", "release", "notes"]);
            }
            _ => panic!("expected add command"),
        }
    }

    #[test]
    fn input_cursor_offset_wraps_after_full_line() {
        assert_eq!(input_cursor_offset("Add Task:", 9), (0, 1));
        assert_eq!(input_cursor_offset("Add Task:\nhello", 9), (5, 1));
    }
}
