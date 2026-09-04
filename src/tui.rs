use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{self, Write, stdout},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use chrono::{DateTime, Local, Utc};
use crossterm::{
    ExecutableCommand,
    event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
        PushKeyboardEnhancementFlags,
    },
    terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, SetTitle, disable_raw_mode, enable_raw_mode,
    },
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Position, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, ListItem, ListState, Paragraph},
};
use tui_input::{Input, InputRequest};

use crate::{
    agent::{
        self, AGENT_CODEX_REASONING_EFFORTS, AGENT_DB_FILE, AGENT_PROVIDER_PRESETS,
        AgentSessionControlState, GitFinalizationState, agent_state_dir, codex_config_path,
        ensure_agent_state_dir, open_agent_store, open_agent_store_at,
        read_codex_default_config_at, remove_codex_provider_config_at, set_codex_default_config_at,
        set_codex_model_reasoning_if_default_at, upsert_codex_provider_config_at,
        valid_environment_variable_name,
    },
    application::{
        AgentLeaseHolderLiveness, AgentProjectScan, delete_task_in_board,
        ensure_status_conversion_allowed, move_task_in_board, move_task_to_archive_in_board,
        project_display_name, reorder_task_in_board, unregister_agent_project_with_recovery,
        update_task_in_board,
    },
    platform::{agent_service_status, restart_running_agent_service},
    runner::{
        AgentRunSettings, agent_codex_session_id_from_log, agent_project_run_log_dir,
        agent_run_settings_from_log, agent_timestamp, agent_timestamp_seconds,
        latest_agent_log_path, preferred_recorded_agent_output_path,
    },
    scheduler::{
        agent_failure_backoff, agent_lease_holder_liveness, interactive_lease_holder_liveness,
        remaining_agent_delay, scan_agent_project,
    },
    session_control::{
        InteractiveAgentLease, InteractiveCodexResumeMode, InteractiveGuardianDisposition,
        agent_session_resume_worker_log_path, cancel_tui_idle_codex_session_interactive,
        codex_session_for_task, codex_session_task_supports_interactive_resume,
        prepare_tui_codex_session_interrupt, queue_tui_codex_session_exec_resume,
        reserve_tui_idle_codex_session_interactive, reserve_tui_shared_codex_session_interactive,
        resume_codex_session_interactively, spawn_agent_session_resume_worker,
        task_supports_interactive_codex_resume, toggle_tui_codex_session_stop,
        tui_stopped_codex_session_control,
    },
    task::{
        TASK_STATUSES, TaskBoard, TaskEntry, TaskSource, TaskStatus, acquire_board_mutation_lock,
        content_with_metadata, ensure_board_store, ensure_existing_board,
        ensure_subtask_board_after_lock, get_tasks_dir, insert_task_in_board,
        read_archived_task_entries, read_task_entries, read_tasks_in_board,
        recoverable_codex_session_id_from_task_content, strip_order_prefix,
        task_content_without_recoverable_codex_session, task_display_text, task_entry_at,
        task_full_display_text, title_from_path,
    },
};

#[cfg(not(test))]
use crate::worker::cleanup_terminal_agent_worker_services;

pub(super) const TODO_BOARD_INDEX: usize = 0;
pub(super) const DONE_BOARD_INDEX: usize = 2;
pub(super) const BACKLOG_BOARD_INDEX: usize = 3;
pub(super) const DEFAULT_TUI_BOARD_INDICES: [usize; 3] = [0, 1, 2];
pub(super) const TUI_BOARD_INDICES_WITH_BACKLOG: [usize; 4] = [3, 0, 1, 2];
pub(super) const TUI_AGENT_PANEL_REFRESH_SECONDS: u64 = 2;
pub(super) const TUI_AGENT_LOG_REFRESH_MILLIS: u64 = 500;
pub(super) const TUI_SESSION_HANDOFF_TIMEOUT_SECONDS: u64 = 15;
pub(super) const TUI_SESSION_RESUME_WORKER_RETRY_MILLIS: u64 = 1_000;
pub(super) const TUI_LEASE_RELEASE_ATTEMPTS: usize = 3;
pub(super) const TUI_LEASE_RELEASE_RETRY_MILLIS: u64 = 50;
pub(super) const TUI_AGENT_TABLE_CODEX_LAST_RUN_GAP: &str = "   ";
pub(super) const TUI_AGENT_TABLE_CODEX_MAX_WIDTH: usize = 20;
pub(super) const TUI_MODEL_DISCOVERY_TIMEOUT_SECONDS: u64 = 5;
pub(super) const TUI_NO_ACTIVE_BOARD_MESSAGE: &str =
    "No active board. Open a project from Agent Projects, or press M for Models.";
pub(super) struct InitializationPromptRawMode;

impl InitializationPromptRawMode {
    pub(super) fn enter() -> Result<Self> {
        enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for InitializationPromptRawMode {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}

pub(super) fn initialization_prompt_choice(key: &KeyEvent) -> Option<bool> {
    if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
        || key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return None;
    }

    match key.code {
        KeyCode::Char('y' | 'Y') => Some(true),
        KeyCode::Char('n' | 'N') => Some(false),
        _ => None,
    }
}

pub(super) fn prompt_to_initialize_tasks() -> Result<bool> {
    print!("Tasks not initialized. Would you like to initialize now? (y/n): ");
    io::stdout().flush()?;

    let choice = {
        let _raw_mode = InitializationPromptRawMode::enter()?;
        loop {
            if let Event::Key(key) = event::read()? {
                if key.code == KeyCode::Char('c')
                    && key.modifiers.contains(KeyModifiers::CONTROL)
                    && matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
                {
                    break None;
                }
                if let Some(choice) = initialization_prompt_choice(&key) {
                    break Some(choice);
                }
            }
        }
    };

    match choice {
        Some(choice) => {
            println!("{}", if choice { 'y' } else { 'n' });
            Ok(choice)
        }
        None => {
            println!();
            anyhow::bail!("Initialization cancelled")
        }
    }
}

pub(super) fn app_title(root: &Path) -> String {
    format!("clt | {}", project_display_name(root))
}

pub(super) fn set_terminal_title(title: &str) -> Result<()> {
    stdout()
        .execute(SetTitle(title))
        .context("Failed to update terminal title")?;
    Ok(())
}

pub(super) fn task_tui_display_text(entry: &TaskEntry, is_selected: bool) -> String {
    if is_selected {
        task_full_display_text(entry)
    } else {
        task_display_text(entry)
    }
}

pub(super) type TaskAgentSessionStates = HashMap<String, AgentSessionControlState>;

pub(super) fn task_has_stopped_agent_flag(
    status: TaskStatus,
    entry: &TaskEntry,
    session_states: &TaskAgentSessionStates,
) -> bool {
    if status == TaskStatus::Done {
        return false;
    }

    let Some(session_id) = recoverable_codex_session_id_from_task_content(&entry.content) else {
        return false;
    };
    session_states.get(session_id) == Some(&AgentSessionControlState::Stopped)
}

pub(super) fn prefix_task_agent_flag(
    text: String,
    status: TaskStatus,
    entry: &TaskEntry,
    session_states: &TaskAgentSessionStates,
) -> String {
    if task_has_stopped_agent_flag(status, entry, session_states) {
        format!("[STOPPED] {text}")
    } else {
        text
    }
}

pub(super) fn task_display_text_with_agent_flag(
    entry: &TaskEntry,
    status: TaskStatus,
    session_states: &TaskAgentSessionStates,
) -> String {
    prefix_task_agent_flag(task_display_text(entry), status, entry, session_states)
}

pub(super) fn task_tui_display_text_with_agent_flag(
    entry: &TaskEntry,
    status: TaskStatus,
    is_selected: bool,
    session_states: &TaskAgentSessionStates,
) -> String {
    prefix_task_agent_flag(
        task_tui_display_text(entry, is_selected),
        status,
        entry,
        session_states,
    )
}

pub(super) fn load_task_agent_session_states(root: &Path) -> TaskAgentSessionStates {
    try_load_task_agent_session_states(root).unwrap_or_default()
}

pub(super) fn try_load_task_agent_session_states(root: &Path) -> Result<TaskAgentSessionStates> {
    let state_dir = agent_state_dir()?;
    if !state_dir.join(AGENT_DB_FILE).is_file() {
        return Ok(TaskAgentSessionStates::default());
    }

    let project_root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let store = open_agent_store_at(&state_dir)?;
    let Some(project) = store
        .list_projects_blocking()?
        .into_iter()
        .find(|project| project.path == project_root)
    else {
        return Ok(TaskAgentSessionStates::default());
    };

    Ok(store
        .session_controls_for_project_blocking(project.id)?
        .into_iter()
        .map(|control| (control.codex_session_id, control.state))
        .collect())
}

pub(super) fn insert_task_at_selection_in_board(
    board_dir: &Path,
    status: TaskStatus,
    state: &ListState,
    description: &str,
    metadata: Option<String>,
) -> Result<()> {
    let index = selected_task_index_in_board(board_dir, status, state);
    insert_task_in_board(board_dir, status, index, description, metadata)
}

pub(super) fn insert_subtask_in_board(
    board_dir: &Path,
    status: TaskStatus,
    parent_task_index: usize,
    expected_parent: &TaskEntry,
    description: &str,
    metadata: Option<String>,
) -> Result<PathBuf> {
    let content = content_with_metadata(description, metadata);
    let _mutation_lock = acquire_board_mutation_lock(board_dir)?;
    let current_parent = task_entry_at(board_dir, status, parent_task_index)?;
    if current_parent.source != expected_parent.source
        || current_parent.content != expected_parent.content
    {
        anyhow::bail!(
            "Parent task changed while creating its subtask; retry with a fresh selection."
        );
    }
    ensure_status_conversion_allowed(board_dir, status)?;
    let subtask_board = ensure_subtask_board_after_lock(board_dir, status, parent_task_index)?;
    TaskBoard::new(&subtask_board).insert_content(TaskStatus::Todo, None, &content)?;
    Ok(subtask_board)
}

pub(super) fn select_first_task_if_present_in_board(
    board_dir: &Path,
    status: TaskStatus,
    state: &mut ListState,
) {
    let has_tasks = read_tasks_in_board(board_dir, status)
        .map(|tasks| !tasks.is_empty())
        .unwrap_or(false);

    state.select(if has_tasks { Some(0) } else { None });
}

pub(super) fn select_last_task_if_present_in_board(
    board_dir: &Path,
    status: TaskStatus,
    state: &mut ListState,
) {
    let last_idx = read_tasks_in_board(board_dir, status)
        .ok()
        .and_then(|tasks| tasks.len().checked_sub(1));

    state.select(last_idx);
}

#[cfg(test)]
pub(super) fn selected_task_index(root: &Path, status: &str, state: &ListState) -> Option<usize> {
    selected_task_index_in_board(&get_tasks_dir(root), TaskStatus::parse(status).ok()?, state)
}

pub(super) fn selected_task_index_in_board(
    board_dir: &Path,
    status: TaskStatus,
    state: &ListState,
) -> Option<usize> {
    let idx = state.selected()?;
    let tasks = read_tasks_in_board(board_dir, status).ok()?;

    if idx < tasks.len() { Some(idx) } else { None }
}

#[cfg(test)]
pub(super) fn selected_task(
    root: &Path,
    status: &str,
    state: &ListState,
) -> Option<(usize, String)> {
    selected_task_in_board(&get_tasks_dir(root), TaskStatus::parse(status).ok()?, state)
}

#[cfg(test)]
pub(super) fn selected_task_in_board(
    board_dir: &Path,
    status: TaskStatus,
    state: &ListState,
) -> Option<(usize, String)> {
    let idx = state.selected()?;
    let tasks = read_tasks_in_board(board_dir, status).ok()?;
    tasks.get(idx).cloned().map(|task| (idx, task))
}

pub(super) fn selected_task_entry_in_board(
    board_dir: &Path,
    status: TaskStatus,
    state: &ListState,
) -> Option<(usize, TaskEntry)> {
    let idx = state.selected()?;
    let tasks = read_task_entries(board_dir, status).ok()?;
    tasks.get(idx).cloned().map(|task| (idx, task))
}

#[cfg(test)]
pub(super) fn normalize_board_selection(root: &Path, status: &str, state: &mut ListState) {
    normalize_board_selection_in_board(
        &get_tasks_dir(root),
        TaskStatus::parse(status).expect("test status must be valid"),
        state,
    );
}

pub(super) fn normalize_board_selection_in_board(
    board_dir: &Path,
    status: TaskStatus,
    state: &mut ListState,
) {
    let selected = state.selected();
    let task_count = read_tasks_in_board(board_dir, status)
        .map(|tasks| tasks.len())
        .unwrap_or(0);

    match (selected, task_count) {
        (Some(0), 0) => state.select(None),
        (Some(idx), count) if idx >= count => state.select(count.checked_sub(1)),
        _ => {}
    }
}

pub(super) fn select_first_archive_task_if_present_in_board(
    board_dir: &Path,
    state: &mut ListState,
) {
    let has_tasks = read_archived_task_entries(board_dir)
        .map(|tasks| !tasks.is_empty())
        .unwrap_or(false);

    state.select(if has_tasks { Some(0) } else { None });
}

pub(super) fn normalize_board_selections_in_board(
    board_dir: &Path,
    statuses: &[TaskStatus],
    states: &mut [ListState],
) {
    for (status, state) in statuses.iter().zip(states.iter_mut()) {
        normalize_board_selection_in_board(board_dir, *status, state);
    }
}

pub(super) fn visible_tui_board_indices(backlog_visible: bool) -> &'static [usize] {
    if backlog_visible {
        &TUI_BOARD_INDICES_WITH_BACKLOG
    } else {
        &DEFAULT_TUI_BOARD_INDICES
    }
}

pub(super) fn adjacent_visible_tui_board(
    selected_board: usize,
    backlog_visible: bool,
    direction: isize,
) -> Option<usize> {
    let visible = visible_tui_board_indices(backlog_visible);
    let position = visible.iter().position(|board| *board == selected_board)?;
    let next = position as isize + direction;
    if next < 0 || next >= visible.len() as isize {
        None
    } else {
        Some(visible[next as usize])
    }
}

pub(super) fn wrapped_visible_tui_board(
    selected_board: usize,
    backlog_visible: bool,
    direction: isize,
) -> usize {
    let visible = visible_tui_board_indices(backlog_visible);
    let position = visible
        .iter()
        .position(|board| *board == selected_board)
        .unwrap_or(0);
    let next = (position as isize + direction).rem_euclid(visible.len() as isize) as usize;
    visible[next]
}

pub(super) fn toggle_tui_backlog_column(
    board_dir: &Path,
    board_states: &mut [ListState],
    selected_board: &mut usize,
    backlog_visible: &mut bool,
) -> String {
    *backlog_visible = !*backlog_visible;
    if *backlog_visible {
        let backlog_count = read_task_entries(board_dir, TaskStatus::Backlog)
            .map(|entries| entries.len())
            .unwrap_or(0);
        format!("Backlog column shown ({backlog_count} tasks). Press B again to hide it.")
    } else {
        if *selected_board == BACKLOG_BOARD_INDEX {
            *selected_board = TODO_BOARD_INDEX;
            for state in board_states.iter_mut() {
                state.select(None);
            }
            select_first_task_if_present_in_board(
                board_dir,
                TaskStatus::Todo,
                &mut board_states[TODO_BOARD_INDEX],
            );
        }
        "Backlog column hidden. Press B to show it.".to_string()
    }
}

pub(super) fn move_selected_tui_task_to_backlog(
    board_dir: &Path,
    statuses: &[TaskStatus],
    board_states: &mut [ListState],
    selected_board: &mut usize,
    backlog_visible: bool,
) -> Result<String> {
    if *selected_board == BACKLOG_BOARD_INDEX {
        return Ok("Task is already in backlog.".to_string());
    }

    let Some(idx) = selected_task_index_in_board(
        board_dir,
        statuses[*selected_board],
        &board_states[*selected_board],
    ) else {
        return Ok("No task selected".to_string());
    };

    let from_board = *selected_board;
    move_task_in_board(
        board_dir,
        statuses[from_board],
        TaskStatus::Backlog,
        &(idx + 1).to_string(),
    )?;

    if backlog_visible {
        *selected_board = BACKLOG_BOARD_INDEX;
        for state in board_states.iter_mut() {
            state.select(None);
        }
        select_last_task_if_present_in_board(
            board_dir,
            TaskStatus::Backlog,
            &mut board_states[BACKLOG_BOARD_INDEX],
        );
    } else {
        normalize_board_selection_in_board(
            board_dir,
            statuses[from_board],
            &mut board_states[from_board],
        );
        board_states[BACKLOG_BOARD_INDEX].select(None);
    }

    Ok("Moved task to backlog".to_string())
}

pub(super) fn move_selected_tui_task_to_archive(
    board_dir: &Path,
    statuses: &[TaskStatus],
    board_states: &mut [ListState],
    selected_board: usize,
) -> Result<String> {
    let Some(idx) = selected_task_index_in_board(
        board_dir,
        statuses[selected_board],
        &board_states[selected_board],
    ) else {
        return Ok("No task selected".to_string());
    };

    move_task_to_archive_in_board(board_dir, statuses[selected_board], &(idx + 1).to_string())?;
    normalize_board_selection_in_board(
        board_dir,
        statuses[selected_board],
        &mut board_states[selected_board],
    );

    Ok("Moved task to archive".to_string())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TuiTaskReorderDirection {
    Up,
    Down,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TuiTaskReorganizeDirection {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TuiTaskBoardMoveDirection {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TuiReorganizeInput {
    Exit,
    Move(TuiTaskReorganizeDirection),
    Ignore,
}

pub(super) fn tui_task_reorder_direction(
    key: &crossterm::event::KeyEvent,
) -> Option<TuiTaskReorderDirection> {
    match key.code {
        KeyCode::Up if key.modifiers.contains(KeyModifiers::SHIFT) => {
            Some(TuiTaskReorderDirection::Up)
        }
        KeyCode::Down if key.modifiers.contains(KeyModifiers::SHIFT) => {
            Some(TuiTaskReorderDirection::Down)
        }
        KeyCode::Char('p' | 'P') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(TuiTaskReorderDirection::Up)
        }
        KeyCode::Char('n' | 'N') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(TuiTaskReorderDirection::Down)
        }
        _ => None,
    }
}

pub(super) fn tui_starts_subtask_input(key: &crossterm::event::KeyEvent) -> bool {
    match key.code {
        KeyCode::Char('n') => key.modifiers.is_empty(),
        KeyCode::Char('+') => key.modifiers.difference(KeyModifiers::SHIFT).is_empty(),
        _ => false,
    }
}

pub(super) fn tui_cancels_task_prompt(key: &crossterm::event::KeyEvent) -> bool {
    matches!(key.code, KeyCode::Esc)
        || (matches!(key.code, KeyCode::Char('c' | 'C'))
            && key.modifiers.contains(KeyModifiers::CONTROL))
}

pub(super) fn tui_task_reorganize_direction(
    key: &crossterm::event::KeyEvent,
) -> Option<TuiTaskReorganizeDirection> {
    match key.code {
        KeyCode::Up => Some(TuiTaskReorganizeDirection::Up),
        KeyCode::Down => Some(TuiTaskReorganizeDirection::Down),
        KeyCode::Left => Some(TuiTaskReorganizeDirection::Left),
        KeyCode::Right => Some(TuiTaskReorganizeDirection::Right),
        _ => None,
    }
}

pub(super) fn tui_toggles_reorganize_mode(key: &crossterm::event::KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('r' | 'R'))
        && key.modifiers.difference(KeyModifiers::SHIFT).is_empty()
}

pub(super) fn tui_reorganize_input(key: &crossterm::event::KeyEvent) -> TuiReorganizeInput {
    if matches!(key.code, KeyCode::Esc) || tui_toggles_reorganize_mode(key) {
        TuiReorganizeInput::Exit
    } else if let Some(direction) = tui_task_reorganize_direction(key) {
        TuiReorganizeInput::Move(direction)
    } else {
        TuiReorganizeInput::Ignore
    }
}

pub(super) fn tui_task_column_title(title: &str, selected: bool, reorganizing: bool) -> String {
    if selected && reorganizing {
        format!(" REORGANIZE MODE: {title} [r/Esc exits] ")
    } else if selected {
        format!("{title}   <<<<<< * >>>>>>     ")
    } else {
        title.to_string()
    }
}

pub(super) fn tui_task_column_border_color(default_color: Color, reorganizing: bool) -> Color {
    if reorganizing {
        Color::Yellow
    } else {
        default_color
    }
}

pub(super) fn render_tui_task_column_header(
    f: &mut ratatui::Frame<'_>,
    area: Rect,
    title: &str,
    task_count: usize,
    selected: bool,
    reorganizing: bool,
    border_color: Color,
) -> Rect {
    let header_color = tui_task_column_border_color(border_color, reorganizing);
    let block = Block::default()
        .title(tui_task_column_title(title, selected, reorganizing))
        .title(Line::from(vec![Span::raw(format!(" {task_count} "))]).alignment(Alignment::Right))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(header_color));
    let inner_area = block.inner(area);
    f.render_widget(block, area);
    inner_area
}

pub(super) fn reorder_selected_tui_task(
    board_dir: &Path,
    status: TaskStatus,
    state: &mut ListState,
    direction: TuiTaskReorderDirection,
) -> String {
    let Some(idx) = selected_task_index_in_board(board_dir, status, state) else {
        state.select(None);
        return "No task selected".to_string();
    };

    let target_idx = match direction {
        TuiTaskReorderDirection::Up if idx == 0 => return "Already at the top".to_string(),
        TuiTaskReorderDirection::Up => idx - 1,
        TuiTaskReorderDirection::Down => {
            let tasks = read_tasks_in_board(board_dir, status).unwrap_or_default();
            if tasks.is_empty() {
                state.select(None);
                return "No task selected".to_string();
            }
            if idx >= tasks.len() - 1 {
                return "Already at the bottom".to_string();
            }
            idx + 1
        }
    };

    let result = reorder_task_in_board(board_dir, status, idx, target_idx);
    state.select(Some(target_idx));

    match result {
        Ok(_) => {
            let direction = match direction {
                TuiTaskReorderDirection::Up => "up",
                TuiTaskReorderDirection::Down => "down",
            };
            format!("Moved task {direction} to position {}", target_idx + 1)
        }
        Err(error) => format!("Error: {error}"),
    }
}

pub(super) fn move_selected_tui_task_between_boards(
    board_dir: &Path,
    statuses: &[TaskStatus],
    board_states: &mut [ListState],
    selected_board: &mut usize,
    backlog_visible: bool,
    direction: TuiTaskBoardMoveDirection,
) -> String {
    let Some(idx) = selected_task_index_in_board(
        board_dir,
        statuses[*selected_board],
        &board_states[*selected_board],
    ) else {
        board_states[*selected_board].select(None);
        return "No task selected".to_string();
    };

    let (offset, boundary_message) = match direction {
        TuiTaskBoardMoveDirection::Left => (-1, "Already at the first board"),
        TuiTaskBoardMoveDirection::Right => (1, "Already at the last board"),
    };
    let Some(to_board) = adjacent_visible_tui_board(*selected_board, backlog_visible, offset)
    else {
        return boundary_message.to_string();
    };
    let from = statuses[*selected_board];
    let to = statuses[to_board];

    match move_task_in_board(board_dir, from, to, &(idx + 1).to_string()) {
        Ok(external_completion) => {
            *selected_board = to_board;
            for state in board_states.iter_mut() {
                state.select(None);
            }
            if to == TaskStatus::Done {
                select_first_task_if_present_in_board(
                    board_dir,
                    to,
                    &mut board_states[*selected_board],
                );
            } else {
                select_last_task_if_present_in_board(
                    board_dir,
                    to,
                    &mut board_states[*selected_board],
                );
            }
            if let Some(session_id) = external_completion {
                format!(
                    "Moved task to Done as external completion; cancelled idle managed Git journal for {session_id}"
                )
            } else {
                format!("Moved task to {to}")
            }
        }
        Err(error) => format!("Error: {error}"),
    }
}

pub(super) fn reorganize_selected_tui_task(
    board_dir: &Path,
    statuses: &[TaskStatus],
    board_states: &mut [ListState],
    selected_board: &mut usize,
    backlog_visible: bool,
    direction: TuiTaskReorganizeDirection,
) -> String {
    match direction {
        TuiTaskReorganizeDirection::Up => reorder_selected_tui_task(
            board_dir,
            statuses[*selected_board],
            &mut board_states[*selected_board],
            TuiTaskReorderDirection::Up,
        ),
        TuiTaskReorganizeDirection::Down => reorder_selected_tui_task(
            board_dir,
            statuses[*selected_board],
            &mut board_states[*selected_board],
            TuiTaskReorderDirection::Down,
        ),
        TuiTaskReorganizeDirection::Left => move_selected_tui_task_between_boards(
            board_dir,
            statuses,
            board_states,
            selected_board,
            backlog_visible,
            TuiTaskBoardMoveDirection::Left,
        ),
        TuiTaskReorganizeDirection::Right => move_selected_tui_task_between_boards(
            board_dir,
            statuses,
            board_states,
            selected_board,
            backlog_visible,
            TuiTaskBoardMoveDirection::Right,
        ),
    }
}

