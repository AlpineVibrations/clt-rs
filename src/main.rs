use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use ratatui::layout::{Alignment, Position};
use std::fs;
use std::io::{self, Write, stdout};
use std::path::{Path, PathBuf};

use crossterm::{
    ExecutableCommand,
    event::{self, Event, KeyCode, KeyModifiers},
    terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, SetTitle, disable_raw_mode, enable_raw_mode,
    },
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, ListItem, ListState, Paragraph},
};
use tui_input::{Input, InputRequest};

const TASK_STATUSES: [&str; 3] = ["todo", "doing", "done"];
const TASK_DETAIL_FILES: [&str; 3] = ["task.md", "README.md", "index.md"];

#[derive(Clone, Debug)]
struct TaskEntry {
    source: TaskSource,
    summary: String,
    content: String,
    metadata: Option<String>,
    has_subtasks: bool,
}

#[derive(Clone, Debug)]
enum TaskSource {
    MarkdownLine { line_index: usize },
    Path { path: PathBuf, is_dir: bool },
}

#[derive(Clone, Debug)]
enum StatusStore {
    MarkdownFile(PathBuf),
    Directory(PathBuf),
}

enum ExpansionSummary {
    AlreadyDirectory {
        status: &'static str,
        dir: PathBuf,
    },
    Expanded {
        status: &'static str,
        dir: PathBuf,
        backup: PathBuf,
        task_count: usize,
    },
}

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
    /// Initializes the tasks directory and status stores
    Init {
        /// Create todo/doing/done folders instead of markdown files
        #[arg(long, default_value_t = false)]
        folders: bool,
    },
    /// Expands markdown status files into folder-backed task files
    Expand {
        /// Optional status to expand (todo, doing, done). Expands all statuses if omitted.
        status: Option<String>,
    },
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
        Some(Commands::Init { folders }) => {
            init_tasks(&root, folders)?;
        }
        Some(Commands::Expand { status }) => {
            expand_tasks(&root, status)?;
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
                    init_tasks(&root, false)?;
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

fn project_display_name(root: &Path) -> String {
    root.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| root.display().to_string())
}

fn app_title(root: &Path) -> String {
    format!("clt | {}", project_display_name(root))
}

fn is_initialized(root: &Path) -> bool {
    let tasks_dir = get_tasks_dir(root);
    if !tasks_dir.exists() {
        return false;
    }
    TASK_STATUSES
        .iter()
        .all(|status| status_store_exists(&tasks_dir, status))
}

fn ensure_task_store(root: &Path) -> Result<()> {
    ensure_board_store(&get_tasks_dir(root))
}

fn status_filename(status: &str) -> Result<&'static str> {
    match status {
        "todo" => Ok("todo.md"),
        "doing" => Ok("doing.md"),
        "done" => Ok("done.md"),
        _ => anyhow::bail!("Invalid status. Use 'todo', 'doing', or 'done'."),
    }
}

fn normalize_status_arg(status: &str) -> Result<&'static str> {
    match status {
        "1" | "todo" => Ok("todo"),
        "2" | "doing" => Ok("doing"),
        "3" | "done" => Ok("done"),
        _ => anyhow::bail!("Invalid status. Use 'todo', 'doing', or 'done'."),
    }
}

fn status_header(status: &str) -> Result<&'static str> {
    match status {
        "todo" => Ok("# To Do Tasks\n"),
        "doing" => Ok("# Doing Tasks\n"),
        "done" => Ok("# Done Tasks\n"),
        _ => anyhow::bail!("Invalid status. Use 'todo', 'doing', or 'done'."),
    }
}

fn status_store_exists(board_dir: &Path, status: &str) -> bool {
    board_dir.join(status).is_dir()
        || status_filename(status)
            .map(|filename| board_dir.join(filename).is_file())
            .unwrap_or(false)
}

fn ensure_board_store(board_dir: &Path) -> Result<()> {
    fs::create_dir_all(board_dir).context("Failed to create tasks directory")?;
    let directory_mode = TASK_STATUSES
        .iter()
        .any(|status| board_dir.join(status).is_dir());

    for status in TASK_STATUSES {
        let dir_path = board_dir.join(status);
        let file_path = board_dir.join(status_filename(status)?);
        if dir_path.is_dir() || file_path.exists() {
            continue;
        }

        if directory_mode {
            fs::create_dir_all(&dir_path)
                .context(format!("Failed to create directory {:?}", dir_path))?;
        } else {
            fs::write(&file_path, status_header(status)?)
                .context(format!("Failed to create file {:?}", file_path))?;
        }
    }

    Ok(())
}

fn get_status_store(board_dir: &Path, status: &str) -> Result<StatusStore> {
    status_filename(status)?;
    ensure_board_store(board_dir)?;

    let dir_path = board_dir.join(status);
    if dir_path.is_dir() {
        return Ok(StatusStore::Directory(dir_path));
    }

    Ok(StatusStore::MarkdownFile(
        board_dir.join(status_filename(status)?),
    ))
}

// find_task_status is no longer needed for index-based referencing
// as the user must specify the source list.

fn read_task_entries(board_dir: &Path, status: &str) -> Result<Vec<TaskEntry>> {
    match get_status_store(board_dir, status)? {
        StatusStore::MarkdownFile(path) => read_markdown_entries(&path),
        StatusStore::Directory(path) => read_directory_entries(&path),
    }
}

fn read_markdown_entries(path: &Path) -> Result<Vec<TaskEntry>> {
    let content = fs::read_to_string(path).context(format!("Failed to read {:?}", path))?;
    let entries = content
        .lines()
        .enumerate()
        .filter_map(|(line_index, line)| {
            let task_text = line.strip_prefix("- ")?;
            Some(task_entry_from_text(
                TaskSource::MarkdownLine { line_index },
                task_text,
                task_text,
                false,
            ))
        })
        .collect();

    Ok(entries)
}

