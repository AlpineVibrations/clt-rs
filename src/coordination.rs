//! Task-level admission and context, separate from execution and Git proof.
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    process::Command,
};

use crate::{
    agent::{
        AGENT_DB_FILE, AgentGitMode, AgentSessionControlState, GitFinalizationState,
        agent_state_dir, open_agent_store_at,
    },
    task::coordination::{self as metadata, Metadata, Review, Scope},
    task::{
        TaskBoard, TaskEntry, TaskSource, TaskStatus, acquire_board_mutation_lock,
        durable_task_identity, get_tasks_dir, read_archived_task_entries,
        recoverable_codex_session_id_from_task_content, task_content_is_manual, task_display_text,
        task_entry_is_ready, task_entry_is_stopped,
    },
};
use anyhow::{Context, Result};

pub(crate) mod finalization;

#[derive(Clone, Debug)]
pub(crate) struct Task {
    pub board: PathBuf,
    pub status: TaskStatus,
    pub index: usize,
    pub entry: TaskEntry,
    pub meta: Metadata,
    pub error: Option<String>,
    pub completion_wait: Option<String>,
    pub owner: String,
}

impl Task {
    fn label(&self) -> String {
        if self.meta.id.is_empty() {
            format!(
                "{} #{} ({})",
                self.status,
                self.index,
                task_display_text(&self.entry)
            )
        } else {
            format!("{} ({})", self.meta.id, task_display_text(&self.entry))
        }
    }
    fn complete(&self) -> bool {
        self.status == TaskStatus::Done && self.completion_wait.is_none()
    }
    fn active(&self) -> bool {
        self.status == TaskStatus::Doing || (self.status == TaskStatus::Done && !self.complete())
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Snapshot {
    pub tasks: Vec<Task>,
    pub changed_files: BTreeSet<String>,
}

fn collect(board: &Path, tasks: &mut Vec<Task>) -> Result<()> {
    let source = TaskBoard::new(board);
    for status in TaskStatus::ALL {
        for (offset, entry) in source.entries(status)?.into_iter().enumerate() {
            if entry.has_subtasks
                && let TaskSource::Path { path, is_dir: true } = &entry.source
            {
                collect(path, tasks)?;
            }
            let (meta, error) = match metadata::read(&entry.content) {
                Ok(meta) => (meta, None),
                Err(error) => (Metadata::default(), Some(format!("{error:#}"))),
            };
            let owner = if task_content_is_manual(&entry.content) {
                "CLI claimed; activity unconfirmed"
            } else if recoverable_codex_session_id_from_task_content(&entry.content).is_some() {
                "saved conversation; idle"
            } else {
                "unclaimed"
            }
            .to_string();
            tasks.push(Task {
                board: board.to_path_buf(),
                status,
                index: offset + 1,
                entry,
                meta,
                error,
                completion_wait: None,
                owner,
            });
        }
    }
    for entry in read_archived_task_entries(board)? {
        let meta = metadata::read(&entry.content).unwrap_or_default();
        if !meta.id.is_empty() {
            tasks.push(Task {
                board: board.to_path_buf(),
                status: TaskStatus::Done,
                index: 0,
                entry,
                meta,
                error: None,
                completion_wait: None,
                owner: "archived".into(),
            });
        }
    }
    Ok(())
}

impl Snapshot {
    pub(crate) fn load(board: &Path) -> Result<Self> {
        Self::load_at(board, &agent_state_dir()?)
    }

    pub(crate) fn load_at(board: &Path, state: &Path) -> Result<Self> {
        Self::load_with_context(board, state, true)
    }

    pub(crate) fn admission(board: &Path) -> Result<Self> {
        Self::load_with_context(board, &agent_state_dir()?, false)
    }

    fn load_with_context(board: &Path, state: &Path, owners: bool) -> Result<Self> {
        let mut snapshot = Self::default();
        if !crate::task::board_has_any_status_store(board) {
            return Ok(snapshot);
        }
        let outer = board
            .ancestors()
            .filter(|path| crate::task::board_has_any_status_store(path))
            .last()
            .unwrap_or(board);
        collect(outer, &mut snapshot.tasks)?;
        // Old boards without links remain cheap and do not need an agent registry.
        let needs_proof = owners
            || snapshot
                .tasks
                .iter()
                .any(|task| !task.meta.id.is_empty() || !inferred_symbols(task).is_empty());
        if needs_proof
            && snapshot.tasks.iter().any(|task| {
                recoverable_codex_session_id_from_task_content(&task.entry.content).is_some()
            })
        {
            snapshot.load_runtime(board, state)?;
        }
        let root = outer.parent().unwrap_or(outer);
        if let Ok(output) = Command::new("git")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .arg("-C")
            .arg(root)
            .args(["status", "--porcelain=v1", "-z", "--untracked-files=normal"])
            .output()
            && output.status.success()
        {
            let mut paths = output.stdout.split(|byte| *byte == 0);
            while let Some(record) = paths.next() {
                if record.len() < 4 {
                    continue;
                }
                snapshot
                    .changed_files
                    .insert(String::from_utf8_lossy(&record[3..]).into_owned());
                if (record[..2].contains(&b'R') || record[..2].contains(&b'C'))
                    && let Some(old) = paths.next()
                {
                    snapshot
                        .changed_files
                        .insert(String::from_utf8_lossy(old).into_owned());
                }
            }
        }
        Ok(snapshot)
    }

    fn load_runtime(&mut self, board: &Path, state: &Path) -> Result<()> {
        if !state.join(AGENT_DB_FILE).is_file() {
            return Ok(());
        }
        let store = open_agent_store_at(state)?;
        self.load_runtime_from_store(board, &store)
    }

    fn load_runtime_from_store(
        &mut self,
        board: &Path,
        store: &crate::agent::TursoAgentStore,
    ) -> Result<()> {
        let board = std::fs::canonicalize(board).unwrap_or_else(|_| board.to_path_buf());
        let Some(project) = store
            .list_projects_blocking()?
            .into_iter()
            .filter(|project| board.starts_with(&project.path))
            .max_by_key(|project| project.path.as_os_str().len())
        else {
            return Ok(());
        };
        let controls = store.session_controls_for_project_blocking(project.id)?;
        let pending = store.list_pending_git_finalizations_blocking(Some(project.id))?;
        for task in &mut self.tasks {
            let Some(session) = recoverable_codex_session_id_from_task_content(&task.entry.content)
            else {
                continue;
            };
            if let Some(control) = controls
                .iter()
                .find(|control| control.codex_session_id == session)
            {
                let kind = if task_content_is_manual(&task.entry.content) {
                    "CLI claimed"
                } else {
                    "automated"
                };
                task.owner = format!(
                    "{kind}; {}",
                    match control.state {
                        AgentSessionControlState::Running => "running",
                        AgentSessionControlState::Stopped => "stopped",
                        _ => "session control pending",
                    }
                );
            }
            if task.status == TaskStatus::Done
                && (!task.meta.id.is_empty()
                    || pending
                        .iter()
                        .any(|journal| journal.codex_session_id == session))
            {
                if let Some(journal) = store.git_finalization_blocking(project.id, session)? {
                    if !journal.state.is_terminal() {
                        task.completion_wait =
                            Some(if journal.state == GitFinalizationState::PushPending {
                                "PUSH-PENDING: waiting for publication proof".into()
                            } else {
                                "FINALIZING: waiting for task commit proof".into()
                            });
                    }
                } else if store
                    .session_git_mode_blocking(project.id, session)?
                    .is_some_and(|mode| mode != AgentGitMode::Off)
                {
                    task.completion_wait = Some(
                        "missing Git journal: restore the prerequisite's completion proof".into(),
                    );
                }
            }
        }
        Ok(())
    }

    pub(crate) fn selected(&self, board: &Path, status: TaskStatus, index: usize) -> Result<&Task> {
        anyhow::ensure!(index > 0, "Task indexes start at 1");
        self.tasks
            .iter()
            .find(|task| task.board == board && task.status == status && task.index == index)
            .context("Task changed; list tasks and select its current index")
    }

    fn referenced(&self, id: &str) -> Result<&Task> {
        let mut matches = self
            .tasks
            .iter()
            .filter(|task| task.meta.id == id && !id.is_empty());
        let task = matches.next().with_context(|| format!("Missing prerequisite {id}; restore the task or remove this dependency with clt coordinate --remove-dependency"))?;
        anyhow::ensure!(
            matches.next().is_none(),
            "Duplicate task ID {id}; repair the duplicate before using this reference"
        );
        Ok(task)
    }

    fn visit(
        &self,
        task: &Task,
        path: &mut Vec<String>,
        done: &mut BTreeSet<String>,
    ) -> Result<()> {
        if !task.meta.id.is_empty() {
            self.referenced(&task.meta.id)?;
            anyhow::ensure!(
                !path.contains(&task.meta.id),
                "Dependency cycle: {} -> {}",
                path.join(" -> "),
                task.meta.id
            );
            if done.contains(&task.meta.id) {
                return Ok(());
            }
            path.push(task.meta.id.clone());
        }
        for id in &task.meta.depends_on {
            self.visit(self.referenced(id)?, path, done)?;
        }
        if !task.meta.id.is_empty() {
            path.pop();
            done.insert(task.meta.id.clone());
        }
        Ok(())
    }

    pub(crate) fn waits(&self, task: &Task) -> Vec<String> {
        if task.complete() {
            return Vec::new();
        }
        if let Some(error) = &task.error {
            return vec![format!("Coordination metadata: {error}")];
        }
        if let Err(error) = self.visit(task, &mut Vec::new(), &mut BTreeSet::new()) {
            return vec![format!("Dependency wait: {error:#}")];
        }
        let mut waits = Vec::new();
        for id in &task.meta.depends_on {
            if let Ok(other) = self.referenced(id)
                && !other.complete()
            {
                waits.push(format!(
                    "Waiting for task {}: {}",
                    other.label(),
                    other
                        .completion_wait
                        .as_deref()
                        .unwrap_or("prerequisite is unfinished")
                ));
            }
        }
        for other in self
            .tasks
            .iter()
            .filter(|other| other.active() && !same_task(task, other))
        {
            // An explicitly waiting consumer must not prevent its prerequisite
            // from finishing. Its own admission/check remains deferred.
            if !task.meta.id.is_empty()
                && self.depends_on(other, &task.meta.id, &mut BTreeSet::new())
            {
                continue;
            }
            if let Some(reason) = overlap(task, other, &self.changed_files) {
                waits.push(reason);
            }
        }
        waits
    }

    fn depends_on(&self, task: &Task, id: &str, seen: &mut BTreeSet<String>) -> bool {
        task.meta.depends_on.iter().any(|dependency| {
            dependency == id
                || (seen.insert(dependency.clone())
                    && self
                        .referenced(dependency)
                        .is_ok_and(|other| self.depends_on(other, id, seen)))
        })
    }

    pub(crate) fn ready(&self, entry: &TaskEntry) -> bool {
        task_entry_is_ready(entry)
            && self
                .tasks
                .iter()
                .find(|task| {
                    task.entry.source == entry.source && task.entry.content == entry.content
                })
                .is_some_and(|task| self.waits(task).is_empty())
    }

    pub(crate) fn context(&self, own_session: Option<&str>) -> String {
        let mut lines = vec!["CLT coordination context (live snapshot):".to_string()];
        let mut tasks: Vec<_> = self.tasks.iter().collect();
        tasks.sort_by_key(|task| {
            !own_session.is_some_and(|session| {
                recoverable_codex_session_id_from_task_content(&task.entry.content) == Some(session)
            })
        });
        for task in tasks {
            let own = own_session.is_some_and(|session| {
                recoverable_codex_session_id_from_task_content(&task.entry.content) == Some(session)
            });
            let waits = self.waits(task);
            if !task.active() && !own && (task.status != TaskStatus::Todo || waits.is_empty()) {
                continue;
            }
            lines.push(format!(
                "- {} [{}; {}]{}",
                task.label(),
                task.status,
                task.owner,
                if own { " (this session)" } else { "" }
            ));
            if let Some(session) =
                recoverable_codex_session_id_from_task_content(&task.entry.content)
            {
                lines.push(format!("  Owner: codex:{session}"));
            }
            lines.push(format!(
                "  Intended work: {}",
                durable_task_identity(&task.entry.content)
                    .unwrap_or_default()
                    .trim_start_matches("v2\n")
            ));
            if task.meta.scope != Scope::default() {
                lines.push(format!(
                    "  Declared scope: {}",
                    serde_json::to_string(&task.meta.scope).unwrap_or_default()
                ));
            }
            for wait in waits {
                lines.push(format!("  {wait}"));
            }
        }
        if lines.len() == 1 {
            lines.push("No other active work or coordination waits.".into());
        }
        if !self.changed_files.is_empty() {
            lines.push(format!(
                "Current changed paths (ownership is not inferred from Git): {}",
                self.changed_files
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        lines.push("Refresh with clt context before editing and finalization. Declare scope changes with clt coordinate before making those edits. Resolve only the affected dependency/overlap; preserve conversations and unrelated work. A CLI claim is not proof of a running process.".into());
        lines.join("\n")
    }

    pub(crate) fn annotation(&self, entry: &TaskEntry) -> String {
        let Some(task) = self
            .tasks
            .iter()
            .find(|task| task.entry.source == entry.source && task.entry.content == entry.content)
        else {
            return String::new();
        };
        let mut parts = Vec::new();
        if !task.meta.id.is_empty() {
            parts.push(format!("id:{}", task.meta.id));
        }
        parts.extend(self.waits(task));
        if task.active() && task_content_is_manual(&entry.content) {
            parts.push(task.owner.clone());
        }
        if parts.is_empty() {
            String::new()
        } else {
            format!(" [{}]", parts.join("; "))
        }
    }

    pub(crate) fn details(&self, entry: &TaskEntry) -> String {
        let mut text = self.annotation(entry);
        if let Ok(meta) = metadata::read(&entry.content)
            && meta.scope != Scope::default()
        {
            text.push_str(&format!(
                " Scope: {}",
                serde_json::to_string(&meta.scope).unwrap_or_default()
            ));
        }
        text
    }
}

fn same_task(a: &Task, b: &Task) -> bool {
    a.board == b.board && a.status == b.status && a.index == b.index
}

fn evidence(a: &Task, b: &Task) -> String {
    let mut pair = [a, b].map(|task| {
        serde_json::json!({
            "id": task.meta.id, "requirements": durable_task_identity(&crate::task::task_content_for_edit(&task.entry.content)),
            "scope": task.meta.scope, "dependencies": task.meta.depends_on,
        })
        .to_string()
    });
    pair.sort();
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, pair.join("\n").as_bytes()).to_string()
}

fn path_overlap(a: &str, b: &str) -> bool {
    let a = a.trim_end_matches('/').trim_start_matches("./");
    let b = b.trim_end_matches('/').trim_start_matches("./");
    a == b
        || a.strip_prefix(b).is_some_and(|rest| rest.starts_with('/'))
        || b.strip_prefix(a).is_some_and(|rest| rest.starts_with('/'))
}

fn inferred_symbols(task: &Task) -> BTreeSet<String> {
    // Conservative evidence: named code symbols and paths, never generic words.
    let text = metadata::strip(&task.entry.content);
    text.split('`')
        .enumerate()
        .filter(|(i, _)| i % 2 == 1)
        .map(|(_, symbol)| symbol.trim())
        .filter(|symbol| {
            symbol.len() > 3
                && !symbol.contains(char::is_whitespace)
                && (symbol.contains('/')
                    || symbol.contains("::")
                    || symbol.contains('_')
                    || symbol.ends_with(".rs"))
        })
        .map(str::to_string)
        .collect()
}

fn overlap(task: &Task, other: &Task, changed: &BTreeSet<String>) -> Option<String> {
    for (key, value) in &task.meta.scope.behavior {
        if let Some(other_value) = other.meta.scope.behavior.get(key)
            && value != other_value
        {
            return Some(format!(
                "Conflict with task {}: behavior {key} requires {value:?} versus {other_value:?}; align the contract or declare a dependency",
                other.label()
            ));
        }
    }
    let stamp = evidence(task, other);
    let mut compatible = false;
    for (owner, peer) in [(task, other), (other, task)] {
        if !peer.meta.id.is_empty()
            && let Some(review) = owner
                .meta
                .reviews
                .iter()
                .find(|review| review.other == peer.meta.id && review.evidence == stamp)
        {
            if !review.compatible {
                return Some(format!(
                    "Conflict with task {}: {}; revise scope, record a compatible assessment, or wait for completion",
                    other.label(),
                    review.reason
                ));
            }
            compatible = true;
        }
    }
    if compatible {
        return None;
    }
    let mut concerns = BTreeSet::new();
    for (kind, ours, theirs) in [
        (
            "module",
            &task.meta.scope.modules,
            &other.meta.scope.modules,
        ),
        (
            "interface",
            &task.meta.scope.interfaces,
            &other.meta.scope.interfaces,
        ),
        (
            "resource",
            &task.meta.scope.resources,
            &other.meta.scope.resources,
        ),
    ] {
        for shared in ours.iter().filter(|item| theirs.contains(item)) {
            concerns.insert(format!("{kind} {shared}"));
        }
    }
    for file in &task.meta.scope.files {
        if other
            .meta
            .scope
            .files
            .iter()
            .any(|other| path_overlap(file, other))
        {
            let dirty = changed.iter().any(|path| path_overlap(file, path));
            concerns.insert(format!(
                "file {file}{}",
                if dirty { " (has current edits)" } else { "" }
            ));
        }
    }
    let ours = inferred_symbols(task);
    let theirs = inferred_symbols(other);
    for symbol in ours.intersection(&theirs) {
        concerns.insert(format!("requirements name {symbol}"));
    }
    for (scope, named) in [(&task.meta.scope, &theirs), (&other.meta.scope, &ours)] {
        for symbol in named {
            let shared_file = scope.files.iter().any(|file| path_overlap(file, symbol));
            let shared_contract = scope
                .modules
                .iter()
                .chain(&scope.interfaces)
                .chain(&scope.resources)
                .any(|name| name == symbol || symbol.starts_with(&format!("{name}::")));
            if shared_file || shared_contract {
                concerns.insert(format!("declared scope meets requirement {symbol}"));
            }
        }
    }
    (!concerns.is_empty()).then(|| format!("Potential overlap with task {}: {}; review the intended behavior and current diff, then use clt coordinate --compatible <id> --reason <evidence> or --depends-on <id>", other.label(), concerns.into_iter().collect::<Vec<_>>().join(", ")))
}

pub(crate) fn ready_tasks(root: &Path) -> Result<Vec<TaskEntry>> {
    let board = get_tasks_dir(root);
    if !crate::task::board_has_any_status_store(&board) {
        return Ok(Vec::new());
    }
    let snapshot = Snapshot::admission(&board)?;
    Ok(TaskBoard::new(&board)
        .entries(TaskStatus::Todo)?
        .into_iter()
        .filter(|entry| snapshot.ready(entry))
        .collect())
}

pub(crate) fn session_waiting(root: &Path, session: &str) -> Result<bool> {
    let snapshot = Snapshot::admission(&get_tasks_dir(root))?;
    Ok(snapshot
        .tasks
        .iter()
        .filter(|task| {
            task.status != TaskStatus::Done
                && recoverable_codex_session_id_from_task_content(&task.entry.content)
                    == Some(session)
        })
        .any(|task| !snapshot.waits(task).is_empty()))
}

pub(crate) fn ensure_admitted(board: &Path, status: TaskStatus, index: usize) -> Result<()> {
    let snapshot = Snapshot::admission(board)?;
    let task = snapshot.selected(board, status, index)?;
    let waits = snapshot.waits(task);
    anyhow::ensure!(waits.is_empty(), "{}", waits.join("\n"));
    Ok(())
}

pub(crate) fn ensure_new_manual_admitted(board: &Path, content: &str) -> Result<()> {
    let mut snapshot = Snapshot::admission(board)?;
    let task = Task {
        board: board.to_path_buf(),
        status: TaskStatus::Doing,
        index: usize::MAX,
        entry: crate::task::task_entry_from_text(
            TaskSource::MarkdownLine {
                line_index: usize::MAX,
            },
            content,
            content,
            false,
        ),
        meta: metadata::read(content)?,
        error: None,
        completion_wait: None,
        owner: "CLI claimed".into(),
    };
    snapshot.tasks.push(task.clone());
    let waits = snapshot.waits(&task);
    anyhow::ensure!(waits.is_empty(), "{}", waits.join("\n"));
    Ok(())
}

#[derive(Default)]
pub(crate) struct Update {
    pub depends_on: Vec<String>,
    pub remove_dependency: Vec<String>,
    pub scope: Option<Scope>,
    pub review: Option<(String, bool, String)>,
}

pub(crate) fn update(
    root: &Path,
    status: TaskStatus,
    index: usize,
    update: Update,
) -> Result<String> {
    let board = get_tasks_dir(root);
    let _lock = acquire_board_mutation_lock(&board)?;
    let mut snapshot = Snapshot::load(&board)?;
    let selected = snapshot.selected(&board, status, index)?.clone();
    anyhow::ensure!(
        selected.completion_wait.is_none(),
        "Provisional tasks cannot change coordination metadata"
    );
    anyhow::ensure!(
        status != TaskStatus::Done
            || (update.depends_on.is_empty()
                && update.remove_dependency.is_empty()
                && update.scope.is_none()
                && update.review.is_none()),
        "Completed tasks can only receive a stable ID"
    );
    let mut meta = metadata::read(&selected.entry.content)?;
    if meta.id.is_empty() {
        meta.id = uuid::Uuid::new_v4().to_string();
    }
    for id in update.depends_on {
        anyhow::ensure!(id != meta.id, "A task cannot depend on itself");
        snapshot.referenced(&id)?;
        if !meta.depends_on.contains(&id) {
            meta.depends_on.push(id);
        }
    }
    meta.depends_on
        .retain(|id| !update.remove_dependency.contains(id));
    if let Some(scope) = update.scope {
        meta.scope = scope;
    }
    let target = snapshot
        .tasks
        .iter_mut()
        .find(|task| same_task(task, &selected))
        .unwrap();
    target.meta = meta.clone();
    let updated = target.clone();
    snapshot.visit(&updated, &mut Vec::new(), &mut BTreeSet::new())?;
    if let Some((other, compatible, reason)) = update.review {
        anyhow::ensure!(
            !reason.trim().is_empty(),
            "An assessment needs concrete evidence and a reason"
        );
        let peer = snapshot.referenced(&other)?;
        anyhow::ensure!(!same_task(&updated, peer), "A task cannot assess itself");
        let stamp = evidence(&updated, peer);
        meta.reviews.retain(|review| review.other != other);
        meta.reviews.push(Review {
            other,
            compatible,
            reason,
            evidence: stamp,
        });
    }
    let content = metadata::write(&selected.entry.content, &meta)?;
    crate::application::ensure_managed_git_task_mutation_allowed(
        &board,
        &selected.entry,
        true,
        Some(&content),
    )?;
    TaskBoard::new(&board).write_entry_content(status, &selected.entry, &content)?;
    Ok(meta.id)
}

pub(crate) fn print_context(root: &Path, check: Option<(TaskStatus, usize)>) -> Result<()> {
    let board = get_tasks_dir(root);
    let snapshot = Snapshot::load(&board)?;
    println!(
        "{}",
        snapshot.context(std::env::var("CODEX_THREAD_ID").ok().as_deref())
    );
    if let Some((status, index)) = check {
        let task = snapshot.selected(&board, status, index)?;
        anyhow::ensure!(
            !task_entry_is_stopped(&task.entry),
            "This task is explicitly stopped"
        );
        let waits = snapshot.waits(task);
        anyhow::ensure!(waits.is_empty(), "{}", waits.join("\n"));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