pub(super) fn task_display_height(
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

pub(super) fn keep_selected_task_visible(
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Mode {
    View,
    Reorganize,
    Input,
    Edit,
    Help,
}

pub(super) const PASTED_CONTENT_MARKER_START: u32 = 0xe000;
pub(super) const PASTED_CONTENT_MARKER_END: u32 = 0xf8ff;

pub(super) struct PastedContent {
    pub(super) marker: char,
    pub(super) content: String,
    pub(super) line_count: usize,
}

impl PastedContent {
    pub(super) fn label(&self) -> String {
        let noun = if self.line_count == 1 {
            "line"
        } else {
            "lines"
        };
        format!("[Pasted Content {} {}]", self.line_count, noun)
    }
}

#[derive(Default)]
pub(super) struct TaskInput {
    pub(super) input: Input,
    pub(super) pasted_content: Vec<PastedContent>,
}

impl TaskInput {
    pub(super) fn new(value: String) -> Self {
        Self {
            input: Input::new(value),
            pasted_content: Vec::new(),
        }
    }

    pub(super) fn reset(&mut self) {
        self.input.reset();
        self.pasted_content.clear();
    }

    pub(super) fn insert_paste(&mut self, content: String) {
        if content.is_empty() {
            return;
        }

        let content = content.replace("\r\n", "\n").replace('\r', "\n");
        if !content.contains('\n') {
            for ch in content.chars() {
                self.input.handle(InputRequest::InsertChar(ch));
            }
            return;
        }

        let marker_value = PASTED_CONTENT_MARKER_START + self.pasted_content.len() as u32;
        let Some(marker) = (marker_value <= PASTED_CONTENT_MARKER_END)
            .then(|| char::from_u32(marker_value))
            .flatten()
        else {
            for ch in content.chars() {
                self.input.handle(InputRequest::InsertChar(ch));
            }
            return;
        };

        let line_count = content.lines().count().max(1);
        self.input.handle(InputRequest::InsertChar(marker));
        self.pasted_content.push(PastedContent {
            marker,
            content,
            line_count,
        });
    }

    pub(super) fn pasted_for_marker(&self, marker: char) -> Option<&PastedContent> {
        self.pasted_content
            .iter()
            .find(|pasted| pasted.marker == marker)
    }

    pub(super) fn display_value(&self) -> String {
        let mut value = String::new();
        for ch in self.input.value().chars() {
            if let Some(pasted) = self.pasted_for_marker(ch) {
                value.push_str(&pasted.label());
            } else {
                value.push(ch);
            }
        }
        value
    }

    pub(super) fn display_cursor(&self) -> usize {
        self.input
            .value()
            .chars()
            .take(self.input.cursor())
            .map(|ch| {
                self.pasted_for_marker(ch)
                    .map(|pasted| pasted.label().chars().count())
                    .unwrap_or(1)
            })
            .sum()
    }

    pub(super) fn submitted_value(&self) -> String {
        let mut value = String::new();
        for ch in self.input.value().chars() {
            if let Some(pasted) = self.pasted_for_marker(ch) {
                value.push_str(&pasted.content);
            } else {
                value.push(ch);
            }
        }
        value
    }
}

pub(super) fn append_styled_wrapped_text(
    lines: &mut Vec<Vec<Span<'static>>>,
    col: &mut usize,
    text: &str,
    width: usize,
    style: Style,
) {
    for ch in text.chars() {
        if ch == '\n' {
            lines.push(Vec::new());
            *col = 0;
            continue;
        }
        if width > 0 && *col >= width {
            lines.push(Vec::new());
            *col = 0;
        }
        lines
            .last_mut()
            .expect("input display always has a line")
            .push(Span::styled(ch.to_string(), style));
        *col += 1;
    }
}

pub(super) fn styled_task_input_lines(
    label: &str,
    input: &TaskInput,
    width: usize,
) -> Vec<Line<'static>> {
    let normal_style = Style::default().fg(Color::White);
    let pasted_style = Style::default().fg(Color::Blue);
    let mut lines = vec![Vec::new()];
    let mut col = 0;

    append_styled_wrapped_text(&mut lines, &mut col, label, width, normal_style);
    for ch in input.input.value().chars() {
        if let Some(pasted) = input.pasted_for_marker(ch) {
            append_styled_wrapped_text(&mut lines, &mut col, &pasted.label(), width, pasted_style);
        } else {
            append_styled_wrapped_text(&mut lines, &mut col, &ch.to_string(), width, normal_style);
        }
    }

    lines.into_iter().map(Line::from).collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TuiPane {
    Tasks,
    AgentProjects,
    Models,
}

pub(super) fn tui_pane_after_tab(current: TuiPane, active_board: bool) -> TuiPane {
    match current {
        TuiPane::Tasks => TuiPane::AgentProjects,
        TuiPane::AgentProjects if active_board => TuiPane::Tasks,
        TuiPane::AgentProjects => TuiPane::AgentProjects,
        TuiPane::Models => TuiPane::AgentProjects,
    }
}

pub(super) fn tui_models_return_pane(opened_from: TuiPane) -> TuiPane {
    match opened_from {
        TuiPane::Tasks => TuiPane::Tasks,
        TuiPane::AgentProjects | TuiPane::Models => TuiPane::AgentProjects,
    }
}

fn tui_toggles_models(key: &KeyEvent) -> bool {
    // Enhanced keyboard reporting can send the unshifted character plus Shift.
    key.modifiers.difference(KeyModifiers::SHIFT).is_empty()
        && match key.code {
            KeyCode::Char('M') => true,
            KeyCode::Char('m') => key.modifiers.contains(KeyModifiers::SHIFT),
            _ => false,
        }
}

pub(super) struct TuiStartState {
    pub(super) active_board: bool,
    pub(super) current_pane: TuiPane,
    pub(super) feedback_buffer: String,
}

#[derive(Clone, Debug, Default)]
pub(super) struct TuiTaskSnapshot {
    pub(super) board_title: String,
    pub(super) board_entries: [Vec<TaskEntry>; 4],
    pub(super) archived_entries: Vec<TaskEntry>,
}

impl TuiTaskSnapshot {
    pub(super) fn load(active_root: &Path, board_dir: &Path, active_board: bool) -> Self {
        if !active_board {
            return Self {
                board_title: "No Active Board".to_string(),
                ..Self::default()
            };
        }

        Self {
            board_title: board_display_name(active_root, board_dir),
            board_entries: TASK_STATUSES
                .map(|status| read_task_entries(board_dir, status).unwrap_or_default()),
            archived_entries: read_archived_task_entries(board_dir).unwrap_or_default(),
        }
    }
}

pub(super) fn tui_task_board_instructions() -> &'static str {
    "Arrows navigate boards and tasks, Enter opens subtasks, e edits, n or + creates a subtask under the selected task, and Space creates a task. Press r to reorganize; use Shift+Arrows to move tasks. Tab opens Agent Projects, M opens Models, and h/? opens Help. Codex: s stops/resumes, i interrupts for interaction, c opens linked idle Doing, completed, or blocked sessions, and l shows logs."
}

pub(super) fn tui_start_state(active_board: bool) -> TuiStartState {
    if active_board {
        TuiStartState {
            active_board,
            current_pane: TuiPane::Tasks,
            feedback_buffer: tui_task_board_instructions().to_string(),
        }
    } else {
        TuiStartState {
            active_board,
            current_pane: TuiPane::AgentProjects,
            feedback_buffer: String::from(TUI_NO_ACTIVE_BOARD_MESSAGE),
        }
    }
}

pub(super) struct TuiAgentProject {
    pub(super) project: agent::AgentProject,
    pub(super) scan: AgentProjectScan,
    pub(super) runtime_state: TuiAgentRuntimeState,
    pub(super) daemon_scan_problem: Option<String>,
    pub(super) failure_problem: Option<String>,
}

impl TuiAgentProject {
    pub(super) fn displayed_problem(&self) -> Option<&str> {
        self.daemon_scan_problem.as_deref().or_else(|| {
            (self.runtime_state == TuiAgentRuntimeState::Error)
                .then_some(self.failure_problem.as_deref())
                .flatten()
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TuiAgentRuntimeState {
    Idle,
    Running,
    Finalizing,
    PushPending,
    Interactive,
    Fenced,
    Stale,
    Error,
}

impl TuiAgentRuntimeState {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Idle => "IDLE",
            Self::Running => "RUNNING",
            Self::Finalizing => "FINAL",
            Self::PushPending => "PUSH",
            Self::Interactive => "INTERACTIVE",
            Self::Fenced => "FENCED",
            Self::Stale => "STALE",
            Self::Error => "ERROR",
        }
    }

    pub(super) fn is_running(self) -> bool {
        self == Self::Running
    }
}

pub(super) struct TuiAgentPanelSnapshot {
    pub(super) projects: Vec<TuiAgentProject>,
    pub(super) daemon_status: String,
}

pub(super) struct TuiAgentPanelRefreshResult {
    pub(super) active_root: PathBuf,
    pub(super) panel_snapshot: Result<TuiAgentPanelSnapshot>,
    pub(super) task_session_states: Result<TaskAgentSessionStates>,
}

pub(super) struct TuiAgentPanelRefreshWorker {
    pub(super) receiver: Option<Receiver<TuiAgentPanelRefreshResult>>,
}

impl TuiAgentPanelRefreshWorker {
    pub(super) fn new() -> Self {
        Self { receiver: None }
    }

    pub(super) fn request(&mut self, active_root: &Path) -> bool {
        self.request_with(active_root, |active_root| TuiAgentPanelRefreshResult {
            panel_snapshot: load_tui_agent_panel_snapshot_with_service_recovery(&active_root),
            task_session_states: try_load_task_agent_session_states(&active_root),
            active_root,
        })
    }

    pub(super) fn request_with(
        &mut self,
        active_root: &Path,
        load: impl FnOnce(PathBuf) -> TuiAgentPanelRefreshResult + Send + 'static,
    ) -> bool {
        if self.receiver.is_some() {
            return false;
        }

        let (sender, receiver) = mpsc::channel();
        let active_root = active_root.to_path_buf();
        thread::spawn(move || {
            let _ = sender.send(load(active_root));
        });
        self.receiver = Some(receiver);
        true
    }

    pub(super) fn try_result(&mut self) -> Option<TuiAgentPanelRefreshResult> {
        let result = self.receiver.as_ref()?.try_recv();
        match result {
            Ok(result) => {
                self.receiver = None;
                Some(result)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.receiver = None;
                None
            }
        }
    }
}

pub(super) struct TuiCurrentProjectRegistration {
    pub(super) path: PathBuf,
    pub(super) name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct TuiAgentProjectRemoval {
    pub(super) path: PathBuf,
    pub(super) name: String,
}

pub(super) enum TuiAgentPanelRow<'a> {
    RegisterCurrentProject(&'a TuiCurrentProjectRegistration),
    Project(&'a TuiAgentProject),
}

pub(super) struct TuiAgentPanel {
    pub(super) projects: Vec<TuiAgentProject>,
    pub(super) current_project_registration: Option<TuiCurrentProjectRegistration>,
    pub(super) daemon_status: String,
    pub(super) state: ListState,
    pub(super) scroll_offset: usize,
    pub(super) last_error: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TuiModelsFocus {
    Providers,
    Models,
}

pub(super) struct TuiModelsPanel {
    pub(super) providers: Vec<agent::AgentModelProvider>,
    pub(super) models: Vec<agent::AgentModelTarget>,
    pub(super) defaults: agent::AgentModelDefaults,
    pub(super) codex_default: String,
    pub(super) codex_default_provider: Option<String>,
    pub(super) codex_default_model: Option<String>,
    pub(super) focus: TuiModelsFocus,
    pub(super) provider_state: ListState,
    pub(super) model_state: ListState,
    pub(super) model_search: String,
    pub(super) provider_viewport_height: usize,
    pub(super) model_viewport_height: usize,
    pub(super) last_error: Option<String>,
}

pub(super) enum TuiModelInputKind {
    SearchModels,
    AddModel {
        provider_id: String,
    },
    CustomProvider {
        step: usize,
        provider_id: String,
        name: String,
        base_url: String,
    },
}

pub(super) struct TuiModelInput {
    pub(super) kind: TuiModelInputKind,
    pub(super) input: Input,
}

impl TuiModelInput {
    pub(super) fn search_models(query: String) -> Self {
        Self {
            kind: TuiModelInputKind::SearchModels,
            input: Input::new(query),
        }
    }

    pub(super) fn add_model(provider_id: String) -> Self {
        Self {
            kind: TuiModelInputKind::AddModel { provider_id },
            input: Input::default(),
        }
    }

    pub(super) fn custom_provider() -> Self {
        Self {
            kind: TuiModelInputKind::CustomProvider {
                step: 0,
                provider_id: String::new(),
                name: String::new(),
                base_url: String::new(),
            },
            input: Input::default(),
        }
    }

    pub(super) fn label(&self) -> &'static str {
        match &self.kind {
            TuiModelInputKind::SearchModels => " Search Models: ",
            TuiModelInputKind::AddModel { .. } => " Model ID: ",
            TuiModelInputKind::CustomProvider { step, .. } => match step {
                0 => " Endpoint Name: ",
                1 => " API Base URL (usually .../v1): ",
                _ => " API Key Env Var (optional): ",
            },
        }
    }

    pub(super) fn guidance(&self) -> &'static str {
        match &self.kind {
            TuiModelInputKind::SearchModels => {
                "Enter filters by model name or ID; submit an empty search to show every model"
            }
            TuiModelInputKind::AddModel { .. } => "Enter the exact model ID used by the endpoint",
            TuiModelInputKind::CustomProvider { step, .. } => match step {
                0 => "Enter a friendly name, for example My Local Server",
                1 => {
                    "Enter the API root, for example http://127.0.0.1:9090/v1; do not include /chat, /models, or /responses"
                }
                _ => {
                    "Enter an API-key environment variable name, or press Enter if none is required"
                }
            },
        }
    }

    pub(super) fn insert_paste(&mut self, content: &str) {
        for ch in content.chars().filter(|ch| *ch != '\r' && *ch != '\n') {
            self.input.handle(InputRequest::InsertChar(ch));
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct TuiCodexSessionTarget {
    pub(super) project_id: i64,
    pub(super) project_path: PathBuf,
    pub(super) session_id: String,
}

impl TuiCodexSessionTarget {
    pub(super) fn new(project: &agent::AgentProject, session_id: String) -> Self {
        Self {
            project_id: project.id,
            project_path: project.path.clone(),
            session_id,
        }
    }
}

pub(super) struct TuiAgentLogView {
    pub(super) project_name: String,
    pub(super) path: Option<PathBuf>,
    pub(super) settings_path: Option<PathBuf>,
    pub(super) settings: AgentRunSettings,
    pub(super) content: String,
    pub(super) is_live: bool,
    pub(super) session_target: Option<TuiCodexSessionTarget>,
}

impl TuiAgentLogView {
    pub(super) fn new(
        project_name: String,
        path: PathBuf,
        settings_path: Option<PathBuf>,
        is_live: bool,
        session_target: Option<TuiCodexSessionTarget>,
    ) -> Result<Self> {
        let mut view = Self {
            project_name,
            path: Some(path),
            settings_path,
            settings: AgentRunSettings::default(),
            content: String::new(),
            is_live,
            session_target,
        };
        view.refresh()?;
        Ok(view)
    }

    pub(super) fn message(project_name: String, content: String) -> Self {
        Self {
            project_name,
            path: None,
            settings_path: None,
            settings: AgentRunSettings::default(),
            content,
            is_live: false,
            session_target: None,
        }
    }

    pub(super) fn refresh(&mut self) -> Result<()> {
        let Some(path) = self.path.as_ref() else {
            return Ok(());
        };
        let bytes =
            fs::read(path).with_context(|| format!("Failed to read agent output log {path:?}"))?;
        self.content = if bytes.is_empty() {
            "Waiting for agent output...".to_string()
        } else {
            String::from_utf8_lossy(&bytes).into_owned()
        };
        self.settings = self
            .settings_path
            .as_deref()
            .and_then(|path| agent_run_settings_from_log(path).ok())
            .unwrap_or_default();
        Ok(())
    }

    pub(super) fn settings_label(&self) -> String {
        format!(
            " Model: {} | Thinking: {} ",
            self.settings.model.as_deref().unwrap_or("unknown"),
            self.settings
                .reasoning_effort
                .as_deref()
                .unwrap_or("unknown"),
        )
    }
}

impl TuiAgentPanel {
    pub(super) fn new(active_root: &Path) -> Self {
        let mut panel = Self {
            projects: Vec::new(),
            current_project_registration: current_project_registration(active_root, &[]),
            daemon_status: "loading".to_string(),
            state: ListState::default(),
            scroll_offset: 0,
            last_error: None,
        };
        panel.restore_or_normalize_selection(None);
        panel
    }

    pub(super) fn refresh(&mut self, active_root: &Path) {
        let selected_row = self.selected_row_identity();
        self.apply_refresh_result(
            active_root,
            selected_row,
            load_tui_agent_panel_snapshot(active_root),
        );
    }

    pub(super) fn apply_refresh_result(
        &mut self,
        active_root: &Path,
        selected_row: Option<TuiAgentPanelRowIdentity>,
        result: Result<TuiAgentPanelSnapshot>,
    ) {
        match result {
            Ok(snapshot) => {
                self.projects = snapshot.projects;
                self.daemon_status = snapshot.daemon_status;
                self.current_project_registration =
                    current_project_registration(active_root, &self.projects);
                self.last_error = None;
                self.restore_or_normalize_selection(selected_row);
            }
            Err(err) => {
                self.last_error = Some(format!("Agent registry unavailable: {err}"));
            }
        }
    }

    pub(super) fn row_count(&self) -> usize {
        self.projects.len() + usize::from(self.current_project_registration.is_some())
    }

    pub(super) fn project_start_index(&self) -> usize {
        usize::from(self.current_project_registration.is_some())
    }

    pub(super) fn selected_row_identity(&self) -> Option<TuiAgentPanelRowIdentity> {
        match self.selected_row()? {
            TuiAgentPanelRow::RegisterCurrentProject(registration) => Some(
                TuiAgentPanelRowIdentity::RegisterCurrentProject(registration.path.clone()),
            ),
            TuiAgentPanelRow::Project(project) => {
                Some(TuiAgentPanelRowIdentity::Project(project.project.id))
            }
        }
    }

    pub(super) fn selected_row(&self) -> Option<TuiAgentPanelRow<'_>> {
        let idx = self.state.selected()?;
        if let Some(registration) = self.current_project_registration.as_ref()
            && idx == 0
        {
            return Some(TuiAgentPanelRow::RegisterCurrentProject(registration));
        }

        self.projects
            .get(idx.checked_sub(self.project_start_index())?)
            .map(TuiAgentPanelRow::Project)
    }

    pub(super) fn selected_project(&self) -> Option<&TuiAgentProject> {
        match self.selected_row()? {
            TuiAgentPanelRow::Project(project) => Some(project),
            TuiAgentPanelRow::RegisterCurrentProject(_) => None,
        }
    }

    pub(super) fn selected_current_project_registration(
        &self,
    ) -> Option<&TuiCurrentProjectRegistration> {
        match self.selected_row()? {
            TuiAgentPanelRow::RegisterCurrentProject(registration) => Some(registration),
            TuiAgentPanelRow::Project(_) => None,
        }
    }

    pub(super) fn restore_or_normalize_selection(
        &mut self,
        selected_row: Option<TuiAgentPanelRowIdentity>,
    ) {
        let row_count = self.row_count();
        if row_count == 0 {
            self.state.select(None);
            self.scroll_offset = 0;
            return;
        }

        if let Some(selected_row) = selected_row {
            match selected_row {
                TuiAgentPanelRowIdentity::RegisterCurrentProject(path) => {
                    if self
                        .current_project_registration
                        .as_ref()
                        .is_some_and(|registration| registration.path == path)
                    {
                        self.state.select(Some(0));
                        return;
                    }

                    if let Some(project_idx) = self
                        .projects
                        .iter()
                        .position(|project| project.project.path == path)
                    {
                        self.state
                            .select(Some(self.project_start_index() + project_idx));
                        return;
                    }
                }
                TuiAgentPanelRowIdentity::Project(project_id) => {
                    if let Some(project_idx) = self
                        .projects
                        .iter()
                        .position(|project| project.project.id == project_id)
                    {
                        self.state
                            .select(Some(self.project_start_index() + project_idx));
                        return;
                    }
                }
            }
        }

        let idx = self
            .state
            .selected()
            .filter(|idx| *idx < row_count)
            .unwrap_or(0);
        self.state.select(Some(idx));
    }

    pub(super) fn select_nearest_row(&mut self, preferred_idx: usize) {
        let row_count = self.row_count();
        if row_count == 0 {
            self.state.select(None);
            self.scroll_offset = 0;
        } else {
            self.state.select(Some(preferred_idx.min(row_count - 1)));
            self.scroll_offset = self.scroll_offset.min(row_count - 1);
        }
    }

    pub(super) fn select_project_for_path(&mut self, path: &Path) -> bool {
        if self
            .current_project_registration
            .as_ref()
            .is_some_and(|registration| registration.path == path)
        {
            self.state.select(Some(0));
            return true;
        }

        if let Some(project_idx) = self
            .projects
            .iter()
            .position(|project| project.project.path == path)
        {
            self.state
                .select(Some(self.project_start_index() + project_idx));
            true
        } else {
            false
        }
    }

    pub(super) fn select_previous(&mut self) {
        let row_count = self.row_count();
        if row_count == 0 {
            self.state.select(None);
            self.scroll_offset = 0;
            return;
        }

        let idx = self.state.selected().unwrap_or(0);
        if idx > 0 {
            self.state.select(Some(idx - 1));
        } else {
            self.state.select(Some(row_count - 1));
        }
    }

    pub(super) fn select_next(&mut self) {
        let row_count = self.row_count();
        if row_count == 0 {
            self.state.select(None);
            self.scroll_offset = 0;
            return;
        }

        let idx = self.state.selected().unwrap_or(0);
        if idx + 1 < row_count {
            self.state.select(Some(idx + 1));
        } else {
            self.state.select(Some(0));
        }
    }

    pub(super) fn keep_selection_visible(&mut self, viewport_height: usize) {
        let row_count = self.row_count();
        if row_count == 0 || viewport_height == 0 {
            self.scroll_offset = 0;
            return;
        }

        let Some(selected_idx) = self.state.selected().filter(|idx| *idx < row_count) else {
            self.scroll_offset = self.scroll_offset.min(row_count - 1);
            return;
        };

        if selected_idx < self.scroll_offset {
            self.scroll_offset = selected_idx;
        }

        let last_visible = self
            .scroll_offset
            .saturating_add(viewport_height.saturating_sub(1));
        if selected_idx > last_visible {
            self.scroll_offset = selected_idx.saturating_sub(viewport_height.saturating_sub(1));
        }
    }
}

impl TuiModelsPanel {
    pub(super) fn new() -> Self {
        Self {
            providers: Vec::new(),
            models: Vec::new(),
            defaults: agent::AgentModelDefaults::default(),
            codex_default: "not explicitly set".to_string(),
            codex_default_provider: None,
            codex_default_model: None,
            focus: TuiModelsFocus::Providers,
            provider_state: ListState::default(),
            model_state: ListState::default(),
            model_search: String::new(),
            provider_viewport_height: 0,
            model_viewport_height: 0,
            last_error: None,
        }
    }

    pub(super) fn selected_provider(&self) -> Option<&agent::AgentModelProvider> {
        self.provider_state
            .selected()
            .and_then(|index| self.providers.get(index))
    }

    pub(super) fn selected_model(&self) -> Option<&agent::AgentModelTarget> {
        self.model_state
            .selected()
            .and_then(|index| self.models.get(index))
    }

    pub(super) fn visible_model_indices(&self) -> Vec<usize> {
        let query = self.model_search.trim().to_lowercase();
        self.models
            .iter()
            .enumerate()
            .filter_map(|(index, model)| {
                (query.is_empty()
                    || model.model_id.to_lowercase().contains(&query)
                    || model.label.to_lowercase().contains(&query))
                .then_some(index)
            })
            .collect()
    }

    pub(super) fn normalize_model_selection(&mut self) {
        let visible = self.visible_model_indices();
        let selected = self.model_state.selected();
        self.model_state.select(
            selected
                .filter(|index| visible.contains(index))
                .or_else(|| visible.first().copied()),
        );
    }

    pub(super) fn set_model_search(&mut self, query: String) -> usize {
        self.model_search = query.trim().to_string();
        self.focus = TuiModelsFocus::Models;
        self.normalize_model_selection();
        self.visible_model_indices().len()
    }

    pub(super) fn refresh(&mut self) {
        let selected_provider = self.selected_provider().map(|provider| provider.id.clone());
        let selected_model = self.selected_model().map(|model| model.model_id.clone());
        let result = (|| -> Result<_> {
            let store = open_agent_store()?;
            let providers = store.list_model_providers_blocking()?;
            let defaults = store.model_defaults_blocking()?;
            Ok((store, providers, defaults))
        })();
        let (store, providers, defaults) = match result {
            Ok(value) => value,
            Err(error) => {
                self.last_error = Some(error.to_string());
                return;
            }
        };

        self.providers = providers;
        self.defaults = defaults;
        match codex_config_path().and_then(|path| read_codex_default_config_at(&path)) {
            Ok((provider, model)) => {
                self.codex_default = match (&provider, &model) {
                    (Some(provider), Some(model)) => format!("{provider}/{model}"),
                    (_, Some(model)) => model.clone(),
                    _ => "not explicitly set".to_string(),
                };
                self.codex_default_provider = provider;
                self.codex_default_model = model;
            }
            Err(error) => {
                self.codex_default = format!("config error: {error}");
                self.codex_default_provider = None;
                self.codex_default_model = None;
            }
        }
        let provider_index = selected_provider
            .as_ref()
            .and_then(|id| {
                self.providers
                    .iter()
                    .position(|provider| &provider.id == id)
            })
            .unwrap_or(0);
        if self.providers.is_empty() {
            self.provider_state.select(None);
            self.models.clear();
            self.model_state.select(None);
        } else {
            self.provider_state.select(Some(provider_index));
            let provider_id = self.providers[provider_index].id.clone();
            match store
                .list_model_targets_blocking(Some(&provider_id))
                .and_then(|mut models| {
                    include_codex_default_model_target(
                        &store,
                        &provider_id,
                        self.codex_default_provider.as_deref(),
                        self.codex_default_model.as_deref(),
                        &mut models,
                    )?;
                    Ok(models)
                }) {
                Ok(models) => {
                    self.models = models;
                    let model_index = selected_model
                        .as_ref()
                        .and_then(|id| self.models.iter().position(|model| &model.model_id == id))
                        .unwrap_or(0);
                    self.model_state
                        .select((!self.models.is_empty()).then_some(model_index));
                    self.normalize_model_selection();
                    self.last_error = None;
                }
                Err(error) => self.last_error = Some(error.to_string()),
            }
        }
    }

    pub(super) fn refresh_models(&mut self) {
        let selected_model = self.selected_model().map(|model| model.model_id.clone());
        let Some(provider_id) = self.selected_provider().map(|provider| provider.id.clone()) else {
            self.models.clear();
            self.model_state.select(None);
            return;
        };
        match open_agent_store().and_then(|store| {
            self.defaults = store.model_defaults_blocking()?;
            let mut models = store.list_model_targets_blocking(Some(&provider_id))?;
            include_codex_default_model_target(
                &store,
                &provider_id,
                self.codex_default_provider.as_deref(),
                self.codex_default_model.as_deref(),
                &mut models,
            )?;
            Ok(models)
        }) {
            Ok(models) => {
                self.models = models;
                let index = selected_model
                    .as_ref()
                    .and_then(|id| self.models.iter().position(|model| &model.model_id == id))
                    .unwrap_or(0);
                self.model_state
                    .select((!self.models.is_empty()).then_some(index));
                self.normalize_model_selection();
                self.last_error = None;
            }
            Err(error) => self.last_error = Some(error.to_string()),
        }
    }

    pub(super) fn select_previous(&mut self) {
        match self.focus {
            TuiModelsFocus::Providers => {
                let len = self.providers.len();
                if len > 0 {
                    let index = self.provider_state.selected().unwrap_or(0);
                    self.provider_state
                        .select(Some(if index == 0 { len - 1 } else { index - 1 }));
                    self.refresh_models();
                }
            }
            TuiModelsFocus::Models => {
                let visible = self.visible_model_indices();
                if !visible.is_empty() {
                    let position = self
                        .model_state
                        .selected()
                        .and_then(|selected| visible.iter().position(|index| *index == selected))
                        .unwrap_or(0);
                    let previous = if position == 0 {
                        visible.len() - 1
                    } else {
                        position - 1
                    };
                    self.model_state.select(Some(visible[previous]));
                }
            }
        }
    }

    pub(super) fn select_next(&mut self) {
        match self.focus {
            TuiModelsFocus::Providers => {
                let len = self.providers.len();
                if len > 0 {
                    let index = self.provider_state.selected().unwrap_or(0);
                    self.provider_state.select(Some((index + 1) % len));
                    self.refresh_models();
                }
            }
            TuiModelsFocus::Models => {
                let visible = self.visible_model_indices();
                if !visible.is_empty() {
                    let position = self
                        .model_state
                        .selected()
                        .and_then(|selected| visible.iter().position(|index| *index == selected))
                        .unwrap_or(0);
                    self.model_state
                        .select(Some(visible[(position + 1) % visible.len()]));
                }
            }
        }
    }

    pub(super) fn select_first(&mut self) {
        match self.focus {
            TuiModelsFocus::Providers => {
                self.provider_state
                    .select((!self.providers.is_empty()).then_some(0));
                self.refresh_models();
            }
            TuiModelsFocus::Models => {
                self.model_state
                    .select(self.visible_model_indices().first().copied());
            }
        }
    }

    pub(super) fn select_last(&mut self) {
        match self.focus {
            TuiModelsFocus::Providers => {
                self.provider_state
                    .select(self.providers.len().checked_sub(1));
                self.refresh_models();
            }
            TuiModelsFocus::Models => {
                self.model_state
                    .select(self.visible_model_indices().last().copied());
            }
        }
    }

    pub(super) fn select_page_up(&mut self) {
        match self.focus {
            TuiModelsFocus::Providers => {
                let Some(selected) = self.provider_state.selected() else {
                    return;
                };
                self.provider_state.select(Some(
                    selected.saturating_sub(self.provider_viewport_height.max(1)),
                ));
                self.refresh_models();
            }
            TuiModelsFocus::Models => {
                let visible = self.visible_model_indices();
                let Some(position) = self
                    .model_state
                    .selected()
                    .and_then(|selected| visible.iter().position(|index| *index == selected))
                else {
                    return;
                };
                let target = position.saturating_sub(self.model_viewport_height.max(1));
                self.model_state.select(Some(visible[target]));
            }
        }
    }

    pub(super) fn select_page_down(&mut self) {
        match self.focus {
            TuiModelsFocus::Providers => {
                let Some(selected) = self.provider_state.selected() else {
                    return;
                };
                let Some(last) = self.providers.len().checked_sub(1) else {
                    return;
                };
                self.provider_state.select(Some(
                    selected
                        .saturating_add(self.provider_viewport_height.max(1))
                        .min(last),
                ));
                self.refresh_models();
            }
            TuiModelsFocus::Models => {
                let visible = self.visible_model_indices();
                let Some(position) = self
                    .model_state
                    .selected()
                    .and_then(|selected| visible.iter().position(|index| *index == selected))
                else {
                    return;
                };
                let target = position
                    .saturating_add(self.model_viewport_height.max(1))
                    .min(visible.len() - 1);
                self.model_state.select(Some(visible[target]));
            }
        }
    }
}

pub(super) fn custom_provider_id(name: &str, providers: &[agent::AgentModelProvider]) -> String {
    let mut stem = String::new();
    let mut pending_separator = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_separator && !stem.is_empty() {
                stem.push('-');
            }
            stem.push(ch.to_ascii_lowercase());
            pending_separator = false;
        } else {
            pending_separator = true;
        }
    }
    if stem.is_empty() {
        stem = "local".to_string();
    }

    let mut candidate = stem.clone();
    let mut suffix = 2;
    while providers.iter().any(|provider| provider.id == candidate) {
        candidate = format!("{stem}-{suffix}");
        suffix += 1;
    }
    candidate
}

pub(super) fn openai_models_url(base_url: &str) -> String {
    let base_url = base_url.trim().trim_end_matches('/');
    if base_url.ends_with("/models") {
        base_url.to_string()
    } else {
        format!("{base_url}/models")
    }
}

pub(super) fn normalize_openai_api_base_url(input: &str) -> Result<String> {
    let base_url = input.trim().trim_end_matches('/');
    if !base_url.starts_with("http://") && !base_url.starts_with("https://") {
        anyhow::bail!("API base URL must start with http:// or https://");
    }
    if base_url.contains(['?', '#']) {
        anyhow::bail!("API base URL cannot contain a query string or fragment");
    }

    let lowercase = base_url.to_ascii_lowercase();
    let operation_paths = ["/chat", "/chat/completions", "/models", "/responses"];
    if operation_paths
        .iter()
        .any(|operation| lowercase.ends_with(operation))
    {
        anyhow::bail!(
            "Enter the API root (for example http://127.0.0.1:9090/v1), not a /chat, /chat/completions, /models, or /responses URL"
        );
    }

    Ok(base_url.to_string())
}

pub(super) fn parse_openai_model_ids(value: &serde_json::Value) -> Result<Vec<String>> {
    let data = value
        .get("data")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            anyhow::anyhow!("the response does not contain an OpenAI-style data list")
        })?;
    let mut model_ids = data
        .iter()
        .filter_map(|model| model.get("id").and_then(serde_json::Value::as_str))
        .map(str::trim)
        .filter(|model_id| !model_id.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    model_ids.sort_by_key(|model_id| model_id.to_ascii_lowercase());
    model_ids.dedup();
    if model_ids.is_empty() {
        anyhow::bail!("the endpoint returned no model IDs");
    }
    Ok(model_ids)
}

pub(super) fn discover_openai_model_ids(
    base_url: &str,
    api_key: Option<&str>,
) -> Result<Vec<String>> {
    let url = openai_models_url(base_url);
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(
            TUI_MODEL_DISCOVERY_TIMEOUT_SECONDS,
        )))
        .build();
    let agent: ureq::Agent = config.into();
    let mut request = agent.get(&url).header("Accept", "application/json");
    if let Some(api_key) = api_key {
        request = request.header("Authorization", format!("Bearer {api_key}"));
    }
    let mut response = request
        .call()
        .with_context(|| format!("could not query {url}"))?;
    let value = response
        .body_mut()
        .read_json::<serde_json::Value>()
        .with_context(|| format!("{url} did not return valid JSON"))?;
    parse_openai_model_ids(&value)
}

pub(super) fn save_discovered_model_ids(
    store: &agent::TursoAgentStore,
    provider_id: &str,
    model_ids: &[String],
) -> Result<usize> {
    let existing = store.list_model_targets_blocking(Some(provider_id))?;
    let mut added = 0;
    for model_id in model_ids {
        if existing.iter().any(|model| model.model_id == *model_id) {
            continue;
        }
        store.upsert_model_target_blocking(&agent::AgentModelTarget {
            provider_id: provider_id.to_string(),
            model_id: model_id.clone(),
            label: model_id.clone(),
            enabled: false,
            favorite: false,
            reasoning_effort: None,
        })?;
        added += 1;
    }
    Ok(added)
}

pub(super) fn discover_tui_provider_models(panel: &mut TuiModelsPanel) -> Result<String> {
    let provider = panel
        .selected_provider()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Select a provider first"))?;
    let base_url = provider
        .base_url
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("{} uses Codex's built-in model catalog", provider.name))?;
    let api_key = match provider.env_key.as_deref() {
        Some(env_key) => Some(
            std::env::var(env_key)
                .ok()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "{env_key} is not set; export it, restart CLT, then press r again"
                    )
                })?,
        ),
        None => None,
    };
    let model_ids = discover_openai_model_ids(base_url, api_key.as_deref())?;
    let store = open_agent_store()?;
    let added = save_discovered_model_ids(&store, &provider.id, &model_ids)?;
    panel.refresh_models();
    panel.focus = TuiModelsFocus::Models;
    Ok(format!(
        "Found {} models for {}; {} new models start OFF. Choose with Up/Down and Space",
        model_ids.len(),
        provider.name,
        added
    ))
}

