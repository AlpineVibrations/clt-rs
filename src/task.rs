use anyhow::{Context, Result};
use std::{
    ffi::OsStr,
    fmt, fs,
    io::Write,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum TaskStatus {
    Todo,
    Doing,
    Done,
    Backlog,
}

impl TaskStatus {
    pub(super) const ALL: [Self; 4] = [Self::Todo, Self::Doing, Self::Done, Self::Backlog];
    pub(super) const SESSION_SEARCH_ORDER: [Self; 4] =
        [Self::Doing, Self::Todo, Self::Backlog, Self::Done];

    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Todo => "todo",
            Self::Doing => "doing",
            Self::Done => "done",
            Self::Backlog => "backlog",
        }
    }

    pub(super) const fn filename(self) -> &'static str {
        match self {
            Self::Todo => "todo.md",
            Self::Doing => "doing.md",
            Self::Done => "done.md",
            Self::Backlog => "backlog.md",
        }
    }

    pub(super) const fn header(self) -> &'static str {
        match self {
            Self::Todo => "# To Do Tasks\n",
            Self::Doing => "# Doing Tasks\n",
            Self::Done => "# Done Tasks\n",
            Self::Backlog => "# Backlog Tasks\n",
        }
    }

    pub(super) fn parse(value: &str) -> Result<Self> {
        match value {
            "backlog" => Ok(Self::Backlog),
            "todo" => Ok(Self::Todo),
            "doing" => Ok(Self::Doing),
            "done" => Ok(Self::Done),
            _ => anyhow::bail!("Invalid status. Use 'backlog', 'todo', 'doing', or 'done'."),
        }
    }

    pub(super) fn parse_arg(value: &str) -> Result<Self> {
        match value {
            "0" => Ok(Self::Backlog),
            "1" => Ok(Self::Todo),
            "2" => Ok(Self::Doing),
            "3" => Ok(Self::Done),
            value => Self::parse(value),
        }
    }

    pub(super) const fn is_active(self) -> bool {
        matches!(self, Self::Todo | Self::Doing)
    }
}

impl fmt::Display for TaskStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

pub(super) const TASK_STATUSES: [TaskStatus; 4] = TaskStatus::ALL;
pub(super) const TASK_DETAIL_FILES: [&str; 3] = ["task.md", "README.md", "index.md"];
const ARCHIVE_STATUS_CANDIDATES: [&str; 2] = ["archived", "archive"];
const BOARD_MUTATION_LOCK_TIMEOUT_MILLIS: u64 = 10_000;
const BOARD_MUTATION_LOCK_RETRY_MILLIS: u64 = 10;
pub(super) const CODEX_TASK_SESSION_PREFIX: &str = "codex:";

#[derive(Clone, Debug)]
pub(super) struct TaskEntry {
    pub(super) source: TaskSource,
    pub(super) summary: String,
    pub(super) content: String,
    pub(super) metadata: Option<String>,
    pub(super) has_subtasks: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum TaskSource {
    MarkdownLine { line_index: usize },
    Path { path: PathBuf, is_dir: bool },
}

#[derive(Clone, Debug)]
pub(super) enum StatusStore {
    MarkdownFile(PathBuf),
    Directory(PathBuf),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct TaskBoard {
    directory: PathBuf,
}

impl TaskBoard {
    pub(super) fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }

    pub(super) fn for_project(root: &Path) -> Self {
        Self::new(get_tasks_dir(root))
    }

    pub(super) fn path(&self) -> &Path {
        &self.directory
    }

    pub(super) fn entries(&self, status: TaskStatus) -> Result<Vec<TaskEntry>> {
        read_task_entries(self.path(), status)
    }

    pub(super) fn entry(&self, status: TaskStatus, task_index: usize) -> Result<TaskEntry> {
        task_entry_at(self.path(), status, task_index)
    }

    pub(super) fn status_store(&self, status: TaskStatus) -> Result<StatusStore> {
        get_status_store(self.path(), status)
    }

    pub(super) fn insert_content(
        &self,
        status: TaskStatus,
        index: Option<usize>,
        content: &str,
    ) -> Result<()> {
        insert_task_content(self.path(), status, index, content)
    }

    pub(super) fn remove_entry(&self, status: TaskStatus, entry: &TaskEntry) -> Result<()> {
        remove_task_entry(self.path(), status, entry)
    }

    pub(super) fn remove_entry_without_reordering(
        &self,
        status: TaskStatus,
        entry: &TaskEntry,
    ) -> Result<()> {
        remove_task_entry_without_reordering(self.path(), status, entry)
    }

    pub(super) fn write_entry_content(
        &self,
        status: TaskStatus,
        entry: &TaskEntry,
        content: &str,
    ) -> Result<()> {
        write_task_entry_content(self.path(), status, entry, content)
    }

    pub(super) fn move_task_after_lock(
        &self,
        from: TaskStatus,
        to: TaskStatus,
        task_index: usize,
    ) -> Result<()> {
        move_task_in_board_after_lock(self.path(), from, to, task_index)
    }

    pub(super) fn move_task_without_reordering_after_lock(
        &self,
        from: TaskStatus,
        to: TaskStatus,
        task_index: usize,
    ) -> Result<()> {
        move_task_without_reordering_after_lock(self.path(), from, to, task_index)
    }