fn read_directory_entries(path: &Path) -> Result<Vec<TaskEntry>> {
    let mut paths = directory_task_paths(path)?;
    paths.sort_by_key(|path| {
        path.file_name()
            .map(|name| name.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    });

    paths
        .into_iter()
        .map(|path| {
            let is_dir = path.is_dir();
            let fallback = title_from_path(&path);
            let content = if is_dir {
                read_directory_task_content(&path).unwrap_or_else(|| fallback.clone())
            } else {
                fs::read_to_string(&path)
                    .with_context(|| format!("Failed to read task file {:?}", path))?
            };
            let summary = first_sentence(&content).unwrap_or(fallback);
            let (description, metadata) = split_description_metadata(&summary);
            let has_subtasks = is_dir && board_has_any_status_store(&path);

            Ok(TaskEntry {
                source: TaskSource::Path { path, is_dir },
                summary: description.to_string(),
                content,
                metadata: metadata.map(str::to_string),
                has_subtasks,
            })
        })
        .collect()
}

fn task_entry_from_text(
    source: TaskSource,
    text: &str,
    content: &str,
    has_subtasks: bool,
) -> TaskEntry {
    let summary = first_sentence(text).unwrap_or_else(|| text.trim().to_string());
    let (description, metadata) = split_description_metadata(&summary);

    TaskEntry {
        source,
        summary: description.to_string(),
        content: content.to_string(),
        metadata: metadata.map(str::to_string),
        has_subtasks,
    }
}

fn board_has_any_status_store(board_dir: &Path) -> bool {
    TASK_STATUSES
        .iter()
        .any(|status| status_store_exists(board_dir, status))
}

fn directory_task_paths(path: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();

    if !path.exists() {
        return Ok(paths);
    }

    for entry in
        fs::read_dir(path).with_context(|| format!("Failed to read directory {:?}", path))?
    {
        let entry = entry?;
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };

        if file_name.starts_with('.') {
            continue;
        }

        let file_type = entry.file_type()?;
        if file_type.is_file() || file_type.is_dir() {
            paths.push(path);
        }
    }

    paths.sort_by_key(|path| {
        path.file_name()
            .map(|name| name.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    });

    Ok(paths)
}

fn read_directory_task_content(path: &Path) -> Option<String> {
    TASK_DETAIL_FILES.iter().find_map(|filename| {
        let detail_path = path.join(filename);
        fs::read_to_string(detail_path).ok()
    })
}

fn directory_task_detail_path(path: &Path) -> PathBuf {
    TASK_DETAIL_FILES
        .iter()
        .map(|filename| path.join(filename))
        .find(|path| path.exists())
        .unwrap_or_else(|| path.join(TASK_DETAIL_FILES[0]))
}

fn title_from_path(path: &Path) -> String {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("task");
    let name = strip_order_prefix(name);
    let stem = Path::new(name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(name);
    let title = stem.replace(['-', '_'], " ");

    if title.trim().is_empty() {
        "task".to_string()
    } else {
        title
    }
}

fn strip_order_prefix(name: &str) -> &str {
    let bytes = name.as_bytes();
    if bytes.len() > 5 && bytes[..4].iter().all(|byte| byte.is_ascii_digit()) && bytes[4] == b'-' {
        &name[5..]
    } else {
        name
    }
}

fn first_sentence(content: &str) -> Option<String> {
    let normalized = normalize_task_text(content);
    if normalized.is_empty() {
        return None;
    }

    let end_idx = normalized
        .char_indices()
        .find(|(_, ch)| matches!(ch, '.' | '!' | '?'))
        .map(|(idx, ch)| idx + ch.len_utf8())
        .unwrap_or(normalized.len());

    Some(normalized[..end_idx].trim().to_string())
}

fn normalize_task_text(content: &str) -> String {
    content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            line.trim_start_matches('#')
                .trim_start()
                .strip_prefix("- ")
                .unwrap_or_else(|| {
                    line.trim_start_matches('#')
                        .trim_start()
                        .strip_prefix("* ")
                        .unwrap_or(line)
                })
                .trim()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn split_description_metadata(value: &str) -> (&str, Option<&str>) {
    if let Some(start) = value.rfind(" (") {
        if value.ends_with(')') {
            return (&value[..start], Some(&value[start + 2..value.len() - 1]));
        }
    }

    (value, None)
}

fn task_display_text(entry: &TaskEntry) -> String {
    match &entry.metadata {
        Some(metadata) => format!("{} ({})", entry.summary, metadata),
        None => entry.summary.clone(),
    }
}

fn parse_one_based_task_index(task_index_str: &str) -> Result<usize> {
    let task_index = task_index_str
        .parse::<usize>()
        .context("Invalid task index. Please provide a number.")?;
    if task_index == 0 {
        anyhow::bail!("Task index must be 1 or greater.");
    }

    Ok(task_index)
}

fn task_entry_at(board_dir: &Path, status: &str, task_index: usize) -> Result<TaskEntry> {
    let entries = read_task_entries(board_dir, status)?;
    entries.get(task_index - 1).cloned().ok_or_else(|| {
        anyhow::anyhow!(
            "Task index {} out of range. Only {} tasks found in {}.",
            task_index,
            entries.len(),
            status
        )
    })
}

fn write_lines(path: &Path, lines: &[String]) -> Result<()> {
    let updated_content = lines.join("\n");
    let final_content = if updated_content.is_empty() {
        updated_content
    } else {
        format!("{}\n", updated_content)
    };

    fs::write(path, final_content).context("Failed to update file")?;
    Ok(())
}

fn remove_task_entry(board_dir: &Path, status: &str, entry: &TaskEntry) -> Result<()> {
    match &entry.source {
        TaskSource::MarkdownLine { line_index } => {
            let StatusStore::MarkdownFile(path) = get_status_store(board_dir, status)? else {
                anyhow::bail!("Task storage changed while removing task.");
            };
            let content = fs::read_to_string(&path).context("Failed to read file")?;
            let mut lines: Vec<String> = content.lines().map(str::to_string).collect();

            if *line_index >= lines.len() {
                anyhow::bail!("Task storage changed while removing task.");
            }

            lines.remove(*line_index);
            write_lines(&path, &lines)?;
        }
        TaskSource::Path { path, is_dir } => {
            if *is_dir {
                fs::remove_dir_all(path)
                    .with_context(|| format!("Failed to remove task directory {:?}", path))?;
            } else {
                fs::remove_file(path)
                    .with_context(|| format!("Failed to remove task file {:?}", path))?;
            }

            if let Some(parent) = path.parent() {
                normalize_directory_order(parent)?;
            }
        }
    }

    Ok(())
}

fn content_with_metadata(description: &str, metadata: Option<String>) -> String {
    match metadata {
        Some(metadata) => format!("{} ({})", description, metadata),
        None => description.to_string(),
    }
}

fn insert_task_content(
    board_dir: &Path,
    status: &str,
    index: Option<usize>,
    content: &str,
) -> Result<()> {
    match get_status_store(board_dir, status)? {
        StatusStore::MarkdownFile(path) => insert_content_into_markdown(&path, index, content),
        StatusStore::Directory(path) => insert_content_into_directory(&path, index, content),
    }
}

fn insert_content_into_markdown(path: &Path, index: Option<usize>, content: &str) -> Result<()> {
    let task_line = format!("- {}", single_line_content(content));
    let file_content = fs::read_to_string(path).context("Failed to read file")?;
    let mut lines: Vec<String> = file_content.lines().map(str::to_string).collect();

    if let Some(idx) = index {
        let task_lines: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, line)| line.starts_with("- "))
            .map(|(i, _)| i)
            .collect();

        if idx < task_lines.len() {
            let actual_idx = task_lines[idx];
            lines.insert(actual_idx, task_line);
        } else {
            lines.push(task_line);
        }
    } else {
        lines.push(task_line);
    }

    write_lines(path, &lines)
}

fn insert_content_into_directory(path: &Path, index: Option<usize>, content: &str) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("Failed to create directory {:?}", path))?;
    let preferred_name = format!(
        "{:04}-{}.md",
        directory_task_paths(path)?.len() + 1,
        slugify(&first_sentence(content).unwrap_or_else(|| "task".to_string()))
    );
    let task_path = unique_child_path(path, &preferred_name);
    fs::write(&task_path, format!("{}\n", content.trim_end()))
        .with_context(|| format!("Failed to write task file {:?}", task_path))?;

    if let Some(idx) = index {
        reorder_path_in_directory(path, &task_path, idx)?;
    } else {
        normalize_directory_order(path)?;
    }

    Ok(())
}

fn single_line_content(content: &str) -> String {
    normalize_task_text(content)
}

fn slugify(value: &str) -> String {
    let mut slug = String::new();
    let mut last_was_dash = false;

    for ch in value.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_was_dash = false;
        } else if !last_was_dash && !slug.is_empty() {
            slug.push('-');
            last_was_dash = true;
        }

        if slug.len() >= 48 {
            break;
        }
    }

    while slug.ends_with('-') {
        slug.pop();
    }

    if slug.is_empty() {
        "task".to_string()
    } else {
        slug
    }
}

fn unique_child_path(parent: &Path, preferred_name: &str) -> PathBuf {
    let preferred = parent.join(preferred_name);
    if !preferred.exists() {
        return preferred;
    }

    let preferred_path = Path::new(preferred_name);
    let stem = preferred_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(preferred_name);
    let extension = preferred_path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| format!(".{}", extension))
        .unwrap_or_default();

    for idx in 2.. {
        let candidate = parent.join(format!("{}-{}{}", stem, idx, extension));
        if !candidate.exists() {
            return candidate;
        }
    }

    unreachable!("unique path search is unbounded")
}

fn normalize_directory_order(path: &Path) -> Result<()> {
    let paths = directory_task_paths(path)?;
    if paths.is_empty() {
        return Ok(());
    }

    let mut temp_paths = Vec::new();
    for (idx, task_path) in paths.iter().enumerate() {
        let original_name = task_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("task");
        let temp_path = unique_child_path(path, &format!(".clt-reorder-{}-{}", idx, original_name));
        fs::rename(task_path, &temp_path).with_context(|| {
            format!(
                "Failed to prepare task {:?} for directory order update",
                task_path
            )
        })?;
        temp_paths.push((temp_path, original_name.to_string()));
    }

    for (idx, (temp_path, original_name)) in temp_paths.into_iter().enumerate() {
        let final_name = format!("{:04}-{}", idx + 1, strip_order_prefix(&original_name));
        let final_path = path.join(final_name);
        fs::rename(&temp_path, &final_path).with_context(|| {
            format!(
                "Failed to finish task {:?} directory order update",
                temp_path
            )
        })?;
    }

    Ok(())
}

fn reorder_path_in_directory(path: &Path, task_path: &Path, to_idx: usize) -> Result<()> {
    let mut paths = directory_task_paths(path)?;
    let Some(from_idx) = paths.iter().position(|path| path == task_path) else {
        anyhow::bail!("Task file disappeared while reordering.");
    };
    let task_path = paths.remove(from_idx);
    let to_idx = to_idx.min(paths.len());
    paths.insert(to_idx, task_path);

    rewrite_directory_order(path, paths)
}

fn rewrite_directory_order(path: &Path, ordered_paths: Vec<PathBuf>) -> Result<()> {
    if ordered_paths.is_empty() {
        return Ok(());
    }

    let mut temp_paths = Vec::new();
    for (idx, task_path) in ordered_paths.iter().enumerate() {
        let original_name = task_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("task");
        let temp_path = unique_child_path(path, &format!(".clt-reorder-{}-{}", idx, original_name));
        fs::rename(task_path, &temp_path).with_context(|| {
            format!(
                "Failed to prepare task {:?} for directory order update",
                task_path
            )
        })?;
        temp_paths.push((temp_path, original_name.to_string()));
    }

    for (idx, (temp_path, original_name)) in temp_paths.into_iter().enumerate() {
        let final_name = format!("{:04}-{}", idx + 1, strip_order_prefix(&original_name));
        let final_path = path.join(final_name);
        fs::rename(&temp_path, &final_path).with_context(|| {
            format!(
                "Failed to finish task {:?} directory order update",
                temp_path
            )
        })?;
    }

    Ok(())
}