pub(super) fn add_tui_model_provider_preset(
    panel: &mut TuiModelsPanel,
    index: usize,
) -> Result<String> {
    let preset = AGENT_PROVIDER_PRESETS
        .get(index)
        .ok_or_else(|| anyhow::anyhow!("Unknown provider preset"))?;
    if let Some(base_url) = preset.base_url {
        upsert_codex_provider_config_at(
            &codex_config_path()?,
            preset.id,
            preset.name,
            base_url,
            preset.env_key,
        )?;
    }
    let store = open_agent_store()?;
    store.upsert_model_provider_blocking(&agent::AgentModelProvider {
        id: preset.id.to_string(),
        name: preset.name.to_string(),
        base_url: preset.base_url.map(str::to_string),
        env_key: preset.env_key.map(str::to_string),
        built_in: preset.built_in,
        enabled: true,
    })?;
    if preset.id == "openrouter" {
        store.upsert_model_target_blocking(&agent::AgentModelTarget {
            provider_id: preset.id.to_string(),
            model_id: "openai/gpt-5.6".to_string(),
            label: "OpenAI GPT-5.6".to_string(),
            enabled: true,
            favorite: false,
            reasoning_effort: None,
        })?;
    }
    panel.refresh();
    if let Some(index) = panel
        .providers
        .iter()
        .position(|provider| provider.id == preset.id)
    {
        panel.provider_state.select(Some(index));
        panel.refresh_models();
    }
    let added = format!("Added/enabled {} provider preset", preset.name);
    if matches!(preset.id, "ollama" | "lmstudio") {
        return Ok(match discover_tui_provider_models(panel) {
            Ok(discovered) => format!("{added}. {discovered}"),
            Err(error) => format!(
                "{added}, but model discovery failed: {error}. Start the server, then press r to retry"
            ),
        });
    }
    Ok(added)
}

pub(super) fn remove_tui_model_provider(panel: &mut TuiModelsPanel) -> Result<String> {
    if panel.focus != TuiModelsFocus::Providers {
        anyhow::bail!("Press Left to select a provider before removing it");
    }
    let provider = panel
        .selected_provider()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("No provider selected"))?;
    if provider.built_in {
        anyhow::bail!(
            "{} is built in and cannot be removed; press Space to disable it",
            provider.name
        );
    }

    let store = open_agent_store()?;
    if !store.delete_model_provider_blocking(&provider.id)? {
        anyhow::bail!("Provider {} no longer exists", provider.name);
    }
    let config_cleanup =
        codex_config_path().and_then(|path| remove_codex_provider_config_at(&path, &provider.id));
    panel.refresh();

    Ok(match config_cleanup {
        Ok(_) => format!(
            "Removed provider {} and its models; affected project/default selections now follow CLT defaults",
            provider.name
        ),
        Err(error) => format!(
            "Removed provider {} and its models, but Codex config cleanup failed: {error}",
            provider.name
        ),
    })
}

pub(super) fn toggle_tui_models_enabled(panel: &mut TuiModelsPanel) -> Result<String> {
    let store = open_agent_store()?;
    let message = match panel.focus {
        TuiModelsFocus::Providers => {
            let provider = panel
                .selected_provider()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("No provider selected"))?;
            store.set_model_provider_enabled_blocking(&provider.id, !provider.enabled)?;
            format!(
                "{} provider {}",
                if provider.enabled {
                    "Disabled"
                } else {
                    "Enabled"
                },
                provider.name
            )
        }
        TuiModelsFocus::Models => {
            let model = panel
                .selected_model()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("No model selected"))?;
            store.set_model_target_flags_blocking(
                &model.provider_id,
                &model.model_id,
                !model.enabled,
                model.favorite,
            )?;
            format!(
                "{} model {}/{}",
                if model.enabled {
                    "Hidden"
                } else {
                    "Made available"
                },
                model.provider_id,
                model.model_id
            )
        }
    };
    panel.refresh();
    Ok(message)
}

pub(super) fn toggle_tui_model_favorite(panel: &mut TuiModelsPanel) -> Result<String> {
    let model = panel
        .selected_model()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Select a model first"))?;
    open_agent_store()?.set_model_target_flags_blocking(
        &model.provider_id,
        &model.model_id,
        model.enabled,
        !model.favorite,
    )?;
    panel.refresh_models();
    Ok(format!(
        "{} favorite: {}/{}",
        if model.favorite { "Removed" } else { "Added" },
        model.provider_id,
        model.model_id
    ))
}

pub(super) fn cycle_tui_model_reasoning(panel: &mut TuiModelsPanel) -> Result<String> {
    let model = panel
        .selected_model()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Select a model first"))?;
    let reasoning = next_agent_codex_setting(
        model.reasoning_effort.as_deref(),
        &AGENT_CODEX_REASONING_EFFORTS,
    );
    let label = reasoning.as_deref().unwrap_or("system").to_string();
    let changed = open_agent_store()?.set_model_target_reasoning_blocking(
        &model.provider_id,
        &model.model_id,
        reasoning.as_deref(),
    )?;
    if !changed {
        anyhow::bail!(
            "Model {}/{} no longer exists",
            model.provider_id,
            model.model_id
        );
    }
    let updated_codex_default = if tui_model_matches_codex_default(
        panel.codex_default_provider.as_deref(),
        panel.codex_default_model.as_deref(),
        &model,
    ) {
        set_codex_model_reasoning_if_default_at(
            &codex_config_path()?,
            &model.provider_id,
            &model.model_id,
            reasoning.as_deref(),
        )?
    } else {
        false
    };
    panel.refresh();
    let mut message = format!(
        "Default reasoning for {}/{}: {}",
        model.provider_id, model.model_id, label
    );
    if updated_codex_default {
        message.push_str("; updated the Codex top-level default");
    }
    Ok(message)
}

pub(super) fn set_tui_model_default(panel: &mut TuiModelsPanel) -> Result<String> {
    let model = panel
        .selected_model()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Select a model first"))?;
    let store = open_agent_store()?;
    store.set_model_provider_enabled_blocking(&model.provider_id, true)?;
    store.set_model_target_flags_blocking(
        &model.provider_id,
        &model.model_id,
        true,
        model.favorite,
    )?;
    store.set_model_default_blocking(&model.provider_id, &model.model_id)?;
    panel.refresh_models();
    Ok(format!(
        "New CLT runs default to {}/{}; existing runs are unchanged",
        model.provider_id, model.model_id
    ))
}

pub(super) fn set_tui_codex_default(panel: &mut TuiModelsPanel) -> Result<String> {
    let model = panel
        .selected_model()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Select a model first"))?;
    let path = codex_config_path()?;
    set_codex_default_config_at(
        &path,
        &model.provider_id,
        &model.model_id,
        model.reasoning_effort.as_deref(),
    )?;
    let provider_id = model.provider_id.clone();
    let model_id = model.model_id.clone();
    let reasoning = model.reasoning_effort.as_deref().unwrap_or("system");
    panel.refresh();
    Ok(format!(
        "Updated Codex top-level default in {} to {}/{} with {} reasoning",
        path.display(),
        provider_id,
        model_id,
        reasoning
    ))
}

pub(super) fn submit_tui_model_input(
    model_input: &mut TuiModelInput,
    panel: &mut TuiModelsPanel,
) -> Result<Option<String>> {
    let entered = model_input.input.value().trim().to_string();
    match &mut model_input.kind {
        TuiModelInputKind::SearchModels => {
            let matches = panel.set_model_search(entered.clone());
            Ok(Some(if entered.is_empty() {
                format!("Model search cleared; showing {matches} models")
            } else {
                format!("Model search {entered:?}: {matches} matches")
            }))
        }
        TuiModelInputKind::AddModel { provider_id } => {
            if entered.is_empty() {
                anyhow::bail!("Model ID cannot be empty");
            }
            open_agent_store()?.upsert_model_target_blocking(&agent::AgentModelTarget {
                provider_id: provider_id.clone(),
                model_id: entered.clone(),
                label: entered.clone(),
                enabled: true,
                favorite: false,
                reasoning_effort: None,
            })?;
            panel.refresh_models();
            Ok(Some(format!("Added model {provider_id}/{entered}")))
        }
        TuiModelInputKind::CustomProvider {
            step,
            provider_id,
            name,
            base_url,
        } => {
            match *step {
                0 => {
                    if entered.is_empty() {
                        anyhow::bail!("Endpoint name cannot be empty");
                    }
                    *provider_id = custom_provider_id(&entered, &panel.providers);
                    *name = entered;
                }
                1 => {
                    *base_url = normalize_openai_api_base_url(&entered)?;
                }
                _ => {
                    if !entered.is_empty() && !valid_environment_variable_name(&entered) {
                        anyhow::bail!(
                            "Environment variable must use the standard NAME_WITH_UNDERSCORES form"
                        );
                    }
                    let env_key = (!entered.is_empty()).then_some(entered.as_str());
                    upsert_codex_provider_config_at(
                        &codex_config_path()?,
                        provider_id,
                        name,
                        base_url,
                        env_key,
                    )?;
                    open_agent_store()?.upsert_model_provider_blocking(
                        &agent::AgentModelProvider {
                            id: provider_id.clone(),
                            name: name.clone(),
                            base_url: Some(base_url.clone()),
                            env_key: env_key.map(str::to_string),
                            built_in: false,
                            enabled: true,
                        },
                    )?;
                    panel.refresh();
                    if let Some(index) = panel
                        .providers
                        .iter()
                        .position(|provider| provider.id == *provider_id)
                    {
                        panel.provider_state.select(Some(index));
                        panel.refresh_models();
                    }
                    let added = format!("Added local endpoint {name} ({provider_id})");
                    let message = match discover_tui_provider_models(panel) {
                        Ok(discovered) => format!("{added}. {discovered}"),
                        Err(error) => format!(
                            "{added}, but model discovery failed: {error}. Press r to retry or a to add a model ID"
                        ),
                    };
                    return Ok(Some(message));
                }
            }
            *step += 1;
            model_input.input = Input::default();
            Ok(None)
        }
    }
}

#[derive(Clone)]
pub(super) enum TuiAgentPanelRowIdentity {
    RegisterCurrentProject(PathBuf),
    Project(i64),
}

pub(super) fn load_tui_agent_panel_snapshot(active_root: &Path) -> Result<TuiAgentPanelSnapshot> {
    load_tui_agent_panel_snapshot_inner(active_root, false)
}

pub(super) fn load_tui_agent_panel_snapshot_with_service_recovery(
    active_root: &Path,
) -> Result<TuiAgentPanelSnapshot> {
    load_tui_agent_panel_snapshot_inner(active_root, true)
}