    pub(super) fn terminal_task_for_session(
        &self,
        session_id: &str,
    ) -> Result<Option<(TaskStatus, TaskEntry)>> {
        terminal_task_for_codex_session_in_board(self.path(), session_id)
    }
}

pub(super) enum ExpansionSummary {
    AlreadyDirectory {
        status: TaskStatus,
        dir: PathBuf,
    },
    Expanded {
        status: TaskStatus,
        dir: PathBuf,
        backup: PathBuf,
        task_count: usize,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TaskBlockingState {
    Blocked,
    Unblocked,
}

pub(super) fn task_entry_is_blocked(entry: &TaskEntry) -> bool {
    task_content_is_blocked(&entry.content)
}

pub(super) fn task_content_is_blocked(content: &str) -> bool {
    let mut state = None;

    for line in content.lines() {
        let heading = line.trim().trim_start_matches(['#', '-', '*', ' ']).trim();
        if heading.eq_ignore_ascii_case("blocked note:") {
            state = Some(TaskBlockingState::Blocked);
        }
        if let Some(line_state) = latest_task_blocking_state_on_line(line) {
            state = Some(line_state);
        }
    }

    state == Some(TaskBlockingState::Blocked)
}

pub(super) fn follow_up_session(content: &str) -> Option<&str> {
    let marker = content.split_whitespace().next_back()?;
    let session_id = marker.strip_prefix("clt-follow-up:")?;
    (!session_id.is_empty()
        && session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        && content.matches("clt-follow-up:").count() == 1
        && codex_session_markers_in_task_content(content).is_empty()
        && durable_task_identity(content).is_some())
    .then_some(session_id)
}

pub(super) fn follow_up_matches_status(content: &str, status: TaskStatus) -> bool {
    match status {
        TaskStatus::Todo => !task_content_is_blocked(content),
        TaskStatus::Doing => {
            task_content_is_blocked(content)
                && content.lines().any(|line| {
                    latest_task_blocking_state_on_line(line) == Some(TaskBlockingState::Blocked)
                })
        }
        _ => false,
    }
}

pub(super) fn latest_task_blocking_state_on_line(line: &str) -> Option<TaskBlockingState> {
    let uppercase = line.to_ascii_uppercase();
    let mut latest = None;

    for (marker, state) in [
        ("BLOCKED ", TaskBlockingState::Blocked),
        ("UNBLOCKED ", TaskBlockingState::Unblocked),
        ("COMPLETED ", TaskBlockingState::Unblocked),
    ] {
        for (index, matched) in uppercase.match_indices(marker) {
            let has_word_boundary = uppercase[..index]
                .chars()
                .next_back()
                .is_none_or(|ch| !ch.is_ascii_alphanumeric() && ch != '_');
            let remainder = &uppercase.as_bytes()[index + matched.len()..];

            if has_word_boundary
                && starts_with_task_note_date(remainder)
                && latest.is_none_or(|(latest_index, _)| index > latest_index)
            {
                latest = Some((index, state));
            }
        }
    }

    latest.map(|(_, state)| state)
}

pub(super) fn starts_with_task_note_date(value: &[u8]) -> bool {
    value.len() >= 11
        && value[0..4].iter().all(u8::is_ascii_digit)
        && value[4] == b'-'
        && value[5..7].iter().all(u8::is_ascii_digit)
        && value[7] == b'-'
        && value[8..10].iter().all(u8::is_ascii_digit)
        && value[10] == b':'
}

pub(super) fn get_tasks_dir(root: &Path) -> std::path::PathBuf {
    root.join("tasks")
}

pub(super) struct BoardMutationLock {
    _file: fs::File,
}

pub(super) fn board_mutation_lock_path(board_dir: &Path) -> Result<PathBuf> {
    let lock_scope = board_dir
        .ancestors()
        .find(|ancestor| ancestor.file_name() == Some(OsStr::new("tasks")))
        .unwrap_or(board_dir);
    let lock_scope = if lock_scope.is_absolute() {
        lock_scope.to_path_buf()
    } else {
        std::env::current_dir()
            .context("Failed to resolve the current directory for task board locking")?
            .join(lock_scope)
    };
    let mut existing_ancestor = lock_scope.as_path();
    let mut missing_components = Vec::new();
    while !existing_ancestor.exists() {
        let component = existing_ancestor.file_name().with_context(|| {
            format!("Task board path {:?} has no existing ancestor", lock_scope)
        })?;
        missing_components.push(component.to_os_string());
        existing_ancestor = existing_ancestor.parent().with_context(|| {
            format!("Task board path {:?} has no existing ancestor", lock_scope)
        })?;
    }
    let mut canonical_lock_scope = fs::canonicalize(existing_ancestor).with_context(|| {
        format!(
            "Failed to resolve existing ancestor {:?} for task board {:?}",
            existing_ancestor, lock_scope
        )
    })?;
    for component in missing_components.into_iter().rev() {
        canonical_lock_scope.push(component);
    }

    // FNV-1a keeps the lock name stable across CLT versions and avoids placing
    // an untracked coordination file inside the user's project. Nested boards
    // share the outer tasks/ lock so parent moves and recursive marker scans are
    // part of the same project-level mutation boundary.
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in canonical_lock_scope.as_os_str().as_encoded_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }

    Ok(std::env::temp_dir().join(format!("clt-board-mutation-{hash:016x}.lock")))
}

pub(super) fn acquire_board_mutation_lock(board_dir: &Path) -> Result<BoardMutationLock> {
    acquire_board_mutation_lock_with_contention_callback(board_dir, || {})
}

pub(super) fn acquire_board_mutation_lock_with_contention_callback(
    board_dir: &Path,
    on_contention: impl FnOnce(),
) -> Result<BoardMutationLock> {
    let lock_path = board_mutation_lock_path(board_dir)?;
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("Failed to open task board mutation lock {:?}", lock_path))?;
    let deadline = Instant::now() + Duration::from_millis(BOARD_MUTATION_LOCK_TIMEOUT_MILLIS);
    let mut on_contention = Some(on_contention);

    loop {
        match file.try_lock() {
            Ok(()) => return Ok(BoardMutationLock { _file: file }),
            Err(fs::TryLockError::WouldBlock) => {
                if let Some(on_contention) = on_contention.take() {
                    on_contention();
                }
                if Instant::now() >= deadline {
                    anyhow::bail!(
                        "Timed out waiting for another CLT task board update to finish in {:?}",
                        board_dir
                    );
                }
                thread::sleep(Duration::from_millis(BOARD_MUTATION_LOCK_RETRY_MILLIS));
            }
            Err(fs::TryLockError::Error(error)) => {
                return Err(error).with_context(|| {
                    format!("Failed to lock task board for update in {:?}", board_dir)
                });
            }
        }
    }
}

pub(super) fn ensure_existing_board(root: &Path) -> Result<bool> {
    let tasks_dir = get_tasks_dir(root);
    if !tasks_dir.is_dir() || !board_has_any_status_store(&tasks_dir) {
        return Ok(false);
    }

    let _mutation_lock = acquire_board_mutation_lock(&tasks_dir)?;
    ensure_board_store(&tasks_dir)?;
    Ok(true)
}

#[cfg(test)]
pub(super) fn ensure_task_store(root: &Path) -> Result<()> {
    ensure_board_store(&get_tasks_dir(root))
}

pub(super) fn status_filename(status: TaskStatus) -> &'static str {
    status.filename()
}

pub(super) fn normalize_status_arg(status: &str) -> Result<TaskStatus> {
    TaskStatus::parse_arg(status)
}

pub(super) fn status_header(status: TaskStatus) -> &'static str {
    status.header()
}

pub(super) fn status_store_exists(board_dir: &Path, status: TaskStatus) -> bool {
    board_dir.join(status.as_str()).is_dir() || board_dir.join(status.filename()).is_file()
}

pub(super) fn ensure_board_store(board_dir: &Path) -> Result<()> {
    fs::create_dir_all(board_dir).context("Failed to create tasks directory")?;
    let directory_mode = TASK_STATUSES
        .iter()
        .any(|status| board_dir.join(status.as_str()).is_dir());

    for status in TASK_STATUSES {
        let dir_path = board_dir.join(status.as_str());
        let file_path = board_dir.join(status_filename(status));
        if dir_path.is_dir() || file_path.exists() {
            continue;
        }

        if directory_mode {
            fs::create_dir_all(&dir_path)
                .context(format!("Failed to create directory {:?}", dir_path))?;
        } else {
            fs::write(&file_path, status_header(status))
                .context(format!("Failed to create file {:?}", file_path))?;
        }
    }

    Ok(())
}