fn move_path_into_directory(
    source_path: &Path,
    dest_dir: &Path,
    index: Option<usize>,
) -> Result<PathBuf> {
    fs::create_dir_all(dest_dir)
        .with_context(|| format!("Failed to create destination directory {:?}", dest_dir))?;

    let original_name = source_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("task.md");
    let preferred_name = format!(
        "{:04}-{}",
        directory_task_paths(dest_dir)?.len() + 1,
        strip_order_prefix(original_name)
    );
    let dest_path = unique_child_path(dest_dir, &preferred_name);
    fs::rename(source_path, &dest_path).with_context(|| {
        format!(
            "Failed to move task {:?} into directory {:?}",
            source_path, dest_dir
        )
    })?;

    if let Some(source_parent) = source_path.parent() {
        normalize_directory_order(source_parent)?;
    }

    if let Some(idx) = index {
        reorder_path_in_directory(dest_dir, &dest_path, idx)?;
    } else {
        normalize_directory_order(dest_dir)?;
    }

    Ok(dest_path)
}

fn convert_status_to_directory(board_dir: &Path, status: &str) -> Result<PathBuf> {
    let dir_path = board_dir.join(status);
    if dir_path.is_dir() {
        return Ok(dir_path);
    }

    let file_path = board_dir.join(status_filename(status)?);
    fs::create_dir_all(&dir_path)
        .with_context(|| format!("Failed to create directory {:?}", dir_path))?;

    if file_path.exists() {
        let entries = read_markdown_entries(&file_path)?;
        for entry in entries {
            insert_content_into_directory(&dir_path, None, &entry.content)?;
        }

        let backup_name = format!("{}.bak", status_filename(status)?);
        let backup_path = unique_child_path(board_dir, &backup_name);
        fs::rename(&file_path, &backup_path).with_context(|| {
            format!(
                "Failed to preserve markdown status file as {:?}",
                backup_path
            )
        })?;
    }

    Ok(dir_path)
}

fn expand_status_for_command(board_dir: &Path, status: &'static str) -> Result<ExpansionSummary> {
    ensure_board_store(board_dir)?;

    let dir_path = board_dir.join(status);
    if dir_path.is_dir() {
        return Ok(ExpansionSummary::AlreadyDirectory {
            status,
            dir: dir_path,
        });
    }

    let file_path = board_dir.join(status_filename(status)?);
    let entries = read_markdown_entries(&file_path)?;
    let task_count = entries.len();
    fs::create_dir_all(&dir_path)
        .with_context(|| format!("Failed to create directory {:?}", dir_path))?;

    for entry in entries {
        insert_content_into_directory(&dir_path, None, &entry.content)?;
    }

    let backup_name = format!("{}.bak", status_filename(status)?);
    let backup_path = unique_child_path(board_dir, &backup_name);
    fs::rename(&file_path, &backup_path).with_context(|| {
        format!(
            "Failed to preserve markdown status file as {:?}",
            backup_path
        )
    })?;

    Ok(ExpansionSummary::Expanded {
        status,
        dir: dir_path,
        backup: backup_path,
        task_count,
    })
}

fn expand_tasks(root: &Path, filter_status: Option<String>) -> Result<()> {
    let board_dir = get_tasks_dir(root);
    let statuses: Vec<&'static str> = match filter_status {
        Some(status) => vec![normalize_status_arg(&status)?],
        None => TASK_STATUSES.to_vec(),
    };

    for status in statuses {
        match expand_status_for_command(&board_dir, status)? {
            ExpansionSummary::AlreadyDirectory { status, dir } => {
                println!("{} is already folder-backed at {:?}", status, dir);
            }
            ExpansionSummary::Expanded {
                status,
                dir,
                backup,
                task_count,
            } => {
                println!(
                    "Expanded {} to {:?} with {} task file(s). Backup: {:?}",
                    status, dir, task_count, backup
                );
            }
        }
    }

    Ok(())
}

fn delete_task(root: &Path, status: &str, task_index_str: &str) -> Result<()> {
    delete_task_in_board(&get_tasks_dir(root), status, task_index_str)
}

fn delete_task_in_board(board_dir: &Path, status: &str, task_index_str: &str) -> Result<()> {
    let task_index = parse_one_based_task_index(task_index_str)?;
    let entry = task_entry_at(board_dir, status, task_index)?;
    remove_task_entry(board_dir, status, &entry)
}

fn move_task(root: &Path, from: &str, to: &str, task_index_str: &str) -> Result<()> {
    move_task_in_board(&get_tasks_dir(root), from, to, task_index_str)
}

fn move_task_in_board(board_dir: &Path, from: &str, to: &str, task_index_str: &str) -> Result<()> {
    let task_index = parse_one_based_task_index(task_index_str)?;
    let entry = task_entry_at(board_dir, from, task_index)?;
    let dest_index = if to == "done" { Some(0) } else { None };

    match (&entry.source, get_status_store(board_dir, to)?) {
        (TaskSource::Path { path, .. }, StatusStore::Directory(dest_dir)) => {
            move_path_into_directory(path, &dest_dir, dest_index)?;
        }
        (TaskSource::Path { path, .. }, StatusStore::MarkdownFile(_)) => {
            let dest_dir = convert_status_to_directory(board_dir, to)?;
            move_path_into_directory(path, &dest_dir, dest_index)?;
        }
        (TaskSource::MarkdownLine { .. }, _) => {
            insert_task_content(board_dir, to, dest_index, &entry.content)?;
            remove_task_entry(board_dir, from, &entry)?;
        }
    }

    Ok(())
}

fn update_task_in_board(
    board_dir: &Path,
    status: &str,
    task_index: usize,
    new_description: &str,
) -> Result<()> {
    let entry = task_entry_at(board_dir, status, task_index)?;

    match entry.source {
        TaskSource::MarkdownLine { line_index } => {
            let StatusStore::MarkdownFile(path) = get_status_store(board_dir, status)? else {
                anyhow::bail!("Task storage changed while updating task.");
            };
            let content = fs::read_to_string(&path).context("Failed to read file")?;
            let mut lines: Vec<String> = content.lines().map(str::to_string).collect();

            if line_index >= lines.len() {
                anyhow::bail!("Task index {} out of range", task_index);
            }

            lines[line_index] = format!("- {}", new_description);
            write_lines(&path, &lines)?;
        }
        TaskSource::Path { path, is_dir } => {
            let target_path = if is_dir {
                directory_task_detail_path(&path)
            } else {
                path
            };
            fs::write(&target_path, format!("{}\n", new_description.trim_end()))
                .with_context(|| format!("Failed to write task file {:?}", target_path))?;
        }
    }

    Ok(())
}

fn reorder_task_in_board(
    board_dir: &Path,
    status: &str,
    from_idx: usize,
    to_idx: usize,
) -> Result<()> {
    match get_status_store(board_dir, status)? {
        StatusStore::MarkdownFile(path) => reorder_markdown_task(&path, from_idx, to_idx),
        StatusStore::Directory(path) => reorder_directory_task(&path, from_idx, to_idx),
    }
}

fn reorder_markdown_task(path: &Path, from_idx: usize, to_idx: usize) -> Result<()> {
    let content = fs::read_to_string(path).context("Failed to read file")?;
    let lines: Vec<String> = content.lines().map(str::to_string).collect();

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
    write_lines(path, &new_lines)
}