pub(super) fn load_tui_agent_panel_snapshot_inner(
    _active_root: &Path,
    recover_stale_service: bool,
) -> Result<TuiAgentPanelSnapshot> {
    let state_dir = agent_state_dir()?;
    let service_status = agent_service_status(&state_dir);
    let store = open_agent_store_at(&state_dir)?;
    let mut checkins = store.list_daemon_checkins_blocking()?;
    let now = agent_timestamp_seconds();
    let service_restarted =
        recover_stale_service && agent_service_needs_restart(&service_status, &checkins, now);
    if service_restarted {
        restart_running_agent_service().context("Failed to restart stale agent service")?;
        for checkin in checkins
            .iter()
            .filter(|checkin| checkin.mode == "service" && !daemon_checkin_is_fresh(checkin, now))
        {
            store.clear_daemon_checkin_blocking(&checkin.holder)?;
        }
        checkins
            .retain(|checkin| checkin.mode != "service" || daemon_checkin_is_fresh(checkin, now));
    }
    let daemon_status = if service_restarted {
        "service restarting".to_string()
    } else {
        format_agent_daemon_runtime_status(&service_status, &checkins, now)
    };
    let projects = store.list_projects_blocking()?;
    let active_leases = store.list_active_leases_blocking(&agent_timestamp())?;
    let pending_git_finalizations = if store.pending_migration_version().is_some() {
        HashMap::new()
    } else {
        store
            .list_pending_git_finalizations_blocking(None)?
            .into_iter()
            .map(|finalization| (finalization.project_id, finalization.state))
            .collect::<HashMap<_, _>>()
    };
    let suspending_session_projects = store.suspending_session_project_ids_blocking()?;
    let failure_backoff = agent_failure_backoff()?;
    let mut interactive_session_projects = HashSet::new();
    for project_id in &suspending_session_projects {
        if store
            .session_controls_for_project_blocking(*project_id)?
            .into_iter()
            .any(|control| {
                matches!(
                    control.state,
                    AgentSessionControlState::Interactive | AgentSessionControlState::StopRequested
                ) && control.interactive_holder.as_deref().is_some_and(|holder| {
                    InteractiveGuardianDisposition::from_guardian_holder(holder).is_some()
                        && !InteractiveGuardianDisposition::guardian_process_is_proven_dead(holder)
                })
            })
        {
            interactive_session_projects.insert(*project_id);
        }
    }

    let projects = projects
        .into_iter()
        .map(|project| {
            let scan = scan_agent_project(&project.path);
            let daemon_scan_problem = tui_agent_daemon_scan_problem(&project);
            let latest_run = if project.enabled
                && project.failure_count > 0
                && (scan.todo_count > 0
                    || scan.doing_count > 0
                    || pending_git_finalizations.contains_key(&project.id))
            {
                store.latest_run_for_project_blocking(project.id)?
            } else {
                None
            };
            let failure_problem =
                tui_agent_failure_problem(&project, latest_run.as_ref(), now, failure_backoff);
            let runtime_state = resolve_tui_agent_runtime_state(
                tui_agent_runtime_state(project.id, &active_leases),
                interactive_session_projects.contains(&project.id),
                suspending_session_projects.contains(&project.id),
                pending_git_finalizations.get(&project.id).copied(),
                daemon_scan_problem.is_some() || failure_problem.is_some(),
            );
            Ok(TuiAgentProject {
                project,
                scan,
                runtime_state,
                daemon_scan_problem,
                failure_problem,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(TuiAgentPanelSnapshot {
        projects,
        daemon_status,
    })
}

pub(super) fn resolve_tui_agent_runtime_state(
    mut runtime_state: TuiAgentRuntimeState,
    has_interactive_session: bool,
    has_suspending_session: bool,
    pending_git_finalization: Option<GitFinalizationState>,
    has_problem: bool,
) -> TuiAgentRuntimeState {
    if matches!(
        runtime_state,
        TuiAgentRuntimeState::Idle | TuiAgentRuntimeState::Fenced
    ) && has_interactive_session
    {
        runtime_state = TuiAgentRuntimeState::Interactive;
    }
    if runtime_state == TuiAgentRuntimeState::Idle && has_suspending_session {
        runtime_state = TuiAgentRuntimeState::Fenced;
    }
    if matches!(
        runtime_state,
        TuiAgentRuntimeState::Idle | TuiAgentRuntimeState::Fenced
    ) && has_problem
    {
        return TuiAgentRuntimeState::Error;
    }
    if matches!(
        runtime_state,
        TuiAgentRuntimeState::Idle | TuiAgentRuntimeState::Fenced
    ) && let Some(finalization_state) = pending_git_finalization
    {
        runtime_state = if finalization_state == GitFinalizationState::PushPending {
            TuiAgentRuntimeState::PushPending
        } else {
            TuiAgentRuntimeState::Finalizing
        };
    }
    runtime_state
}

pub(super) fn tui_agent_daemon_scan_problem(project: &agent::AgentProject) -> Option<String> {
    if !project.enabled {
        return None;
    }

    let status = project.last_daemon_scan_status.as_deref()?;
    let external_project = project.path.starts_with("/Volumes");
    match status {
        "unavailable" if external_project => Some(format!(
            "External project scan failed: {}. In macOS System Settings > Privacy & Security > Full Disk Access, allow CLT and restart the agent.",
            project
                .last_daemon_scan_error
                .as_deref()
                .unwrap_or("the daemon cannot access the project")
        )),
        "missing" if external_project => Some(format!(
            "External project is unavailable. Make sure the drive is mounted and accessible: {}",
            project.path.display()
        )),
        "unavailable" => Some(format!(
            "Daemon project scan failed: {}",
            project
                .last_daemon_scan_error
                .as_deref()
                .unwrap_or("the project cannot be read")
        )),
        "missing" => Some(format!(
            "Project folder is unavailable: {}",
            project.path.display()
        )),
        "uninitialized" => Some(format!(
            "No CLT task board found. Open {} and run `clt init`.",
            project.path.display()
        )),
        _ => None,
    }
}

pub(super) fn tui_agent_failure_problem(
    project: &agent::AgentProject,
    latest_run: Option<&agent::AgentRunRecord>,
    now: u64,
    failure_backoff: Duration,
) -> Option<String> {
    if !project.enabled || project.failure_count <= 0 {
        return None;
    }
    let run = latest_run.filter(|run| matches!(run.status.as_str(), "failure" | "timeout"))?;
    let summary = run
        .summary
        .as_deref()
        .unwrap_or("No failure summary was recorded")
        .strip_prefix("Codex runner failed before completion: ")
        .unwrap_or_else(|| {
            run.summary
                .as_deref()
                .unwrap_or("No failure summary was recorded")
        });
    let retry = remaining_agent_delay(project.last_failure_at.as_deref(), now, failure_backoff)
        .map(|remaining| format!("Automatic retry in {remaining}s while the daemon is active."))
        .unwrap_or_else(|| "Automatic retry is ready when the daemon is active.".to_string());
    let correction = if summary.contains("not committed exactly once at the frozen task boundary") {
        "This build checkpoints dirty Todo definitions automatically before the next attempt."
    } else {
        "Correct the reported cause before forcing another attempt."
    };

    Some(format!(
        "Last agent run failed: {summary}\n{retry} {correction} Press r to clear the cooldown and retry now."
    ))
}

pub(super) fn agent_service_needs_restart(
    service_status: &str,
    checkins: &[agent::AgentDaemonCheckin],
    now: u64,
) -> bool {
    service_status == "running"
        && checkins
            .iter()
            .filter(|checkin| checkin.mode == "service")
            .any(|checkin| !daemon_checkin_is_fresh(checkin, now))
        && !checkins
            .iter()
            .filter(|checkin| checkin.mode == "service")
            .any(|checkin| daemon_checkin_is_fresh(checkin, now))
}

pub(super) fn tui_agent_runtime_state(
    project_id: i64,
    active_leases: &[agent::AgentLeaseRecord],
) -> TuiAgentRuntimeState {
    let Some(lease) = active_leases
        .iter()
        .find(|lease| lease.project_id == project_id)
    else {
        return TuiAgentRuntimeState::Idle;
    };

    if let Some(liveness) = interactive_lease_holder_liveness(&lease.holder) {
        return match liveness {
            AgentLeaseHolderLiveness::Dead => TuiAgentRuntimeState::Stale,
            AgentLeaseHolderLiveness::CurrentProcess
            | AgentLeaseHolderLiveness::Alive
            | AgentLeaseHolderLiveness::Unknown => TuiAgentRuntimeState::Fenced,
        };
    }

    match agent_lease_holder_liveness(&lease.holder) {
        AgentLeaseHolderLiveness::Dead => TuiAgentRuntimeState::Stale,
        AgentLeaseHolderLiveness::CurrentProcess
        | AgentLeaseHolderLiveness::Alive
        | AgentLeaseHolderLiveness::Unknown => TuiAgentRuntimeState::Running,
    }
}

pub(super) fn format_agent_daemon_runtime_status(
    service_status: &str,
    checkins: &[agent::AgentDaemonCheckin],
    now: u64,
) -> String {
    let (fresh, stale): (Vec<_>, Vec<_>) = checkins
        .iter()
        .partition(|checkin| daemon_checkin_is_fresh(checkin, now));

    if !fresh.is_empty() {
        let active = format_daemon_checkin_modes(&fresh, "active");
        let has_service_checkin = fresh.iter().any(|checkin| checkin.mode == "service");
        if service_status == "running" && !has_service_checkin {
            return format!("{active}; service no-check-in");
        }
        return active;
    }

    if !stale.is_empty() {
        return format_daemon_checkin_modes(&stale, "stale");
    }

    match service_status {
        "running" => "service active (no check-in)".to_string(),
        "installed" => "service disabled".to_string(),
        "not-installed" => "disabled".to_string(),
        "unsupported" => "unsupported".to_string(),
        status if status.starts_with("unknown") => "unknown".to_string(),
        status => status.to_string(),
    }
}

pub(super) fn daemon_checkin_is_fresh(checkin: &agent::AgentDaemonCheckin, now: u64) -> bool {
    checkin
        .expires_at
        .parse::<u64>()
        .map(|expires_at| expires_at > now)
        .unwrap_or(false)
}

pub(super) fn format_daemon_checkin_modes(
    checkins: &[&agent::AgentDaemonCheckin],
    suffix: &str,
) -> String {
    let service_count = checkins
        .iter()
        .filter(|checkin| checkin.mode == "service")
        .count();
    let cli_count = checkins
        .iter()
        .filter(|checkin| checkin.mode == "cli")
        .count();
    let other_count = checkins.len().saturating_sub(service_count + cli_count);

    let label = match (service_count > 0, cli_count > 0, other_count > 0) {
        (true, true, _) => "service+cli",
        (true, false, false) => "service",
        (false, true, false) => "cli",
        (true, false, true) => "service+other",
        (false, true, true) => "cli+other",
        (false, false, true) => "daemon",
        (false, false, false) => "daemon",
    };

    if checkins.len() > 1 {
        format!("{label} {suffix} ({})", checkins.len())
    } else {
        format!("{label} {suffix}")
    }
}

pub(super) fn current_project_registration(
    active_root: &Path,
    projects: &[TuiAgentProject],
) -> Option<TuiCurrentProjectRegistration> {
    if projects
        .iter()
        .any(|project| project.project.path == active_root)
    {
        return None;
    }

    Some(TuiCurrentProjectRegistration {
        path: active_root.to_path_buf(),
        name: project_display_name(active_root),
    })
}

pub(super) fn register_selected_current_project(
    panel: &mut TuiAgentPanel,
    active_root: &Path,
) -> Result<String> {
    let Some(registration) = panel
        .selected_current_project_registration()
        .map(|registration| (registration.path.clone(), registration.name.clone()))
    else {
        return Ok("No current project registration row selected".to_string());
    };

    if !ensure_existing_board(&registration.0)? {
        return Ok(format!(
            "Project is not initialized: {}",
            registration.0.display()
        ));
    }

    let store = open_agent_store()?;
    let created = store.register_project_blocking(&registration.0, &registration.1)?;
    panel.refresh(active_root);

    if created {
        Ok(format!("Registered current project: {}", registration.1))
    } else {
        Ok(format!(
            "Project already registered: {}",
            registration.0.display()
        ))
    }
}

pub(super) fn selected_tui_agent_project_removal(
    panel: &TuiAgentPanel,
) -> Option<TuiAgentProjectRemoval> {
    panel
        .selected_project()
        .map(|entry| TuiAgentProjectRemoval {
            path: entry.project.path.clone(),
            name: entry.project.name.clone(),
        })
}

pub(super) fn tui_agent_project_removal_prompt(removal: &TuiAgentProjectRemoval) -> String {
    format!(
        "Remove agent project '{}' from the list? Press y to confirm; n or Esc cancels.",
        removal.name
    )
}

pub(super) fn remove_tui_agent_project(
    panel: &mut TuiAgentPanel,
    active_root: &Path,
    removal: &TuiAgentProjectRemoval,
) -> Result<String> {
    let state_dir = ensure_agent_state_dir()?;
    let store = open_agent_store_at(&state_dir)?;
    #[cfg(not(test))]
    cleanup_terminal_agent_worker_services(&state_dir, &store, Some(&removal.path))?;
    remove_tui_agent_project_with_store(panel, active_root, removal, &store, &state_dir)
}

pub(super) fn remove_tui_agent_project_with_store(
    panel: &mut TuiAgentPanel,
    active_root: &Path,
    removal: &TuiAgentProjectRemoval,
    store: &agent::TursoAgentStore,
    state_dir: &Path,
) -> Result<String> {
    let selected_idx = panel.state.selected().unwrap_or(0);
    let removed = unregister_agent_project_with_recovery(store, state_dir, &removal.path)?;

    if removed {
        panel
            .projects
            .retain(|entry| entry.project.path != removal.path);
        panel.current_project_registration =
            current_project_registration(active_root, &panel.projects);
        panel.last_error = None;
        panel.select_nearest_row(selected_idx);
        Ok(format!("Removed agent project: {}", removal.name))
    } else {
        panel.refresh(active_root);
        Ok(format!(
            "Project is no longer registered: {}",
            removal.path.display()
        ))
    }
}

pub(super) fn toggle_selected_tui_agent_project(
    panel: &mut TuiAgentPanel,
    active_root: &Path,
) -> Result<String> {
    let Some(project) = panel.selected_project().map(|entry| entry.project.clone()) else {
        return Ok("No registered project selected".to_string());
    };

    let enabled = !project.enabled;
    let store = open_agent_store()?;
    let changed = store.set_project_enabled_blocking(project.id, enabled)?;
    panel.refresh(active_root);

    if changed {
        let action = if enabled { "Turned on" } else { "Turned off" };
        Ok(format!("{} agent project: {}", action, project.name))
    } else {
        Ok(format!(
            "Project is no longer registered: {}",
            project.path.display()
        ))
    }
}

pub(super) fn retry_selected_tui_agent_project(
    panel: &mut TuiAgentPanel,
    active_root: &Path,
) -> Result<String> {
    let Some(project) = panel.selected_project().map(|entry| entry.project.clone()) else {
        return Ok("No registered project selected".to_string());
    };

    let store = open_agent_store()?;
    let changed = store.clear_project_failure_backoff_for_path_blocking(&project.path)?;
    panel.refresh(active_root);

    if changed {
        Ok(format!(
            "Queued {} for immediate retry; the daemon will run it when active",
            project.name
        ))
    } else {
        Ok(format!(
            "Project is no longer registered: {}",
            project.path.display()
        ))
    }
}

pub(super) fn cycle_selected_tui_agent_project_git_mode(
    panel: &mut TuiAgentPanel,
    active_root: &Path,
) -> Result<String> {
    let Some(project) = panel.selected_project().map(|entry| entry.project.clone()) else {
        return Ok("No registered project selected".to_string());
    };

    let mode = project.git_mode.next();
    let store = open_agent_store()?;
    let changed = store.set_project_git_mode_blocking(project.id, mode)?;
    panel.refresh(active_root);

    if changed {
        Ok(format!("Git mode for {}: {}", project.name, mode.label()))
    } else {
        Ok(format!(
            "Project is no longer registered: {}",
            project.path.display()
        ))
    }
}

pub(super) fn next_agent_codex_setting(current: Option<&str>, choices: &[&str]) -> Option<String> {
    let current_idx = choices
        .iter()
        .position(|choice| Some(*choice) == current)
        .unwrap_or(0);
    let next = choices[(current_idx + 1) % choices.len()];
    (!next.is_empty()).then(|| next.to_string())
}

pub(super) fn update_selected_tui_agent_codex_settings(
    panel: &mut TuiAgentPanel,
    active_root: &Path,
    provider: Option<String>,
    model: Option<String>,
    reasoning_effort: Option<String>,
    fast_enabled: bool,
) -> Result<bool> {
    let Some(project_id) = panel.selected_project().map(|entry| entry.project.id) else {
        return Ok(false);
    };

    let store = open_agent_store()?;
    let changed = store.set_project_codex_settings_blocking(
        project_id,
        provider.as_deref(),
        model.as_deref(),
        reasoning_effort.as_deref(),
        fast_enabled,
    )?;
    panel.refresh(active_root);
    Ok(changed)
}

pub(super) fn cycle_selected_tui_agent_codex_model(
    panel: &mut TuiAgentPanel,
    active_root: &Path,
) -> Result<String> {
    let Some(project) = panel.selected_project().map(|entry| entry.project.clone()) else {
        return Ok("No registered project selected".to_string());
    };
    let store = open_agent_store()?;
    let targets = store.list_enabled_model_targets_blocking()?;
    let current_idx = project.codex_model.as_ref().and_then(|model| {
        let provider = project.codex_provider.as_deref().unwrap_or("openai");
        targets
            .iter()
            .position(|target| target.provider_id == provider && target.model_id == *model)
            .map(|index| index + 1)
    });
    let next_idx = (current_idx.unwrap_or(0) + 1) % (targets.len() + 1);
    let (provider, model, label) = if next_idx == 0 {
        (None, None, "CLT default".to_string())
    } else {
        let target = &targets[next_idx - 1];
        (
            Some(target.provider_id.clone()),
            Some(target.model_id.clone()),
            format!("{}/{}", target.provider_id, target.model_id),
        )
    };
    let changed = update_selected_tui_agent_codex_settings(
        panel,
        active_root,
        provider,
        model,
        project.codex_reasoning_effort,
        project.codex_fast_enabled,
    )?;

    if changed {
        Ok(format!("Codex model for {}: {}", project.name, label))
    } else {
        Ok(format!(
            "Project is no longer registered: {}",
            project.path.display()
        ))
    }
}

pub(super) fn cycle_selected_tui_agent_codex_reasoning(
    panel: &mut TuiAgentPanel,
    active_root: &Path,
) -> Result<String> {
    let Some(project) = panel.selected_project().map(|entry| entry.project.clone()) else {
        return Ok("No registered project selected".to_string());
    };
    let reasoning = next_agent_codex_setting(
        project.codex_reasoning_effort.as_deref(),
        &AGENT_CODEX_REASONING_EFFORTS,
    );
    let label = reasoning.as_deref().unwrap_or("default").to_string();
    let changed = update_selected_tui_agent_codex_settings(
        panel,
        active_root,
        project.codex_provider,
        project.codex_model,
        reasoning,
        project.codex_fast_enabled,
    )?;

    if changed {
        Ok(format!("Codex thinking for {}: {}", project.name, label))
    } else {
        Ok(format!(
            "Project is no longer registered: {}",
            project.path.display()
        ))
    }
}

pub(super) fn toggle_selected_tui_agent_codex_fast(
    panel: &mut TuiAgentPanel,
    active_root: &Path,
) -> Result<String> {
    let Some(project) = panel.selected_project().map(|entry| entry.project.clone()) else {
        return Ok("No registered project selected".to_string());
    };
    let fast_enabled = !project.codex_fast_enabled;
    let changed = update_selected_tui_agent_codex_settings(
        panel,
        active_root,
        project.codex_provider,
        project.codex_model,
        project.codex_reasoning_effort,
        fast_enabled,
    )?;

    if changed {
        Ok(format!(
            "Codex fast mode for {}: {}",
            project.name,
            if fast_enabled { "ON" } else { "OFF" }
        ))
    } else {
        Ok(format!(
            "Project is no longer registered: {}",
            project.path.display()
        ))
    }
}

pub(super) fn tui_agent_panel_refresh_interval() -> Duration {
    Duration::from_secs(TUI_AGENT_PANEL_REFRESH_SECONDS)
}

pub(super) fn tui_agent_log_refresh_interval() -> Duration {
    Duration::from_millis(TUI_AGENT_LOG_REFRESH_MILLIS)
}

pub(super) fn tui_agent_panel_instructions() -> &'static str {
    "Up/Down selects, Enter opens/adds, Space toggles ON/OFF, Delete removes with confirmation, g cycles Git off/commit/push, m cycles the selected target, M opens Models, f toggles fast, t cycles thinking, r retries after fixing an error, l shows output. s directly stops the selected active, interactive, or fenced session; with output open it controls that exact session. With output open: i takes over a live session and c continues an idle session. Tab returns to Kanban."
}

pub(super) fn tui_agent_log_title(log_view: &TuiAgentLogView) -> String {
    let status = if log_view.is_live { "LIVE" } else { "LATEST" };
    let controls = if log_view.session_target.is_some() {
        "s/i/c controls; l/Esc closes"
    } else {
        "l/Esc closes"
    };
    format!(
        "Agent Output [{status}]: {} ({controls})",
        log_view.project_name,
    )
}

pub(super) fn selected_tui_agent_log_view(
    panel: &TuiAgentPanel,
) -> Result<Option<TuiAgentLogView>> {
    let state_dir = agent_state_dir()?;
    selected_tui_agent_log_view_at(panel, &state_dir)
}

pub(super) fn selected_tui_task_or_project_log_view_for_path(
    panel: &mut TuiAgentPanel,
    project_path: &Path,
    task_status: TaskStatus,
    task: Option<&TaskEntry>,
) -> Result<Option<TuiAgentLogView>> {
    let state_dir = agent_state_dir()?;
    selected_tui_task_or_project_log_view_for_path_at(
        panel,
        project_path,
        task_status,
        task,
        &state_dir,
    )
}

pub(super) fn selected_tui_task_or_project_log_view_for_path_at(
    panel: &mut TuiAgentPanel,
    project_path: &Path,
    task_status: TaskStatus,
    task: Option<&TaskEntry>,
    state_dir: &Path,
) -> Result<Option<TuiAgentLogView>> {
    match task {
        Some(task) => selected_tui_task_log_view_for_path_at(
            panel,
            project_path,
            task_status,
            task,
            state_dir,
        ),
        None => {
            if !panel.select_project_for_path(project_path) {
                return Ok(None);
            }
            selected_tui_agent_log_view_at(panel, state_dir)
        }
    }
}

pub(super) fn selected_tui_task_log_view_for_path_at(
    panel: &mut TuiAgentPanel,
    project_path: &Path,
    task_status: TaskStatus,
    task: &TaskEntry,
    state_dir: &Path,
) -> Result<Option<TuiAgentLogView>> {
    if !panel.select_project_for_path(project_path) {
        return Ok(None);
    }

    selected_tui_task_log_view_at(panel, task_status, task, state_dir)
}

pub(super) fn selected_tui_task_log_view_at(
    panel: &TuiAgentPanel,
    _task_status: TaskStatus,
    task: &TaskEntry,
    state_dir: &Path,
) -> Result<Option<TuiAgentLogView>> {
    let Some(selected) = panel.selected_project() else {
        return Ok(None);
    };

    let Some(session_id) = codex_session_for_task(task) else {
        return Ok(None);
    };
    let session_target = Some(TuiCodexSessionTarget::new(
        &selected.project,
        session_id.clone(),
    ));

    let live_path = if selected.runtime_state.is_running()
        || matches!(
            selected.runtime_state,
            TuiAgentRuntimeState::Interactive | TuiAgentRuntimeState::Fenced
        ) {
        active_agent_log_for_codex_session(selected, state_dir, &session_id)?
    } else {
        None
    };

    let (path, settings_path, is_live) = match live_path {
        Some(path) => (Some(path.clone()), Some(path), true),
        None => {
            let store = open_agent_store_at(state_dir)?;
            let run =
                store.latest_run_for_codex_session_blocking(selected.project.id, &session_id)?;
            (
                run.as_ref().and_then(preferred_recorded_agent_output_path),
                run.as_ref()
                    .and_then(|run| run.stderr_path.as_ref())
                    .map(PathBuf::from),
                false,
            )
        }
    };

    path.map(|path| {
        TuiAgentLogView::new(
            selected.project.name.clone(),
            path,
            settings_path,
            is_live,
            session_target,
        )
    })
    .transpose()
}

pub(super) fn active_agent_log_for_codex_session(
    selected: &TuiAgentProject,
    state_dir: &Path,
    session_id: &str,
) -> Result<Option<PathBuf>> {
    let store = open_agent_store_at(state_dir)?;
    if let Some(control) = store.session_control_blocking(selected.project.id, session_id)? {
        if matches!(
            control.state,
            AgentSessionControlState::Running
                | AgentSessionControlState::StopRequested
                | AgentSessionControlState::InterruptRequested
        ) {
            return Ok(control
                .stderr_path
                .map(PathBuf::from)
                .filter(|path| path.is_file()));
        }
        return Ok(None);
    }

    if !selected.runtime_state.is_running() {
        return Ok(None);
    }

    let Some(path) = latest_agent_log_path(
        &agent_project_run_log_dir(state_dir, &selected.project)?,
        "err",
    )?
    else {
        return Ok(None);
    };

    Ok((agent_codex_session_id_from_log(&path)?.as_deref() == Some(session_id)).then_some(path))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TuiCodexSessionAvailability {
    Idle,
    SelectedSessionBusy,
    ProjectBusy,
}

pub(super) fn tui_codex_session_availability_for_path(
    panel: &mut TuiAgentPanel,
    project_path: &Path,
    session_id: &str,
) -> Result<TuiCodexSessionAvailability> {
    let state_dir = agent_state_dir()?;
    tui_codex_session_availability_for_path_at(panel, project_path, session_id, &state_dir)
}

pub(super) fn tui_codex_session_availability_for_path_at(
    panel: &mut TuiAgentPanel,
    project_path: &Path,
    session_id: &str,
    state_dir: &Path,
) -> Result<TuiCodexSessionAvailability> {
    if !panel.select_project_for_path(project_path) {
        return Ok(TuiCodexSessionAvailability::Idle);
    }
    let Some(selected) = panel.selected_project() else {
        return Ok(TuiCodexSessionAvailability::Idle);
    };

    let store = open_agent_store_at(state_dir)?;
    let controls = store.session_controls_for_project_blocking(selected.project.id)?;
    if controls.iter().any(|control| {
        control.codex_session_id == session_id && control.state != AgentSessionControlState::Stopped
    }) {
        return Ok(TuiCodexSessionAvailability::SelectedSessionBusy);
    }
    if controls.iter().any(|control| {
        control.codex_session_id != session_id && control.state != AgentSessionControlState::Stopped
    }) {
        return Ok(TuiCodexSessionAvailability::ProjectBusy);
    }

    if selected.runtime_state.is_running() {
        if active_agent_log_for_codex_session(selected, state_dir, session_id)?.is_some() {
            Ok(TuiCodexSessionAvailability::SelectedSessionBusy)
        } else {
            Ok(TuiCodexSessionAvailability::ProjectBusy)
        }
    } else {
        Ok(TuiCodexSessionAvailability::Idle)
    }
}

pub(super) fn selected_tui_agent_log_view_at(
    panel: &TuiAgentPanel,
    state_dir: &Path,
) -> Result<Option<TuiAgentLogView>> {
    let Some(selected) = panel.selected_project() else {
        return Ok(None);
    };

    let fenced_session = if matches!(
        selected.runtime_state,
        TuiAgentRuntimeState::Interactive | TuiAgentRuntimeState::Fenced
    ) {
        let store = open_agent_store_at(state_dir)?;
        store
            .session_controls_for_project_blocking(selected.project.id)?
            .into_iter()
            .rev()
            .find_map(|control| {
                if control.state == AgentSessionControlState::Stopped {
                    return None;
                }
                control
                    .stderr_path
                    .or(control.stdout_path)
                    .map(PathBuf::from)
                    .filter(|path| path.is_file())
                    .map(|path| (path, control.codex_session_id))
            })
    } else {
        None
    };
    let live_path = if fenced_session.is_none() && selected.runtime_state.is_running() {
        latest_agent_log_path(
            &agent_project_run_log_dir(state_dir, &selected.project)?,
            "err",
        )?
    } else {
        None
    };

    let (path, settings_path, is_live, session_target) = match fenced_session {
        Some((path, session_id)) => (
            Some(path.clone()),
            Some(path),
            true,
            Some(TuiCodexSessionTarget::new(&selected.project, session_id)),
        ),
        None => match live_path {
            Some(path) => {
                let session_target = agent_codex_session_id_from_log(&path)?
                    .map(|session_id| TuiCodexSessionTarget::new(&selected.project, session_id));
                (Some(path.clone()), Some(path), true, session_target)
            }
            None => {
                let store = open_agent_store_at(state_dir)?;
                let run = store.latest_run_for_project_blocking(selected.project.id)?;
                let session_target = run
                    .as_ref()
                    .and_then(|run| run.codex_session_id.clone())
                    .map(|session_id| TuiCodexSessionTarget::new(&selected.project, session_id));
                let path = run.as_ref().and_then(preferred_recorded_agent_output_path);
                let settings_path = run
                    .as_ref()
                    .and_then(|run| run.stderr_path.as_ref())
                    .map(PathBuf::from);
                (path, settings_path, false, session_target)
            }
        },
    };

    path.map(|path| {
        TuiAgentLogView::new(
            selected.project.name.clone(),
            path,
            settings_path,
            is_live,
            session_target,
        )
    })
    .transpose()
}

pub(super) fn sync_open_tui_agent_log_view(
    panel: &TuiAgentPanel,
    log_view: &mut Option<TuiAgentLogView>,
) {
    if log_view.is_none() {
        return;
    }

    let selected_view =
        agent_state_dir().and_then(|state_dir| selected_tui_agent_log_view_at(panel, &state_dir));
    replace_open_tui_agent_log_view(panel, log_view, selected_view);
}

pub(super) fn sync_open_tui_task_log_view(
    panel: &mut TuiAgentPanel,
    project_path: &Path,
    task_status: TaskStatus,
    task: Option<&TaskEntry>,
    log_view: &mut Option<TuiAgentLogView>,
) {
    if log_view.is_none() {
        return;
    }

    let selected_view = agent_state_dir().and_then(|state_dir| {
        selected_tui_task_or_project_log_view_for_path_at(
            panel,
            project_path,
            task_status,
            task,
            &state_dir,
        )
    });
    replace_open_tui_task_log_view(panel, task.is_some(), log_view, selected_view);
}

#[cfg(test)]
pub(super) fn sync_open_tui_task_log_view_at(
    panel: &mut TuiAgentPanel,
    project_path: &Path,
    task_status: TaskStatus,
    task: Option<&TaskEntry>,
    log_view: &mut Option<TuiAgentLogView>,
    state_dir: &Path,
) {
    if log_view.is_none() {
        return;
    }

    let selected_view = selected_tui_task_or_project_log_view_for_path_at(
        panel,
        project_path,
        task_status,
        task,
        state_dir,
    );
    replace_open_tui_task_log_view(panel, task.is_some(), log_view, selected_view);
}

#[cfg(test)]
pub(super) fn sync_open_tui_agent_log_view_at(
    panel: &TuiAgentPanel,
    log_view: &mut Option<TuiAgentLogView>,
    state_dir: &Path,
) {
    if log_view.is_none() {
        return;
    }

    let selected_view = selected_tui_agent_log_view_at(panel, state_dir);
    replace_open_tui_agent_log_view(panel, log_view, selected_view);
}

pub(super) fn replace_open_tui_agent_log_view(
    panel: &TuiAgentPanel,
    log_view: &mut Option<TuiAgentLogView>,
    selected_view: Result<Option<TuiAgentLogView>>,
) {
    let project_name = panel
        .selected_project()
        .map(|selected| selected.project.name.clone())
        .or_else(|| {
            panel
                .selected_current_project_registration()
                .map(|registration| registration.name.clone())
        })
        .unwrap_or_else(|| "No Project Selected".to_string());

    *log_view = Some(match selected_view {
        Ok(Some(view)) => view,
        Ok(None) => {
            let content = if panel.selected_current_project_registration().is_some() {
                "Register this project before viewing agent output".to_string()
            } else if panel.selected_project().is_some() {
                "No agent output recorded for selected project".to_string()
            } else {
                "No registered project selected".to_string()
            };
            TuiAgentLogView::message(project_name, content)
        }
        Err(err) => {
            TuiAgentLogView::message(project_name, format!("Error loading agent output: {err}"))
        }
    });
}

pub(super) fn replace_open_tui_task_log_view(
    panel: &TuiAgentPanel,
    task_selected: bool,
    log_view: &mut Option<TuiAgentLogView>,
    selected_view: Result<Option<TuiAgentLogView>>,
) {
    let project_name = panel
        .selected_project()
        .map(|selected| selected.project.name.clone())
        .or_else(|| {
            panel
                .selected_current_project_registration()
                .map(|registration| registration.name.clone())
        })
        .unwrap_or_else(|| "No Project Selected".to_string());

    *log_view = Some(match selected_view {
        Ok(Some(view)) => view,
        Ok(None) => {
            let content = if panel.selected_current_project_registration().is_some() {
                "Register this project before viewing agent output".to_string()
            } else if panel.selected_project().is_some() {
                if task_selected {
                    "No agent output recorded for selected task".to_string()
                } else {
                    "No agent output recorded for selected project".to_string()
                }
            } else {
                "No registered project selected".to_string()
            };
            TuiAgentLogView::message(project_name, content)
        }
        Err(err) => {
            TuiAgentLogView::message(project_name, format!("Error loading agent output: {err}"))
        }
    });
}

pub(super) fn tui_feedback_console_height(
    total_height: u16,
    total_width: u16,
    content: &str,
    agent_log_open: bool,
) -> u16 {
    let minimum_height = 3.min(total_height);
    let maximum_height = (total_height / 2).max(minimum_height).min(total_height);
    if agent_log_open {
        return maximum_height;
    }

    let content_width = total_width.saturating_sub(2) as usize;
    let content_height = wrap_input_text(content, content_width)
        .lines()
        .count()
        .max(1)
        .min(u16::MAX as usize) as u16;
    content_height
        .saturating_add(2)
        .max(minimum_height)
        .min(maximum_height)
}

pub(super) fn tui_log_scroll_offset(content: &str, viewport_height: u16) -> u16 {
    let line_count = content.lines().count().max(1);
    let offset = line_count.saturating_sub(viewport_height as usize);
    offset.min(u16::MAX as usize) as u16
}

pub(super) fn format_tui_agent_panel_top_status_with_time(
    current_time: &str,
    daemon_status: &str,
    project_count: usize,
    enabled_count: usize,
    running_count: usize,
) -> String {
    format!(
        " {current_time}  daemon status: {daemon_status}  {project_count} projects  {enabled_count} enabled  {running_count} running "
    )
}

pub(super) fn truncate_to_width(value: &str, width: usize) -> String {
    let len = value.chars().count();
    if len <= width {
        return value.to_string();
    }

    if width <= 3 {
        return value.chars().take(width).collect();
    }

    let mut truncated: String = value.chars().take(width - 3).collect();
    truncated.push_str("...");
    truncated
}

pub(super) fn fit_cell(value: &str, width: usize) -> String {
    let value = truncate_to_width(value, width);
    format!("{value:<width$}")
}

pub(super) fn fit_cell_right(value: &str, width: usize) -> String {
    let value = truncate_to_width(value, width);
    format!("{value:>width$}")
}

pub(super) fn format_agent_table_last_run(project: &agent::AgentProject) -> String {
    let Some(raw) = project.last_run_at.as_deref() else {
        return "-".to_string();
    };
    let Ok(seconds) = raw.parse::<i64>() else {
        return raw.to_string();
    };
    let Some(utc) = DateTime::<Utc>::from_timestamp(seconds, 0) else {
        return raw.to_string();
    };

    utc.with_timezone(&Local).format("%m-%d %H:%M").to_string()
}

pub(super) fn active_board_marker(is_current_board: bool) -> &'static str {
    if is_current_board { "*" } else { "" }
}

pub(super) fn compact_agent_model_setting(provider: Option<&str>, model: Option<&str>) -> String {
    let model = model.unwrap_or("default");
    let compact = model
        .strip_prefix("gpt-")
        .unwrap_or(model)
        .replace("-codex", "");
    match provider.filter(|provider| *provider != "openai") {
        Some(provider) => format!("{provider}:{compact}"),
        None => compact,
    }
}

pub(super) fn compact_agent_thinking_setting(thinking: Option<&str>) -> &str {
    match thinking {
        None => "def",
        Some("medium") => "med",
        Some(value) => value,
    }
}

pub(super) fn compact_agent_codex_settings(
    provider: Option<&str>,
    model: Option<&str>,
    thinking: Option<&str>,
    fast_enabled: bool,
) -> String {
    let mut settings = Vec::new();

    if model.is_some() {
        settings.push(compact_agent_model_setting(provider, model));
    }
    if thinking.is_some() {
        settings.push(compact_agent_thinking_setting(thinking).to_string());
    }
    if fast_enabled {
        settings.push("fast".to_string());
    }

    if settings.is_empty() {
        "default".to_string()
    } else {
        settings.join("/")
    }
}

pub(super) fn agent_codex_column_width(
    projects: &[TuiAgentProject],
    include_registration: bool,
) -> usize {
    let settings_width = projects
        .iter()
        .map(|item| {
            compact_agent_codex_settings(
                item.project.codex_provider.as_deref(),
                item.project.codex_model.as_deref(),
                item.project.codex_reasoning_effort.as_deref(),
                item.project.codex_fast_enabled,
            )
            .chars()
            .count()
        })
        .max()
        .unwrap_or(0);
    let registration_width = if include_registration {
        "Enter/Space".len()
    } else {
        0
    };

    "CODEX"
        .len()
        .max(settings_width)
        .max(registration_width)
        .min(TUI_AGENT_TABLE_CODEX_MAX_WIDTH)
}

pub(super) fn agent_project_column_width(
    projects: &[TuiAgentProject],
    registration: Option<&TuiCurrentProjectRegistration>,
    table_width: usize,
    codex_width: usize,
) -> usize {
    let desired_width = projects
        .iter()
        .map(|item| item.project.name.chars().count())
        .chain(registration.map(|item| item.name.chars().count()))
        .max()
        .unwrap_or(0)
        .max("PROJECT".len());
    let fixed_width = if table_width < 120 { 52 } else { 53 } + codex_width;
    let available_width = table_width.saturating_sub(fixed_width);
    let max_project_width = if available_width > "PATH".len() {
        available_width - "PATH".len()
    } else {
        available_width
    };

    desired_width.min(max_project_width)
}

pub(super) fn format_agent_project_table_row(
    idx: usize,
    item: &TuiAgentProject,
    width: usize,
    project_width: usize,
    codex_width: usize,
    is_current_board: bool,
) -> String {
    let marker = active_board_marker(is_current_board);
    let state = if item.project.enabled { "ON" } else { "OFF" };
    let runtime_state = item.runtime_state.label();
    let git = item.project.git_mode.tui_label();
    let codex = compact_agent_codex_settings(
        item.project.codex_provider.as_deref(),
        item.project.codex_model.as_deref(),
        item.project.codex_reasoning_effort.as_deref(),
        item.project.codex_fast_enabled,
    );
    let todo = item.scan.todo_count.to_string();
    let doing = item.scan.doing_count.to_string();
    let last_run = format_agent_table_last_run(&item.project);
    let path_or_error = item
        .displayed_problem()
        .unwrap_or_else(|| item.project.path.to_str().unwrap_or("<non-UTF-8 path>"));

    if width < 120 {
        let path_width = width.saturating_sub(52 + project_width + codex_width);
        return truncate_to_width(
            &format!(
                "{}{} {} {} {} {} {} {} {}{}{} {}",
                fit_cell(marker, 1),
                fit_cell_right(&(idx + 1).to_string(), 3),
                fit_cell(state, 6),
                fit_cell(git, 4),
                fit_cell(runtime_state, 7),
                fit_cell(&item.project.name, project_width),
                fit_cell_right(&todo, 4),
                fit_cell_right(&doing, 5),
                fit_cell(&codex, codex_width),
                TUI_AGENT_TABLE_CODEX_LAST_RUN_GAP,
                fit_cell(&last_run, 11),
                fit_cell(path_or_error, path_width)
            ),
            width,
        );
    }

    let marker_width = 1;
    let number_width = 4;
    let state_width = 6;
    let runtime_width = 7;
    let git_width = 4;
    let todo_width = 4;
    let doing_width = 5;
    let last_run_width = 11;
    let gap_count = 8 + TUI_AGENT_TABLE_CODEX_LAST_RUN_GAP.len();
    let fixed_width = number_width
        + marker_width
        + state_width
        + runtime_width
        + git_width
        + codex_width
        + todo_width
        + doing_width
        + last_run_width
        + gap_count;
    let path_width = width.saturating_sub(fixed_width + project_width);

    truncate_to_width(
        &format!(
            "{}{} {} {} {} {} {} {} {}{}{} {}",
            fit_cell(marker, marker_width),
            fit_cell_right(&(idx + 1).to_string(), number_width),
            fit_cell(state, state_width),
            fit_cell(git, git_width),
            fit_cell(runtime_state, runtime_width),
            fit_cell(&item.project.name, project_width),
            fit_cell_right(&todo, todo_width),
            fit_cell_right(&doing, doing_width),
            fit_cell(&codex, codex_width),
            TUI_AGENT_TABLE_CODEX_LAST_RUN_GAP,
            fit_cell(&last_run, last_run_width),
            fit_cell(path_or_error, path_width)
        ),
        width,
    )
}

pub(super) fn format_current_project_registration_row(
    registration: &TuiCurrentProjectRegistration,
    width: usize,
    project_width: usize,
    codex_width: usize,
) -> String {
    if width < 120 {
        let path_width = width.saturating_sub(52 + project_width + codex_width);
        return truncate_to_width(
            &format!(
                "{}{} {} {} {} {} {} {} {}{}{} {}",
                fit_cell("+", 1),
                fit_cell_right("", 3),
                fit_cell("ADD", 6),
                fit_cell("-", 4),
                fit_cell("-", 7),
                fit_cell(&registration.name, project_width),
                fit_cell_right("-", 4),
                fit_cell_right("-", 5),
                fit_cell("Enter/Space", codex_width),
                TUI_AGENT_TABLE_CODEX_LAST_RUN_GAP,
                fit_cell("Enter/Space", 11),
                fit_cell(&registration.path.display().to_string(), path_width)
            ),
            width,
        );
    }

    let marker_width = 1;
    let number_width = 4;
    let state_width = 6;
    let runtime_width = 7;
    let git_width = 4;
    let todo_width = 4;
    let doing_width = 5;
    let last_run_width = 11;
    let gap_count = 8 + TUI_AGENT_TABLE_CODEX_LAST_RUN_GAP.len();
    let fixed_width = number_width
        + marker_width
        + state_width
        + runtime_width
        + git_width
        + codex_width
        + todo_width
        + doing_width
        + last_run_width
        + gap_count;
    let path_width = width.saturating_sub(fixed_width + project_width);

    truncate_to_width(
        &format!(
            "{}{} {} {} {} {} {} {} {}{}{} {}",
            fit_cell("+", marker_width),
            fit_cell_right("", number_width),
            fit_cell("ADD", state_width),
            fit_cell("-", git_width),
            fit_cell("-", runtime_width),
            fit_cell(&registration.name, project_width),
            fit_cell_right("-", todo_width),
            fit_cell_right("-", doing_width),
            fit_cell("-", codex_width),
            TUI_AGENT_TABLE_CODEX_LAST_RUN_GAP,
            fit_cell("Enter/Space", last_run_width),
            fit_cell(&registration.path.display().to_string(), path_width)
        ),
        width,
    )
}

pub(super) fn format_agent_project_table_header(
    width: usize,
    project_width: usize,
    codex_width: usize,
) -> String {
    if width < 120 {
        let path_width = width.saturating_sub(52 + project_width + codex_width);
        return truncate_to_width(
            &format!(
                "{}{} {} {} {} {} {} {} {}{}{} {}",
                fit_cell("", 1),
                fit_cell_right("#", 3),
                fit_cell("STATUS", 6),
                fit_cell("GIT", 4),
                fit_cell("AGENT", 7),
                fit_cell("PROJECT", project_width),
                fit_cell_right("TODO", 4),
                fit_cell_right("DOING", 5),
                fit_cell("CODEX", codex_width),
                TUI_AGENT_TABLE_CODEX_LAST_RUN_GAP,
                fit_cell("LAST RUN", 11),
                fit_cell("PATH / ERROR", path_width)
            ),
            width,
        );
    }

    let marker_width = 1;
    let number_width = 4;
    let state_width = 6;
    let runtime_width = 7;
    let git_width = 4;
    let todo_width = 4;
    let doing_width = 5;
    let last_run_width = 11;
    let gap_count = 8 + TUI_AGENT_TABLE_CODEX_LAST_RUN_GAP.len();
    let fixed_width = number_width
        + marker_width
        + state_width
        + runtime_width
        + git_width
        + codex_width
        + todo_width
        + doing_width
        + last_run_width
        + gap_count;
    let path_width = width.saturating_sub(fixed_width + project_width);

    truncate_to_width(
        &format!(
            "{}{} {} {} {} {} {} {} {}{}{} {}",
            fit_cell("", marker_width),
            fit_cell_right("#", number_width),
            fit_cell("STATUS", state_width),
            fit_cell("GIT", git_width),
            fit_cell("AGENT", runtime_width),
            fit_cell("PROJECT", project_width),
            fit_cell_right("TODO", todo_width),
            fit_cell_right("DOING", doing_width),
            fit_cell("CODEX", codex_width),
            TUI_AGENT_TABLE_CODEX_LAST_RUN_GAP,
            fit_cell("LAST RUN", last_run_width),
            fit_cell("PATH / ERROR", path_width)
        ),
        width,
    )
}

pub(super) fn render_tui_agent_panel(
    f: &mut ratatui::Frame<'_>,
    area: Rect,
    panel: &TuiAgentPanel,
    active_root: &Path,
    text_color: Color,
    c_highlight: Color,
    current_time: &str,
) {
    let enabled_count = panel
        .projects
        .iter()
        .filter(|item| item.project.enabled)
        .count();
    let running_count = panel
        .projects
        .iter()
        .filter(|item| item.runtime_state.is_running())
        .count();
    let row_count = panel.row_count();
    let block = Block::default()
        .title(" Agent Projects  <<<<<< * >>>>>> ")
        .title(
            Line::from(vec![Span::raw(
                format_tui_agent_panel_top_status_with_time(
                    current_time,
                    &panel.daemon_status,
                    panel.projects.len(),
                    enabled_count,
                    running_count,
                ),
            )])
            .alignment(Alignment::Right),
        )
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));
    let inner_area = block.inner(area);
    f.render_widget(block, area);

    if inner_area.height == 0 || inner_area.width == 0 {
        return;
    }

    if row_count == 0 {
        f.render_widget(
            Paragraph::new("No registered projects. Run: clt agent register .")
                .style(Style::default().fg(Color::Indexed(244))),
            inner_area,
        );
        return;
    }

    let table_width = inner_area.width as usize;
    let row_viewport_height = inner_area.height.saturating_sub(2) as usize;
    let codex_width = agent_codex_column_width(
        &panel.projects,
        panel.current_project_registration.is_some(),
    );
    let project_width = agent_project_column_width(
        &panel.projects,
        panel.current_project_registration.as_ref(),
        table_width,
        codex_width,
    );

    let header_area = Rect {
        x: inner_area.x,
        y: inner_area.y,
        width: inner_area.width,
        height: 1,
    };
    f.render_widget(
        Paragraph::new(format_agent_project_table_header(
            table_width,
            project_width,
            codex_width,
        ))
        .style(Style::default().fg(Color::Indexed(244))),
        header_area,
    );

    if inner_area.height > 1 {
        let separator_area = Rect {
            x: inner_area.x,
            y: inner_area.y + 1,
            width: inner_area.width,
            height: 1,
        };
        f.render_widget(
            Paragraph::new("-".repeat(table_width)).style(Style::default().fg(Color::Indexed(238))),
            separator_area,
        );
    }

    if row_viewport_height == 0 {
        return;
    }

    let selected_idx = panel.state.selected();
    let highlight_style = Style::default().fg(Color::Black).bg(c_highlight);

    for (row, idx) in (0..row_count)
        .skip(panel.scroll_offset)
        .take(row_viewport_height)
        .enumerate()
    {
        let (text, row_style) = if idx == 0 {
            if let Some(registration) = panel.current_project_registration.as_ref() {
                (
                    format_current_project_registration_row(
                        registration,
                        table_width,
                        project_width,
                        codex_width,
                    ),
                    Style::default().fg(Color::LightGreen),
                )
            } else {
                let item = &panel.projects[idx];
                (
                    format_agent_project_table_row(
                        idx,
                        item,
                        table_width,
                        project_width,
                        codex_width,
                        item.project.path == active_root,
                    ),
                    if item.displayed_problem().is_some() {
                        Style::default().fg(Color::LightRed)
                    } else if item.project.enabled {
                        Style::default().fg(text_color)
                    } else {
                        Style::default().fg(Color::Indexed(244))
                    },
                )
            }
        } else {
            let project_idx = idx - panel.project_start_index();
            let item = &panel.projects[project_idx];
            (
                format_agent_project_table_row(
                    project_idx,
                    item,
                    table_width,
                    project_width,
                    codex_width,
                    item.project.path == active_root,
                ),
                if item.displayed_problem().is_some() {
                    Style::default().fg(Color::LightRed)
                } else if item.project.enabled {
                    Style::default().fg(text_color)
                } else {
                    Style::default().fg(Color::Indexed(244))
                },
            )
        };
        let style = if Some(idx) == selected_idx {
            highlight_style
        } else {
            row_style
        };
        let item_area = Rect {
            x: inner_area.x,
            y: inner_area.y + 2 + row as u16,
            width: inner_area.width,
            height: 1,
        };
        f.render_widget(Paragraph::new(text).style(style), item_area);
    }
}

pub(super) fn tui_models_instructions() -> &'static str {
    "n adds a provider. Select a non-built-in provider on the left and press x/Delete to remove it. Local endpoints discover /models automatically; r refreshes. New models start OFF: Right, Up/Down, PageUp/PageDown, Home/End, then Space chooses them. / searches model names and IDs. a manually adds an ID. f favorites; t cycles model reasoning; d sets CLT; c sets Codex. M, Tab, or Esc returns to the previous pane. API keys come only from environment variables."
}

