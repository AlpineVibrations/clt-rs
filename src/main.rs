use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use ratatui::layout::Alignment;
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
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};

#[derive(Parser)]
#[command(name = "lls-cli-task")]
#[command(about = "A simple file-system-backed task management system", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
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

    match cli.command {
        Some(Commands::Init) => {
            init_tasks()?;
        }
        Some(Commands::Add {
            description,
            metadata,
        }) => {
            let msg = add_task(&description, metadata)?;
            println!("{}", msg);
        }
        Some(Commands::Status {
            from,
            task_index,
            to,
        }) => {
            move_task(&from, &to, &task_index)?;
        }
        Some(Commands::Done { status, task_index }) => {
            if status == "done" {
                println!("Task is already done.");
            } else {
                move_task(&status, "done", &task_index)?;
                println!("Task {} from {} marked as done.", task_index, status);
            }
        }
        Some(Commands::Delete { status, task_index }) => {
            delete_task(&status, &task_index)?;
            println!("Task {} from {} deleted successfully.", task_index, status);
        }
        Some(Commands::List { status }) => {
            list_tasks(status)?;
        }
        None => {
            if !is_initialized() {
                print!("Tasks not initialized. Would you like to initialize now? (y/n): ");
                io::stdout().flush()?;

                let mut response = String::new();
                io::stdin().read_line(&mut response)?;

                if response.trim().to_lowercase() == "y" {
                    init_tasks()?;
                } else {
                    println!(
                        "Initialization skipped. Please run 'init' to set up your task lists."
                    );
                    return Ok(());
                }
            }
            tui_view()?;
        }
    }

    Ok(())
}

fn is_initialized() -> bool {
    let tasks_dir = Path::new("tasks");
    if !tasks_dir.exists() {
        return false;
    }
    let files = ["todo.md", "doing.md", "done.md"];
    files.iter().all(|f| tasks_dir.join(f).exists())
}

fn get_file_path(status: &str) -> Result<std::path::PathBuf> {
    match status {
        "todo" => Ok(Path::new("tasks/todo.md").to_path_buf()),
        "doing" => Ok(Path::new("tasks/doing.md").to_path_buf()),
        "done" => Ok(Path::new("tasks/done.md").to_path_buf()),
        _ => anyhow::bail!("Invalid status. Use 'todo', 'doing', or 'done'."),
    }
}

// find_task_status is no longer needed for index-based referencing
// as the user must specify the source list.