fn reorder_directory_task(path: &Path, from_idx: usize, to_idx: usize) -> Result<()> {
    let mut paths = directory_task_paths(path)?;
    paths.sort_by_key(|path| {
        path.file_name()
            .map(|name| name.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    });

    if from_idx >= paths.len() {
        anyhow::bail!("Task index out of range");
    }

    let task_path = paths.remove(from_idx);
    let to_idx = to_idx.min(paths.len());
    paths.insert(to_idx, task_path);
    rewrite_directory_order(path, paths)
}

fn list_tasks(root: &Path, filter_status: Option<String>) -> Result<()> {
    let board_dir = get_tasks_dir(root);

    if let Some(ref s) = filter_status {
        let status = match s.as_str() {
            "1" => "todo",
            "2" => "doing",
            "3" => "done",
            _ => s.as_str(),
        };

        println!("\n--- {} ---", status.to_uppercase());
        for (index, entry) in read_task_entries(&board_dir, status)?.iter().enumerate() {
            println!(
                "{}. {}{}",
                index + 1,
                task_display_text(entry),
                if entry.has_subtasks {
                    " [subtasks]"
                } else {
                    ""
                }
            );
        }
    } else {
        for status in TASK_STATUSES {
            println!("\n--- {} ---", status.to_uppercase());
            for (index, entry) in read_task_entries(&board_dir, status)?.iter().enumerate() {
                println!(
                    "{}. {}{}",
                    index + 1,
                    task_display_text(entry),
                    if entry.has_subtasks {
                        " [subtasks]"
                    } else {
                        ""
                    }
                );
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
    insert_task_in_board(&get_tasks_dir(root), status, index, description, metadata)
}

fn insert_task_in_board(
    board_dir: &Path,
    status: &str,
    index: Option<usize>,
    description: &str,
    metadata: Option<String>,
) -> Result<()> {
    let content = content_with_metadata(description, metadata);
    insert_task_content(board_dir, status, index, &content)
}

fn read_tasks(root: &Path, status: &str) -> Result<Vec<String>> {
    read_tasks_in_board(&get_tasks_dir(root), status)
}

fn read_tasks_in_board(board_dir: &Path, status: &str) -> Result<Vec<String>> {
    Ok(read_task_entries(board_dir, status)?
        .iter()
        .map(|entry| format!("- {}", task_display_text(entry)))
        .collect())
}

fn select_first_task_if_present_in_board(board_dir: &Path, status: &str, state: &mut ListState) {
    let has_tasks = read_tasks_in_board(board_dir, status)
        .map(|tasks| !tasks.is_empty())
        .unwrap_or(false);

    state.select(if has_tasks { Some(0) } else { None });
}

fn select_last_task_if_present_in_board(board_dir: &Path, status: &str, state: &mut ListState) {
    let last_idx = read_tasks_in_board(board_dir, status)
        .ok()
        .and_then(|tasks| tasks.len().checked_sub(1));

    state.select(last_idx);
}

fn selected_task_index(root: &Path, status: &str, state: &ListState) -> Option<usize> {
    selected_task_index_in_board(&get_tasks_dir(root), status, state)
}

fn selected_task_index_in_board(
    board_dir: &Path,
    status: &str,
    state: &ListState,
) -> Option<usize> {
    let idx = state.selected()?;
    let tasks = read_tasks_in_board(board_dir, status).ok()?;

    if idx < tasks.len() { Some(idx) } else { None }
}

fn selected_task(root: &Path, status: &str, state: &ListState) -> Option<(usize, String)> {
    selected_task_in_board(&get_tasks_dir(root), status, state)
}

fn selected_task_in_board(
    board_dir: &Path,
    status: &str,
    state: &ListState,
) -> Option<(usize, String)> {
    let idx = state.selected()?;
    let tasks = read_tasks_in_board(board_dir, status).ok()?;
    tasks.get(idx).cloned().map(|task| (idx, task))
}

fn selected_task_entry_in_board(
    board_dir: &Path,
    status: &str,
    state: &ListState,
) -> Option<(usize, TaskEntry)> {
    let idx = state.selected()?;
    let tasks = read_task_entries(board_dir, status).ok()?;
    tasks.get(idx).cloned().map(|task| (idx, task))
}

fn normalize_board_selection(root: &Path, status: &str, state: &mut ListState) {
    normalize_board_selection_in_board(&get_tasks_dir(root), status, state);
}

fn normalize_board_selection_in_board(board_dir: &Path, status: &str, state: &mut ListState) {
    let selected = state.selected();
    let task_count = read_tasks_in_board(board_dir, status)
        .map(|tasks| tasks.len())
        .unwrap_or(0);

    match (selected, task_count) {
        (Some(idx), 0) if idx == 0 => state.select(None),
        (Some(idx), count) if idx >= count => state.select(count.checked_sub(1)),
        _ => {}
    }
}

fn normalize_board_selections_in_board(
    board_dir: &Path,
    statuses: &[&str],
    states: &mut [ListState],
) {
    for (status, state) in statuses.iter().zip(states.iter_mut()) {
        normalize_board_selection_in_board(board_dir, status, state);
    }
}

fn task_display_height(
    task: &str,
    idx: usize,
    selected_idx: Option<usize>,
    col_width: usize,
) -> usize {
    let cleaned = task.replace("- ", "");
    let desc = if let Some(start) = cleaned.rfind(" (") {
        if cleaned.ends_with(')') {
            &cleaned[..start]
        } else {
            &cleaned[..]
        }
    } else {
        &cleaned[..]
    };

    if Some(idx) == selected_idx {
        wrap_text(desc, col_width.saturating_sub(5))
            .lines()
            .count()
            .max(1)
    } else {
        1
    }
}

fn keep_selected_task_visible(
    tasks: &[String],
    selected_idx: Option<usize>,
    scroll_offset: &mut usize,
    viewport_height: usize,
    col_width: usize,
) {
    if tasks.is_empty() || viewport_height == 0 {
        *scroll_offset = 0;
        return;
    }

    let Some(selected_idx) = selected_idx.filter(|idx| *idx < tasks.len()) else {
        *scroll_offset = (*scroll_offset).min(tasks.len() - 1);
        return;
    };

    if selected_idx < *scroll_offset {
        *scroll_offset = selected_idx;
    }

    while *scroll_offset < selected_idx {
        let visible_height: usize = tasks[*scroll_offset..=selected_idx]
            .iter()
            .enumerate()
            .map(|(offset, task)| {
                let idx = *scroll_offset + offset;
                task_display_height(task, idx, Some(selected_idx), col_width)
            })
            .sum();

        if visible_height <= viewport_height {
            break;
        }

        *scroll_offset += 1;
    }
}

enum Mode {
    View,
    Input,
    Edit,
    Help,
}

struct TerminalSession;

impl TerminalSession {
    fn enter(title: &str) -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = stdout();
        if let Err(err) = stdout.execute(EnterAlternateScreen) {
            let _ = disable_raw_mode();
            return Err(err.into());
        }
        if let Err(err) = stdout.execute(SetTitle(title)) {
            let _ = stdout.execute(LeaveAlternateScreen);
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

fn wrap_input_text(text: &str, width: usize) -> String {
    if width == 0 {
        return text.to_string();
    }

    let mut result = String::new();
    let mut col = 0;

    for ch in text.chars() {
        if ch == '\n' {
            result.push(ch);
            col = 0;
            continue;
        }

        if col >= width {
            result.push('\n');
            col = 0;
        }

        result.push(ch);
        col += 1;
    }

    result
}

fn input_cursor_offset_at(text: &str, width: usize, cursor_idx: usize) -> (u16, u16) {
    if width == 0 {
        return (0, 0);
    }

    let cursor_idx = clamp_to_char_boundary(text, cursor_idx.min(text.len()));
    let mut row = 0;
    let mut col = 0;

    for ch in text[..cursor_idx].chars() {
        if ch == '\n' {
            row += 1;
            col = 0;
            continue;
        }

        if col >= width {
            row += 1;
            col = 0;
        }

        col += 1;
    }

    if col >= width {
        (0, (row + 1) as u16)
    } else {
        (col as u16, row as u16)
    }
}

fn byte_index_at_char(text: &str, char_idx: usize) -> usize {
    text.char_indices()
        .nth(char_idx)
        .map(|(idx, _)| idx)
        .unwrap_or(text.len())
}

fn input_cursor_offset_at_char(text: &str, width: usize, cursor_chars: usize) -> (usize, usize) {
    if width == 0 {
        return (0, 0);
    }

    let mut row = 0;
    let mut col = 0;

    for ch in text.chars().take(cursor_chars) {
        if ch == '\n' {
            row += 1;
            col = 0;
            continue;
        }

        if col >= width {
            row += 1;
            col = 0;
        }

        col += 1;
    }

    if col >= width {
        (0, row + 1)
    } else {
        (col, row)
    }
}

fn char_index_for_input_offset(
    text: &str,
    width: usize,
    target_row: usize,
    target_col: usize,
) -> usize {
    if width == 0 {
        return 0;
    }

    let mut row = 0;
    let mut col = 0;

    for (char_idx, ch) in text.chars().enumerate() {
        if row == target_row && col >= target_col {
            return char_idx;
        }

        if ch == '\n' {
            if row == target_row {
                return char_idx;
            }
            row += 1;
            col = 0;
            continue;
        }

        if col >= width {
            row += 1;
            col = 0;
            if row == target_row && col >= target_col {
                return char_idx;
            }
        }

        col += 1;
    }

    text.chars().count()
}

fn move_input_cursor_row(input: &mut Input, label: &str, width: usize, row_delta: isize) {
    if width == 0 {
        return;
    }

    let full_text = format!("{}{}", label, input.value());
    let label_chars = label.chars().count();
    let input_chars = input.value().chars().count();
    let cursor_chars = label_chars + input.cursor();
    let (target_col, current_row) = input_cursor_offset_at_char(&full_text, width, cursor_chars);
    let target_row = current_row.saturating_add_signed(row_delta);

    let target_chars = char_index_for_input_offset(&full_text, width, target_row, target_col);
    let input_cursor = target_chars.saturating_sub(label_chars).min(input_chars);
    input.handle(InputRequest::SetCursor(input_cursor));
}

fn handle_input_key(input: &mut Input, key: crossterm::event::KeyEvent, label: &str, width: usize) {
    let word_modifier =
        key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::ALT);

    let request = match key.code {
        KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(InputRequest::GoToStart)
        }
        KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(InputRequest::GoToEnd)
        }
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(InputRequest::DeleteLine)
        }
        KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(InputRequest::DeleteTillEnd)
        }
        KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(InputRequest::DeletePrevWord)
        }
        KeyCode::Backspace if word_modifier => Some(InputRequest::DeletePrevWord),
        KeyCode::Delete if word_modifier => Some(InputRequest::DeleteNextWord),
        KeyCode::Left if word_modifier => Some(InputRequest::GoToPrevWord),
        KeyCode::Right if word_modifier => Some(InputRequest::GoToNextWord),
        KeyCode::Left => Some(InputRequest::GoToPrevChar),
        KeyCode::Right => Some(InputRequest::GoToNextChar),
        KeyCode::Home => Some(InputRequest::GoToStart),
        KeyCode::End => Some(InputRequest::GoToEnd),
        KeyCode::Backspace => Some(InputRequest::DeletePrevChar),
        KeyCode::Delete => Some(InputRequest::DeleteNextChar),
        KeyCode::Char(c)
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            Some(InputRequest::InsertChar(c))
        }
        _ => None,
    };

    if let Some(request) = request {
        input.handle(request);
        return;
    }

    match key.code {
        KeyCode::Up => move_input_cursor_row(input, label, width, -1),
        KeyCode::Down => move_input_cursor_row(input, label, width, 1),
        _ => {}
    }
}