pub(super) fn provider_env_status(provider: &agent::AgentModelProvider) -> String {
    match provider.env_key.as_deref() {
        Some(key) => {
            let visible = std::env::var_os(key)
                .is_some_and(|value| !value.to_string_lossy().trim().is_empty());
            format!("{key}:{}", if visible { "visible" } else { "missing" })
        }
        None => "no API key".to_string(),
    }
}

pub(super) fn tui_models_provider_header() -> &'static str {
    "USE TYPE    PROVIDER (ID)"
}

pub(super) fn tui_models_add_provider_hint() -> &'static str {
    "[n] Add provider"
}

pub(super) fn tui_models_provider_choice_prompt() -> &'static str {
    "Add provider: [1] OpenAI  [2] OpenRouter  [3] Ollama  [4] LM Studio  [5] Local/custom endpoint  [Esc] Cancel"
}

pub(super) fn include_codex_default_model_target(
    store: &agent::TursoAgentStore,
    selected_provider_id: &str,
    codex_provider_id: Option<&str>,
    codex_model_id: Option<&str>,
    models: &mut Vec<agent::AgentModelTarget>,
) -> Result<()> {
    let codex_provider_id = codex_provider_id.unwrap_or("openai");
    let Some(codex_model_id) = codex_model_id else {
        return Ok(());
    };
    if selected_provider_id != codex_provider_id
        || models
            .iter()
            .any(|model| model.provider_id == codex_provider_id && model.model_id == codex_model_id)
    {
        return Ok(());
    }

    let target = agent::AgentModelTarget {
        provider_id: codex_provider_id.to_string(),
        model_id: codex_model_id.to_string(),
        label: codex_model_id.to_string(),
        enabled: true,
        favorite: false,
        reasoning_effort: None,
    };
    store.upsert_model_target_blocking(&target)?;
    let insertion_index = models
        .iter()
        .position(|model| !model.favorite)
        .unwrap_or(models.len());
    models.insert(insertion_index, target);
    Ok(())
}

pub(super) fn tui_models_provider_row(provider: &agent::AgentModelProvider) -> String {
    format!(
        "{:<3} {:<7} {} ({})",
        if provider.enabled { "ON" } else { "OFF" },
        if provider.built_in {
            "BUILTIN"
        } else {
            "CUSTOM"
        },
        provider.name,
        provider.id
    )
}

pub(super) fn tui_models_model_header() -> String {
    format!(
        "{:<3} {:<3} {:<3} {:<5} {:<7} {:<16} {}",
        "USE", "FAV", "CLT", "CODEX", "THINK", "MODEL", "ID"
    )
}

pub(super) fn tui_model_matches_clt_default(
    defaults: &agent::AgentModelDefaults,
    codex_provider_id: Option<&str>,
    codex_model_id: Option<&str>,
    model: &agent::AgentModelTarget,
) -> bool {
    match (
        defaults.provider_id.as_deref(),
        defaults.model_id.as_deref(),
    ) {
        (Some(provider_id), Some(model_id)) => {
            provider_id == model.provider_id && model_id == model.model_id
        }
        _ => tui_model_matches_codex_default(codex_provider_id, codex_model_id, model),
    }
}

pub(super) fn tui_model_matches_codex_default(
    provider_id: Option<&str>,
    model_id: Option<&str>,
    model: &agent::AgentModelTarget,
) -> bool {
    let provider_id = provider_id.unwrap_or("openai");
    model_id == Some(model.model_id.as_str()) && provider_id == model.provider_id
}

pub(super) fn tui_models_model_row(
    model: &agent::AgentModelTarget,
    is_clt_default: bool,
    is_codex_default: bool,
) -> String {
    format!(
        "{:<3} {:<3} {:<3} {:<5} {:<7} {:<16} {}",
        if model.enabled { "ON" } else { "OFF" },
        if model.favorite { "YES" } else { "-" },
        if is_clt_default { "YES" } else { "-" },
        if is_codex_default { "YES" } else { "-" },
        model.reasoning_effort.as_deref().unwrap_or("system"),
        model.label,
        model.model_id
    )
}

pub(super) fn render_tui_models_panel(
    f: &mut ratatui::Frame<'_>,
    area: Rect,
    panel: &TuiModelsPanel,
    text_color: Color,
    c_highlight: Color,
    provider_env_statuses: &HashMap<String, String>,
) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(38), Constraint::Percentage(62)])
        .split(area);
    let clt_default = match (
        panel.defaults.provider_id.as_deref(),
        panel.defaults.model_id.as_deref(),
    ) {
        (Some(provider), Some(model)) => format!("{provider}/{model}"),
        _ => "Codex config".to_string(),
    };
    let default_label = format!(
        " CLT default: {clt_default} | Codex: {} ",
        panel.codex_default
    );
    let provider_focused = panel.focus == TuiModelsFocus::Providers;
    let provider_block = Block::default()
        .title(if provider_focused {
            " Providers  <<<<<< * >>>>>> "
        } else {
            " Providers "
        })
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if provider_focused {
            Color::Yellow
        } else {
            Color::Indexed(244)
        }));
    let provider_inner = provider_block.inner(chunks[0]);
    f.render_widget(provider_block, chunks[0]);

    if let Some(error) = panel.last_error.as_deref() {
        f.render_widget(
            Paragraph::new(error).style(Style::default().fg(Color::Red)),
            provider_inner,
        );
        return;
    }

    let add_hint_height = provider_inner.height.min(1);
    if add_hint_height > 0 {
        f.render_widget(
            Paragraph::new(tui_models_add_provider_hint())
                .style(Style::default().fg(Color::LightGreen)),
            Rect::new(
                provider_inner.x,
                provider_inner.y,
                provider_inner.width,
                add_hint_height,
            ),
        );
    }
    if provider_inner.height > add_hint_height {
        f.render_widget(
            Paragraph::new(truncate_to_width(
                tui_models_provider_header(),
                provider_inner.width as usize,
            ))
            .style(Style::default().fg(Color::Cyan)),
            Rect::new(
                provider_inner.x,
                provider_inner.y.saturating_add(add_hint_height),
                provider_inner.width,
                1,
            ),
        );
    }
    let provider_list_inner = Rect::new(
        provider_inner.x,
        provider_inner
            .y
            .saturating_add(add_hint_height)
            .saturating_add(1),
        provider_inner.width,
        provider_inner
            .height
            .saturating_sub(add_hint_height)
            .saturating_sub(1),
    );
    let provider_selected = panel.provider_state.selected();
    let provider_height = provider_list_inner.height as usize;
    let provider_offset = provider_selected
        .unwrap_or(0)
        .saturating_sub(provider_height.saturating_sub(1));
    for (index, provider) in panel
        .providers
        .iter()
        .enumerate()
        .skip(provider_offset)
        .take(provider_height)
    {
        let row = tui_models_provider_row(provider);
        let style = if Some(index) == provider_selected {
            if provider_focused {
                Style::default().fg(Color::Black).bg(c_highlight)
            } else {
                Style::default().fg(Color::White).bg(Color::DarkGray)
            }
        } else if provider.enabled {
            Style::default().fg(text_color)
        } else {
            Style::default().fg(Color::Indexed(244))
        };
        f.render_widget(
            Paragraph::new(truncate_to_width(&row, provider_list_inner.width as usize))
                .style(style),
            Rect::new(
                provider_list_inner.x,
                provider_list_inner.y + (index - provider_offset) as u16,
                provider_list_inner.width,
                1,
            ),
        );
    }

    let models_focused = panel.focus == TuiModelsFocus::Models;
    let selected_provider_name = panel
        .selected_provider()
        .map(|provider| provider.name.as_str())
        .unwrap_or("No provider");
    let model_title = if panel.model_search.is_empty() {
        format!(" {selected_provider_name} Models ")
    } else {
        format!(
            " {selected_provider_name} Models  Search: {} ",
            panel.model_search
        )
    };
    let models_block = Block::default()
        .title(if models_focused {
            format!("{model_title} <<<<<< * >>>>>>")
        } else {
            model_title
        })
        .title(Line::from(default_label).alignment(Alignment::Right))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if models_focused {
            Color::Yellow
        } else {
            Color::Indexed(244)
        }));
    let models_inner = models_block.inner(chunks[1]);
    f.render_widget(models_block, chunks[1]);

    if models_inner.height > 0 {
        let detail = panel
            .selected_provider()
            .map(|provider| {
                format!(
                    "Provider: {}  Auth: {}  Endpoint: {}  [r] discover  Wire: responses",
                    provider.id,
                    provider_env_statuses
                        .get(&provider.id)
                        .map(String::as_str)
                        .unwrap_or("unknown"),
                    provider.base_url.as_deref().unwrap_or("Codex built-in")
                )
            })
            .unwrap_or_default();
        f.render_widget(
            Paragraph::new(truncate_to_width(&detail, models_inner.width as usize))
                .style(Style::default().fg(Color::Indexed(244))),
            Rect::new(models_inner.x, models_inner.y, models_inner.width, 1),
        );
    }
    if models_inner.height > 1 {
        let header = tui_models_model_header();
        f.render_widget(
            Paragraph::new(truncate_to_width(&header, models_inner.width as usize))
                .style(Style::default().fg(Color::Cyan)),
            Rect::new(
                models_inner.x,
                models_inner.y.saturating_add(1),
                models_inner.width,
                1,
            ),
        );
    }
    let models_list_inner = Rect::new(
        models_inner.x,
        models_inner.y.saturating_add(2),
        models_inner.width,
        models_inner.height.saturating_sub(2),
    );

    let model_selected = panel.model_state.selected();
    let models_height = models_list_inner.height as usize;
    let visible_model_indices = panel.visible_model_indices();
    if models_list_inner.height > 0 {
        let empty_message = if panel.models.is_empty() {
            Some("No models yet. Press r to discover or a to add a model ID.".to_string())
        } else if visible_model_indices.is_empty() {
            Some(format!(
                "No models match {:?}. Press / to change or clear the search.",
                panel.model_search
            ))
        } else {
            None
        };
        if let Some(message) = empty_message {
            f.render_widget(
                Paragraph::new(message).style(Style::default().fg(Color::Yellow)),
                models_list_inner,
            );
        }
    }
    let selected_visible_position = model_selected
        .and_then(|selected| {
            visible_model_indices
                .iter()
                .position(|index| *index == selected)
        })
        .unwrap_or(0);
    let models_offset = selected_visible_position.saturating_sub(models_height.saturating_sub(1));
    for (visible_position, model_index) in visible_model_indices
        .iter()
        .enumerate()
        .skip(models_offset)
        .take(models_height)
    {
        let model = &panel.models[*model_index];
        let row = tui_models_model_row(
            model,
            tui_model_matches_clt_default(
                &panel.defaults,
                panel.codex_default_provider.as_deref(),
                panel.codex_default_model.as_deref(),
                model,
            ),
            tui_model_matches_codex_default(
                panel.codex_default_provider.as_deref(),
                panel.codex_default_model.as_deref(),
                model,
            ),
        );
        let style = if Some(*model_index) == model_selected {
            if models_focused {
                Style::default().fg(Color::Black).bg(c_highlight)
            } else {
                Style::default().fg(Color::White).bg(Color::DarkGray)
            }
        } else if model.enabled {
            Style::default().fg(text_color)
        } else {
            Style::default().fg(Color::Indexed(244))
        };
        f.render_widget(
            Paragraph::new(truncate_to_width(&row, models_list_inner.width as usize)).style(style),
            Rect::new(
                models_list_inner.x,
                models_list_inner.y + (visible_position - models_offset) as u16,
                models_list_inner.width,
                1,
            ),
        );
    }
}

pub(super) fn tui_console_content<'a>(
    agent_pane: bool,
    panel: &'a TuiAgentPanel,
    log_view: Option<&'a TuiAgentLogView>,
    feedback: &'a str,
) -> (&'a str, Color) {
    if agent_pane && let Some(error) = panel.last_error.as_deref() {
        return (error, Color::Red);
    }
    if let Some(log_view) = log_view {
        return (&log_view.content, Color::Gray);
    }
    if agent_pane
        && let Some(problem) = panel
            .selected_project()
            .and_then(TuiAgentProject::displayed_problem)
    {
        return (problem, Color::LightRed);
    }

    (feedback, Color::Gray)
}

pub(super) fn tui_keyboard_enhancement_flags() -> KeyboardEnhancementFlags {
    KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
        | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
}

pub(super) struct TerminalSession {
    pub(super) keyboard_enhancement_enabled: bool,
    pub(super) active: bool,
}

impl TerminalSession {
    pub(super) fn enter(title: &str) -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = stdout();
        if let Err(err) = stdout.execute(EnterAlternateScreen) {
            let _ = disable_raw_mode();
            return Err(err.into());
        }
        if let Err(err) = stdout.execute(EnableBracketedPaste) {
            let _ = stdout.execute(LeaveAlternateScreen);
            let _ = disable_raw_mode();
            return Err(err.into());
        }

        // Request mode 2 modified-key reporting. In particular, tmux only forwards its
        // unambiguous extended key sequences when the application inside the pane asks for
        // them, which keeps Shift+Up/Down distinct from plain task navigation.
        #[cfg(not(windows))]
        if let Err(err) = stdout.execute(PushKeyboardEnhancementFlags(
            tui_keyboard_enhancement_flags(),
        )) {
            let _ = stdout.execute(DisableBracketedPaste);
            let _ = stdout.execute(LeaveAlternateScreen);
            let _ = disable_raw_mode();
            return Err(err.into());
        }
        let keyboard_enhancement_enabled = cfg!(not(windows));

        if let Err(err) = set_terminal_title(title) {
            if keyboard_enhancement_enabled {
                let _ = stdout.execute(PopKeyboardEnhancementFlags);
            }
            let _ = stdout.execute(DisableBracketedPaste);
            let _ = stdout.execute(LeaveAlternateScreen);
            let _ = disable_raw_mode();
            return Err(err);
        }

        Ok(Self {
            keyboard_enhancement_enabled,
            active: true,
        })
    }

    pub(super) fn suspend(&mut self) {
        if !self.active {
            return;
        }
        if self.keyboard_enhancement_enabled {
            let _ = stdout().execute(PopKeyboardEnhancementFlags);
        }
        let _ = stdout().execute(DisableBracketedPaste);
        let _ = disable_raw_mode();
        let _ = stdout().execute(LeaveAlternateScreen);
        let _ = stdout().flush();
        self.active = false;
    }

    pub(super) fn resume(&mut self, title: &str) -> Result<()> {
        if self.active {
            return Ok(());
        }
        *self = Self::enter(title)?;
        Ok(())
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        self.suspend();
    }
}