pub(super) fn get_status_store(board_dir: &Path, status: TaskStatus) -> Result<StatusStore> {
    ensure_board_store(board_dir)?;

    let dir_path = board_dir.join(status.as_str());
    if dir_path.is_dir() {
        return Ok(StatusStore::Directory(dir_path));
    }

    Ok(StatusStore::MarkdownFile(
        board_dir.join(status_filename(status)),
    ))
}

pub(super) fn get_archive_status_store(board_dir: &Path) -> Option<StatusStore> {
    ARCHIVE_STATUS_CANDIDATES.iter().find_map(|status| {
        let dir_path = board_dir.join(status);
        if dir_path.is_dir() {
            return Some(StatusStore::Directory(dir_path));
        }

        let file_path = board_dir.join(format!("{status}.md"));
        if file_path.is_file() {
            Some(StatusStore::MarkdownFile(file_path))
        } else {
            None
        }
    })
}

pub(super) fn get_or_create_archive_status_store(board_dir: &Path) -> Result<StatusStore> {
    if let Some(store) = get_archive_status_store(board_dir) {
        return Ok(store);
    }

    let archive_dir = board_dir.join(ARCHIVE_STATUS_CANDIDATES[0]);
    fs::create_dir_all(&archive_dir)
        .with_context(|| format!("Failed to create archive directory {:?}", archive_dir))?;
    Ok(StatusStore::Directory(archive_dir))
}

// find_task_status is no longer needed for index-based referencing
// as the user must specify the source list.

pub(super) fn read_task_entries(board_dir: &Path, status: TaskStatus) -> Result<Vec<TaskEntry>> {
    match get_status_store(board_dir, status)? {
        StatusStore::MarkdownFile(path) => read_markdown_entries(&path),
        StatusStore::Directory(path) => read_directory_entries(&path),
    }
}

pub(super) fn read_archived_task_entries(board_dir: &Path) -> Result<Vec<TaskEntry>> {
    match get_archive_status_store(board_dir) {
        Some(StatusStore::MarkdownFile(path)) => read_markdown_entries(&path),
        Some(StatusStore::Directory(path)) => read_directory_entries(&path),
        None => Ok(Vec::new()),
    }
}