fn clamp_to_char_boundary(text: &str, idx: usize) -> usize {
    let mut idx = idx.min(text.len());
    while idx > 0 && !text.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

#[cfg(test)]
fn previous_char_boundary(text: &str, idx: usize) -> usize {
    let idx = clamp_to_char_boundary(text, idx);
    text[..idx]
        .char_indices()
        .last()
        .map(|(char_idx, _)| char_idx)
        .unwrap_or(0)
}

#[cfg(test)]
fn next_char_boundary(text: &str, idx: usize) -> usize {
    let idx = clamp_to_char_boundary(text, idx);
    if idx >= text.len() {
        return text.len();
    }

    text[idx..]
        .char_indices()
        .nth(1)
        .map(|(offset, _)| idx + offset)
        .unwrap_or(text.len())
}

fn board_display_name(root: &Path, board_dir: &Path) -> String {
    let tasks_dir = get_tasks_dir(root);
    if board_dir == tasks_dir {
        return project_display_name(root);
    }

    let relative = board_dir.strip_prefix(&tasks_dir).unwrap_or(board_dir);
    let parts: Vec<String> = relative
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(name) => name.to_str().map(|name| {
                title_from_path(Path::new(strip_order_prefix(name)))
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            }),
            _ => None,
        })
        .collect();

    if parts.is_empty() {
        project_display_name(root)
    } else {
        format!("{} / {}", project_display_name(root), parts.join(" / "))
    }
}