pub(super) fn wrap_text(text: &str, width: usize) -> String {
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

pub(super) fn wrap_input_text(text: &str, width: usize) -> String {
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

pub(super) fn input_cursor_offset_at(text: &str, width: usize, cursor_idx: usize) -> (u16, u16) {
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

pub(super) fn byte_index_at_char(text: &str, char_idx: usize) -> usize {
    text.char_indices()
        .nth(char_idx)
        .map(|(idx, _)| idx)
        .unwrap_or(text.len())
}

pub(super) fn input_cursor_offset_at_char(
    text: &str,
    width: usize,
    cursor_chars: usize,
) -> (usize, usize) {
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

pub(super) fn char_index_for_input_offset(
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

pub(super) fn move_input_cursor_row(
    input: &mut Input,
    label: &str,
    width: usize,
    row_delta: isize,
) {
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

pub(super) fn handle_input_key(
    input: &mut Input,
    key: crossterm::event::KeyEvent,
    label: &str,
    width: usize,
) {
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
        KeyCode::Char('b' | 'B') if key.modifiers.contains(KeyModifiers::ALT) => {
            Some(InputRequest::GoToPrevWord)
        }
        KeyCode::Char('f' | 'F') if key.modifiers.contains(KeyModifiers::ALT) => {
            Some(InputRequest::GoToNextWord)
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

pub(super) fn clamp_to_char_boundary(text: &str, idx: usize) -> usize {
    let mut idx = idx.min(text.len());
    while idx > 0 && !text.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

#[cfg(test)]
pub(super) fn previous_char_boundary(text: &str, idx: usize) -> usize {
    let idx = clamp_to_char_boundary(text, idx);
    text[..idx]
        .char_indices()
        .last()
        .map(|(char_idx, _)| char_idx)
        .unwrap_or(0)
}

#[cfg(test)]
pub(super) fn next_char_boundary(text: &str, idx: usize) -> usize {
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

pub(super) fn board_display_name(root: &Path, board_dir: &Path) -> String {
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

pub(super) fn tui_console_block<'a>(title: &'a str, right_title: Option<&'a str>) -> Block<'a> {
    let block = Block::default().borders(Borders::ALL).title(title);

    if let Some(right_title) = right_title {
        block.title(Line::from(right_title).alignment(Alignment::Right))
    } else {
        block
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TuiCodexHandoffStage {
    WaitingForAutomatedExit,
    PreparingIdleSession,
    PreparingSharedSession,
    EnteringInteractive,
    QueueingExecResume,
    RestoringTaskControls,
}

impl TuiCodexHandoffStage {
    pub(super) fn message(self) -> &'static str {
        match self {
            Self::WaitingForAutomatedExit => {
                "Requesting the automated Codex process to stop...\nWaiting for the current run to exit safely."
            }
            Self::PreparingIdleSession => {
                "Reserving the Codex session for interactive use...\nThe session will open as soon as the handoff is ready."
            }
            Self::PreparingSharedSession => {
                "Another Codex task is using this project.\nReserving this idle session for writable interactive use alongside it..."
            }
            Self::EnteringInteractive => {
                "Entering interactive Codex...\nExit Codex when you are ready to return to CLT."
            }
            Self::QueueingExecResume => {
                "Interactive Codex exited.\nReturning the same session to automated exec mode..."
            }
            Self::RestoringTaskControls => {
                "Interactive Codex exited.\nRestoring the task's Codex session controls..."
            }
        }
    }
}

pub(super) fn draw_tui_codex_handoff_status<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    stage: TuiCodexHandoffStage,
) -> std::result::Result<(), B::Error> {
    terminal.draw(|frame| {
        let area = frame.area();
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Codex handoff ");
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let status_height = inner.height.min(2);
        let status_area = Rect::new(
            inner.x,
            inner.y + inner.height.saturating_sub(status_height) / 2,
            inner.width,
            status_height,
        );
        frame.render_widget(
            Paragraph::new(stage.message()).alignment(Alignment::Center),
            status_area,
        );
    })?;
    Ok(())
}

pub(super) fn write_tui_codex_handoff_status<W: Write>(
    output: &mut W,
    stage: TuiCodexHandoffStage,
) -> io::Result<()> {
    writeln!(output, "\n{}", stage.message())?;
    output.flush()
}

pub(super) type TuiTerminal = Terminal<CrosstermBackend<std::io::Stdout>>;

pub(super) fn viewed_tui_codex_session_target(
    log_view: Option<&TuiAgentLogView>,
) -> Result<TuiCodexSessionTarget> {
    let log_view = log_view.context("Open agent output with l before controlling its session")?;
    log_view.session_target.clone().context(
        "The displayed output does not identify a Codex session yet; wait for its session ID",
    )
}

pub(super) fn selected_tui_agent_session_target(
    panel: &TuiAgentPanel,
) -> Result<TuiCodexSessionTarget> {
    let state_dir = ensure_agent_state_dir()?;
    selected_tui_agent_session_target_at(panel, &state_dir)
}

pub(super) fn selected_tui_agent_session_target_at(
    panel: &TuiAgentPanel,
    state_dir: &Path,
) -> Result<TuiCodexSessionTarget> {
    let selected = panel
        .selected_project()
        .context("Select a registered agent project before controlling its session")?;
    let controls = open_agent_store_at(state_dir)?
        .session_controls_for_project_blocking(selected.project.id)?
        .into_iter()
        .filter(|control| control.state != AgentSessionControlState::Stopped)
        .collect::<Vec<_>>();
    let interactive = controls
        .iter()
        .filter(|control| {
            matches!(
                control.state,
                AgentSessionControlState::Interactive | AgentSessionControlState::StopRequested
            ) && control
                .interactive_holder
                .as_deref()
                .and_then(InteractiveGuardianDisposition::from_guardian_holder)
                .is_some()
        })
        .collect::<Vec<_>>();
    let control = if interactive.len() == 1 {
        interactive[0]
    } else if controls.len() == 1 {
        &controls[0]
    } else if controls.is_empty() {
        anyhow::bail!(
            "The selected project has no active or fenced Codex session to stop; open recorded output with l to control an older session"
        );
    } else {
        anyhow::bail!(
            "The selected project has multiple controllable Codex sessions; open the intended output with l before pressing s"
        );
    };

    Ok(TuiCodexSessionTarget::new(
        &selected.project,
        control.codex_session_id.clone(),
    ))
}

pub(super) fn run_tui_codex_session_interrupt(
    terminal: &mut TuiTerminal,
    terminal_session: &mut TerminalSession,
    return_title: &str,
    target: &TuiCodexSessionTarget,
    label: &str,
) -> Result<String> {
    draw_tui_codex_handoff_status(terminal, TuiCodexHandoffStage::WaitingForAutomatedExit)?;
    let interactive_lease =
        prepare_tui_codex_session_interrupt(target.project_id, &target.session_id)?;

    let _ = draw_tui_codex_handoff_status(terminal, TuiCodexHandoffStage::EnteringInteractive);
    terminal_session.suspend();
    let _ =
        write_tui_codex_handoff_status(&mut stdout(), TuiCodexHandoffStage::EnteringInteractive);
    let provisional_holder = interactive_lease.holder.clone();
    let resume_result = resume_codex_session_interactively(
        &target.project_path,
        target.project_id,
        &target.session_id,
        &provisional_holder,
        InteractiveCodexResumeMode::ResumeExec,
    );
    let _ = write_tui_codex_handoff_status(&mut stdout(), TuiCodexHandoffStage::QueueingExecResume);
    let guardian_completed = resume_result.as_ref().is_ok_and(|status| status.success());
    let queue_result = if guardian_completed {
        Ok(())
    } else {
        queue_tui_codex_session_exec_resume(
            target.project_id,
            &target.session_id,
            &provisional_holder,
        )
    };
    let release_result = interactive_lease.release();
    let worker_result = if guardian_completed {
        Ok(None)
    } else if queue_result.is_ok() {
        spawn_agent_session_resume_worker(
            &target.project_path,
            target.project_id,
            &target.session_id,
        )
        .map(Some)
    } else {
        Ok(None)
    };
    terminal_session.resume(return_title)?;
    terminal.clear()?;

    let interactive_summary = match resume_result {
        Ok(status) if status.success() => {
            format!("Returned from interactive Codex for {label}.")
        }
        Ok(status) => format!("Interactive Codex exited with {status}."),
        Err(error) => format!("Interactive Codex error: {error}."),
    };
    let supervision_log = if guardian_completed {
        agent_state_dir().ok().map(|state_dir| {
            agent_session_resume_worker_log_path(&state_dir, target.project_id, &target.session_id)
                .display()
                .to_string()
        })
    } else {
        worker_result
            .as_ref()
            .ok()
            .and_then(|path| path.as_ref())
            .map(|path| path.display().to_string())
    };
    let mut followup_errors = Vec::new();
    if let Err(error) = queue_result {
        followup_errors.push(format!("could not queue exact exec resume: {error}"));
    }
    if let Err(error) = release_result {
        followup_errors.push(format!("could not release the interactive lease: {error}"));
    }
    if let Err(error) = worker_result {
        followup_errors.push(format!(
            "could not start the exact-session resume worker: {error}"
        ));
    }

    Ok(if followup_errors.is_empty() {
        match supervision_log {
            Some(log) => format!(
                "{interactive_summary} The same session is queued and supervised for automated exec resume (worker log: {log})."
            ),
            None => interactive_summary,
        }
    } else {
        format!("{interactive_summary} CLT {}", followup_errors.join("; "))
    })
}

pub(super) fn run_tui_codex_session_continue(
    terminal: &mut TuiTerminal,
    terminal_session: &mut TerminalSession,
    return_title: &str,
    target: &TuiCodexSessionTarget,
    label: &str,
    shares_project: bool,
    require_resumable_task: bool,
) -> Result<String> {
    draw_tui_codex_handoff_status(
        terminal,
        if shares_project {
            TuiCodexHandoffStage::PreparingSharedSession
        } else {
            TuiCodexHandoffStage::PreparingIdleSession
        },
    )?;
    let stopped_control = tui_stopped_codex_session_control(target.project_id, &target.session_id)?;
    let restore_stopped = stopped_control.is_some();
    let (interactive_lease, provisional_holder) = if shares_project {
        (
            None,
            InteractiveAgentLease::holder_for_shared_session(restore_stopped),
        )
    } else {
        let lease = InteractiveAgentLease::try_acquire_idle(target.project_id, restore_stopped)?
            .context(
                "Another Codex task began using this project; press c again to open this session alongside it",
            )?;
        let holder = lease.holder.clone();
        (Some(lease), holder)
    };
    let stopped_run_token = stopped_control
        .as_ref()
        .and_then(|control| control.run_token.as_deref());
    let reservation_result = if shares_project {
        reserve_tui_shared_codex_session_interactive(
            target.project_id,
            &target.session_id,
            &provisional_holder,
            stopped_run_token,
        )
    } else {
        reserve_tui_idle_codex_session_interactive(
            target.project_id,
            &target.session_id,
            &provisional_holder,
            stopped_run_token,
        )
    };
    if !reservation_result.as_ref().is_ok_and(|reserved| *reserved) {
        let release_result = interactive_lease.map_or(Ok(()), InteractiveAgentLease::release);
        return match (reservation_result, release_result) {
            (Ok(false), Ok(())) if shares_project => anyhow::bail!(
                "The active project run or selected session changed before shared Codex could open; try again"
            ),
            (Ok(false), Ok(())) => {
                anyhow::bail!(
                    "This Codex session became busy before it could be reserved; try again"
                )
            }
            (Err(error), Ok(())) => Err(error),
            (Ok(false), Err(error)) => Err(error)
                .context("The Codex session changed, and its project lease could not be released"),
            (Err(reserve_error), Err(release_error)) => Err(reserve_error).context(format!(
                "The project lease also could not be released: {release_error}"
            )),
            (Ok(true), _) => unreachable!(),
        };
    }

    if require_resumable_task {
        let task_is_resumable = codex_session_task_supports_interactive_resume(
            &target.project_path,
            &target.session_id,
        );
        if !task_is_resumable.as_ref().is_ok_and(|resumable| *resumable) {
            let cancel_result = cancel_tui_idle_codex_session_interactive(
                target.project_id,
                &target.session_id,
                &provisional_holder,
            );
            let release_result = interactive_lease.map_or(Ok(()), InteractiveAgentLease::release);
            return match (task_is_resumable, cancel_result, release_result) {
                (Ok(false), Ok(true), Ok(())) => anyhow::bail!(
                    "This task changed before its Codex session could open; c is only available from Done, Doing, or currently blocked Todo tasks"
                ),
                (Err(error), Ok(true), Ok(())) => Err(error)
                    .context("Unable to revalidate the Codex task before interactive resume"),
                (task, cancel, release) => anyhow::bail!(
                    "Unable to open the Codex task safely (task: {}; reservation: {}; lease: {})",
                    task.map(|_| "changed".to_string())
                        .unwrap_or_else(|error| error.to_string()),
                    cancel
                        .map(|cancelled| if cancelled {
                            "released"
                        } else {
                            "still fenced"
                        }
                        .to_string())
                        .unwrap_or_else(|error| error.to_string()),
                    release
                        .map(|()| "released".to_string())
                        .unwrap_or_else(|error| error.to_string()),
                ),
            };
        }
    }

    let _ = draw_tui_codex_handoff_status(terminal, TuiCodexHandoffStage::EnteringInteractive);
    terminal_session.suspend();
    let _ =
        write_tui_codex_handoff_status(&mut stdout(), TuiCodexHandoffStage::EnteringInteractive);
    let resume_result = resume_codex_session_interactively(
        &target.project_path,
        target.project_id,
        &target.session_id,
        &provisional_holder,
        if shares_project {
            InteractiveCodexResumeMode::WritableShared
        } else {
            InteractiveCodexResumeMode::WritableIdle
        },
    );
    let _ =
        write_tui_codex_handoff_status(&mut stdout(), TuiCodexHandoffStage::RestoringTaskControls);
    let guardian_completed = resume_result.as_ref().is_ok_and(|status| status.success());
    let cancel_result = if guardian_completed {
        Ok(())
    } else {
        match cancel_tui_idle_codex_session_interactive(
            target.project_id,
            &target.session_id,
            &provisional_holder,
        ) {
            Ok(true) => Ok(()),
            Ok(false) => Err(anyhow::anyhow!(
                "the guardian-owned session reservation remains fenced"
            )),
            Err(error) => Err(error),
        }
    };
    let release_result = interactive_lease.map_or(Ok(()), InteractiveAgentLease::release);
    terminal_session.resume(return_title)?;
    terminal.clear()?;

    Ok(match (resume_result, cancel_result, release_result) {
        (Ok(status), Ok(()), Ok(())) if status.success() => {
            format!("Returned from Codex session for {label}; press c to open it again")
        }
        (Ok(status), Ok(()), Ok(())) => format!("Codex session exited with status {status}"),
        (Err(error), Ok(()), Ok(())) => format!("Error: {error}"),
        (resume_result, cancel_result, release_result) => format!(
            "Codex interactive cleanup was incomplete (session: {}; reservation: {}; lease: {})",
            resume_result
                .map(|status| status.to_string())
                .unwrap_or_else(|error| error.to_string()),
            cancel_result
                .map(|()| "released".to_string())
                .unwrap_or_else(|error| error.to_string()),
            release_result
                .map(|()| "released".to_string())
                .unwrap_or_else(|error| error.to_string()),
        ),
    })
}

pub(super) fn tui_view(root: &Path) -> Result<PathBuf> {
    tui_view_with_active_board(root, true)
}

pub(super) fn tui_view_without_active_board(root: &Path) -> Result<PathBuf> {
    tui_view_with_active_board(root, false)
}

pub(super) struct TuiApp {
    pub(super) active_root: PathBuf,
    pub(super) board_stack: Vec<PathBuf>,
    pub(super) active_board: bool,
    pub(super) current_mode: Mode,
    pub(super) task_input: TaskInput,
    pub(super) feedback_buffer: String,
    pub(super) archive_view: bool,
    pub(super) backlog_visible: bool,
    pub(super) current_pane: TuiPane,
    pub(super) models_return_pane: TuiPane,
    pub(super) agent_panel: TuiAgentPanel,
    pub(super) task_agent_session_states: TaskAgentSessionStates,
    pub(super) models_panel: TuiModelsPanel,
    pub(super) model_input: Option<TuiModelInput>,
    pub(super) awaiting_model_provider_choice: bool,
    pub(super) pending_agent_project_removal: Option<TuiAgentProjectRemoval>,
    pub(super) agent_log_view: Option<TuiAgentLogView>,
    pub(super) selected_board: usize,
    pub(super) editing_task_idx: Option<usize>,
    pub(super) subtask_parent: Option<(usize, TaskEntry)>,
    pub(super) board_states: [ListState; 4],
    pub(super) board_scroll_offsets: [usize; 4],
    pub(super) archive_state: ListState,
    pub(super) archive_scroll_offset: usize,
    pub(super) task_snapshot: TuiTaskSnapshot,
    pub(super) current_time: String,
    pub(super) provider_env_statuses: HashMap<String, String>,
}

impl TuiApp {
    pub(super) fn new(root: &Path, start_with_active_board: bool) -> Self {
        let start_state = tui_start_state(start_with_active_board);
        let board_stack = if start_with_active_board {
            vec![get_tasks_dir(root)]
        } else {
            Vec::new()
        };
        let board_dir = board_stack
            .last()
            .cloned()
            .unwrap_or_else(|| get_tasks_dir(root));

        Self {
            active_root: root.to_path_buf(),
            board_stack,
            active_board: start_state.active_board,
            current_mode: Mode::View,
            task_input: TaskInput::default(),
            feedback_buffer: start_state.feedback_buffer,
            archive_view: false,
            backlog_visible: false,
            current_pane: start_state.current_pane,
            models_return_pane: tui_models_return_pane(start_state.current_pane),
            agent_panel: TuiAgentPanel::new(root),
            task_agent_session_states: TaskAgentSessionStates::default(),
            models_panel: TuiModelsPanel::new(),
            model_input: None,
            awaiting_model_provider_choice: false,
            pending_agent_project_removal: None,
            agent_log_view: None,
            selected_board: TODO_BOARD_INDEX,
            editing_task_idx: None,
            subtask_parent: None,
            board_states: std::array::from_fn(|_| ListState::default()),
            board_scroll_offsets: [0; 4],
            archive_state: ListState::default(),
            archive_scroll_offset: 0,
            task_snapshot: TuiTaskSnapshot {
                board_title: if start_with_active_board {
                    board_display_name(root, &board_dir)
                } else {
                    "No Active Board".to_string()
                },
                ..TuiTaskSnapshot::default()
            },
            current_time: String::new(),
            provider_env_statuses: HashMap::new(),
        }
    }

    pub(super) fn board_dir(&self) -> PathBuf {
        self.board_stack
            .last()
            .cloned()
            .unwrap_or_else(|| get_tasks_dir(&self.active_root))
    }

    pub(super) fn refresh_task_snapshot(&mut self) {
        let board_dir = self.board_dir();
        self.task_snapshot =
            TuiTaskSnapshot::load(&self.active_root, &board_dir, self.active_board);
    }

    pub(super) fn normalize_cached_task_selections(&mut self) {
        if !self.active_board {
            return;
        }

        if self.archive_view {
            normalize_cached_list_selection(
                &mut self.archive_state,
                self.task_snapshot.archived_entries.len(),
            );
        } else {
            for (state, entries) in self
                .board_states
                .iter_mut()
                .zip(self.task_snapshot.board_entries.iter())
            {
                normalize_cached_list_selection(state, entries.len());
            }
        }
    }

    pub(super) fn prepare_render(&mut self, size: Rect) {
        let input_height = tui_input_height(self, size.width);
        let (console_content, _) = tui_console_content(
            self.current_pane == TuiPane::AgentProjects,
            &self.agent_panel,
            self.agent_log_view.as_ref(),
            &self.feedback_buffer,
        );
        let console_height = tui_feedback_console_height(
            size.height,
            size.width,
            console_content,
            self.agent_log_view.is_some(),
        );
        let content_height = size
            .height
            .saturating_sub(input_height)
            .saturating_sub(console_height);

        if self.current_pane == TuiPane::AgentProjects {
            self.agent_panel
                .keep_selection_visible(content_height.saturating_sub(4) as usize);
        } else if self.current_pane == TuiPane::Models {
            self.models_panel.provider_viewport_height = content_height.saturating_sub(4) as usize;
            self.models_panel.model_viewport_height = content_height.saturating_sub(4) as usize;
        } else if self.archive_view {
            let display_tasks = self
                .task_snapshot
                .archived_entries
                .iter()
                .enumerate()
                .map(|(idx, entry)| {
                    format!(
                        "- {}",
                        task_tui_display_text(entry, Some(idx) == self.archive_state.selected())
                    )
                })
                .collect::<Vec<_>>();
            keep_selected_task_visible(
                &display_tasks,
                self.archive_state.selected(),
                &mut self.archive_scroll_offset,
                content_height.saturating_sub(2) as usize,
                size.width as usize,
            );
        } else {
            let visible_boards = visible_tui_board_indices(self.backlog_visible);
            let col_width = (size.width / visible_boards.len() as u16) as usize;
            for board_index in visible_boards {
                let selected = self.board_states[*board_index].selected();
                let display_tasks = self.task_snapshot.board_entries[*board_index]
                    .iter()
                    .enumerate()
                    .map(|(idx, entry)| {
                        format!(
                            "- {}",
                            task_tui_display_text_with_agent_flag(
                                entry,
                                TASK_STATUSES[*board_index],
                                Some(idx) == selected,
                                &self.task_agent_session_states,
                            )
                        )
                    })
                    .collect::<Vec<_>>();
                keep_selected_task_visible(
                    &display_tasks,
                    selected,
                    &mut self.board_scroll_offsets[*board_index],
                    content_height.saturating_sub(2) as usize,
                    col_width,
                );
            }
        }
    }
}

pub(super) fn normalize_cached_list_selection(state: &mut ListState, item_count: usize) {
    if item_count == 0 {
        state.select(None);
    } else if state
        .selected()
        .is_some_and(|selected| selected >= item_count)
    {
        state.select(Some(item_count - 1));
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TuiEffect {
    RefreshAgentPanel,
    RefreshModels,
    RefreshSelectedProviderModels,
    SyncAgentLog,
    SyncTaskLog,
    RefreshAgentLog,
    RefreshTaskSnapshot,
    RefreshClock,
    PaneKey(KeyEvent),
    Quit,
}

pub(super) fn update_tui_pane(app: &mut TuiApp, key: KeyEvent) -> Option<Vec<TuiEffect>> {
    if app.current_mode == Mode::Help {
        if matches!(
            key.code,
            KeyCode::Enter | KeyCode::Esc | KeyCode::Char('h' | 'H' | '?')
        ) {
            app.current_mode = Mode::View;
        }
        return Some(Vec::new());
    }
    if app.current_mode != Mode::View {
        return Some(vec![TuiEffect::PaneKey(key)]);
    }

    if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
        let previous_pane = app.current_pane;
        app.current_pane = if previous_pane == TuiPane::Models {
            app.models_return_pane
        } else {
            tui_pane_after_tab(previous_pane, app.active_board)
        };
        app.feedback_buffer = match app.current_pane {
            TuiPane::AgentProjects => tui_agent_panel_instructions(),
            TuiPane::Models => tui_models_instructions(),
            TuiPane::Tasks => tui_task_board_instructions(),
        }
        .to_string();
        if previous_pane == TuiPane::Tasks && app.current_pane == TuiPane::AgentProjects {
            app.agent_panel.select_project_for_path(&app.active_root);
        }
        return Some(match app.current_pane {
            TuiPane::AgentProjects => vec![TuiEffect::RefreshAgentPanel, TuiEffect::SyncAgentLog],
            TuiPane::Tasks => vec![TuiEffect::SyncTaskLog],
            TuiPane::Models => Vec::new(),
        });
    }

    if matches!(key.code, KeyCode::Char('h' | 'H' | '?')) {
        app.current_mode = Mode::Help;
        return Some(Vec::new());
    }
    if key.code == KeyCode::Char('q') {
        return Some(vec![TuiEffect::Quit]);
    }

    match app.current_pane {
        TuiPane::Models => update_tui_models_pane(app, key),
        TuiPane::AgentProjects => update_tui_agent_projects_pane(app, key),
        TuiPane::Tasks => update_tui_tasks_pane(app, key),
    }
}

pub(super) fn update_tui_models_pane(app: &mut TuiApp, key: KeyEvent) -> Option<Vec<TuiEffect>> {
    match key.code {
        _ if key.code == KeyCode::Esc || tui_toggles_models(&key) => {
            app.current_pane = app.models_return_pane;
            app.feedback_buffer = if app.current_pane == TuiPane::AgentProjects {
                tui_agent_panel_instructions()
            } else {
                tui_task_board_instructions()
            }
            .to_string();
            Some(Vec::new())
        }
        KeyCode::Left => {
            app.models_panel.focus = TuiModelsFocus::Providers;
            app.feedback_buffer = tui_models_instructions().to_string();
            Some(Vec::new())
        }
        KeyCode::Right => {
            app.models_panel.focus = TuiModelsFocus::Models;
            app.feedback_buffer = tui_models_instructions().to_string();
            Some(Vec::new())
        }
        KeyCode::Up
        | KeyCode::Down
        | KeyCode::Home
        | KeyCode::End
        | KeyCode::PageUp
        | KeyCode::PageDown => {
            let provider_changed = update_tui_models_selection(&mut app.models_panel, key.code);
            Some(if provider_changed {
                vec![TuiEffect::RefreshSelectedProviderModels]
            } else {
                Vec::new()
            })
        }
        _ => Some(vec![TuiEffect::PaneKey(key)]),
    }
}

pub(super) fn update_tui_models_selection(panel: &mut TuiModelsPanel, code: KeyCode) -> bool {
    match panel.focus {
        TuiModelsFocus::Providers => {
            let Some(last) = panel.providers.len().checked_sub(1) else {
                panel.provider_state.select(None);
                return false;
            };
            let selected = panel.provider_state.selected().unwrap_or(0).min(last);
            let next = match code {
                KeyCode::Up => selected.checked_sub(1).unwrap_or(last),
                KeyCode::Down => (selected + 1) % (last + 1),
                KeyCode::Home => 0,
                KeyCode::End => last,
                KeyCode::PageUp => selected.saturating_sub(panel.provider_viewport_height.max(1)),
                KeyCode::PageDown => selected
                    .saturating_add(panel.provider_viewport_height.max(1))
                    .min(last),
                _ => return false,
            };
            panel.provider_state.select(Some(next));
            next != selected
        }
        TuiModelsFocus::Models => {
            let visible = panel.visible_model_indices();
            let Some(last) = visible.len().checked_sub(1) else {
                panel.model_state.select(None);
                return false;
            };
            let selected_position = panel
                .model_state
                .selected()
                .and_then(|selected| visible.iter().position(|index| *index == selected))
                .unwrap_or(0)
                .min(last);
            let next_position = match code {
                KeyCode::Up => selected_position.checked_sub(1).unwrap_or(last),
                KeyCode::Down => (selected_position + 1) % (last + 1),
                KeyCode::Home => 0,
                KeyCode::End => last,
                KeyCode::PageUp => {
                    selected_position.saturating_sub(panel.model_viewport_height.max(1))
                }
                KeyCode::PageDown => selected_position
                    .saturating_add(panel.model_viewport_height.max(1))
                    .min(last),
                _ => return false,
            };
            panel.model_state.select(Some(visible[next_position]));
            false
        }
    }
}

pub(super) fn update_tui_agent_projects_pane(
    app: &mut TuiApp,
    key: KeyEvent,
) -> Option<Vec<TuiEffect>> {
    match key.code {
        KeyCode::Esc if app.active_board => {
            app.current_pane = TuiPane::Tasks;
            app.feedback_buffer = tui_task_board_instructions().to_string();
            Some(vec![TuiEffect::SyncTaskLog])
        }
        KeyCode::Esc => {
            app.feedback_buffer = TUI_NO_ACTIVE_BOARD_MESSAGE.to_string();
            Some(Vec::new())
        }
        _ if tui_toggles_models(&key) => {
            app.models_return_pane = tui_models_return_pane(app.current_pane);
            app.current_pane = TuiPane::Models;
            app.feedback_buffer = tui_models_instructions().to_string();
            Some(vec![TuiEffect::RefreshModels])
        }
        KeyCode::Up => {
            app.agent_panel.select_previous();
            Some(vec![TuiEffect::SyncAgentLog])
        }
        KeyCode::Down => {
            app.agent_panel.select_next();
            Some(vec![TuiEffect::SyncAgentLog])
        }
        _ => Some(vec![TuiEffect::PaneKey(key)]),
    }
}

pub(super) fn update_tui_tasks_pane(app: &mut TuiApp, key: KeyEvent) -> Option<Vec<TuiEffect>> {
    if app.archive_view {
        return match key.code {
            KeyCode::Char('A') => {
                app.archive_view = false;
                app.archive_state.select(None);
                app.archive_scroll_offset = 0;
                app.feedback_buffer = "Returned to Kanban view".to_string();
                Some(Vec::new())
            }
            KeyCode::Up | KeyCode::Down => {
                move_cached_list_selection(
                    &mut app.archive_state,
                    app.task_snapshot.archived_entries.len(),
                    key.code == KeyCode::Down,
                );
                Some(Vec::new())
            }
            _ => Some(vec![TuiEffect::PaneKey(key)]),
        };
    }

    if !tui_toggles_models(&key)
        && key
            .modifiers
            .intersects(KeyModifiers::SHIFT | KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return Some(vec![TuiEffect::PaneKey(key)]);
    }

    match key.code {
        _ if tui_toggles_models(&key) => {
            app.models_return_pane = tui_models_return_pane(app.current_pane);
            app.current_pane = TuiPane::Models;
            app.feedback_buffer = tui_models_instructions().to_string();
            Some(vec![TuiEffect::RefreshModels])
        }
        KeyCode::Esc if app.agent_log_view.is_none() => {
            app.board_states[app.selected_board].select(None);
            app.feedback_buffer = "Task unselected".to_string();
            Some(vec![TuiEffect::SyncTaskLog])
        }
        KeyCode::Up | KeyCode::Down => {
            move_cached_list_selection(
                &mut app.board_states[app.selected_board],
                app.task_snapshot.board_entries[app.selected_board].len(),
                key.code == KeyCode::Down,
            );
            Some(vec![TuiEffect::SyncTaskLog])
        }
        KeyCode::Left | KeyCode::Right => {
            app.selected_board = wrapped_visible_tui_board(
                app.selected_board,
                app.backlog_visible,
                if key.code == KeyCode::Left { -1 } else { 1 },
            );
            for state in &mut app.board_states {
                state.select(None);
            }
            normalize_cached_list_selection(
                &mut app.board_states[app.selected_board],
                app.task_snapshot.board_entries[app.selected_board].len(),
            );
            if !app.task_snapshot.board_entries[app.selected_board].is_empty() {
                app.board_states[app.selected_board].select(Some(0));
            }
            Some(vec![TuiEffect::SyncTaskLog])
        }
        KeyCode::Char(digit @ '0'..='3') => {
            app.selected_board = match digit {
                '0' => {
                    app.backlog_visible = true;
                    BACKLOG_BOARD_INDEX
                }
                '1' => TODO_BOARD_INDEX,
                '2' => 1,
                '3' => DONE_BOARD_INDEX,
                _ => unreachable!(),
            };
            for state in &mut app.board_states {
                state.select(None);
            }
            if !app.task_snapshot.board_entries[app.selected_board].is_empty() {
                app.board_states[app.selected_board].select(Some(0));
            }
            if digit == '0' {
                app.feedback_buffer = "Backlog column shown and focused.".to_string();
            }
            Some(vec![TuiEffect::SyncTaskLog])
        }
        _ => Some(vec![TuiEffect::PaneKey(key)]),
    }
}

pub(super) fn move_cached_list_selection(state: &mut ListState, item_count: usize, forward: bool) {
    if item_count == 0 {
        state.select(None);
        return;
    }
    let selected = state.selected().unwrap_or(0).min(item_count - 1);
    let next = if forward {
        (selected + 1) % item_count
    } else {
        selected.checked_sub(1).unwrap_or(item_count - 1)
    };
    state.select(Some(next));
}

pub(super) fn tui_input_height(app: &TuiApp, width: u16) -> u16 {
    if app.model_input.is_some() {
        return 3;
    }
    if !matches!(app.current_mode, Mode::Input | Mode::Edit) {
        return 0;
    }

    let label = if app.current_mode == Mode::Input {
        if app.subtask_parent.is_some() {
            " Add Subtask: "
        } else {
            " Add Task: "
        }
    } else {
        " Edit Task: "
    };
    let display_value = app.task_input.display_value();
    let full_text = format!("{label}{display_value}");
    let available_width = width.saturating_sub(2) as usize;
    let wrapped = wrap_input_text(&full_text, available_width);
    let lines = wrapped.lines().count();
    let cursor_idx =
        label.len() + byte_index_at_char(&display_value, app.task_input.display_cursor());
    let cursor_row = input_cursor_offset_at(&full_text, available_width, cursor_idx).1 as usize;
    (lines.max(cursor_row + 1) + 2).max(3) as u16
}

pub(super) fn execute_tui_effect(
    app: &mut TuiApp,
    effect: TuiEffect,
    agent_panel_refresh: &mut TuiAgentPanelRefreshWorker,
    last_agent_panel_refresh: &mut Instant,
    last_agent_log_refresh: &mut Instant,
) -> bool {
    match effect {
        TuiEffect::RefreshAgentPanel => {
            if agent_panel_refresh.request(&app.active_root) {
                *last_agent_panel_refresh = Instant::now();
            }
        }
        TuiEffect::RefreshModels => {
            app.models_panel.refresh();
            app.provider_env_statuses = app
                .models_panel
                .providers
                .iter()
                .map(|provider| (provider.id.clone(), provider_env_status(provider)))
                .collect();
        }
        TuiEffect::RefreshSelectedProviderModels => app.models_panel.refresh_models(),
        TuiEffect::SyncAgentLog => {
            sync_open_tui_agent_log_view(&app.agent_panel, &mut app.agent_log_view);
            *last_agent_log_refresh = Instant::now();
        }
        TuiEffect::SyncTaskLog => {
            let selected_task = app.board_states[app.selected_board]
                .selected()
                .and_then(|idx| app.task_snapshot.board_entries[app.selected_board].get(idx));
            sync_open_tui_task_log_view(
                &mut app.agent_panel,
                &app.active_root,
                TASK_STATUSES[app.selected_board],
                selected_task,
                &mut app.agent_log_view,
            );
            *last_agent_log_refresh = Instant::now();
        }
        TuiEffect::RefreshAgentLog => {
            if let Some(log_view) = app.agent_log_view.as_mut()
                && let Err(err) = log_view.refresh()
            {
                log_view.content = format!("Error refreshing agent output: {err}");
            }
            *last_agent_log_refresh = Instant::now();
        }
        TuiEffect::RefreshTaskSnapshot => app.refresh_task_snapshot(),
        TuiEffect::RefreshClock => app.current_time = Local::now().format("%H:%M").to_string(),
        TuiEffect::PaneKey(_) => unreachable!("pane keys require the terminal effect executor"),
        TuiEffect::Quit => return true,
    }
    false
}

pub(super) fn render_tui(f: &mut ratatui::Frame<'_>, app: &TuiApp) {
    let statuses = TASK_STATUSES;
    let titles = ["To Do", "Doing", "Done", "Backlog"];
    let text_color = Color::Indexed(248);
    let c_highlight = Color::Indexed(221);
    let colors = [
        Color::Indexed(110),
        Color::Indexed(108),
        Color::Indexed(139),
        Color::Indexed(244),
    ];
    let size = f.area();
    let board_title = &app.task_snapshot.board_title;
    let (console_title, console_right_title) = if let Some(log_view) = app.agent_log_view.as_ref() {
        (tui_agent_log_title(log_view), None)
    } else if app.current_pane == TuiPane::AgentProjects {
        ("Agent Projects Console".to_string(), None)
    } else if app.current_pane == TuiPane::Models {
        ("Models Console".to_string(), None)
    } else if app.archive_view {
        (format!("{board_title} Archive Console"), None)
    } else if !app.backlog_visible {
        let backlog_count = app.task_snapshot.board_entries[BACKLOG_BOARD_INDEX].len();
        (
            format!("{board_title} Console"),
            Some(format!(" Backlog: {backlog_count} [B] ")),
        )
    } else {
        (format!("{board_title} Console"), None)
    };

    let input_height = tui_input_height(app, size.width);

    let console_height = {
        let (console_content, _) = tui_console_content(
            app.current_pane == TuiPane::AgentProjects,
            &app.agent_panel,
            app.agent_log_view.as_ref(),
            app.feedback_buffer.as_str(),
        );
        tui_feedback_console_height(
            size.height,
            size.width,
            console_content,
            app.agent_log_view.is_some(),
        )
    };

    // Main layout: active pane, input area (if active), and feedback console
    let main_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(input_height),
            Constraint::Length(console_height),
        ])
        .split(size);

    let content_area = main_layout[0];
    if app.current_pane == TuiPane::AgentProjects {
        render_tui_agent_panel(
            f,
            content_area,
            &app.agent_panel,
            &app.active_root,
            text_color,
            c_highlight,
            &app.current_time,
        );
    } else if app.current_pane == TuiPane::Models {
        render_tui_models_panel(
            f,
            content_area,
            &app.models_panel,
            text_color,
            c_highlight,
            &app.provider_env_statuses,
        );
    } else if app.archive_view {
        let selected_idx = app.archive_state.selected();
        let col_width = content_area.width as usize;
        let entries = &app.task_snapshot.archived_entries;
        let display_tasks: Vec<String> = entries
            .iter()
            .enumerate()
            .map(|(idx, entry)| {
                format!(
                    "- {}",
                    task_tui_display_text(entry, Some(idx) == selected_idx)
                )
            })
            .collect();
        let highlight_style = Style::default().fg(Color::Black).bg(c_highlight);
        let block = Block::default()
            .title(" Archived - press A again to leave ")
            .title(
                Line::from(vec![Span::raw(format!(" {} ", entries.len()))])
                    .alignment(Alignment::Right),
            )
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Indexed(244)));

        let inner_area = block.inner(content_area);

        let mut current_y = 0;
        for (idx, (t, entry)) in display_tasks
            .iter()
            .zip(entries.iter())
            .enumerate()
            .skip(app.archive_scroll_offset)
        {
            let cleaned = t.strip_prefix("- ").unwrap_or(t);
            let is_selected = Some(idx) == selected_idx;
            let mut wrapped_content = if is_selected {
                wrap_text(cleaned, col_width.saturating_sub(5))
            } else {
                cleaned.to_string()
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
            let style = if is_selected {
                highlight_style
            } else {
                Style::default().fg(text_color)
            };
            let item_text = format!("{}. {}", idx + 1, wrapped_content);
            f.render_widget(Paragraph::new(item_text).style(style), item_area);

            current_y += line_count;
            if inner_area.y + current_y as u16 >= content_area.height {
                break;
            }
        }

        f.render_widget(block, content_area);
    } else {
        let visible_boards = visible_tui_board_indices(app.backlog_visible);
        let column_count = visible_boards.len();
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(vec![
                Constraint::Ratio(1, column_count as u32);
                column_count
            ])
            .split(content_area);

        for (column_index, board_index) in visible_boards.iter().copied().enumerate() {
            let status = statuses[board_index];
            let selected_idx = app.board_states[board_index].selected();
            let col_width = (size.width / column_count as u16) as usize;
            let entries = &app.task_snapshot.board_entries[board_index];
            let tasks: Vec<String> = entries
                .iter()
                .map(|entry| {
                    format!(
                        "- {}",
                        task_display_text_with_agent_flag(
                            entry,
                            status,
                            &app.task_agent_session_states,
                        )
                    )
                })
                .collect();
            let display_tasks: Vec<String> = entries
                .iter()
                .enumerate()
                .map(|(idx, entry)| {
                    format!(
                        "- {}",
                        task_tui_display_text_with_agent_flag(
                            entry,
                            status,
                            Some(idx) == selected_idx,
                            &app.task_agent_session_states,
                        )
                    )
                })
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

            let task_focus_active = matches!(app.current_mode, Mode::View | Mode::Reorganize)
                && app.current_pane == TuiPane::Tasks;
            let highlight_style = if task_focus_active {
                Style::default().fg(Color::Black).bg(c_highlight)
            } else {
                // Use a more subtle highlight when in Input/Edit mode
                Style::default().fg(Color::White).bg(Color::DarkGray)
            };

            let reorganizing = matches!(app.current_mode, Mode::Reorganize);
            let task_list_inner = render_tui_task_column_header(
                f,
                chunks[column_index],
                titles[board_index],
                tasks.len(),
                app.selected_board == board_index,
                reorganizing,
                colors[board_index],
            );
            let mut current_y = 0;
            for (idx, (t, entry)) in display_tasks
                .iter()
                .zip(entries.iter())
                .enumerate()
                .skip(app.board_scroll_offsets[board_index])
            {
                let cleaned = t.strip_prefix("- ").unwrap_or(t);
                let is_selected = Some(idx) == selected_idx;

                let style = if is_selected {
                    highlight_style
                } else {
                    Style::default().fg(text_color)
                };

                let mut wrapped_content = if is_selected {
                    wrap_text(cleaned, col_width.saturating_sub(5))
                } else {
                    cleaned.to_string()
                };
                if entry.has_subtasks {
                    wrapped_content.push_str(" >");
                }

                let line_count = wrapped_content.lines().count().max(1);
                if current_y >= task_list_inner.height as usize {
                    break;
                }

                let visible_height = (line_count as u16)
                    .min(task_list_inner.height.saturating_sub(current_y as u16));
                let item_area = ratatui::layout::Rect {
                    x: task_list_inner.x,
                    y: task_list_inner.y + current_y as u16,
                    width: task_list_inner.width,
                    height: visible_height,
                };

                let item_text = format!("{}. {}", idx + 1, wrapped_content);
                f.render_widget(Paragraph::new(item_text).style(style), item_area);

                current_y += line_count;
                if current_y >= task_list_inner.height as usize {
                    break;
                }
            }
        }
    }

    if let Some(model_input) = app.model_input.as_ref() {
        let label = model_input.label();
        let input_text = format!("{}{}", label, model_input.input.value());
        let input_paragraph = Paragraph::new(input_text.as_str())
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Models Input (Enter advances/saves, Esc cancels)"),
            )
            .style(Style::default().fg(Color::White));
        f.render_widget(input_paragraph, main_layout[1]);
        let cursor = label.chars().count() + model_input.input.visual_cursor();
        let input_inner = main_layout[1].inner(ratatui::layout::Margin {
            horizontal: 1,
            vertical: 1,
        });
        f.set_cursor_position(Position::new(
            input_inner.x + (cursor as u16).min(input_inner.width.saturating_sub(1)),
            input_inner.y,
        ));
    } else if matches!(app.current_mode, Mode::Input) || matches!(app.current_mode, Mode::Edit) {
        let label = if matches!(app.current_mode, Mode::Input) {
            if app.subtask_parent.is_some() {
                " Add Subtask: "
            } else {
                " Add Task: "
            }
        } else {
            " Edit Task: "
        };
        let display_value = app.task_input.display_value();
        let input_text = format!("{}{}", label, display_value);
        // Subtract 2 for the borders of the block
        let available_width = size.width.saturating_sub(2) as usize;
        let input_lines = styled_task_input_lines(label, &app.task_input, available_width);
        let input_paragraph = Paragraph::new(input_lines)
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
            label.len() + byte_index_at_char(&display_value, app.task_input.display_cursor()),
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

    let (console_content, console_color) = tui_console_content(
        app.current_pane == TuiPane::AgentProjects,
        &app.agent_panel,
        app.agent_log_view.as_ref(),
        app.feedback_buffer.as_str(),
    );
    let feedback_area = *main_layout.last().unwrap();
    let wrapped_console_content = wrap_input_text(
        console_content,
        feedback_area.width.saturating_sub(2) as usize,
    );
    let mut console_block =
        tui_console_block(console_title.as_str(), console_right_title.as_deref());
    if let Some(log_view) = app
        .agent_log_view
        .as_ref()
        .filter(|view| view.path.is_some())
    {
        console_block = console_block.title_bottom(log_view.settings_label());
    }
    let feedback_paragraph = Paragraph::new(wrapped_console_content.as_str())
        .block(console_block)
        .style(Style::default().fg(console_color))
        .scroll((
            tui_log_scroll_offset(
                &wrapped_console_content,
                feedback_area.height.saturating_sub(2),
            ),
            0,
        ));

    // The feedback area is always the last element of main_layout
    f.render_widget(feedback_paragraph, feedback_area);

    if matches!(app.current_mode, Mode::Help) {
        let help_text = "TUI Commands:\n\n\
                                 [Space]        - Create new task / toggle selected agent project\n\
                                 [n/+]          - Create subtask under selected task\n\
                                 [Enter]        - Open subtasks, edit selected task, or open selected agent project\n\
                                 [e]            - Edit selected task\n\
                                 [g]            - Cycle selected project's Git mode: off/commit/push\n\
                                 [s]            - Stop/resume linked task or displayed Agent Output session\n\
                                 [i]            - Take over linked/displayed live session, then auto-restart exec\n\
                                 [c]            - Open linked idle Doing, Done/blocked, or displayed session\n\
                                 [l]            - Toggle active/selected project's live/current agent output\n\
                                 [a]            - Move selected task to archive\n\
                                 [A]            - Toggle archive view\n\
                                 [b]            - Move selected task to backlog\n\
                                 [B]            - Show/hide backlog column\n\
                                 [Backspace]    - Return to parent board\n\
                                 [d/Del]        - Delete selected task\n\
                                 [Agent Del]    - Remove selected project after confirmation\n\
                                 [Tab]          - Toggle task board and agent projects\n\
                                 [Agent m]      - Cycle selected target\n\
                                 [M]            - Open Models from Tasks or Agent Projects\n\
                                 [Models n/r/a] - Add provider / discover models / manually add ID\n\
                                 [Models /]     - Search model names and IDs\n\
                                 [Models x/Del] - Remove selected non-built-in provider\n\
                                 [Models d/c]   - Set CLT default / explicitly set Codex default\n\
                                 [Arrows]       - Navigate boards, tasks, providers, and models\n\
                                 [PgUp/Dn Home/End] - Jump through Models lists\n\
                                 [r]            - Toggle sticky Reorganize mode (r/Esc exits)\n\
                                 [Shift+Arrows] - Reorder/Move tasks\n\
                                 [Ctrl-P/N]     - Reorder task Up/Down\n\
                                 [0, 1, 2, 3]   - Focus Backlog/To Do/Doing/Done\n\
                                 [Input Arrows]         - Move cursor in wrapped input\n\
                                 [Ctrl/Alt+Left/Right]  - Jump input cursor by word\n\
                                 [Ctrl+A/E/W/U/K]       - Edit input line\n\
                                 [h / ?]        - Toggle Help\n\
                                 [q]            - Quit";

        let area = f.area();
        let popover_width = area.width.min(70);
        let popover_height = area.height.min(27);
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
}

pub(super) fn execute_tui_key_effect(
    app: &mut TuiApp,
    key: KeyEvent,
    terminal: &mut TuiTerminal,
    terminal_session: &mut TerminalSession,
    agent_panel_refresh: &mut TuiAgentPanelRefreshWorker,
    last_agent_panel_refresh: &mut Instant,
    last_agent_log_refresh: &mut Instant,
) -> Result<bool> {
    let board_dir = app.board_dir();
    let statuses = TASK_STATUSES;
    let input_available_width = terminal.size()?.width.saturating_sub(2) as usize;
    if let Some(removal) = app.pending_agent_project_removal.take() {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                app.feedback_buffer = match remove_tui_agent_project(
                    &mut app.agent_panel,
                    &app.active_root,
                    &removal,
                ) {
                    Ok(message) => {
                        *last_agent_panel_refresh = Instant::now();
                        message
                    }
                    Err(error) => format!("Error: {error}"),
                };
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                app.feedback_buffer = format!("Kept agent project: {}", removal.name);
            }
            _ => {
                app.feedback_buffer = tui_agent_project_removal_prompt(&removal);
                app.pending_agent_project_removal = Some(removal);
            }
        }
        return Ok(false);
    }
    if app.awaiting_model_provider_choice {
        match key.code {
            KeyCode::Esc => {
                app.awaiting_model_provider_choice = false;
                app.feedback_buffer = "Provider selection cancelled".to_string();
            }
            KeyCode::Char(digit @ '1'..='4') => {
                app.awaiting_model_provider_choice = false;
                let index = digit as usize - '1' as usize;
                app.feedback_buffer =
                    match add_tui_model_provider_preset(&mut app.models_panel, index) {
                        Ok(message) => message,
                        Err(error) => format!("Error: {error}"),
                    };
            }
            KeyCode::Char('5') => {
                app.awaiting_model_provider_choice = false;
                app.model_input = Some(TuiModelInput::custom_provider());
                app.feedback_buffer = app
                    .model_input
                    .as_ref()
                    .expect("custom provider input was just created")
                    .guidance()
                    .to_string();
            }
            _ => {
                app.feedback_buffer = tui_models_provider_choice_prompt().to_string();
            }
        }
        return Ok(false);
    }
    if let Some(input) = app.model_input.as_mut() {
        match key.code {
            KeyCode::Esc => {
                app.model_input = None;
                app.feedback_buffer = "Models input cancelled".to_string();
            }
            KeyCode::Enter => match submit_tui_model_input(input, &mut app.models_panel) {
                Ok(Some(message)) => {
                    app.model_input = None;
                    app.feedback_buffer = message;
                }
                Ok(None) => {
                    app.feedback_buffer = input.guidance().to_string();
                }
                Err(error) => app.feedback_buffer = format!("Error: {error}"),
            },
            _ => {
                let label = input.label();
                handle_input_key(&mut input.input, key, label, input_available_width)
            }
        }
        return Ok(false);
    }
    if app.current_mode == Mode::View
        && key.code == KeyCode::Esc
        && app.agent_log_view.take().is_some()
    {
        app.feedback_buffer = "Closed agent output log".to_string();
        return Ok(false);
    }
    if let Some(effects) = update_tui_pane(app, key) {
        let mut execute_pane_key = false;
        for effect in effects {
            if matches!(effect, TuiEffect::PaneKey(_)) {
                execute_pane_key = true;
            } else if execute_tui_effect(
                app,
                effect,
                agent_panel_refresh,
                last_agent_panel_refresh,
                last_agent_log_refresh,
            ) {
                return Ok(true);
            }
        }
        if !execute_pane_key {
            return Ok(false);
        }
    }
    match app.current_mode {
        Mode::View => {
            if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
                let previous_pane = app.current_pane;
                app.current_pane = if previous_pane == TuiPane::Models {
                    app.models_return_pane
                } else {
                    tui_pane_after_tab(app.current_pane, app.active_board)
                };
                if app.current_pane == TuiPane::AgentProjects {
                    if previous_pane == TuiPane::Tasks {
                        app.agent_panel.select_project_for_path(&app.active_root);
                    }
                    sync_open_tui_agent_log_view(&app.agent_panel, &mut app.agent_log_view);
                    if agent_panel_refresh.request(&app.active_root) {
                        *last_agent_panel_refresh = Instant::now();
                    }
                } else if app.current_pane == TuiPane::Tasks {
                    let selected_task = selected_task_entry_in_board(
                        &board_dir,
                        statuses[app.selected_board],
                        &app.board_states[app.selected_board],
                    )
                    .map(|(_, task)| task);
                    sync_open_tui_task_log_view(
                        &mut app.agent_panel,
                        &app.active_root,
                        statuses[app.selected_board],
                        selected_task.as_ref(),
                        &mut app.agent_log_view,
                    );
                    *last_agent_log_refresh = Instant::now();
                }
                app.feedback_buffer = if app.current_pane == TuiPane::AgentProjects {
                    tui_agent_panel_instructions().to_string()
                } else {
                    tui_task_board_instructions().to_string()
                };
            } else if matches!(key.code, KeyCode::Esc) && app.agent_log_view.take().is_some() {
                app.feedback_buffer = "Closed agent output log".to_string();
            } else if app.current_pane == TuiPane::Models {
                match key.code {
                    _ if key.code == KeyCode::Esc || tui_toggles_models(&key) => {
                        app.current_pane = app.models_return_pane;
                        app.feedback_buffer = if app.current_pane == TuiPane::AgentProjects {
                            tui_agent_panel_instructions().to_string()
                        } else {
                            tui_task_board_instructions().to_string()
                        };
                    }
                    KeyCode::Char('q') => return Ok(true),
                    KeyCode::Char('h') | KeyCode::Char('H') | KeyCode::Char('?') => {
                        app.current_mode = Mode::Help
                    }
                    KeyCode::Left => {
                        app.models_panel.focus = TuiModelsFocus::Providers;
                        app.feedback_buffer = tui_models_instructions().to_string();
                    }
                    KeyCode::Right => {
                        app.models_panel.focus = TuiModelsFocus::Models;
                        app.feedback_buffer = tui_models_instructions().to_string();
                    }
                    KeyCode::Up => app.models_panel.select_previous(),
                    KeyCode::Down => app.models_panel.select_next(),
                    KeyCode::PageUp => app.models_panel.select_page_up(),
                    KeyCode::PageDown => app.models_panel.select_page_down(),
                    KeyCode::Home => app.models_panel.select_first(),
                    KeyCode::End => app.models_panel.select_last(),
                    KeyCode::Char('/') => {
                        app.models_panel.focus = TuiModelsFocus::Models;
                        app.model_input = Some(TuiModelInput::search_models(
                            app.models_panel.model_search.clone(),
                        ));
                        app.feedback_buffer = app
                            .model_input
                            .as_ref()
                            .expect("model search input was just created")
                            .guidance()
                            .to_string();
                    }
                    KeyCode::Char(' ') => {
                        app.feedback_buffer = match toggle_tui_models_enabled(&mut app.models_panel)
                        {
                            Ok(message) => message,
                            Err(error) => format!("Error: {error}"),
                        };
                    }
                    KeyCode::Char('x') | KeyCode::Char('X') | KeyCode::Delete => {
                        app.feedback_buffer = match remove_tui_model_provider(&mut app.models_panel)
                        {
                            Ok(message) => message,
                            Err(error) => format!("Error: {error}"),
                        };
                    }
                    KeyCode::Char('f') | KeyCode::Char('F') => {
                        app.models_panel.focus = TuiModelsFocus::Models;
                        app.feedback_buffer = match toggle_tui_model_favorite(&mut app.models_panel)
                        {
                            Ok(message) => message,
                            Err(error) => format!("Error: {error}"),
                        };
                    }
                    KeyCode::Char('t') | KeyCode::Char('T') => {
                        app.models_panel.focus = TuiModelsFocus::Models;
                        app.feedback_buffer = match cycle_tui_model_reasoning(&mut app.models_panel)
                        {
                            Ok(message) => message,
                            Err(error) => format!("Error: {error}"),
                        };
                    }
                    KeyCode::Char('d') | KeyCode::Char('D') => {
                        app.models_panel.focus = TuiModelsFocus::Models;
                        app.feedback_buffer = match set_tui_model_default(&mut app.models_panel) {
                            Ok(message) => message,
                            Err(error) => format!("Error: {error}"),
                        };
                    }
                    KeyCode::Char('c') | KeyCode::Char('C') => {
                        app.models_panel.focus = TuiModelsFocus::Models;
                        app.feedback_buffer = match set_tui_codex_default(&mut app.models_panel) {
                            Ok(message) => message,
                            Err(error) => format!("Error: {error}"),
                        };
                    }
                    KeyCode::Char('a') | KeyCode::Char('A') => {
                        if let Some(provider) = app.models_panel.selected_provider() {
                            app.model_input = Some(TuiModelInput::add_model(provider.id.clone()));
                            app.feedback_buffer = format!("Add a model ID for {}", provider.name);
                        } else {
                            app.feedback_buffer =
                                "Add a provider before adding a model".to_string();
                        }
                    }
                    KeyCode::Char('r') | KeyCode::Char('R') => {
                        app.feedback_buffer =
                            match discover_tui_provider_models(&mut app.models_panel) {
                                Ok(message) => message,
                                Err(error) => {
                                    format!("Model discovery failed: {error}")
                                }
                            };
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') => {
                        app.awaiting_model_provider_choice = true;
                        app.feedback_buffer = tui_models_provider_choice_prompt().to_string();
                    }
                    _ => app.feedback_buffer = tui_models_instructions().to_string(),
                }
            } else if app.current_pane == TuiPane::AgentProjects {
                match key.code {
                    KeyCode::Esc => {
                        if app.active_board {
                            app.current_pane = TuiPane::Tasks;
                            app.feedback_buffer = tui_task_board_instructions().to_string();
                        } else {
                            app.feedback_buffer = TUI_NO_ACTIVE_BOARD_MESSAGE.to_string();
                        }
                    }
                    KeyCode::Char('q') => return Ok(true),
                    KeyCode::Char('h') | KeyCode::Char('H') | KeyCode::Char('?') => {
                        app.current_mode = Mode::Help;
                    }
                    KeyCode::Delete => {
                        if let Some(removal) = selected_tui_agent_project_removal(&app.agent_panel)
                        {
                            app.agent_log_view = None;
                            app.feedback_buffer = tui_agent_project_removal_prompt(&removal);
                            app.pending_agent_project_removal = Some(removal);
                        } else {
                            app.feedback_buffer =
                                "No registered project selected to remove".to_string();
                        }
                    }
                    KeyCode::Enter => {
                        if app
                            .agent_panel
                            .selected_current_project_registration()
                            .is_some()
                        {
                            match register_selected_current_project(
                                &mut app.agent_panel,
                                &app.active_root,
                            ) {
                                Ok(message) => {
                                    *last_agent_panel_refresh = Instant::now();
                                    app.feedback_buffer = message;
                                }
                                Err(e) => app.feedback_buffer = format!("Error: {}", e),
                            }
                            return Ok(false);
                        }

                        let Some(project) = app
                            .agent_panel
                            .selected_project()
                            .map(|entry| entry.project.clone())
                        else {
                            app.feedback_buffer = "No registered project selected".to_string();
                            return Ok(false);
                        };

                        match ensure_existing_board(&project.path) {
                            Ok(true) => {}
                            Ok(false) => {
                                app.feedback_buffer = format!(
                                    "Project is not initialized: {}",
                                    project.path.display()
                                );
                                return Ok(false);
                            }
                            Err(error) => {
                                app.feedback_buffer = format!(
                                    "Failed to repair project board {}: {}",
                                    project.path.display(),
                                    error
                                );
                                return Ok(false);
                            }
                        }

                        match std::env::set_current_dir(&project.path) {
                            Ok(_) => {
                                app.active_root = project.path.clone();
                                app.task_agent_session_states.clear();
                                if agent_panel_refresh.request(&app.active_root) {
                                    *last_agent_panel_refresh = Instant::now();
                                } else {
                                    *last_agent_panel_refresh = Instant::now()
                                        .checked_sub(tui_agent_panel_refresh_interval())
                                        .unwrap_or_else(Instant::now);
                                }
                                app.active_board = true;
                                app.board_stack.clear();
                                app.board_stack.push(get_tasks_dir(&app.active_root));
                                app.selected_board = TODO_BOARD_INDEX;
                                for state in app.board_states.iter_mut() {
                                    state.select(None);
                                }
                                app.board_scroll_offsets = [0usize; 4];
                                app.archive_state.select(None);
                                app.archive_scroll_offset = 0;
                                app.archive_view = false;
                                app.current_pane = TuiPane::Tasks;
                                let board_dir = get_tasks_dir(&app.active_root);
                                select_first_task_if_present_in_board(
                                    &board_dir,
                                    statuses[app.selected_board],
                                    &mut app.board_states[app.selected_board],
                                );
                                app.feedback_buffer = match set_terminal_title(&app_title(
                                    &app.active_root,
                                )) {
                                    Ok(_) => {
                                        format!("Opened project board: {}", project.name)
                                    }
                                    Err(err) => {
                                        format!(
                                            "Opened project board: {}; failed to update title: {}",
                                            project.name, err
                                        )
                                    }
                                };
                            }
                            Err(err) => {
                                app.feedback_buffer = format!(
                                    "Failed to switch to {}: {}",
                                    project.path.display(),
                                    err
                                );
                            }
                        }
                    }
                    KeyCode::Char(' ') => {
                        if app
                            .agent_panel
                            .selected_current_project_registration()
                            .is_some()
                        {
                            match register_selected_current_project(
                                &mut app.agent_panel,
                                &app.active_root,
                            ) {
                                Ok(message) => {
                                    *last_agent_panel_refresh = Instant::now();
                                    app.feedback_buffer = message;
                                }
                                Err(e) => app.feedback_buffer = format!("Error: {}", e),
                            }
                            return Ok(false);
                        }

                        match toggle_selected_tui_agent_project(
                            &mut app.agent_panel,
                            &app.active_root,
                        ) {
                            Ok(message) => {
                                *last_agent_panel_refresh = Instant::now();
                                app.feedback_buffer = message;
                            }
                            Err(e) => app.feedback_buffer = format!("Error: {}", e),
                        }
                    }
                    KeyCode::Char('g') | KeyCode::Char('G') => {
                        if app
                            .agent_panel
                            .selected_current_project_registration()
                            .is_some()
                        {
                            app.feedback_buffer =
                                "Register current project before changing its Git mode".to_string();
                            return Ok(false);
                        }

                        match cycle_selected_tui_agent_project_git_mode(
                            &mut app.agent_panel,
                            &app.active_root,
                        ) {
                            Ok(message) => {
                                *last_agent_panel_refresh = Instant::now();
                                app.feedback_buffer = message;
                            }
                            Err(e) => app.feedback_buffer = format!("Error: {}", e),
                        }
                    }
                    _ if tui_toggles_models(&key) => {
                        app.models_return_pane = tui_models_return_pane(app.current_pane);
                        app.models_panel.refresh();
                        app.current_pane = TuiPane::Models;
                        app.feedback_buffer = tui_models_instructions().to_string();
                    }
                    KeyCode::Char('m') => {
                        if app
                            .agent_panel
                            .selected_current_project_registration()
                            .is_some()
                        {
                            app.feedback_buffer =
                                "Register current project before changing its Codex model"
                                    .to_string();
                            return Ok(false);
                        }

                        match cycle_selected_tui_agent_codex_model(
                            &mut app.agent_panel,
                            &app.active_root,
                        ) {
                            Ok(message) => {
                                *last_agent_panel_refresh = Instant::now();
                                app.feedback_buffer = message;
                            }
                            Err(e) => app.feedback_buffer = format!("Error: {}", e),
                        }
                    }
                    KeyCode::Char('f') | KeyCode::Char('F') => {
                        if app
                            .agent_panel
                            .selected_current_project_registration()
                            .is_some()
                        {
                            app.feedback_buffer =
                                "Register current project before changing Codex fast mode"
                                    .to_string();
                            return Ok(false);
                        }

                        match toggle_selected_tui_agent_codex_fast(
                            &mut app.agent_panel,
                            &app.active_root,
                        ) {
                            Ok(message) => {
                                *last_agent_panel_refresh = Instant::now();
                                app.feedback_buffer = message;
                            }
                            Err(e) => app.feedback_buffer = format!("Error: {}", e),
                        }
                    }
                    KeyCode::Char('t') | KeyCode::Char('T') => {
                        if app
                            .agent_panel
                            .selected_current_project_registration()
                            .is_some()
                        {
                            app.feedback_buffer =
                                "Register current project before changing Codex thinking"
                                    .to_string();
                            return Ok(false);
                        }

                        match cycle_selected_tui_agent_codex_reasoning(
                            &mut app.agent_panel,
                            &app.active_root,
                        ) {
                            Ok(message) => {
                                *last_agent_panel_refresh = Instant::now();
                                app.feedback_buffer = message;
                            }
                            Err(e) => app.feedback_buffer = format!("Error: {}", e),
                        }
                    }
                    KeyCode::Char('r') | KeyCode::Char('R') => {
                        app.feedback_buffer = match retry_selected_tui_agent_project(
                            &mut app.agent_panel,
                            &app.active_root,
                        ) {
                            Ok(message) => message,
                            Err(error) => {
                                format!("Unable to retry selected project: {error}")
                            }
                        };
                        *last_agent_panel_refresh = Instant::now();
                    }
                    KeyCode::Char('s') => {
                        let target_result = if app.agent_log_view.is_some() {
                            viewed_tui_codex_session_target(app.agent_log_view.as_ref())
                        } else {
                            selected_tui_agent_session_target(&app.agent_panel)
                        };
                        let target = match target_result {
                            Ok(target) => target,
                            Err(error) => {
                                app.feedback_buffer = error.to_string();
                                return Ok(false);
                            }
                        };
                        app.feedback_buffer = match toggle_tui_codex_session_stop(
                            target.project_id,
                            &target.session_id,
                        ) {
                            Ok(message) => message,
                            Err(error) => format!(
                                "Unable to stop or resume the displayed Codex session: {error}"
                            ),
                        };
                        app.agent_panel.refresh(&app.active_root);
                        *last_agent_panel_refresh = Instant::now();
                    }
                    KeyCode::Char('i') => {
                        let label = app
                            .agent_log_view
                            .as_ref()
                            .map(|view| view.project_name.clone())
                            .unwrap_or_else(|| "the selected project".to_string());
                        let target =
                            match viewed_tui_codex_session_target(app.agent_log_view.as_ref()) {
                                Ok(target) => target,
                                Err(error) => {
                                    app.feedback_buffer = error.to_string();
                                    return Ok(false);
                                }
                            };
                        match run_tui_codex_session_interrupt(
                            terminal,
                            terminal_session,
                            &app_title(&app.active_root),
                            &target,
                            &label,
                        ) {
                            Ok(message) => {
                                app.agent_log_view = None;
                                app.feedback_buffer = message;
                            }
                            Err(error) if !terminal_session.active => {
                                return Err(error);
                            }
                            Err(error) => {
                                app.feedback_buffer = format!(
                                    "Unable to interrupt the displayed Codex session: {error}"
                                );
                            }
                        }
                        app.agent_panel.refresh(&app.active_root);
                        *last_agent_panel_refresh = Instant::now();
                        if app.active_board {
                            normalize_board_selections_in_board(
                                &board_dir,
                                &statuses,
                                &mut app.board_states,
                            );
                        }
                    }
                    KeyCode::Char('c') => {
                        let label = app
                            .agent_log_view
                            .as_ref()
                            .map(|view| view.project_name.clone())
                            .unwrap_or_else(|| "the selected project".to_string());
                        let target =
                            match viewed_tui_codex_session_target(app.agent_log_view.as_ref()) {
                                Ok(target) => target,
                                Err(error) => {
                                    app.feedback_buffer = error.to_string();
                                    return Ok(false);
                                }
                            };
                        app.agent_panel.refresh(&app.active_root);
                        *last_agent_panel_refresh = Instant::now();
                        let availability = match tui_codex_session_availability_for_path(
                            &mut app.agent_panel,
                            &target.project_path,
                            &target.session_id,
                        ) {
                            Ok(availability) => availability,
                            Err(error) => {
                                app.feedback_buffer =
                                    format!("Unable to check the displayed Codex session: {error}");
                                return Ok(false);
                            }
                        };
                        if availability == TuiCodexSessionAvailability::SelectedSessionBusy {
                            app.feedback_buffer =
                                                "The displayed Codex session is active; press i to take it over interactively."
                                                    .to_string();
                            return Ok(false);
                        }
                        let shares_project =
                            availability == TuiCodexSessionAvailability::ProjectBusy;
                        match run_tui_codex_session_continue(
                            terminal,
                            terminal_session,
                            &app_title(&app.active_root),
                            &target,
                            &label,
                            shares_project,
                            false,
                        ) {
                            Ok(message) => {
                                app.agent_log_view = None;
                                app.feedback_buffer = message;
                            }
                            Err(error) if !terminal_session.active => {
                                return Err(error);
                            }
                            Err(error) => {
                                app.feedback_buffer = format!(
                                    "Unable to continue the displayed Codex session: {error}"
                                );
                            }
                        }
                        app.agent_panel.refresh(&app.active_root);
                        *last_agent_panel_refresh = Instant::now();
                        if app.active_board {
                            normalize_board_selections_in_board(
                                &board_dir,
                                &statuses,
                                &mut app.board_states,
                            );
                        }
                    }
                    KeyCode::Char('l') | KeyCode::Char('L') => {
                        if app.agent_log_view.take().is_some() {
                            app.feedback_buffer = "Closed agent output log".to_string();
                            return Ok(false);
                        }

                        match selected_tui_agent_log_view(&app.agent_panel) {
                            Ok(Some(log_view)) => {
                                let output_kind = if log_view.is_live {
                                    "live agent output"
                                } else {
                                    "latest agent output"
                                };
                                app.feedback_buffer =
                                    format!("Showing {output_kind} for {}", log_view.project_name);
                                app.agent_log_view = Some(log_view);
                                *last_agent_log_refresh = Instant::now();
                            }
                            Ok(None) => {
                                app.feedback_buffer = if app
                                    .agent_panel
                                    .selected_current_project_registration()
                                    .is_some()
                                {
                                    "Register current project before viewing agent output"
                                        .to_string()
                                } else {
                                    "No agent output recorded for selected project".to_string()
                                };
                            }
                            Err(e) => app.feedback_buffer = format!("Error: {}", e),
                        }
                    }
                    KeyCode::Up => {
                        app.agent_panel.select_previous();
                        sync_open_tui_agent_log_view(&app.agent_panel, &mut app.agent_log_view);
                        *last_agent_log_refresh = Instant::now();
                    }
                    KeyCode::Down => {
                        app.agent_panel.select_next();
                        sync_open_tui_agent_log_view(&app.agent_panel, &mut app.agent_log_view);
                        *last_agent_log_refresh = Instant::now();
                    }
                    _ => {
                        app.feedback_buffer = tui_agent_panel_instructions().to_string();
                    }
                }
            } else if app.archive_view {
                match key.code {
                    KeyCode::Char('A') | KeyCode::Char('a')
                        if key.modifiers.contains(KeyModifiers::SHIFT) =>
                    {
                        app.archive_view = false;
                        app.archive_state.select(None);
                        app.archive_scroll_offset = 0;
                        app.feedback_buffer = "Returned to Kanban view".to_string();
                    }
                    KeyCode::Char('q') => return Ok(true),
                    KeyCode::Char('h') | KeyCode::Char('H') | KeyCode::Char('?') => {
                        app.current_mode = Mode::Help;
                    }
                    KeyCode::Up => {
                        let tasks = read_archived_task_entries(&board_dir).unwrap_or_default();
                        if !tasks.is_empty() {
                            let i = app.archive_state.selected().unwrap_or(0);
                            if i > 0 {
                                app.archive_state.select(Some(i - 1));
                            } else {
                                app.archive_state.select(Some(tasks.len() - 1));
                            }
                        }
                    }
                    KeyCode::Down => {
                        let tasks = read_archived_task_entries(&board_dir).unwrap_or_default();
                        if !tasks.is_empty() {
                            let i = app.archive_state.selected().unwrap_or(0);
                            if i < tasks.len() - 1 {
                                app.archive_state.select(Some(i + 1));
                            } else {
                                app.archive_state.select(Some(0));
                            }
                        }
                    }
                    _ => {
                        app.feedback_buffer =
                            "Archive view is read-only. Press A again to leave.".to_string();
                    }
                }
            } else if tui_toggles_models(&key) {
                app.models_return_pane = tui_models_return_pane(app.current_pane);
                app.models_panel.refresh();
                app.current_pane = TuiPane::Models;
                app.feedback_buffer = tui_models_instructions().to_string();
            } else if let Some(direction) = tui_task_reorder_direction(&key) {
                app.feedback_buffer = reorder_selected_tui_task(
                    &board_dir,
                    statuses[app.selected_board],
                    &mut app.board_states[app.selected_board],
                    direction,
                );
            } else if tui_toggles_reorganize_mode(&key) {
                app.current_mode = Mode::Reorganize;
                app.feedback_buffer =
                    "Reorganize mode active: use Arrows to move tasks; press r or Esc to exit."
                        .to_string();
            } else if tui_starts_subtask_input(&key) {
                if let Some((idx, entry)) = selected_task_entry_in_board(
                    &board_dir,
                    statuses[app.selected_board],
                    &app.board_states[app.selected_board],
                ) {
                    app.current_mode = Mode::Input;
                    app.subtask_parent = Some((idx + 1, entry));
                    app.task_input.reset();
                    app.feedback_buffer = "Enter the new subtask description.".to_string();
                } else {
                    app.feedback_buffer =
                        "Select a parent task before creating a subtask.".to_string();
                }
            } else if key.modifiers.contains(KeyModifiers::SHIFT) {
                match key.code {
                    KeyCode::Char('A') | KeyCode::Char('a') => {
                        app.archive_view = true;
                        app.current_mode = Mode::View;
                        app.archive_scroll_offset = 0;
                        select_first_archive_task_if_present_in_board(
                            &board_dir,
                            &mut app.archive_state,
                        );
                        app.feedback_buffer =
                            "Archive view. Press A again to leave archive view.".to_string();
                    }
                    KeyCode::Char('B') | KeyCode::Char('b') => {
                        app.feedback_buffer = toggle_tui_backlog_column(
                            &board_dir,
                            &mut app.board_states,
                            &mut app.selected_board,
                            &mut app.backlog_visible,
                        );
                    }
                    KeyCode::Left => {
                        app.feedback_buffer = move_selected_tui_task_between_boards(
                            &board_dir,
                            &statuses,
                            &mut app.board_states,
                            &mut app.selected_board,
                            app.backlog_visible,
                            TuiTaskBoardMoveDirection::Left,
                        );
                    }
                    KeyCode::Right => {
                        app.feedback_buffer = move_selected_tui_task_between_boards(
                            &board_dir,
                            &statuses,
                            &mut app.board_states,
                            &mut app.selected_board,
                            app.backlog_visible,
                            TuiTaskBoardMoveDirection::Right,
                        );
                    }
                    _ => {}
                }
            } else if key.modifiers.contains(KeyModifiers::CONTROL)
                || key.modifiers.contains(KeyModifiers::ALT)
            {
                // Other Alt/Ctrl modifiers are not used for moving tasks.
                _ = ();
            } else {
                match key.code {
                    KeyCode::Esc => {
                        let state = &mut app.board_states[app.selected_board];
                        state.select(None);
                        app.feedback_buffer = "Task unselected".to_string();
                    }
                    KeyCode::Char('B') => {
                        app.feedback_buffer = toggle_tui_backlog_column(
                            &board_dir,
                            &mut app.board_states,
                            &mut app.selected_board,
                            &mut app.backlog_visible,
                        );
                    }
                    KeyCode::Char('b') => {
                        app.feedback_buffer = match move_selected_tui_task_to_backlog(
                            &board_dir,
                            &statuses,
                            &mut app.board_states,
                            &mut app.selected_board,
                            app.backlog_visible,
                        ) {
                            Ok(message) => message,
                            Err(error) => format!("Error: {error}"),
                        };
                    }
                    KeyCode::Char('a') => {
                        app.feedback_buffer = match move_selected_tui_task_to_archive(
                            &board_dir,
                            &statuses,
                            &mut app.board_states,
                            app.selected_board,
                        ) {
                            Ok(message) => message,
                            Err(error) => format!("Error: {error}"),
                        };
                    }
                    KeyCode::Char('A') => {
                        app.archive_view = true;
                        app.current_mode = Mode::View;
                        app.archive_scroll_offset = 0;
                        select_first_archive_task_if_present_in_board(
                            &board_dir,
                            &mut app.archive_state,
                        );
                        app.feedback_buffer =
                            "Archive view. Press A again to leave archive view.".to_string();
                    }
                    KeyCode::Char('s') => {
                        let selected_status = statuses[app.selected_board];
                        let Some((_, task)) = selected_task_entry_in_board(
                            &board_dir,
                            selected_status,
                            &app.board_states[app.selected_board],
                        ) else {
                            app.feedback_buffer = "No task selected".to_string();
                            return Ok(false);
                        };
                        let Some(session_id) = codex_session_for_task(&task) else {
                            app.feedback_buffer =
                                "No Codex session linked to this task.".to_string();
                            return Ok(false);
                        };
                        app.agent_panel.refresh(&app.active_root);
                        *last_agent_panel_refresh = Instant::now();
                        if !app.agent_panel.select_project_for_path(&app.active_root) {
                            app.feedback_buffer =
                                "Register this project before controlling its Codex session."
                                    .to_string();
                            return Ok(false);
                        }
                        let Some(project_id) = app
                            .agent_panel
                            .selected_project()
                            .map(|selected| selected.project.id)
                        else {
                            app.feedback_buffer =
                                "Register this project before controlling its Codex session."
                                    .to_string();
                            return Ok(false);
                        };
                        app.feedback_buffer =
                            match toggle_tui_codex_session_stop(project_id, &session_id) {
                                Ok(message) => message,
                                Err(error) => {
                                    format!("Unable to stop or resume the Codex session: {error}")
                                }
                            };
                    }
                    KeyCode::Char('i') => {
                        let selected_status = statuses[app.selected_board];
                        let Some((_, task)) = selected_task_entry_in_board(
                            &board_dir,
                            selected_status,
                            &app.board_states[app.selected_board],
                        ) else {
                            app.feedback_buffer = "No task selected".to_string();
                            return Ok(false);
                        };
                        let Some(session_id) = codex_session_for_task(&task) else {
                            app.feedback_buffer =
                                "No Codex session linked to this task.".to_string();
                            return Ok(false);
                        };
                        app.agent_panel.refresh(&app.active_root);
                        *last_agent_panel_refresh = Instant::now();
                        if !app.agent_panel.select_project_for_path(&app.active_root) {
                            app.feedback_buffer =
                                "Register this project before interrupting its Codex session."
                                    .to_string();
                            return Ok(false);
                        }
                        let Some(project) = app
                            .agent_panel
                            .selected_project()
                            .map(|selected| selected.project.clone())
                        else {
                            app.feedback_buffer =
                                "Register this project before interrupting its Codex session."
                                    .to_string();
                            return Ok(false);
                        };
                        let target = TuiCodexSessionTarget::new(&project, session_id);
                        let label = task_display_text(&task);
                        match run_tui_codex_session_interrupt(
                            terminal,
                            terminal_session,
                            &app_title(&app.active_root),
                            &target,
                            &label,
                        ) {
                            Ok(message) => {
                                app.agent_log_view = None;
                                app.feedback_buffer = message;
                            }
                            Err(error) if !terminal_session.active => {
                                return Err(error);
                            }
                            Err(error) => {
                                app.feedback_buffer =
                                    format!("Unable to interrupt the Codex session: {error}");
                            }
                        }
                        app.agent_panel.refresh(&app.active_root);
                        *last_agent_panel_refresh = Instant::now();
                        normalize_board_selections_in_board(
                            &board_dir,
                            &statuses,
                            &mut app.board_states,
                        );
                    }
                    KeyCode::Char('c') => {
                        let selected_status = statuses[app.selected_board];
                        let Some((_, task)) = selected_task_entry_in_board(
                            &board_dir,
                            selected_status,
                            &app.board_states[app.selected_board],
                        ) else {
                            app.feedback_buffer = "No task selected".to_string();
                            return Ok(false);
                        };

                        if !task_supports_interactive_codex_resume(selected_status, &task) {
                            app.feedback_buffer =
                                                "Codex sessions can be resumed from linked Doing, Done, or blocked Todo tasks."
                                                    .to_string();
                            return Ok(false);
                        }

                        let Some(session_id) = codex_session_for_task(&task) else {
                            app.feedback_buffer =
                                "No Codex session linked to this task.".to_string();
                            return Ok(false);
                        };
                        app.agent_panel.refresh(&app.active_root);
                        *last_agent_panel_refresh = Instant::now();
                        let availability = match tui_codex_session_availability_for_path(
                            &mut app.agent_panel,
                            &app.active_root,
                            &session_id,
                        ) {
                            Ok(availability) => availability,
                            Err(error) => {
                                app.feedback_buffer = format!(
                                    "Unable to check whether the Codex session is available: {error}"
                                );
                                return Ok(false);
                            }
                        };
                        if availability == TuiCodexSessionAvailability::SelectedSessionBusy {
                            app.feedback_buffer =
                                                "This exact Codex session is already running or in an interactive handoff; stop or wait for it before resuming it again."
                                                    .to_string();
                            return Ok(false);
                        }
                        let shares_project =
                            availability == TuiCodexSessionAvailability::ProjectBusy;
                        let Some(project) = app
                            .agent_panel
                            .selected_project()
                            .map(|selected| selected.project.clone())
                        else {
                            app.feedback_buffer =
                                "Register this project before resuming its Codex session."
                                    .to_string();
                            return Ok(false);
                        };
                        let target = TuiCodexSessionTarget::new(&project, session_id);
                        let label = task_display_text(&task);
                        match run_tui_codex_session_continue(
                            terminal,
                            terminal_session,
                            &app_title(&app.active_root),
                            &target,
                            &label,
                            shares_project,
                            true,
                        ) {
                            Ok(message) => {
                                app.agent_log_view = None;
                                app.feedback_buffer = message;
                            }
                            Err(error) if !terminal_session.active => {
                                return Err(error);
                            }
                            Err(error) => {
                                app.feedback_buffer = format!("Error: {error}");
                            }
                        }
                        app.agent_panel.refresh(&app.active_root);
                        *last_agent_panel_refresh = Instant::now();
                        normalize_board_selections_in_board(
                            &board_dir,
                            &statuses,
                            &mut app.board_states,
                        );
                    }
                    KeyCode::Char('l') | KeyCode::Char('L') => {
                        if app.agent_log_view.take().is_some() {
                            app.feedback_buffer = "Closed agent output log".to_string();
                            return Ok(false);
                        }

                        let selected_status = statuses[app.selected_board];
                        let selected_task = selected_task_entry_in_board(
                            &board_dir,
                            selected_status,
                            &app.board_states[app.selected_board],
                        )
                        .map(|(_, task)| task);

                        app.agent_panel.refresh(&app.active_root);
                        *last_agent_panel_refresh = Instant::now();
                        match selected_tui_task_or_project_log_view_for_path(
                            &mut app.agent_panel,
                            &app.active_root,
                            selected_status,
                            selected_task.as_ref(),
                        ) {
                            Ok(Some(log_view)) => {
                                let output_kind = if log_view.is_live {
                                    "live agent output"
                                } else {
                                    "recorded agent output"
                                };
                                app.feedback_buffer =
                                    format!("Showing {output_kind} for {}", log_view.project_name);
                                app.agent_log_view = Some(log_view);
                                *last_agent_log_refresh = Instant::now();
                            }
                            Ok(None) => {
                                app.feedback_buffer = if app.agent_panel.last_error.is_some() {
                                    app.agent_panel.last_error.clone().unwrap_or_default()
                                } else if app
                                    .agent_panel
                                    .selected_current_project_registration()
                                    .is_some()
                                {
                                    "Register current project before viewing agent output"
                                        .to_string()
                                } else if selected_task.is_some() {
                                    "No agent output recorded for selected task".to_string()
                                } else {
                                    "No agent output recorded for selected project".to_string()
                                };
                            }
                            Err(e) => app.feedback_buffer = format!("Error: {}", e),
                        }
                    }
                    KeyCode::Char('q') => return Ok(true),
                    KeyCode::Backspace => {
                        if app.board_stack.len() > 1 {
                            app.board_stack.pop();
                            app.selected_board = TODO_BOARD_INDEX;
                            for state in app.board_states.iter_mut() {
                                state.select(None);
                            }
                            let parent_board = app
                                .board_stack
                                .last()
                                .cloned()
                                .unwrap_or_else(|| get_tasks_dir(&app.active_root));
                            select_first_task_if_present_in_board(
                                &parent_board,
                                statuses[app.selected_board],
                                &mut app.board_states[app.selected_board],
                            );
                            app.feedback_buffer = "Returned to parent board".to_string();
                        } else {
                            app.feedback_buffer = "Already at the top board".to_string();
                        }
                    }
                    KeyCode::Enter => {
                        if let Some((idx, entry)) = selected_task_entry_in_board(
                            &board_dir,
                            statuses[app.selected_board],
                            &app.board_states[app.selected_board],
                        ) {
                            match &entry.source {
                                TaskSource::Path { path, is_dir: true } if entry.has_subtasks => {
                                    {
                                        let _mutation_lock = acquire_board_mutation_lock(path)?;
                                        ensure_board_store(path)?;
                                    }
                                    app.board_stack.push(path.clone());
                                    app.selected_board = TODO_BOARD_INDEX;
                                    for state in app.board_states.iter_mut() {
                                        state.select(None);
                                    }
                                    select_first_task_if_present_in_board(
                                        path,
                                        statuses[app.selected_board],
                                        &mut app.board_states[app.selected_board],
                                    );
                                    app.feedback_buffer = "Opened subtask board".to_string();
                                }
                                _ => {
                                    app.current_mode = Mode::Edit;
                                    app.editing_task_idx = Some(idx + 1);
                                    app.task_input = TaskInput::new(
                                        task_content_without_recoverable_codex_session(
                                            &entry.content,
                                        ),
                                    );
                                }
                            }
                        } else {
                            app.board_states[app.selected_board].select(None);
                            app.current_mode = Mode::Input;
                            app.subtask_parent = None;
                            app.task_input.reset();
                        }
                    }
                    KeyCode::Char('e') | KeyCode::Char('E') => {
                        if let Some((idx, entry)) = selected_task_entry_in_board(
                            &board_dir,
                            statuses[app.selected_board],
                            &app.board_states[app.selected_board],
                        ) {
                            app.current_mode = Mode::Edit;
                            app.editing_task_idx = Some(idx + 1);
                            app.task_input = TaskInput::new(
                                task_content_without_recoverable_codex_session(&entry.content),
                            );
                        } else {
                            app.feedback_buffer = "No task selected".to_string();
                        }
                    }
                    KeyCode::Char(' ') => {
                        if selected_task_index_in_board(
                            &board_dir,
                            statuses[app.selected_board],
                            &app.board_states[app.selected_board],
                        )
                        .is_none()
                        {
                            app.board_states[app.selected_board].select(None);
                        }
                        app.current_mode = Mode::Input;
                        app.subtask_parent = None;
                        app.task_input.reset();
                    }
                    KeyCode::Char('0') => {
                        app.backlog_visible = true;
                        app.selected_board = BACKLOG_BOARD_INDEX;
                        for state in app.board_states.iter_mut() {
                            state.select(None);
                        }
                        select_first_task_if_present_in_board(
                            &board_dir,
                            statuses[app.selected_board],
                            &mut app.board_states[app.selected_board],
                        );
                        app.feedback_buffer = "Backlog column shown and focused.".to_string();
                    }
                    KeyCode::Char('1') => {
                        app.selected_board = TODO_BOARD_INDEX;
                        for state in app.board_states.iter_mut() {
                            state.select(None);
                        }
                        select_first_task_if_present_in_board(
                            &board_dir,
                            statuses[app.selected_board],
                            &mut app.board_states[app.selected_board],
                        );
                    }
                    KeyCode::Char('2') => {
                        app.selected_board = 1;
                        for state in app.board_states.iter_mut() {
                            state.select(None);
                        }
                        select_first_task_if_present_in_board(
                            &board_dir,
                            statuses[app.selected_board],
                            &mut app.board_states[app.selected_board],
                        );
                    }
                    KeyCode::Char('3') => {
                        app.selected_board = DONE_BOARD_INDEX;
                        for state in app.board_states.iter_mut() {
                            state.select(None);
                        }
                        select_first_task_if_present_in_board(
                            &board_dir,
                            statuses[app.selected_board],
                            &mut app.board_states[app.selected_board],
                        );
                    }
                    KeyCode::Char('d') | KeyCode::Char('D') | KeyCode::Delete => {
                        if let Some(idx) = selected_task_index_in_board(
                            &board_dir,
                            statuses[app.selected_board],
                            &app.board_states[app.selected_board],
                        ) {
                            let status = statuses[app.selected_board];
                            match delete_task_in_board(&board_dir, status, &(idx + 1).to_string()) {
                                Ok(_) => {
                                    app.feedback_buffer =
                                        format!("Deleted task {} from {}", idx + 1, status);
                                    app.board_states[app.selected_board].select(if idx > 0 {
                                        Some(idx - 1)
                                    } else {
                                        None
                                    });
                                }
                                Err(e) => app.feedback_buffer = format!("Error: {}", e),
                            }
                        } else {
                            app.board_states[app.selected_board].select(None);
                            app.feedback_buffer = "No task selected to delete".to_string();
                        }
                    }
                    KeyCode::Char('h') | KeyCode::Char('H') | KeyCode::Char('?') => {
                        app.current_mode = Mode::Help;
                    }
                    KeyCode::Up => {
                        let state = &mut app.board_states[app.selected_board];
                        let tasks = read_tasks_in_board(&board_dir, statuses[app.selected_board])
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
                        let state = &mut app.board_states[app.selected_board];
                        let tasks = read_tasks_in_board(&board_dir, statuses[app.selected_board])
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
                        app.selected_board =
                            wrapped_visible_tui_board(app.selected_board, app.backlog_visible, -1);
                        for state in app.board_states.iter_mut() {
                            state.select(None);
                        }
                        select_first_task_if_present_in_board(
                            &board_dir,
                            statuses[app.selected_board],
                            &mut app.board_states[app.selected_board],
                        );
                    }
                    KeyCode::Right => {
                        app.selected_board =
                            wrapped_visible_tui_board(app.selected_board, app.backlog_visible, 1);
                        for state in app.board_states.iter_mut() {
                            state.select(None);
                        }
                        select_first_task_if_present_in_board(
                            &board_dir,
                            statuses[app.selected_board],
                            &mut app.board_states[app.selected_board],
                        );
                    }
                    KeyCode::Char(c) if c.is_ascii_digit() => {
                        let new_pos = (c as u8 - b'0') as usize;
                        if let Some(idx) = selected_task_index_in_board(
                            &board_dir,
                            statuses[app.selected_board],
                            &app.board_states[app.selected_board],
                        ) {
                            if new_pos > 0 {
                                match reorder_task_in_board(
                                    &board_dir,
                                    statuses[app.selected_board],
                                    idx,
                                    new_pos - 1,
                                ) {
                                    Ok(_) => {
                                        app.feedback_buffer =
                                            format!("Reordered task to position {}", new_pos)
                                    }
                                    Err(e) => app.feedback_buffer = format!("Error: {}", e),
                                }
                            }
                        } else {
                            app.board_states[app.selected_board].select(None);
                            app.feedback_buffer = "No task selected".to_string();
                        }
                    }
                    _ => {}
                }
            }
        }
        Mode::Reorganize => match tui_reorganize_input(&key) {
            TuiReorganizeInput::Exit => {
                app.current_mode = Mode::View;
                app.feedback_buffer = "Reorganize mode exited.".to_string();
            }
            TuiReorganizeInput::Move(direction) => {
                app.feedback_buffer = reorganize_selected_tui_task(
                    &board_dir,
                    &statuses,
                    &mut app.board_states,
                    &mut app.selected_board,
                    app.backlog_visible,
                    direction,
                );
            }
            TuiReorganizeInput::Ignore => {
                app.feedback_buffer =
                    "Reorganize mode active: use Arrows to move tasks; press r or Esc to exit."
                        .to_string();
            }
        },
        Mode::Help => match key.code {
            KeyCode::Enter
            | KeyCode::Esc
            | KeyCode::Char('h')
            | KeyCode::Char('H')
            | KeyCode::Char('?') => {
                app.current_mode = Mode::View;
            }
            _ => {}
        },
        Mode::Input if tui_cancels_task_prompt(&key) => {
            app.current_mode = Mode::View;
            app.subtask_parent = None;
            app.task_input.reset();
        }
        Mode::Input => match key.code {
            KeyCode::Enter => {
                let task_value = app.task_input.submitted_value();
                if !task_value.trim().is_empty() {
                    if let Some((parent_idx, expected_parent)) = app.subtask_parent.as_ref() {
                        match insert_subtask_in_board(
                            &board_dir,
                            statuses[app.selected_board],
                            *parent_idx,
                            expected_parent,
                            &task_value,
                            None,
                        ) {
                            Ok(subtask_board) => {
                                app.board_stack.push(subtask_board.clone());
                                app.selected_board = TODO_BOARD_INDEX;
                                for state in app.board_states.iter_mut() {
                                    state.select(None);
                                }
                                select_last_task_if_present_in_board(
                                    &subtask_board,
                                    statuses[app.selected_board],
                                    &mut app.board_states[app.selected_board],
                                );
                                app.feedback_buffer =
                                    "Subtask added and nested board opened.".to_string();
                            }
                            Err(e) => app.feedback_buffer = format!("Error: {}", e),
                        }
                    } else {
                        match insert_task_at_selection_in_board(
                            &board_dir,
                            statuses[app.selected_board],
                            &app.board_states[app.selected_board],
                            &task_value,
                            None,
                        ) {
                            Ok(_) => app.feedback_buffer = "Task added successfully.".to_string(),
                            Err(e) => app.feedback_buffer = format!("Error: {}", e),
                        }
                    }
                } else {
                    app.feedback_buffer = "Task description cannot be empty.".to_string();
                }
                app.current_mode = Mode::View;
                app.subtask_parent = None;
                app.task_input.reset();
            }
            _ => {
                let label = if app.subtask_parent.is_some() {
                    " Add Subtask: "
                } else {
                    " Add Task: "
                };
                handle_input_key(&mut app.task_input.input, key, label, input_available_width)
            }
        },
        Mode::Edit if tui_cancels_task_prompt(&key) => {
            app.current_mode = Mode::View;
            app.task_input.reset();
            app.editing_task_idx = None;
        }
        Mode::Edit => match key.code {
            KeyCode::Enter => {
                let task_value = app.task_input.submitted_value();
                if !task_value.trim().is_empty() {
                    if let Some(idx) = app.editing_task_idx {
                        match update_task_in_board(
                            &board_dir,
                            statuses[app.selected_board],
                            idx,
                            &task_value,
                        ) {
                            Ok(_) => {
                                app.feedback_buffer = format!("Task {} updated successfully.", idx)
                            }
                            Err(e) => app.feedback_buffer = format!("Error: {}", e),
                        }
                    }
                } else {
                    app.feedback_buffer = "Task description cannot be empty.".to_string();
                }
                app.current_mode = Mode::View;
                app.task_input.reset();
                app.editing_task_idx = None;
            }
            _ => handle_input_key(
                &mut app.task_input.input,
                key,
                " Edit Task: ",
                input_available_width,
            ),
        },
    }
    Ok(false)
}

pub(super) fn tui_view_with_active_board(
    root: &Path,
    start_with_active_board: bool,
) -> Result<PathBuf> {
    let title = app_title(root);
    let mut terminal_session = TerminalSession::enter(&title)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    let mut app = TuiApp::new(root, start_with_active_board);
    let mut agent_panel_refresh = TuiAgentPanelRefreshWorker::new();
    let mut last_agent_panel_refresh = Instant::now();
    let mut last_agent_log_refresh = Instant::now();
    execute_tui_effect(
        &mut app,
        TuiEffect::RefreshAgentPanel,
        &mut agent_panel_refresh,
        &mut last_agent_panel_refresh,
        &mut last_agent_log_refresh,
    );

    loop {
        if !app.active_board && app.current_pane == TuiPane::Tasks {
            app.current_pane = TuiPane::AgentProjects;
            app.archive_view = false;
        }

        for effect in [TuiEffect::RefreshTaskSnapshot, TuiEffect::RefreshClock] {
            execute_tui_effect(
                &mut app,
                effect,
                &mut agent_panel_refresh,
                &mut last_agent_panel_refresh,
                &mut last_agent_log_refresh,
            );
        }
        app.normalize_cached_task_selections();

        if let Some(refresh) = agent_panel_refresh.try_result() {
            if refresh.active_root == app.active_root {
                let selected_row = app.agent_panel.selected_row_identity();
                app.agent_panel.apply_refresh_result(
                    &app.active_root,
                    selected_row,
                    refresh.panel_snapshot,
                );
                if let Ok(states) = refresh.task_session_states {
                    app.task_agent_session_states = states;
                }
                let sync_effect = if app.current_pane == TuiPane::AgentProjects {
                    Some(TuiEffect::SyncAgentLog)
                } else if app.current_pane == TuiPane::Tasks
                    && app.active_board
                    && !app.archive_view
                {
                    Some(TuiEffect::SyncTaskLog)
                } else {
                    None
                };
                if let Some(effect) = sync_effect {
                    execute_tui_effect(
                        &mut app,
                        effect,
                        &mut agent_panel_refresh,
                        &mut last_agent_panel_refresh,
                        &mut last_agent_log_refresh,
                    );
                }
            } else {
                last_agent_panel_refresh = Instant::now()
                    .checked_sub(tui_agent_panel_refresh_interval())
                    .unwrap_or_else(Instant::now);
            }
        }
        if last_agent_panel_refresh.elapsed() >= tui_agent_panel_refresh_interval() {
            execute_tui_effect(
                &mut app,
                TuiEffect::RefreshAgentPanel,
                &mut agent_panel_refresh,
                &mut last_agent_panel_refresh,
                &mut last_agent_log_refresh,
            );
        }
        if last_agent_log_refresh.elapsed() >= tui_agent_log_refresh_interval() {
            execute_tui_effect(
                &mut app,
                TuiEffect::RefreshAgentLog,
                &mut agent_panel_refresh,
                &mut last_agent_panel_refresh,
                &mut last_agent_log_refresh,
            );
        }

        app.prepare_render(terminal.size()?.into());
        terminal.draw(|f| render_tui(f, &app))?;

        #[allow(clippy::collapsible_if)]
        if event::poll(std::time::Duration::from_millis(100))? {
            match event::read()? {
                Event::Paste(content) if app.model_input.is_some() => {
                    if let Some(input) = app.model_input.as_mut() {
                        input.insert_paste(&content);
                    }
                }
                Event::Paste(content) if matches!(app.current_mode, Mode::Input | Mode::Edit) => {
                    app.task_input.insert_paste(content);
                }
                Event::Key(key)
                    if execute_tui_key_effect(
                        &mut app,
                        key,
                        &mut terminal,
                        &mut terminal_session,
                        &mut agent_panel_refresh,
                        &mut last_agent_panel_refresh,
                        &mut last_agent_log_refresh,
                    )? =>
                {
                    break;
                }
                Event::Key(_) => {}
                _ => {}
            }

            if app.agent_log_view.is_some()
                && app.current_pane == TuiPane::Tasks
                && app.active_board
                && !app.archive_view
                && matches!(app.current_mode, Mode::View)
            {
                execute_tui_effect(
                    &mut app,
                    TuiEffect::SyncTaskLog,
                    &mut agent_panel_refresh,
                    &mut last_agent_panel_refresh,
                    &mut last_agent_log_refresh,
                );
            }
        }
    }

    Ok(app.active_root)
}

#[cfg(test)]
pub(crate) mod tests;