pub(super) fn read_markdown_entries(path: &Path) -> Result<Vec<TaskEntry>> {
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

pub(super) fn read_directory_entries(path: &Path) -> Result<Vec<TaskEntry>> {
    let paths = directory_task_paths(path)?;

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

pub(super) fn task_entry_from_text(
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

pub(super) fn board_has_any_status_store(board_dir: &Path) -> bool {
    TASK_STATUSES
        .iter()
        .any(|status| status_store_exists(board_dir, *status))
}

pub(super) fn directory_task_paths(path: &Path) -> Result<Vec<PathBuf>> {
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
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        // Managed completions prepend without renaming any existing task.
        (front_order_rank(&name).is_none(), name)
    });

    Ok(paths)
}

pub(super) fn read_directory_task_content(path: &Path) -> Option<String> {
    TASK_DETAIL_FILES.iter().find_map(|filename| {
        let detail_path = path.join(filename);
        fs::read_to_string(detail_path).ok()
    })
}

pub(super) fn directory_task_detail_path(path: &Path) -> PathBuf {
    TASK_DETAIL_FILES
        .iter()
        .map(|filename| path.join(filename))
        .find(|path| path.exists())
        .unwrap_or_else(|| path.join(TASK_DETAIL_FILES[0]))
}

pub(super) fn title_from_path(path: &Path) -> String {
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

pub(super) fn strip_order_prefix(name: &str) -> &str {
    if front_order_rank(name).is_some() {
        return &name[26..];
    }
    let bytes = name.as_bytes();
    if bytes.len() > 5 && bytes[..4].iter().all(|byte| byte.is_ascii_digit()) && bytes[4] == b'-' {
        &name[5..]
    } else {
        name
    }
}

fn front_order_rank(name: &str) -> Option<u64> {
    let bytes = name.as_bytes();
    (bytes.len() > 26
        && bytes.starts_with(b"0000-")
        && bytes[5..25].iter().all(u8::is_ascii_digit)
        && bytes[25] == b'-')
        .then(|| name[5..25].parse().ok())
        .flatten()
}

fn task_name_without_reordering(path: &Path, name: &str, prepend: bool) -> Result<String> {
    let paths = directory_task_paths(path)?;
    if !prepend {
        return Ok(format!("{:04}-{name}", paths.len() + 1));
    }
    // Reserve a descending rank ahead of legacy four-digit order prefixes.
    // Only the new task changes; sealed Git manifests retain every other path.
    let rank = paths
        .iter()
        .filter_map(|path| path.file_name()?.to_str().and_then(front_order_rank))
        .min()
        .unwrap_or(u64::MAX)
        .checked_sub(1)
        .context("No remaining space to prepend a task without reordering")?;
    Ok(format!("0000-{rank:020}-{name}"))
}

pub(super) fn first_sentence(content: &str) -> Option<String> {
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

pub(super) fn codex_session_id_from_task_content(content: &str) -> Option<&str> {
    let marker = content.split_whitespace().next_back()?;
    let session_id = marker.strip_prefix(CODEX_TASK_SESSION_PREFIX)?;

    (!session_id.is_empty()
        && session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'))
    .then_some(session_id)
}

pub(super) fn codex_session_markers_in_task_content(content: &str) -> Vec<(usize, usize, &str)> {
    content
        .match_indices(CODEX_TASK_SESSION_PREFIX)
        .filter_map(|(start, _)| {
            if start > 0
                && !content[..start]
                    .chars()
                    .next_back()
                    .is_some_and(char::is_whitespace)
            {
                return None;
            }

            let marker = &content[start..];
            let end = marker
                .char_indices()
                .find(|(_, ch)| ch.is_whitespace())
                .map(|(offset, _)| start + offset)
                .unwrap_or(content.len());
            let session_id = content[start..end].strip_prefix(CODEX_TASK_SESSION_PREFIX)?;
            (!session_id.is_empty()
                && session_id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'))
            .then_some((start, end, session_id))
        })
        .collect()
}

pub(super) fn recoverable_codex_session_id_from_task_content(content: &str) -> Option<&str> {
    if let Some(session_id) = codex_session_id_from_task_content(content) {
        return Some(session_id);
    }

    let mut markers = codex_session_markers_in_task_content(content)
        .into_iter()
        .filter(|(_, end, session_id)| {
            codex_session_id_is_uuid(session_id)
                || content[*end..].starts_with('\r')
                || content[*end..].starts_with('\n')
        });
    let (_, _, session_id) = markers.next()?;
    markers
        .all(|(_, _, candidate)| candidate == session_id)
        .then_some(session_id)
}

pub(super) fn codex_session_id_is_uuid(session_id: &str) -> bool {
    session_id.len() == 36
        && session_id.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

pub(super) fn task_content_without_codex_session(content: &str) -> &str {
    let trimmed = content.trim_end();
    let Some(session_id) = codex_session_id_from_task_content(trimmed) else {
        return trimmed;
    };
    let marker_len = CODEX_TASK_SESSION_PREFIX.len() + session_id.len();

    trimmed[..trimmed.len() - marker_len].trim_end()
}

pub(super) fn task_content_without_matching_codex_sessions(
    content: &str,
    session_id: &str,
) -> String {
    let mut result = String::with_capacity(content.len());
    let mut cursor = 0;

    for (start, end, candidate) in codex_session_markers_in_task_content(content) {
        if candidate != session_id {
            continue;
        }

        let mut removal_start = start;
        if content[..start]
            .chars()
            .next_back()
            .is_some_and(|ch| matches!(ch, ' ' | '\t'))
        {
            removal_start -= 1;
        }
        result.push_str(&content[cursor..removal_start]);
        cursor = end;
    }

    result.push_str(&content[cursor..]);
    result.trim_end().to_string()
}

pub(super) fn task_content_without_recoverable_codex_session(content: &str) -> String {
    let trimmed = content.trim_end();
    match recoverable_codex_session_id_from_task_content(trimmed) {
        Some(session_id) => task_content_without_matching_codex_sessions(trimmed, session_id),
        None => trimmed.to_string(),
    }
}

pub(super) fn task_content_with_codex_session(content: &str, session_id: &str) -> String {
    let content = task_content_without_codex_session(content);
    let content = task_content_without_matching_codex_sessions(content, session_id);
    if content.is_empty() {
        format!("{CODEX_TASK_SESSION_PREFIX}{session_id}")
    } else {
        format!("{content} {CODEX_TASK_SESSION_PREFIX}{session_id}")
    }
}

pub(super) fn normalize_task_text(content: &str) -> String {
    task_content_without_recoverable_codex_session(content)
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

pub(super) fn durable_task_identity(content: &str) -> Option<String> {
    let content = task_content_without_recoverable_codex_session(content);
    let mut canonical_lines = Vec::new();
    let mut skipping_outcome_section = false;
    for line in content.lines() {
        let trimmed = line.trim();
        let heading = trimmed.trim_start_matches('#').trim();
        let normalized_heading = heading.trim_end_matches(':').to_ascii_lowercase();
        let outcome_heading = matches!(
            normalized_heading.as_str(),
            "completion note" | "blocked note" | "unblocked note"
        );
        if outcome_heading {
            skipping_outcome_section = true;
            continue;
        }
        if skipping_outcome_section {
            if trimmed.starts_with('#') {
                skipping_outcome_section = false;
            } else {
                continue;
            }
        }

        let uppercase = trimmed.to_ascii_uppercase();
        let mut note_start = None;
        for marker in ["COMPLETED ", "BLOCKED ", "UNBLOCKED "] {
            for (index, matched) in uppercase.match_indices(marker) {
                let has_word_boundary = uppercase[..index]
                    .chars()
                    .next_back()
                    .is_none_or(|ch| !ch.is_ascii_alphanumeric() && ch != '_');
                if has_word_boundary
                    && starts_with_task_note_date(&uppercase.as_bytes()[index + matched.len()..])
                {
                    note_start =
                        Some(note_start.map_or(index, |current: usize| current.min(index)));
                }
            }
        }
        let stable = note_start.map_or(trimmed, |index| &trimmed[..index]);
        let stable = stable
            .trim_end_matches(|ch: char| ch.is_whitespace() || matches!(ch, '—' | '-' | ':' | ';'))
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if !stable.is_empty() {
            canonical_lines.push(stable);
        }
    }
    (!canonical_lines.is_empty()).then(|| format!("v2\n{}", canonical_lines.join("\n")))
}

pub(super) fn split_description_metadata(value: &str) -> (&str, Option<&str>) {
    if let Some(start) = value.rfind(" (")
        && value.ends_with(')')
    {
        return (&value[..start], Some(&value[start + 2..value.len() - 1]));
    }

    (value, None)
}

pub(super) fn task_display_text(entry: &TaskEntry) -> String {
    match &entry.metadata {
        Some(metadata) => format!("{} ({})", entry.summary, metadata),
        None => entry.summary.clone(),
    }
}

pub(super) fn task_full_display_text(entry: &TaskEntry) -> String {
    let content = normalize_task_text(&entry.content);
    if content.is_empty() {
        task_display_text(entry)
    } else {
        content
    }
}

pub(super) fn parse_one_based_task_index(task_index_str: &str) -> Result<usize> {
    let task_index = task_index_str
        .parse::<usize>()
        .context("Invalid task index. Please provide a number.")?;
    if task_index == 0 {
        anyhow::bail!("Task index must be 1 or greater.");
    }

    Ok(task_index)
}

pub(super) fn task_entry_at(
    board_dir: &Path,
    status: TaskStatus,
    task_index: usize,
) -> Result<TaskEntry> {
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

pub(super) fn write_lines(path: &Path, lines: &[String]) -> Result<()> {
    let updated_content = lines.join("\n");
    let final_content = if updated_content.is_empty() {
        updated_content
    } else {
        format!("{}\n", updated_content)
    };

    #[cfg(unix)]
    {
        replace_file_atomically(path, final_content.as_bytes())
    }
    #[cfg(not(unix))]
    {
        fs::write(path, final_content).with_context(|| format!("Failed to write file {:?}", path))
    }
}

#[cfg(unix)]
pub(super) fn replace_file_atomically(path: &Path, content: &[u8]) -> Result<()> {
    replace_file_atomically_with_before_publish(path, content, |_| Ok(()))
}

#[cfg(unix)]
pub(super) fn replace_file_atomically_with_before_publish(
    path: &Path,
    content: &[u8],
    before_publish: impl FnOnce(&Path) -> Result<()>,
) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("File {:?} has no parent directory", path))?;
    let existing_metadata = fs::metadata(path)
        .with_context(|| format!("Failed to read file metadata for {:?}", path))?;
    if existing_metadata.permissions().readonly() {
        anyhow::bail!("Refusing to replace read-only board file {:?}", path);
    }
    let file_name = path.file_name().and_then(OsStr::to_str).unwrap_or("board");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = parent.join(format!(
        ".{file_name}.clt-{}-{nonce}.tmp",
        std::process::id()
    ));
    let write_result = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .with_context(|| format!("Failed to create temporary board file {:?}", temporary))?;
        file.write_all(content)
            .with_context(|| format!("Failed to write temporary board file {:?}", temporary))?;
        fs::set_permissions(&temporary, existing_metadata.permissions())
            .with_context(|| format!("Failed to preserve permissions for board file {:?}", path))?;
        file.sync_all()
            .with_context(|| format!("Failed to sync temporary board file {:?}", temporary))?;
        before_publish(&temporary)?;
        fs::rename(&temporary, path)
            .with_context(|| format!("Failed to atomically replace board file {:?}", path))?;
        #[cfg(unix)]
        fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .with_context(|| format!("Failed to sync board directory {:?}", parent))?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result
}

pub(super) fn is_clt_atomic_task_temporary_name(name: &str) -> bool {
    let Some(name) = name.strip_prefix('.') else {
        return false;
    };
    let Some((original_name, suffix)) = name.rsplit_once(".clt-") else {
        return false;
    };
    let Some(suffix) = suffix.strip_suffix(".tmp") else {
        return false;
    };
    let Some((pid, nonce)) = suffix.split_once('-') else {
        return false;
    };
    !original_name.is_empty()
        && !pid.is_empty()
        && pid.bytes().all(|byte| byte.is_ascii_digit())
        && !nonce.is_empty()
        && nonce.bytes().all(|byte| byte.is_ascii_digit())
}

pub(super) fn cleanup_clt_atomic_task_temporaries_in_directory(path: &Path) -> Result<usize> {
    if !path.is_dir() {
        return Ok(0);
    }
    let mut removed = 0;
    for entry in fs::read_dir(path)
        .with_context(|| format!("Failed to inspect task directory {:?}", path))?
    {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !is_clt_atomic_task_temporary_name(name)
            || !entry
                .file_type()
                .with_context(|| {
                    format!("Failed to inspect temporary task file {:?}", entry.path())
                })?
                .is_file()
        {
            continue;
        }
        fs::remove_file(entry.path()).with_context(|| {
            format!(
                "Failed to remove orphaned task temp file {:?}",
                entry.path()
            )
        })?;
        removed += 1;
    }
    if removed > 0 {
        sync_task_directory(path)?;
    }
    Ok(removed)
}

pub(super) fn cleanup_clt_atomic_task_temporaries(board_dir: &Path) -> Result<usize> {
    let mut removed = cleanup_clt_atomic_task_temporaries_in_directory(board_dir)?;
    for status in TASK_STATUSES {
        removed +=
            cleanup_clt_atomic_task_temporaries_in_directory(&board_dir.join(status.as_str()))?;
    }
    Ok(removed)
}

pub(super) fn remove_task_entry(
    board_dir: &Path,
    status: TaskStatus,
    entry: &TaskEntry,
) -> Result<()> {
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

pub(super) fn remove_task_entry_without_reordering(
    board_dir: &Path,
    status: TaskStatus,
    entry: &TaskEntry,
) -> Result<()> {
    match &entry.source {
        TaskSource::MarkdownLine { line_index } => {
            let StatusStore::MarkdownFile(path) = get_status_store(board_dir, status)? else {
                anyhow::bail!("Task storage changed while removing a managed Git task.");
            };
            let content = fs::read_to_string(&path).context("Failed to read file")?;
            let mut lines = content.lines().map(str::to_string).collect::<Vec<_>>();
            if *line_index >= lines.len() {
                anyhow::bail!("Task storage changed while removing a managed Git task.");
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
                sync_task_directory(parent)?;
            }
        }
    }
    Ok(())
}

pub(super) fn content_with_metadata(description: &str, metadata: Option<String>) -> String {
    match metadata {
        Some(metadata) => format!("{} ({})", description, metadata),
        None => description.to_string(),
    }
}

pub(super) fn insert_task_content(
    board_dir: &Path,
    status: TaskStatus,
    index: Option<usize>,
    content: &str,
) -> Result<()> {
    match get_status_store(board_dir, status)? {
        StatusStore::MarkdownFile(path) => insert_content_into_markdown(&path, index, content),
        StatusStore::Directory(path) => insert_content_into_directory(&path, index, content),
    }
}

pub(super) fn insert_content_into_markdown(
    path: &Path,
    index: Option<usize>,
    content: &str,
) -> Result<()> {
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

pub(super) fn insert_content_into_directory(
    path: &Path,
    index: Option<usize>,
    content: &str,
) -> Result<()> {
    insert_content_into_directory_with_before_publish(path, index, content, |_| Ok(()))
}

pub(super) fn insert_content_into_directory_without_reordering(
    path: &Path,
    content: &str,
) -> Result<PathBuf> {
    insert_content_into_directory_without_reordering_at(path, content, false)
}

fn insert_content_into_directory_without_reordering_at(
    path: &Path,
    content: &str,
    prepend: bool,
) -> Result<PathBuf> {
    fs::create_dir_all(path).with_context(|| format!("Failed to create directory {:?}", path))?;
    let name = format!(
        "{}.md",
        slugify(&first_sentence(content).unwrap_or_else(|| "task".to_string()))
    );
    let preferred_name = task_name_without_reordering(path, &name, prepend)?;
    let task_path = unique_child_path(path, &preferred_name);
    write_new_task_file_atomically(
        &task_path,
        format!("{}\n", content.trim_end()).as_bytes(),
        |_| Ok(()),
    )?;
    Ok(task_path)
}

pub(super) fn insert_content_into_directory_with_before_publish(
    path: &Path,
    index: Option<usize>,
    content: &str,
    before_publish: impl FnOnce(&Path) -> Result<()>,
) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("Failed to create directory {:?}", path))?;
    let preferred_name = format!(
        "{:04}-{}.md",
        directory_task_paths(path)?.len() + 1,
        slugify(&first_sentence(content).unwrap_or_else(|| "task".to_string()))
    );
    let task_path = unique_child_path(path, &preferred_name);
    write_new_task_file_atomically(
        &task_path,
        format!("{}\n", content.trim_end()).as_bytes(),
        before_publish,
    )?;

    if let Some(idx) = index {
        reorder_path_in_directory(path, &task_path, idx)?;
    } else {
        normalize_directory_order(path)?;
    }

    Ok(())
}

pub(super) fn write_new_task_file_atomically(
    path: &Path,
    content: &[u8],
    before_publish: impl FnOnce(&Path) -> Result<()>,
) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("Task file {:?} has no parent directory", path))?;
    let file_name = path.file_name().and_then(OsStr::to_str).unwrap_or("task");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = parent.join(format!(
        ".{file_name}.clt-{}-{nonce}.tmp",
        std::process::id()
    ));
    let write_result = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .with_context(|| format!("Failed to create temporary task file {:?}", temporary))?;
        file.write_all(content)
            .with_context(|| format!("Failed to write temporary task file {:?}", temporary))?;
        file.sync_all()
            .with_context(|| format!("Failed to sync temporary task file {:?}", temporary))?;
        before_publish(&temporary)?;
        if path.exists() {
            anyhow::bail!("Refusing to replace existing task file {:?}", path);
        }
        fs::rename(&temporary, path)
            .with_context(|| format!("Failed to atomically publish task file {:?}", path))?;
        #[cfg(unix)]
        fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .with_context(|| format!("Failed to sync task directory {:?}", parent))?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result
}

#[cfg(unix)]
pub(super) fn sync_task_directory(path: &Path) -> Result<()> {
    fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .with_context(|| format!("Failed to sync task directory {:?}", path))
}

#[cfg(not(unix))]
pub(super) fn sync_task_directory(_path: &Path) -> Result<()> {
    Ok(())
}

pub(super) fn single_line_content(content: &str) -> String {
    let normalized = normalize_task_text(content);
    match recoverable_codex_session_id_from_task_content(content) {
        Some(session_id) => task_content_with_codex_session(&normalized, session_id),
        None => normalized,
    }
}

pub(super) fn slugify(value: &str) -> String {
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

pub(super) fn unique_child_path(parent: &Path, preferred_name: &str) -> PathBuf {
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

pub(super) fn normalize_directory_order(path: &Path) -> Result<()> {
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

pub(super) fn reorder_path_in_directory(
    path: &Path,
    task_path: &Path,
    to_idx: usize,
) -> Result<()> {
    let mut paths = directory_task_paths(path)?;
    let Some(from_idx) = paths.iter().position(|path| path == task_path) else {
        anyhow::bail!("Task file disappeared while reordering.");
    };
    let task_path = paths.remove(from_idx);
    let to_idx = to_idx.min(paths.len());
    paths.insert(to_idx, task_path);

    rewrite_directory_order(path, paths)
}

pub(super) fn rewrite_directory_order(path: &Path, ordered_paths: Vec<PathBuf>) -> Result<()> {
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

pub(super) fn move_path_into_directory(
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

pub(super) fn move_path_into_directory_without_reordering(
    source_path: &Path,
    dest_dir: &Path,
    prepend: bool,
) -> Result<PathBuf> {
    fs::create_dir_all(dest_dir)
        .with_context(|| format!("Failed to create destination directory {:?}", dest_dir))?;
    let original_name = source_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("task.md");
    let preferred_name =
        task_name_without_reordering(dest_dir, strip_order_prefix(original_name), prepend)?;
    let dest_path = unique_child_path(dest_dir, &preferred_name);
    fs::rename(source_path, &dest_path).with_context(|| {
        format!(
            "Failed to atomically move managed Git task {:?} into {:?}",
            source_path, dest_dir
        )
    })?;
    sync_task_directory(dest_dir)?;
    if let Some(source_parent) = source_path.parent()
        && source_parent != dest_dir
    {
        sync_task_directory(source_parent)?;
    }
    Ok(dest_path)
}

pub(super) fn write_task_entry_content(
    board_dir: &Path,
    status: TaskStatus,
    entry: &TaskEntry,
    content: &str,
) -> Result<()> {
    write_task_entry_content_with_before_replace(board_dir, status, entry, content, || {})
}

pub(super) fn write_task_entry_content_with_before_replace(
    board_dir: &Path,
    status: TaskStatus,
    entry: &TaskEntry,
    content: &str,
    before_replace: impl FnOnce(),
) -> Result<()> {
    match &entry.source {
        TaskSource::MarkdownLine { line_index } => {
            let StatusStore::MarkdownFile(path) = get_status_store(board_dir, status)? else {
                anyhow::bail!("Task storage changed while updating task.");
            };
            let stored_content = fs::read_to_string(&path).context("Failed to read file")?;
            let mut lines: Vec<String> = stored_content.lines().map(str::to_string).collect();

            if *line_index >= lines.len() {
                anyhow::bail!("Task storage changed while updating task.");
            }
            if lines[*line_index].strip_prefix("- ") != Some(entry.content.as_str()) {
                anyhow::bail!("Task content changed while updating task; retry with a fresh read.");
            }

            before_replace();
            lines[*line_index] = format!("- {content}");
            write_lines(&path, &lines)?;
        }
        TaskSource::Path { path, is_dir } => {
            let target_path = if *is_dir {
                directory_task_detail_path(path)
            } else {
                path.clone()
            };
            let replacement = format!("{}\n", content.trim_end());
            if target_path.exists() {
                let stored_content = fs::read_to_string(&target_path)
                    .with_context(|| format!("Failed to read task file {:?}", target_path))?;
                if stored_content != entry.content {
                    anyhow::bail!(
                        "Task content changed while updating {:?}; retry with a fresh read.",
                        target_path
                    );
                }
                before_replace();
                fs::write(&target_path, replacement)
                    .with_context(|| format!("Failed to write task file {:?}", target_path))?;
            } else if *is_dir && path.is_dir() && entry.content.trim_end() == title_from_path(path)
            {
                before_replace();
                let mut target = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&target_path)
                    .with_context(|| {
                        format!("Task content changed while creating {:?}", target_path)
                    })?;
                target
                    .write_all(replacement.as_bytes())
                    .with_context(|| format!("Failed to write task file {:?}", target_path))?;
            } else {
                anyhow::bail!("Task storage changed while updating task.");
            }
        }
    }

    Ok(())
}

pub(super) fn reorder_markdown_task(path: &Path, from_idx: usize, to_idx: usize) -> Result<()> {
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

pub(super) fn reorder_directory_task(path: &Path, from_idx: usize, to_idx: usize) -> Result<()> {
    let mut paths = directory_task_paths(path)?;

    if from_idx >= paths.len() {
        anyhow::bail!("Task index out of range");
    }

    let task_path = paths.remove(from_idx);
    let to_idx = to_idx.min(paths.len());
    paths.insert(to_idx, task_path);
    rewrite_directory_order(path, paths)
}

pub(super) fn parse_add_task_args(args: Vec<String>) -> Result<(String, Option<String>)> {
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

pub(super) fn looks_like_metadata(value: &str) -> bool {
    value.contains(',')
        || (value.chars().any(|c| c.is_ascii_alphabetic())
            && value
                .chars()
                .all(|c| !c.is_ascii_lowercase() && !matches!(c, '"' | '\'')))
}

pub(super) fn add_task(root: &Path, description: &str, metadata: Option<String>) -> Result<String> {
    insert_task(root, TaskStatus::Todo, None, description, metadata)
        .map(|_| "Task added successfully.".to_string())
}

pub(super) fn insert_task(
    root: &Path,
    status: TaskStatus,
    index: Option<usize>,
    description: &str,
    metadata: Option<String>,
) -> Result<()> {
    insert_task_in_board(&get_tasks_dir(root), status, index, description, metadata)
}

pub(super) fn insert_task_in_board(
    board_dir: &Path,
    status: TaskStatus,
    index: Option<usize>,
    description: &str,
    metadata: Option<String>,
) -> Result<()> {
    let content = content_with_metadata(description, metadata);
    let _mutation_lock = acquire_board_mutation_lock(board_dir)?;
    insert_task_content(board_dir, status, index, &content)
}

#[cfg(test)]
pub(super) fn read_tasks(root: &Path, status: &str) -> Result<Vec<String>> {
    read_tasks_in_board(&get_tasks_dir(root), TaskStatus::parse(status)?)
}

pub(super) fn read_tasks_in_board(board_dir: &Path, status: TaskStatus) -> Result<Vec<String>> {
    Ok(read_task_entries(board_dir, status)?
        .iter()
        .map(|entry| format!("- {}", task_display_text(entry)))
        .collect())
}

pub(super) fn init_tasks(root: &Path, folders: bool) -> Result<()> {
    let tasks_dir = get_tasks_dir(root);
    let tasks_dir_existed = tasks_dir.exists();
    let _mutation_lock = acquire_board_mutation_lock(&tasks_dir)?;
    if !tasks_dir_existed {
        fs::create_dir_all(&tasks_dir).context("Failed to create tasks directory")?;
        println!("Created directory: {:?}", tasks_dir);
    }

    let directory_mode = folders
        || TASK_STATUSES
            .iter()
            .any(|status| tasks_dir.join(status.as_str()).is_dir());

    for status in TASK_STATUSES {
        let dir_path = tasks_dir.join(status.as_str());
        let file_path = tasks_dir.join(status_filename(status));
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
            file.write_all(status_header(status).as_bytes())
                .context(format!("Failed to write to file {:?}", file_path))?;
            println!("Created file: {:?}", file_path);
        }
    }

    println!("Initialization complete.");
    Ok(())
}

pub(super) fn task_status_for_codex_session_in_board(
    board_dir: &Path,
    session_id: &str,
) -> Result<Option<TaskStatus>> {
    Ok(task_for_codex_session_in_board(board_dir, session_id)?.map(|(status, _)| status))
}

pub(super) fn task_for_codex_session_in_board(
    board_dir: &Path,
    session_id: &str,
) -> Result<Option<(TaskStatus, TaskEntry)>> {
    task_for_codex_session_in_board_matching(board_dir, session_id, true)
}

/// Orphan retirement must retain even displaced, ambiguous, or archived markers.
/// Scan the complete task tree rather than selecting one marker from active tasks.
pub(super) fn task_tree_contains_session_marker(
    board_dir: &Path,
    session_id: &str,
) -> Result<bool> {
    fn contains_marker(path: &Path, marker: &[u8]) -> Result<bool> {
        let metadata = fs::symlink_metadata(path)
            .with_context(|| format!("Failed to inspect task evidence at {}", path.display()))?;
        if metadata.is_dir() {
            for entry in fs::read_dir(path)? {
                if contains_marker(&entry?.path(), marker)? {
                    return Ok(true);
                }
            }
            return Ok(false);
        }
        anyhow::ensure!(
            metadata.is_file(),
            "Cannot rule out a task session marker in non-regular task evidence at {}",
            path.display()
        );
        Ok(fs::read(path)?
            .windows(marker.len())
            .any(|bytes| bytes == marker))
    }

    contains_marker(board_dir, format!("codex:{session_id}").as_bytes())
}

pub(super) fn terminal_task_for_codex_session_in_board(
    board_dir: &Path,
    session_id: &str,
) -> Result<Option<(TaskStatus, TaskEntry)>> {
    task_for_codex_session_in_board_matching(board_dir, session_id, false)
}

pub(super) fn task_for_codex_session_in_board_matching(
    board_dir: &Path,
    session_id: &str,
    recover_displaced_marker: bool,
) -> Result<Option<(TaskStatus, TaskEntry)>> {
    for status in TaskStatus::SESSION_SEARCH_ORDER {
        let tasks = read_task_entries(board_dir, status)?;
        for task in tasks {
            let task_session_id = if recover_displaced_marker {
                recoverable_codex_session_id_from_task_content(&task.content)
            } else {
                codex_session_id_from_task_content(&task.content)
            };
            if task_session_id == Some(session_id) {
                return Ok(Some((status, task)));
            }
            if task.has_subtasks
                && let TaskSource::Path { path, is_dir: true } = &task.source
                && let Some(nested_task) = task_for_codex_session_in_board_matching(
                    path,
                    session_id,
                    recover_displaced_marker,
                )?
            {
                return Ok(Some(nested_task));
            }
        }
    }
    Ok(None)
}

pub(super) fn convert_status_to_directory(board_dir: &Path, status: TaskStatus) -> Result<PathBuf> {
    let dir_path = board_dir.join(status.as_str());
    if dir_path.is_dir() {
        return Ok(dir_path);
    }

    let file_path = board_dir.join(status_filename(status));
    fs::create_dir_all(&dir_path)
        .with_context(|| format!("Failed to create directory {:?}", dir_path))?;

    if file_path.exists() {
        let entries = read_markdown_entries(&file_path)?;
        for entry in entries {
            insert_content_into_directory(&dir_path, None, &entry.content)?;
        }

        let backup_name = format!("{}.bak", status_filename(status));
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

pub(super) fn convert_archive_to_directory(archive_file: &Path) -> Result<PathBuf> {
    let board_dir = archive_file
        .parent()
        .context("Archive file has no parent directory")?;
    let archive_name = archive_file
        .file_stem()
        .and_then(|name| name.to_str())
        .context("Archive file has an invalid name")?;
    let archive_dir = board_dir.join(archive_name);
    fs::create_dir_all(&archive_dir)
        .with_context(|| format!("Failed to create archive directory {:?}", archive_dir))?;

    let entries = read_markdown_entries(archive_file)?;
    for entry in entries {
        insert_content_into_directory(&archive_dir, None, &entry.content)?;
    }

    let backup_name = format!(
        "{}.bak",
        archive_file
            .file_name()
            .and_then(|name| name.to_str())
            .context("Archive file has an invalid name")?
    );
    let backup_path = unique_child_path(board_dir, &backup_name);
    fs::rename(archive_file, &backup_path)
        .with_context(|| format!("Failed to preserve archive file as {:?}", backup_path))?;

    Ok(archive_dir)
}

pub(super) fn expand_status_for_command(
    board_dir: &Path,
    status: TaskStatus,
) -> Result<ExpansionSummary> {
    ensure_board_store(board_dir)?;

    let dir_path = board_dir.join(status.as_str());
    if dir_path.is_dir() {
        return Ok(ExpansionSummary::AlreadyDirectory {
            status,
            dir: dir_path,
        });
    }

    let file_path = board_dir.join(status_filename(status));
    let entries = read_markdown_entries(&file_path)?;
    let task_count = entries.len();
    fs::create_dir_all(&dir_path)
        .with_context(|| format!("Failed to create directory {:?}", dir_path))?;

    for entry in entries {
        insert_content_into_directory(&dir_path, None, &entry.content)?;
    }

    let backup_name = format!("{}.bak", status_filename(status));
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

pub(super) fn move_task_without_reordering_after_lock(
    board_dir: &Path,
    from: TaskStatus,
    to: TaskStatus,
    task_index: usize,
) -> Result<()> {
    move_task_without_reordering_with_after_destination(board_dir, from, to, task_index, || Ok(()))
}

pub(super) fn move_task_without_reordering_with_after_destination(
    board_dir: &Path,
    from: TaskStatus,
    to: TaskStatus,
    task_index: usize,
    after_destination: impl FnOnce() -> Result<()>,
) -> Result<()> {
    cleanup_clt_atomic_task_temporaries(board_dir)?;
    let entry = task_entry_at(board_dir, from, task_index)?;
    match (&entry.source, get_status_store(board_dir, to)?) {
        (TaskSource::Path { path, .. }, StatusStore::Directory(dest_dir)) => {
            move_path_into_directory_without_reordering(path, &dest_dir, to == TaskStatus::Done)?;
            after_destination()?;
        }
        (TaskSource::Path { .. }, StatusStore::MarkdownFile(_)) => {
            anyhow::bail!(
                "Managed Git tasks cannot move from a folder-backed {from} status into Markdown-backed {to}; expand {to} to folder storage and commit that board layout before scheduling the task"
            );
        }
        (TaskSource::MarkdownLine { .. }, StatusStore::Directory(dest_dir)) => {
            insert_content_into_directory_without_reordering_at(
                &dest_dir,
                &entry.content,
                to == TaskStatus::Done,
            )?;
            after_destination()?;
            remove_task_entry_without_reordering(board_dir, from, &entry)?;
        }
        (TaskSource::MarkdownLine { .. }, StatusStore::MarkdownFile(dest_file)) => {
            let dest_index = (to == TaskStatus::Done).then_some(0);
            insert_content_into_markdown(&dest_file, dest_index, &entry.content)?;
            after_destination()?;
            remove_task_entry_without_reordering(board_dir, from, &entry)?;
        }
    }
    Ok(())
}

pub(super) fn move_task_in_board_after_lock(
    board_dir: &Path,
    from: TaskStatus,
    to: TaskStatus,
    task_index: usize,
) -> Result<()> {
    let entry = task_entry_at(board_dir, from, task_index)?;
    let dest_index = (to == TaskStatus::Done).then_some(0);

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

#[cfg(test)]
pub(super) fn attach_codex_session_to_task(
    project_root: &Path,
    status: TaskStatus,
    entry: &TaskEntry,
    session_id: &str,
) -> Result<String> {
    attach_codex_session_to_task_with_before_replace(project_root, status, entry, session_id, || {})
}

#[cfg(test)]
pub(super) fn attach_codex_session_to_task_with_before_replace(
    project_root: &Path,
    status: TaskStatus,
    entry: &TaskEntry,
    session_id: &str,
    before_replace: impl FnOnce(),
) -> Result<String> {
    let board_dir = get_tasks_dir(project_root);
    let _mutation_lock = acquire_board_mutation_lock(&board_dir)?;
    attach_codex_session_to_task_after_lock(project_root, status, entry, session_id, before_replace)
}

pub(super) fn attach_codex_session_to_task_after_lock(
    project_root: &Path,
    status: TaskStatus,
    entry: &TaskEntry,
    session_id: &str,
    before_replace: impl FnOnce(),
) -> Result<String> {
    let board_dir = get_tasks_dir(project_root);
    if let Some((_, existing)) = task_for_codex_session_in_board(&board_dir, session_id)? {
        if existing.source != entry.source
            || codex_session_id_from_task_content(&existing.content) == Some(session_id)
        {
            return Ok(existing.content);
        }

        let content = task_content_with_codex_session(&existing.content, session_id);
        write_task_entry_content_with_before_replace(
            &board_dir,
            status,
            &existing,
            &content,
            before_replace,
        )?;
        return Ok(content);
    }
    let current = read_task_entries(&board_dir, status)?
        .into_iter()
        .find(|current| current.source == entry.source)
        .context("Task storage changed before its Codex session could be attached")?;
    if current.content != entry.content {
        anyhow::bail!("Task content changed before its Codex session could be attached; retry");
    }
    let content = task_content_with_codex_session(&current.content, session_id);
    write_task_entry_content_with_before_replace(
        &board_dir,
        status,
        &current,
        &content,
        before_replace,
    )?;
    Ok(content)
}

pub(super) fn ensure_subtask_board_after_lock(
    board_dir: &Path,
    status: TaskStatus,
    parent_task_index: usize,
) -> Result<PathBuf> {
    let status_dir = convert_status_to_directory(board_dir, status)?;
    let entry = task_entry_at(board_dir, status, parent_task_index)?;

    let task_dir = match entry.source {
        TaskSource::Path { path, is_dir: true } => path,
        TaskSource::Path {
            path,
            is_dir: false,
        } => {
            let preferred_name = path
                .file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or("task");
            let task_dir = unique_child_path(&status_dir, preferred_name);
            fs::create_dir(&task_dir)
                .with_context(|| format!("Failed to create subtask board {:?}", task_dir))?;
            let detail_path = task_dir.join(TASK_DETAIL_FILES[0]);
            if let Err(error) = fs::rename(&path, &detail_path) {
                let _ = fs::remove_dir(&task_dir);
                return Err(error).with_context(|| {
                    format!(
                        "Failed to move task {:?} into subtask board {:?}",
                        path, task_dir
                    )
                });
            }
            ensure_board_store(&task_dir)?;
            reorder_path_in_directory(&status_dir, &task_dir, parent_task_index - 1)?;

            match task_entry_at(board_dir, status, parent_task_index)?.source {
                TaskSource::Path { path, is_dir: true } => path,
                _ => anyhow::bail!("Task storage changed while creating its subtask board."),
            }
        }
        TaskSource::MarkdownLine { .. } => {
            anyhow::bail!("Task storage did not expand before creating its subtask board.")
        }
    };

    ensure_board_store(&task_dir)?;
    Ok(task_dir)
}

#[cfg(test)]
mod tests;