fn tui_view(root: &Path) -> Result<()> {
    // Setup terminal
    let title = app_title(root);
    let _terminal_session = TerminalSession::enter(&title)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    let mut board_stack = vec![get_tasks_dir(root)];

    let mut current_mode = Mode::View;
    let mut task_input = Input::default();
    let mut feedback_buffer = String::from(
        "Kanban View! Enter opens subtasks or edits, Backspace returns to parent, Space creates a task, Shift+Arrows or I/K reorder, Shift+Arrows or J/L move tasks, 'd' deletes, 'q' quits.",
    );

    let mut selected_board = 0; // 0: todo, 1: doing, 2: done
    let mut editing_task_idx: Option<usize> = None;
    let mut board_states = [
        ListState::default(),
        ListState::default(),
        ListState::default(),
    ];
    let mut board_scroll_offsets = [0usize; 3];

    let statuses = TASK_STATUSES;
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
        let board_dir = board_stack
            .last()
            .cloned()
            .unwrap_or_else(|| get_tasks_dir(root));
        normalize_board_selections_in_board(&board_dir, &statuses, &mut board_states);

        terminal.draw(|f| {
            let size = f.area();
            let console_title = format!("{} Console", board_display_name(root, &board_dir));

            // Calculate input height if in Input or Edit mode
            let input_height =
                if matches!(current_mode, Mode::Input) || matches!(current_mode, Mode::Edit) {
                    let label = if matches!(current_mode, Mode::Input) {
                        " Add Task: "
                    } else {
                        " Edit Task: "
                    };
                    let full_text = format!("{}{}", label, task_input.value());
                    // Subtract 2 for the borders of the block
                    let available_width = size.width.saturating_sub(2) as usize;
                    let wrapped = wrap_input_text(&full_text, available_width);
                    let lines = wrapped.lines().count();
                    let cursor_idx =
                        label.len() + byte_index_at_char(task_input.value(), task_input.cursor());
                    let cursor_row =
                        input_cursor_offset_at(&full_text, available_width, cursor_idx).1 as usize;
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
                let entries = read_task_entries(&board_dir, status).unwrap_or_default();
                let tasks: Vec<String> = entries
                    .iter()
                    .map(|entry| format!("- {}", task_display_text(entry)))
                    .collect();
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
                keep_selected_task_visible(
                    &tasks,
                    selected_idx,
                    &mut board_scroll_offsets[i],
                    inner_area.height as usize,
                    col_width,
                );

                let mut current_y = 0;
                for (idx, (t, entry)) in tasks
                    .iter()
                    .zip(entries.iter())
                    .enumerate()
                    .skip(board_scroll_offsets[i])
                {
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

                    let mut wrapped_content = if is_selected {
                        wrap_text(desc, col_width.saturating_sub(5))
                    } else {
                        desc.to_string()
                    };
                    if entry.has_subtasks {
                        wrapped_content.push_str(" >");
                    }

                    let line_count = wrapped_content.lines().count().max(1);
                    if current_y >= inner_area.height as usize {
                        break;
                    }

                    let visible_height =
                        (line_count as u16).min(inner_area.height.saturating_sub(current_y as u16));
                    let item_area = ratatui::layout::Rect {
                        x: inner_area.x,
                        y: inner_area.y + current_y as u16,
                        width: inner_area.width,
                        height: visible_height,
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
                let input_text = format!("{}{}", label, task_input.value());
                // Subtract 2 for the borders of the block
                let available_width = size.width.saturating_sub(2) as usize;
                let wrapped_input = wrap_input_text(&input_text, available_width);
                let input_paragraph = Paragraph::new(wrapped_input.as_str())
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title("Input Mode (Enter to save, Esc to cancel)"),
                    )
                    .style(Style::default().fg(Color::White));
                f.render_widget(input_paragraph, main_layout[1]);

                let (cursor_x, cursor_y) = input_cursor_offset_at(
                    &input_text,
                    available_width,
                    label.len() + byte_index_at_char(task_input.value(), task_input.cursor()),
                );
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
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(console_title.as_str()),
                )
                .style(Style::default().fg(Color::Gray));

            // The feedback area is always the last element of main_layout
            let feedback_area = *main_layout.last().unwrap();
            f.render_widget(feedback_paragraph, feedback_area);

            if matches!(current_mode, Mode::Help) {
                let help_text = "TUI Commands:\n\n\
                                 [Space]        - Create new task\n\
                                 [Enter]        - Open subtasks or edit selected task / Save input\n\
                                 [e]            - Edit selected task\n\
                                 [Backspace]    - Return to parent board\n\
                                 [d/Del]        - Delete selected task\n\
                                 [Arrows]       - Navigate boards and tasks\n\
                                 [Shift+Arrows] - Reorder/Move tasks\n\
                                 [I, K]         - Move task Up/Down\n\
                                 [J, L]         - Move task Left/Right\n\
                                 [1, 2, 3]      - Switch board focus\n\
                                 [Input Arrows]         - Move cursor in wrapped input\n\
                                 [Ctrl/Alt+Left/Right]  - Jump input cursor by word\n\
                                 [Ctrl+A/E/W/U/K]       - Edit input line\n\
                                 [h / ?]        - Toggle Help\n\
                                 [q]            - Quit";

                let area = f.area();
                let popover_width = area.width.min(70);
                let popover_height = area.height.min(18);
                let x = area.width.saturating_sub(popover_width) / 2;
                let y = area.height.saturating_sub(popover_height) / 2;

                let popover_area = ratatui::layout::Rect {
                    x,
                    y,
                    width: popover_width,
                    height: popover_height,
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
                let input_available_width = terminal.size()?.width.saturating_sub(2) as usize;
                match current_mode {
                    Mode::View => {
                        if key.modifiers.contains(KeyModifiers::SHIFT) {
                            match key.code {
                                KeyCode::Up => {
                                    if let Some(idx) = selected_task_index_in_board(
                                        &board_dir,
                                        statuses[selected_board],
                                        &board_states[selected_board],
                                    ) {
                                        if idx > 0 {
                                            match reorder_task_in_board(
                                                &board_dir,
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
                                            board_states[selected_board].select(Some(idx - 1));
                                        } else {
                                            feedback_buffer = "Already at the top".to_string();
                                        }
                                    } else {
                                        board_states[selected_board].select(None);
                                        feedback_buffer = "No task selected".to_string();
                                    }
                                }
                                KeyCode::Down => {
                                    if let Some(idx) = selected_task_index_in_board(
                                        &board_dir,
                                        statuses[selected_board],
                                        &board_states[selected_board],
                                    ) {
                                        let tasks = read_tasks_in_board(
                                            &board_dir,
                                            statuses[selected_board],
                                        )
                                        .unwrap_or_default();
                                        if tasks.is_empty() {
                                            board_states[selected_board].select(None);
                                            feedback_buffer = "No task selected".to_string();
                                        } else if idx < tasks.len() - 1 {
                                            match reorder_task_in_board(
                                                &board_dir,
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
                                            board_states[selected_board].select(Some(idx + 1));
                                        } else {
                                            feedback_buffer = "Already at the bottom".to_string();
                                        }
                                    } else {
                                        board_states[selected_board].select(None);
                                        feedback_buffer = "No task selected".to_string();
                                    }
                                }
                                KeyCode::Left => {
                                    if let Some(idx) = selected_task_index_in_board(
                                        &board_dir,
                                        statuses[selected_board],
                                        &board_states[selected_board],
                                    ) {
                                        if selected_board > 0 {
                                            let to_board = selected_board - 1;
                                            let from = statuses[selected_board];
                                            let to = statuses[to_board];
                                            match move_task_in_board(
                                                &board_dir,
                                                from,
                                                to,
                                                &(idx + 1).to_string(),
                                            ) {
                                                Ok(_) => {
                                                    selected_board = to_board;
                                                    for state in board_states.iter_mut() {
                                                        state.select(None);
                                                    }
                                                    select_last_task_if_present_in_board(
                                                        &board_dir,
                                                        to,
                                                        &mut board_states[selected_board],
                                                    );
                                                    feedback_buffer =
                                                        format!("Moved task to {}", to)
                                                }
                                                Err(e) => feedback_buffer = format!("Error: {}", e),
                                            }
                                        } else {
                                            feedback_buffer =
                                                "Already at the first board".to_string();
                                        }
                                    } else {
                                        board_states[selected_board].select(None);
                                        feedback_buffer = "No task selected".to_string();
                                    }
                                }
                                KeyCode::Right => {
                                    if let Some(idx) = selected_task_index_in_board(
                                        &board_dir,
                                        statuses[selected_board],
                                        &board_states[selected_board],
                                    ) {
                                        if selected_board < 2 {
                                            let to_board = selected_board + 1;
                                            let from = statuses[selected_board];
                                            let to = statuses[to_board];
                                            match move_task_in_board(
                                                &board_dir,
                                                from,
                                                to,
                                                &(idx + 1).to_string(),
                                            ) {
                                                Ok(_) => {
                                                    selected_board = to_board;
                                                    for state in board_states.iter_mut() {
                                                        state.select(None);
                                                    }
                                                    select_last_task_if_present_in_board(
                                                        &board_dir,
                                                        to,
                                                        &mut board_states[selected_board],
                                                    );
                                                    feedback_buffer =
                                                        format!("Moved task to {}", to)
                                                }
                                                Err(e) => feedback_buffer = format!("Error: {}", e),
                                            }
                                        } else {
                                            feedback_buffer =
                                                "Already at the last board".to_string();
                                        }
                                    } else {
                                        board_states[selected_board].select(None);
                                        feedback_buffer = "No task selected".to_string();
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
                                KeyCode::Backspace => {
                                    if board_stack.len() > 1 {
                                        board_stack.pop();
                                        selected_board = 0;
                                        for state in board_states.iter_mut() {
                                            state.select(None);
                                        }
                                        let parent_board = board_stack
                                            .last()
                                            .cloned()
                                            .unwrap_or_else(|| get_tasks_dir(root));
                                        select_first_task_if_present_in_board(
                                            &parent_board,
                                            statuses[selected_board],
                                            &mut board_states[selected_board],
                                        );
                                        feedback_buffer = "Returned to parent board".to_string();
                                    } else {
                                        feedback_buffer = "Already at the top board".to_string();
                                    }
                                }
                                KeyCode::Enter => {
                                    if let Some((idx, entry)) = selected_task_entry_in_board(
                                        &board_dir,
                                        statuses[selected_board],
                                        &board_states[selected_board],
                                    ) {
                                        match &entry.source {
                                            TaskSource::Path { path, is_dir: true }
                                                if entry.has_subtasks =>
                                            {
                                                ensure_board_store(path)?;
                                                board_stack.push(path.clone());
                                                selected_board = 0;
                                                for state in board_states.iter_mut() {
                                                    state.select(None);
                                                }
                                                select_first_task_if_present_in_board(
                                                    path,
                                                    statuses[selected_board],
                                                    &mut board_states[selected_board],
                                                );
                                                feedback_buffer =
                                                    "Opened subtask board".to_string();
                                            }
                                            _ => {
                                                current_mode = Mode::Edit;
                                                editing_task_idx = Some(idx + 1);
                                                task_input = Input::new(
                                                    entry.content.trim_end().to_string(),
                                                );
                                            }
                                        }
                                    } else {
                                        board_states[selected_board].select(None);
                                        current_mode = Mode::Input;
                                        task_input.reset();
                                    }
                                }
                                KeyCode::Char('e') | KeyCode::Char('E') => {
                                    if let Some((idx, entry)) = selected_task_entry_in_board(
                                        &board_dir,
                                        statuses[selected_board],
                                        &board_states[selected_board],
                                    ) {
                                        current_mode = Mode::Edit;
                                        editing_task_idx = Some(idx + 1);
                                        task_input =
                                            Input::new(entry.content.trim_end().to_string());
                                    } else {
                                        feedback_buffer = "No task selected".to_string();
                                    }
                                }
                                KeyCode::Char(' ') => {
                                    if selected_task_index_in_board(
                                        &board_dir,
                                        statuses[selected_board],
                                        &board_states[selected_board],
                                    )
                                    .is_none()
                                    {
                                        board_states[selected_board].select(None);
                                    }
                                    current_mode = Mode::Input;
                                    task_input.reset();
                                }
                                KeyCode::Char('1') => {
                                    selected_board = 0;
                                    for state in board_states.iter_mut() {
                                        state.select(None);
                                    }
                                    select_first_task_if_present_in_board(
                                        &board_dir,
                                        statuses[selected_board],
                                        &mut board_states[selected_board],
                                    );
                                }
                                KeyCode::Char('2') => {
                                    selected_board = 1;
                                    for state in board_states.iter_mut() {
                                        state.select(None);
                                    }
                                    select_first_task_if_present_in_board(
                                        &board_dir,
                                        statuses[selected_board],
                                        &mut board_states[selected_board],
                                    );
                                }
                                KeyCode::Char('3') => {
                                    selected_board = 2;
                                    for state in board_states.iter_mut() {
                                        state.select(None);
                                    }
                                    select_first_task_if_present_in_board(
                                        &board_dir,
                                        statuses[selected_board],
                                        &mut board_states[selected_board],
                                    );
                                }
                                KeyCode::Char('i') | KeyCode::Char('I') => {
                                    if let Some(idx) = selected_task_index_in_board(
                                        &board_dir,
                                        statuses[selected_board],
                                        &board_states[selected_board],
                                    ) {
                                        if idx > 0 {
                                            match reorder_task_in_board(
                                                &board_dir,
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
                                            board_states[selected_board].select(Some(idx - 1));
                                        } else {
                                            feedback_buffer = "Already at the top".to_string();
                                        }
                                    } else {
                                        board_states[selected_board].select(None);
                                        feedback_buffer = "No task selected".to_string();
                                    }
                                }
                                KeyCode::Char('k') | KeyCode::Char('K') => {
                                    if let Some(idx) = selected_task_index_in_board(
                                        &board_dir,
                                        statuses[selected_board],
                                        &board_states[selected_board],
                                    ) {
                                        let tasks = read_tasks_in_board(
                                            &board_dir,
                                            statuses[selected_board],
                                        )
                                        .unwrap_or_default();
                                        if tasks.is_empty() {
                                            board_states[selected_board].select(None);
                                            feedback_buffer = "No task selected".to_string();
                                        } else if idx < tasks.len() - 1 {
                                            match reorder_task_in_board(
                                                &board_dir,
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
                                            board_states[selected_board].select(Some(idx + 1));
                                        } else {
                                            feedback_buffer = "Already at the bottom".to_string();
                                        }
                                    } else {
                                        board_states[selected_board].select(None);
                                        feedback_buffer = "No task selected".to_string();
                                    }
                                }
                                KeyCode::Char('j') | KeyCode::Char('J') => {
                                    if let Some(idx) = selected_task_index_in_board(
                                        &board_dir,
                                        statuses[selected_board],
                                        &board_states[selected_board],
                                    ) {
                                        if selected_board > 0 {
                                            let to_board = selected_board - 1;
                                            let from = statuses[selected_board];
                                            let to = statuses[to_board];
                                            match move_task_in_board(
                                                &board_dir,
                                                from,
                                                to,
                                                &(idx + 1).to_string(),
                                            ) {
                                                Ok(_) => {
                                                    selected_board = to_board;
                                                    for state in board_states.iter_mut() {
                                                        state.select(None);
                                                    }
                                                    select_last_task_if_present_in_board(
                                                        &board_dir,
                                                        to,
                                                        &mut board_states[selected_board],
                                                    );
                                                    feedback_buffer =
                                                        format!("Moved task to {}", to)
                                                }
                                                Err(e) => feedback_buffer = format!("Error: {}", e),
                                            }
                                        } else {
                                            feedback_buffer =
                                                "Already at the first board".to_string();
                                        }
                                    } else {
                                        board_states[selected_board].select(None);
                                        feedback_buffer = "No task selected".to_string();
                                    }
                                }
                                KeyCode::Char('l') | KeyCode::Char('L') => {
                                    if let Some(idx) = selected_task_index_in_board(
                                        &board_dir,
                                        statuses[selected_board],
                                        &board_states[selected_board],
                                    ) {
                                        if selected_board < 2 {
                                            let to_board = selected_board + 1;
                                            let from = statuses[selected_board];
                                            let to = statuses[to_board];
                                            match move_task_in_board(
                                                &board_dir,
                                                from,
                                                to,
                                                &(idx + 1).to_string(),
                                            ) {
                                                Ok(_) => {
                                                    selected_board = to_board;
                                                    for state in board_states.iter_mut() {
                                                        state.select(None);
                                                    }
                                                    select_last_task_if_present_in_board(
                                                        &board_dir,
                                                        to,
                                                        &mut board_states[selected_board],
                                                    );
                                                    feedback_buffer =
                                                        format!("Moved task to {}", to)
                                                }
                                                Err(e) => feedback_buffer = format!("Error: {}", e),
                                            }
                                        } else {
                                            feedback_buffer =
                                                "Already at the last board".to_string();
                                        }
                                    } else {
                                        board_states[selected_board].select(None);
                                        feedback_buffer = "No task selected".to_string();
                                    }
                                }
                                KeyCode::Char('d') | KeyCode::Char('D') | KeyCode::Delete => {
                                    if let Some(idx) = selected_task_index_in_board(
                                        &board_dir,
                                        statuses[selected_board],
                                        &board_states[selected_board],
                                    ) {
                                        let status = statuses[selected_board];
                                        match delete_task_in_board(
                                            &board_dir,
                                            status,
                                            &(idx + 1).to_string(),
                                        ) {
                                            Ok(_) => {
                                                feedback_buffer = format!(
                                                    "Deleted task {} from {}",
                                                    idx + 1,
                                                    status
                                                );
                                                board_states[selected_board].select(if idx > 0 {
                                                    Some(idx - 1)
                                                } else {
                                                    None
                                                });
                                            }
                                            Err(e) => feedback_buffer = format!("Error: {}", e),
                                        }
                                    } else {
                                        board_states[selected_board].select(None);
                                        feedback_buffer = "No task selected to delete".to_string();
                                    }
                                }
                                KeyCode::Char('h') | KeyCode::Char('H') | KeyCode::Char('?') => {
                                    current_mode = Mode::Help;
                                }
                                KeyCode::Up => {
                                    let state = &mut board_states[selected_board];
                                    let tasks =
                                        read_tasks_in_board(&board_dir, statuses[selected_board])
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
                                    let tasks =
                                        read_tasks_in_board(&board_dir, statuses[selected_board])
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
                                    select_first_task_if_present_in_board(
                                        &board_dir,
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
                                    select_first_task_if_present_in_board(
                                        &board_dir,
                                        statuses[selected_board],
                                        &mut board_states[selected_board],
                                    );
                                }
                                KeyCode::Char(c) if c.is_ascii_digit() => {
                                    let new_pos = (c as u8 - b'0') as usize;
                                    if let Some(idx) = selected_task_index_in_board(
                                        &board_dir,
                                        statuses[selected_board],
                                        &board_states[selected_board],
                                    ) {
                                        if new_pos > 0 {
                                            match reorder_task_in_board(
                                                &board_dir,
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
                                    } else {
                                        board_states[selected_board].select(None);
                                        feedback_buffer = "No task selected".to_string();
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
                            if !task_input.value().trim().is_empty() {
                                let index = selected_task_index_in_board(
                                    &board_dir,
                                    statuses[selected_board],
                                    &board_states[selected_board],
                                )
                                .map(|idx| idx + 1);
                                match insert_task_in_board(
                                    &board_dir,
                                    statuses[selected_board],
                                    index,
                                    task_input.value(),
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
                            task_input.reset();
                        }
                        KeyCode::Esc => {
                            current_mode = Mode::View;
                            task_input.reset();
                        }
                        _ => handle_input_key(
                            &mut task_input,
                            key,
                            " Add Task: ",
                            input_available_width,
                        ),
                    },
                    Mode::Edit => match key.code {
                        KeyCode::Enter => {
                            if !task_input.value().trim().is_empty() {
                                if let Some(idx) = editing_task_idx {
                                    match update_task_in_board(
                                        &board_dir,
                                        statuses[selected_board],
                                        idx,
                                        task_input.value(),
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
                            task_input.reset();
                            editing_task_idx = None;
                        }
                        KeyCode::Esc => {
                            current_mode = Mode::View;
                            task_input.reset();
                            editing_task_idx = None;
                        }
                        _ => handle_input_key(
                            &mut task_input,
                            key,
                            " Edit Task: ",
                            input_available_width,
                        ),
                    },
                }
            }
        }
    }

    Ok(())
}

fn init_tasks(root: &Path, folders: bool) -> Result<()> {
    let tasks_dir = get_tasks_dir(root);
    if !tasks_dir.exists() {
        fs::create_dir_all(&tasks_dir).context("Failed to create tasks directory")?;
        println!("Created directory: {:?}", tasks_dir);
    }

    let directory_mode = folders
        || TASK_STATUSES
            .iter()
            .any(|status| tasks_dir.join(status).is_dir());

    for status in TASK_STATUSES {
        let dir_path = tasks_dir.join(status);
        let file_path = tasks_dir.join(status_filename(status)?);
        if dir_path.is_dir() {
            println!("Directory already exists: {:?}", dir_path);
        } else if file_path.exists() {
            println!("File already exists: {:?}", file_path);
        } else if directory_mode {
            fs::create_dir_all(&dir_path)
                .context(format!("Failed to create directory {:?}", dir_path))?;
            println!("Created directory: {:?}", dir_path);
        } else {
            let mut file = fs::File::create(&file_path)
                .context(format!("Failed to create file {:?}", file_path))?;
            file.write_all(status_header(status)?.as_bytes())
                .context(format!("Failed to write to file {:?}", file_path))?;
            println!("Created file: {:?}", file_path);
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
    fn init_tasks_can_create_folder_backed_statuses() {
        let root = temp_root("init-folders");

        init_tasks(&root, true).unwrap();

        assert!(root.join("tasks/todo").is_dir());
        assert!(root.join("tasks/doing").is_dir());
        assert!(root.join("tasks/done").is_dir());
        assert!(!root.join("tasks/todo.md").exists());

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
    fn expand_tasks_can_expand_one_markdown_status_to_folder() {
        let root = temp_root("expand-one");
        let tasks_dir = root.join("tasks");
        fs::create_dir_all(&tasks_dir).unwrap();
        fs::write(
            tasks_dir.join("todo.md"),
            "# To Do Tasks\n- first task\n- second task\n",
        )
        .unwrap();
        fs::write(tasks_dir.join("doing.md"), "# Doing Tasks\n").unwrap();
        fs::write(tasks_dir.join("done.md"), "# Done Tasks\n").unwrap();

        expand_tasks(&root, Some("todo".to_string())).unwrap();

        assert!(tasks_dir.join("todo").is_dir());
        assert!(tasks_dir.join("todo.md.bak").exists());
        assert!(tasks_dir.join("doing.md").exists());
        let entries = read_task_entries(&tasks_dir, "todo").unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].summary, "first task");
        assert_eq!(entries[1].summary, "second task");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn expand_tasks_without_status_expands_all_statuses() {
        let root = temp_root("expand-all");
        add_task(&root, "todo task", None).unwrap();
        move_task(&root, "todo", "doing", "1").unwrap();

        expand_tasks(&root, None).unwrap();

        assert!(root.join("tasks/todo").is_dir());
        assert!(root.join("tasks/doing").is_dir());
        assert!(root.join("tasks/done").is_dir());
        assert!(root.join("tasks/todo.md.bak").exists());
        assert!(root.join("tasks/doing.md.bak").exists());
        assert!(root.join("tasks/done.md.bak").exists());

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
    fn project_display_name_uses_folder_name_with_root_fallback() {
        assert_eq!(
            project_display_name(Path::new("/Users/pro/code/lls/clt")),
            "clt"
        );
        assert_eq!(project_display_name(Path::new("/")), "/");
    }

    #[test]
    fn app_title_includes_project_name() {
        assert_eq!(
            app_title(Path::new("/Users/pro/code/lls/example")),
            "clt | example"
        );
    }

    #[test]
    fn move_task_writes_destination_and_removes_source() {
        let root = temp_root("move");

        add_task(&root, "ship the fix", None).unwrap();
        move_task(&root, "todo", "doing", "1").unwrap();

        let todo = fs::read_to_string(root.join("tasks/todo.md")).unwrap();
        let doing = fs::read_to_string(root.join("tasks/doing.md")).unwrap();

        assert_eq!(todo, "# To Do Tasks\n");
        assert_eq!(doing, "# Doing Tasks\n- ship the fix\n");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn move_task_to_done_adds_to_top() {
        let root = temp_root("move-done-top");

        add_task(&root, "older done task", None).unwrap();
        add_task(&root, "newer done task", None).unwrap();
        move_task(&root, "todo", "done", "1").unwrap();
        move_task(&root, "todo", "done", "1").unwrap();

        let done = fs::read_to_string(root.join("tasks/done.md")).unwrap();

        assert_eq!(done, "# Done Tasks\n- newer done task\n- older done task\n");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn folder_backed_status_reads_task_files_as_first_sentence() {
        let root = temp_root("folder-read");
        let todo_dir = root.join("tasks/todo");
        fs::create_dir_all(&todo_dir).unwrap();
        fs::write(
            todo_dir.join("write-launch-plan.md"),
            "Write launch plan. Include rollout details and owners.\n\nAdd links later.\n",
        )
        .unwrap();

        let tasks = read_tasks(&root, "todo").unwrap();

        assert_eq!(tasks, vec!["- Write launch plan."]);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn moving_folder_backed_task_preserves_long_file_content() {
        let root = temp_root("folder-move");
        let todo_dir = root.join("tasks/todo");
        fs::create_dir_all(&todo_dir).unwrap();
        fs::write(
            todo_dir.join("research-api.md"),
            "Research the API migration. This file keeps the longer task notes.\n\n- Audit callers\n- Draft rollout\n",
        )
        .unwrap();

        move_task(&root, "todo", "doing", "1").unwrap();

        assert!(directory_task_paths(&todo_dir).unwrap().is_empty());
        let doing_entries = read_task_entries(&root.join("tasks"), "doing").unwrap();
        assert_eq!(doing_entries.len(), 1);
        assert_eq!(doing_entries[0].summary, "Research the API migration.");
        assert!(doing_entries[0].content.contains("Audit callers"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn moving_folder_task_converts_markdown_destination_to_directory() {
        let root = temp_root("folder-convert-dest");
        let tasks_dir = root.join("tasks");
        fs::create_dir_all(tasks_dir.join("todo")).unwrap();
        fs::write(
            tasks_dir.join("todo/long-task.md"),
            "Move this rich task. Preserve all follow-up detail.\n\nSecond paragraph.\n",
        )
        .unwrap();
        fs::write(
            tasks_dir.join("doing.md"),
            "# Doing Tasks\n- existing task\n",
        )
        .unwrap();

        move_task(&root, "todo", "doing", "1").unwrap();

        assert!(tasks_dir.join("doing").is_dir());
        assert!(tasks_dir.join("doing.md.bak").exists());
        let doing_entries = read_task_entries(&tasks_dir, "doing").unwrap();
        assert_eq!(doing_entries.len(), 2);
        assert!(
            doing_entries
                .iter()
                .any(|entry| entry.summary == "existing task")
        );
        assert!(
            doing_entries
                .iter()
                .any(|entry| entry.content.contains("Second paragraph."))
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn folder_task_with_status_stores_is_detected_as_subtask_board() {
        let root = temp_root("folder-subtasks");
        let epic_dir = root.join("tasks/doing/ship-epic");
        fs::create_dir_all(&epic_dir).unwrap();
        fs::write(epic_dir.join("task.md"), "Ship epic. Parent task detail.\n").unwrap();
        fs::write(epic_dir.join("todo.md"), "# To Do Tasks\n- draft spec\n").unwrap();
        fs::write(epic_dir.join("doing.md"), "# Doing Tasks\n").unwrap();
        fs::write(epic_dir.join("done.md"), "# Done Tasks\n").unwrap();

        let entries = read_task_entries(&root.join("tasks"), "doing").unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].summary, "Ship epic.");
        assert!(entries[0].has_subtasks);
        assert_eq!(
            read_tasks_in_board(&epic_dir, "todo").unwrap(),
            vec!["- draft spec"]
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn selected_task_ignores_stale_selection() {
        let root = temp_root("stale-selection");
        ensure_task_store(&root).unwrap();

        let mut state = ListState::default();
        state.select(Some(0));

        assert_eq!(selected_task(&root, "todo", &state), None);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn selected_task_index_ignores_stale_selection() {
        let root = temp_root("stale-index");
        add_task(&root, "only task", None).unwrap();

        let mut state = ListState::default();
        state.select(Some(1));

        assert_eq!(selected_task_index(&root, "todo", &state), None);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn normalize_board_selection_clears_empty_board_selection() {
        let root = temp_root("normalize-empty");
        ensure_task_store(&root).unwrap();

        let mut state = ListState::default();
        state.select(Some(0));

        normalize_board_selection(&root, "todo", &mut state);

        assert_eq!(state.selected(), None);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn normalize_board_selection_clamps_out_of_range_selection() {
        let root = temp_root("normalize-range");
        add_task(&root, "only task", None).unwrap();

        let mut state = ListState::default();
        state.select(Some(4));

        normalize_board_selection(&root, "todo", &mut state);

        assert_eq!(state.selected(), Some(0));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn keep_selected_task_visible_scrolls_down_to_selection() {
        let tasks = vec![
            "- task one".to_string(),
            "- task two".to_string(),
            "- task three".to_string(),
            "- task four".to_string(),
        ];
        let mut scroll_offset = 0;

        keep_selected_task_visible(&tasks, Some(3), &mut scroll_offset, 3, 20);

        assert_eq!(scroll_offset, 1);
    }

    #[test]
    fn keep_selected_task_visible_scrolls_up_to_selection() {
        let tasks = vec![
            "- task one".to_string(),
            "- task two".to_string(),
            "- task three".to_string(),
        ];
        let mut scroll_offset = 2;

        keep_selected_task_visible(&tasks, Some(0), &mut scroll_offset, 3, 20);

        assert_eq!(scroll_offset, 0);
    }

    #[test]
    fn input_cursor_offset_tracks_cursor_inside_wrapped_text() {
        let text = " Add Task: hello world";

        assert_eq!(wrap_input_text(text, 10), " Add Task:\n hello wor\nld");
        assert_eq!(
            input_cursor_offset_at(text, 10, " Add Task: hello".len()),
            (6, 1)
        );
        assert_eq!(input_cursor_offset_at(text, 10, text.len()), (2, 2));
    }

    #[test]
    fn input_cursor_helpers_preserve_utf8_boundaries() {
        let text = "aéb";
        let inside_e = 2;

        assert_eq!(clamp_to_char_boundary(text, inside_e), 1);
        assert_eq!(previous_char_boundary(text, text.len()), 3);
        assert_eq!(previous_char_boundary(text, 3), 1);
        assert_eq!(next_char_boundary(text, 1), 3);
        assert_eq!(next_char_boundary(text, 3), text.len());
    }

    #[test]
    fn input_key_handler_moves_and_edits_by_words() {
        let mut input = Input::new("first second third".to_string());

        handle_input_key(
            &mut input,
            crossterm::event::KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL),
            " Add Task: ",
            80,
        );
        handle_input_key(
            &mut input,
            crossterm::event::KeyEvent::new(KeyCode::Char('X'), KeyModifiers::NONE),
            " Add Task: ",
            80,
        );

        assert_eq!(input.value(), "first second Xthird");

        handle_input_key(
            &mut input,
            crossterm::event::KeyEvent::new(KeyCode::Backspace, KeyModifiers::CONTROL),
            " Add Task: ",
            80,
        );

        assert_eq!(input.value(), "first second third");
    }

    #[test]
    fn input_key_handler_moves_vertically_through_wrapped_input() {
        let mut input = Input::new("hello world".to_string());

        handle_input_key(
            &mut input,
            crossterm::event::KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
            " Add Task: ",
            10,
        );

        assert_eq!(input.cursor(), 1);

        handle_input_key(
            &mut input,
            crossterm::event::KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            " Add Task: ",
            10,
        );

        assert_eq!(input.cursor(), input.value().chars().count());
    }
}