fn delete_task(status: &str, task_index_str: &str) -> Result<()> {
    let path = get_file_path(status)?;
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

fn move_task(from: &str, to: &str, task_index_str: &str) -> Result<()> {
    let src_path = get_file_path(from)?;
    let dest_path = get_file_path(to)?;

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

fn update_task(status: &str, task_index: usize, new_description: &str) -> Result<()> {
    let path = get_file_path(status)?;
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

fn reorder_task(status: &str, from_idx: usize, to_idx: usize) -> Result<()> {
    let path = get_file_path(status)?;
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

fn list_tasks(filter_status: Option<String>) -> Result<()> {
    if let Some(ref s) = filter_status {
        let status = match s.as_str() {
            "1" => "todo",
            "2" => "doing",
            "3" => "done",
            _ => s.as_str(),
        };

        let path = get_file_path(status)?;
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
            let path = get_file_path(status)?;
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

fn add_task(description: &str, metadata: Option<String>) -> Result<String> {
    insert_task("todo", None, description, metadata).map(|_| "Task added successfully.".to_string())
}

fn insert_task(
    status: &str,
    index: Option<usize>,
    description: &str,
    metadata: Option<String>,
) -> Result<()> {
    let path = get_file_path(status)?;
    if !path.exists() {
        anyhow::bail!("Tasks not initialized. Please run 'init' first.");
    }

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

fn read_tasks(status: &str) -> Result<Vec<String>> {
    let path = get_file_path(status)?;
    let content = fs::read_to_string(&path).context(format!("Failed to read {:?}", path))?;
    let tasks = content
        .lines()
        .filter(|l| l.starts_with("- "))
        .map(|l| l.to_string())
        .collect();
    Ok(tasks)
}

enum Mode {
    View,
    Input,
    Edit,
    Help,
}

fn wrap_text(text: &str, width: usize) -> String {
    if width == 0 {
        return text.to_string();
    }
    let mut result = String::new();
    let mut current_line = String::new();

    for word in text.split_whitespace() {
        if current_line.is_empty() {
            current_line.push_str(word);
        } else if current_line.len() + 1 + word.len() <= width {
            current_line.push(' ');
            current_line.push_str(word);
        } else {
            result.push_str(&current_line);
            result.push('\n');
            current_line.clear();
            current_line.push_str(word);
        }
    }
    result.push_str(&current_line);
    result
}

fn tui_view() -> Result<()> {
    // Setup terminal
    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
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
    let text_color = Color::DarkGray;
    let c_highlight = Color::Indexed(222);
    let colors = [c_1, c_2, c_3];

    loop {
        terminal.draw(|f| {
            let size = f.area();

            // Main layout: Kanban board, input area (if active), and feedback console
            let main_layout = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(0),
                    if matches!(current_mode, Mode::Input) || matches!(current_mode, Mode::Edit) {
                        Constraint::Length(3)
                    } else {
                        Constraint::Length(0)
                    },
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
                let tasks = read_tasks(status).unwrap_or_default();
                let items: Vec<ListItem> = tasks
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

                let list = List::new(items.clone())
                    .block(
                        Block::default()
                            .title(format!(
                                "{} {}",
                                titles[i],
                                if selected_board == i {
                                    "  <<<<<< * >>>>>>     "
                                } else {
                                    ""
                                }
                            ))
                            // .title(Line::from(vec![Span::raw(" TODO")]))
                            .title(
                                Line::from(vec![Span::raw(format!("{} tasks ", &items.len()))])
                                    .alignment(Alignment::Right),
                            )
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(colors[i])),
                    )
                    .style(Style::default().fg(text_color))
                    .highlight_style(highlight_style);

                f.render_stateful_widget(list, chunks[i], &mut board_states[i]);
            }

            if matches!(current_mode, Mode::Input) || matches!(current_mode, Mode::Edit) {
                let label = if matches!(current_mode, Mode::Input) {
                    " Add Task: "
                } else {
                    " Edit Task: "
                };
                let input_text = format!("{}{}", label, input_buffer);
                let input_paragraph = Paragraph::new(input_text)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title("Input Mode (Enter to save, Esc to cancel)"),
                    )
                    .style(Style::default().fg(Color::White));
                f.render_widget(input_paragraph, main_layout[1]);
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
                                        let tasks = read_tasks(statuses[selected_board])
                                            .unwrap_or_default();
                                        if idx < tasks.len() - 1 {
                                            match reorder_task(
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
                                            match move_task(&from, &to, &(idx + 1).to_string()) {
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
                                            match move_task(&from, &to, &(idx + 1).to_string()) {
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
                                KeyCode::Char('q') => break,
                                KeyCode::Enter => {
                                    let state = &board_states[selected_board];
                                    if let Some(idx) = state.selected() {
                                        current_mode = Mode::Edit;
                                        editing_task_idx = Some(idx + 1);
                                        let tasks = read_tasks(statuses[selected_board])
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
                                    board_states[selected_board].select(Some(0));
                                }
                                KeyCode::Char('2') => {
                                    selected_board = 1;
                                    for state in board_states.iter_mut() {
                                        state.select(None);
                                    }
                                    board_states[selected_board].select(Some(0));
                                }
                                KeyCode::Char('3') => {
                                    selected_board = 2;
                                    for state in board_states.iter_mut() {
                                        state.select(None);
                                    }
                                    board_states[selected_board].select(Some(0));
                                }
                                KeyCode::Char('i') | KeyCode::Char('I') => {
                                    let state = &mut board_states[selected_board];
                                    if let Some(idx) = state.selected() {
                                        if idx > 0 {
                                            match reorder_task(
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
                                        let tasks = read_tasks(statuses[selected_board])
                                            .unwrap_or_default();
                                        if idx < tasks.len() - 1 {
                                            match reorder_task(
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
                                            match move_task(&from, &to, &(idx + 1).to_string()) {
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
                                            match move_task(&from, &to, &(idx + 1).to_string()) {
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
                                        match delete_task(status, &(idx + 1).to_string()) {
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
                                    let tasks =
                                        read_tasks(statuses[selected_board]).unwrap_or_default();
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
                                    let tasks =
                                        read_tasks(statuses[selected_board]).unwrap_or_default();
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
                                    board_states[selected_board].select(Some(0));
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
                                    board_states[selected_board].select(Some(0));
                                }
                                KeyCode::Char(c) if c.is_ascii_digit() => {
                                    let new_pos = (c as u8 - b'0') as usize;
                                    let state = &mut board_states[selected_board];
                                    if let Some(idx) = state.selected() {
                                        if new_pos > 0 {
                                            match reorder_task(
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
                                    match update_task(statuses[selected_board], idx, &input_buffer)
                                    {
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
        ("todo.md", "\n"),
        ("doing.md", "\n"),
        ("done.md", "\n"),
    ];

    for (filename, content) in files {
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
