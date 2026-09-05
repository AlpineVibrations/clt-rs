use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};

use crate::{
    agent::{self, AgentGitMode, GitFinalizationState, open_agent_store_at, with_agent_store_at},
    application::{AgentLeaseHolderLiveness, AgentTaskSelection},
    platform::{configure_agent_child_command, stop_agent_child_process},
    runner::{agent_timestamp, agent_timestamp_after},
    scheduler::{
        agent_lease_for_project, agent_lease_holder_liveness, agent_lease_renew_interval,
        remaining_agent_delay,
    },
    session_control::InteractiveAgentLease,
    task::{
        CODEX_TASK_SESSION_PREFIX, StatusStore, TASK_DETAIL_FILES, TASK_STATUSES, TaskBoard,
        TaskEntry, TaskSource, TaskStatus, acquire_board_mutation_lock,
        acquire_board_mutation_lock_with_contention_callback,
        attach_codex_session_to_task_after_lock, blocked_follow_up_session,
        cleanup_clt_atomic_task_temporaries, codex_session_id_from_task_content,
        codex_session_markers_in_task_content, durable_task_identity, get_status_store,
        get_tasks_dir, move_task_without_reordering_after_lock, read_task_entries,
        remove_task_entry_without_reordering, starts_with_task_note_date, task_content_is_blocked,
        task_entry_is_blocked, task_tree_contains_session_marker,
        terminal_task_for_codex_session_in_board, title_from_path,
    },
};

const AGENT_GIT_REMOTE_TIMEOUT_SECONDS: u64 = 30;
// A commit-and-push reconciliation can perform two three-step remote proofs
// around one push. Its dedicated renewable lease stays beyond that bounded
// single-pass worst case without inheriting the ordinary one-hour worker TTL.
pub(super) const AGENT_GIT_FINALIZATION_LEASE_SECONDS: u64 =
    AGENT_GIT_REMOTE_TIMEOUT_SECONDS * 8 + 60;
pub(super) const AGENT_GIT_IDENTITY_NAME: &str = "CLT Agent";
pub(super) const AGENT_GIT_IDENTITY_EMAIL: &str = "clt-agent@localhost";

pub(super) struct AgentGitFinalizationLease {
    lease: Option<InteractiveAgentLease>,
    pub(super) holder: String,
    stop_heartbeat: Option<mpsc::Sender<()>>,
    heartbeat: Option<thread::JoinHandle<()>>,
    heartbeat_error: Arc<Mutex<Option<String>>>,
}

impl AgentGitFinalizationLease {
    fn start(lease: InteractiveAgentLease, timeout: Duration) -> Result<Self> {
        let state_dir = lease.state_dir.clone();
        let project_id = lease.project_id;
        let holder = lease.holder.clone();
        let timeout_seconds = timeout.as_secs().max(1);
        let renew_interval = agent_lease_renew_interval(timeout);
        let (stop_heartbeat, stop_receiver) = mpsc::channel();
        let heartbeat_error = Arc::new(Mutex::new(None));
        let heartbeat_error_for_thread = Arc::clone(&heartbeat_error);
        let heartbeat_holder = holder.clone();
        let heartbeat = thread::Builder::new()
            .name(format!("clt-git-finalizer-{project_id}"))
            .spawn(move || loop {
                match stop_receiver.recv_timeout(renew_interval) {
                    Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                let expires_at = agent_timestamp_after(timeout_seconds);
                let renewal = with_agent_store_at(&state_dir, |store| {
                    store.renew_git_finalization_lease_blocking(
                        project_id,
                        &heartbeat_holder,
                        &expires_at,
                    )
                });
                let error = match renewal {
                    Ok(true) => continue,
                    Ok(false) => format!(
                        "Git finalizer lost its exact project lease for project {project_id}"
                    ),
                    Err(error) => format!(
                        "Git finalizer could not renew its exact project lease for project {project_id}: {error:#}"
                    ),
                };
                let mut recorded = heartbeat_error_for_thread
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if recorded.is_none() {
                    *recorded = Some(error);
                }
                break;
            })
            .context("Failed to start the Git finalization lease heartbeat")?;
        Ok(Self {
            lease: Some(lease),
            holder,
            stop_heartbeat: Some(stop_heartbeat),
            heartbeat: Some(heartbeat),
            heartbeat_error,
        })
    }

    fn project_id(&self) -> i64 {
        self.lease
            .as_ref()
            .expect("Git finalization lease remains present until release")
            .project_id
    }

    fn state_dir(&self) -> &Path {
        &self
            .lease
            .as_ref()
            .expect("Git finalization lease remains present until release")
            .state_dir
    }

    fn heartbeat_error(&self) -> Option<String> {
        self.heartbeat_error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub(super) fn ensure_owned(&self) -> Result<()> {
        if let Some(error) = self.heartbeat_error() {
            anyhow::bail!(error);
        }
        let owned = with_agent_store_at(self.state_dir(), |store| {
            store.git_finalization_lease_is_owned_blocking(
                self.project_id(),
                &self.holder,
                &agent_timestamp(),
            )
        })?;
        if !owned {
            anyhow::bail!(
                "Git finalizer lost its exact project lease for project {}",
                self.project_id()
            );
        }
        if let Some(error) = self.heartbeat_error() {
            anyhow::bail!(error);
        }
        Ok(())
    }

    fn stop_heartbeat(&mut self) -> Result<()> {
        if let Some(stop) = self.stop_heartbeat.take() {
            let _ = stop.send(());
        }
        if let Some(heartbeat) = self.heartbeat.take() {
            heartbeat
                .join()
                .map_err(|_| anyhow::anyhow!("Git finalization lease heartbeat panicked"))?;
        }
        Ok(())
    }

    pub(super) fn release(mut self) -> Result<()> {
        let heartbeat_result = self.stop_heartbeat();
        let fence_result = self.ensure_owned();
        let release_result = self
            .lease
            .take()
            .expect("Git finalization lease is released once")
            .release();
        heartbeat_result?;
        fence_result?;
        release_result
    }
}

impl Drop for AgentGitFinalizationLease {
    fn drop(&mut self) {
        let _ = self.stop_heartbeat();
        drop(self.lease.take());
    }
}

pub(super) fn try_acquire_agent_git_finalization_lease_with_timeout(
    state_dir: &Path,
    project: &agent::AgentProject,
    reclaim_current_process_leases: bool,
    timeout: Duration,
) -> Result<Option<AgentGitFinalizationLease>> {
    let holder = InteractiveAgentLease::holder_for_current_process_with_prefix("clt-git-finalizer");
    let existing = agent_lease_for_project(state_dir, project.id)?;
    let reclaim_holder = existing.as_ref().and_then(|lease| {
        matches!(
            agent_lease_holder_liveness(&lease.holder),
            AgentLeaseHolderLiveness::Dead
        )
        .then_some(lease.holder.as_str())
        .or_else(|| {
            (reclaim_current_process_leases
                && agent_lease_holder_liveness(&lease.holder)
                    == AgentLeaseHolderLiveness::CurrentProcess)
                .then_some(lease.holder.as_str())
        })
    });
    let acquired_at = agent_timestamp();
    let expires_at = agent_timestamp_after(timeout.as_secs().max(1));
    let acquired = with_agent_store_at(state_dir, |store| {
        store.try_acquire_git_finalization_lease_blocking(
            project.id,
            &holder,
            &acquired_at,
            &expires_at,
            reclaim_holder,
        )
    })?;
    if !acquired {
        return Ok(None);
    }
    let lease = InteractiveAgentLease {
        state_dir: state_dir.to_path_buf(),
        project_id: project.id,
        holder,
        released: false,
    };
    AgentGitFinalizationLease::start(lease, timeout).map(Some)
}

pub(super) fn try_acquire_agent_git_finalization_lease(
    state_dir: &Path,
    project: &agent::AgentProject,
    reclaim_current_process_leases: bool,
) -> Result<Option<AgentGitFinalizationLease>> {
    try_acquire_agent_git_finalization_lease_with_timeout(
        state_dir,
        project,
        reclaim_current_process_leases,
        Duration::from_secs(AGENT_GIT_FINALIZATION_LEASE_SECONDS),
    )
}

pub(super) fn record_agent_git_push_retry_error(
    state_dir: &Path,
    project_id: i64,
    error: &anyhow::Error,
    finalization_lease: &AgentGitFinalizationLease,
) -> Result<()> {
    record_agent_git_push_retry_error_message(
        state_dir,
        project_id,
        &format!("{error:#}"),
        finalization_lease,
    )
}

pub(super) fn record_agent_git_push_retry_error_message(
    state_dir: &Path,
    project_id: i64,
    error: &str,
    finalization_lease: &AgentGitFinalizationLease,
) -> Result<()> {
    for _ in 0..3 {
        finalization_lease.ensure_owned()?;
        let finalization = with_agent_store_at(state_dir, |store| {
            Ok(store
                .list_pending_git_finalizations_blocking(Some(project_id))?
                .into_iter()
                .find(|finalization| finalization.state == GitFinalizationState::PushPending))
        })?;
        let Some(finalization) = finalization else {
            return Ok(());
        };
        finalization_lease.ensure_owned()?;
        let changed = with_agent_store_at(state_dir, |store| {
            store.compare_and_set_git_finalization_blocking(
                finalization.project_id,
                &finalization.codex_session_id,
                finalization.generation,
                GitFinalizationState::PushPending,
                finalization.owner_run_token.as_deref(),
                None,
                Some(error),
                &agent_timestamp(),
            )
        })?;
        if changed {
            return Ok(());
        }
    }
    anyhow::bail!(
        "Git push retry state changed repeatedly before CLT could persist its retry backoff"
    )
}

pub(super) fn agent_git_push_retry_backoff_remaining(
    finalizations: &[agent::GitFinalizationRecord],
    now: u64,
    failure_backoff: Duration,
) -> Option<u64> {
    finalizations
        .iter()
        .find(|finalization| {
            finalization.state == GitFinalizationState::PushPending
                && finalization.last_error.is_some()
        })
        .and_then(|finalization| {
            remaining_agent_delay(Some(&finalization.updated_at), now, failure_backoff)
        })
}

pub(super) fn repair_working_git_task_link(
    store: &agent::TursoAgentStore,
    project_root: &Path,
    finalization: &agent::GitFinalizationRecord,
) -> Result<bool> {
    repair_working_git_task_link_with_before_lock(store, project_root, finalization, || {})
}

pub(super) fn exact_working_git_finalization_snapshot(
    current: &agent::GitFinalizationRecord,
    expected: &agent::GitFinalizationRecord,
) -> bool {
    current.state == GitFinalizationState::Working
        && expected.state == GitFinalizationState::Working
        && current.project_id == expected.project_id
        && current.codex_session_id == expected.codex_session_id
        && current.generation == expected.generation
        && current.task_identity == expected.task_identity
        && current.owner_run_token == expected.owner_run_token
        && current.git_mode == expected.git_mode
        && current.starting_head == expected.starting_head
        && current.branch_ref == expected.branch_ref
        && current.upstream_ref == expected.upstream_ref
        && current.worktree_baseline == expected.worktree_baseline
        && current.commit_oid == expected.commit_oid
        && current.created_at == expected.created_at
}

pub(super) fn cancel_orphaned_working_git_finalization(
    store: &agent::TursoAgentStore,
    project_root: &Path,
    expected: &agent::GitFinalizationRecord,
    lease: &AgentGitFinalizationLease,
) -> Result<bool> {
    cancel_orphaned_working_git_finalization_with_before_lock(
        store,
        project_root,
        expected,
        lease,
        || {},
    )
}

pub(super) fn cancel_orphaned_working_git_finalization_with_before_lock(
    store: &agent::TursoAgentStore,
    project_root: &Path,
    expected: &agent::GitFinalizationRecord,
    lease: &AgentGitFinalizationLease,
    before_lock: impl FnOnce(),
) -> Result<bool> {
    if expected.state != GitFinalizationState::Working
        || expected.task_identity.is_some()
        || expected.commit_oid.is_some()
        || expected.owner_run_token.is_some()
    {
        return Ok(false);
    }
    // Validate the baseline, and reject every non-null sealed field even if its
    // type is malformed. Retirement must never discard saved commit proof.
    if AgentGitWorktreeBaseline::from_json(&expected.worktree_baseline).is_err() {
        return Ok(false);
    }
    let baseline: serde_json::Value = serde_json::from_str(&expected.worktree_baseline)?;
    if [
        "staged_non_task_patch_ids",
        "staged_index_tree",
        "manifest_parent_head",
    ]
    .iter()
    .any(|field| baseline.get(field).is_some_and(|value| !value.is_null()))
    {
        return Ok(false);
    }
    anyhow::ensure!(
        lease.project_id() == expected.project_id,
        "Orphan journal recovery requires the matching project lease"
    );
    let registered_root = store
        .list_projects_blocking()?
        .into_iter()
        .find(|project| project.id == expected.project_id)
        .context("Orphan journal project is no longer registered")?
        .path;
    // Registered paths may retain aliases such as macOS /var -> /private/var.
    anyhow::ensure!(
        fs::canonicalize(project_root)? == fs::canonicalize(&registered_root)?,
        "Orphan journal recovery requires the registered project directory"
    );
    lease.ensure_owned()?;
    before_lock();
    let board_dir = get_tasks_dir(project_root);
    let _mutation_lock = acquire_board_mutation_lock(&board_dir)?;
    if task_tree_contains_session_marker(&board_dir, &expected.codex_session_id)? {
        return Ok(false);
    }
    lease.ensure_owned()?;
    store.cancel_orphaned_working_git_finalization_blocking(
        expected,
        &lease.holder,
        "Abandoned unbound Git journal: no task identity or board marker remains",
        &agent_timestamp(),
    )
}

/// Retire only unbound orphan journals; this does not finalize or publish work.
pub(super) fn reconcile_orphaned_agent_git_journals(
    state_dir: &Path,
    project: &agent::AgentProject,
) -> Result<usize> {
    let pending = with_agent_store_at(state_dir, |store| {
        store.list_pending_git_finalizations_blocking(Some(project.id))
    })?;
    if pending.is_empty() {
        return Ok(0);
    }
    let lease = try_acquire_agent_git_finalization_lease(state_dir, project, false)?
        .context("Project is still owned by an agent or interactive session; retry reconciliation after it stops")?;
    let result: Result<usize> = (|| {
        let store = open_agent_store_at(state_dir)?;
        let mut retired = 0;
        for journal in store.list_pending_git_finalizations_blocking(Some(project.id))? {
            if cancel_orphaned_working_git_finalization(&store, &project.path, &journal, &lease)? {
                retired += 1;
            }
        }
        Ok(retired)
    })();
    let released = lease.release();
    let retired = result?;
    released?;
    Ok(retired)
}

pub(super) fn cancel_unlinked_working_git_finalization(
    store: &agent::TursoAgentStore,
    project_root: &Path,
    finalization: &agent::GitFinalizationRecord,
    owner_run_token: &str,
) -> Result<bool> {
    cancel_unlinked_working_git_finalization_with_before_lock(
        store,
        project_root,
        finalization,
        owner_run_token,
        || {},
    )
}

pub(super) fn cancel_unlinked_working_git_finalization_with_before_lock(
    store: &agent::TursoAgentStore,
    project_root: &Path,
    finalization: &agent::GitFinalizationRecord,
    owner_run_token: &str,
    before_lock: impl FnOnce(),
) -> Result<bool> {
    cancel_unlinked_working_git_finalization_with_lock_callbacks(
        store,
        project_root,
        finalization,
        owner_run_token,
        before_lock,
        || {},
        || {},
    )
}

pub(super) fn cancel_unlinked_working_git_finalization_with_lock_callbacks(
    store: &agent::TursoAgentStore,
    project_root: &Path,
    finalization: &agent::GitFinalizationRecord,
    owner_run_token: &str,
    before_lock: impl FnOnce(),
    after_validation: impl FnOnce(),
    on_contention: impl FnOnce(),
) -> Result<bool> {
    before_lock();
    let board_dir = get_tasks_dir(project_root);
    let _mutation_lock =
        acquire_board_mutation_lock_with_contention_callback(&board_dir, on_contention)?;
    if TaskBoard::new(&board_dir)
        .terminal_task_for_session(&finalization.codex_session_id)?
        .is_some()
    {
        return Ok(false);
    }
    let Some(current) =
        store.git_finalization_blocking(finalization.project_id, &finalization.codex_session_id)?
    else {
        return Ok(false);
    };
    if !exact_working_git_finalization_snapshot(&current, finalization)
        || current.owner_run_token.as_deref() != Some(owner_run_token)
    {
        return Ok(false);
    }
    after_validation();
    store.compare_and_set_owned_git_finalization_blocking(
        current.project_id,
        &current.codex_session_id,
        current.generation,
        GitFinalizationState::Cancelled,
        owner_run_token,
        None,
        None,
        &agent_timestamp(),
    )
}

pub(super) fn repair_working_git_task_link_with_before_lock(
    store: &agent::TursoAgentStore,
    project_root: &Path,
    finalization: &agent::GitFinalizationRecord,
    before_lock: impl FnOnce(),
) -> Result<bool> {
    repair_working_git_task_link_with_lock_callbacks(
        store,
        project_root,
        finalization,
        before_lock,
        || {},
        || {},
    )
}

pub(super) fn repair_working_git_task_link_with_lock_callbacks(
    store: &agent::TursoAgentStore,
    project_root: &Path,
    finalization: &agent::GitFinalizationRecord,
    before_lock: impl FnOnce(),
    after_validation: impl FnOnce(),
    on_contention: impl FnOnce(),
) -> Result<bool> {
    let Some(task_identity) = finalization.task_identity.as_deref() else {
        return Ok(false);
    };
    let Some(starting_head) = finalization.starting_head.as_deref() else {
        return Ok(false);
    };
    if !working_git_history_is_safe(store, project_root, finalization, starting_head)? {
        return Ok(false);
    }
    before_lock();
    let board_dir = get_tasks_dir(project_root);
    let _mutation_lock =
        acquire_board_mutation_lock_with_contention_callback(&board_dir, on_contention)?;
    let Some(current) =
        store.git_finalization_blocking(finalization.project_id, &finalization.codex_session_id)?
    else {
        return Ok(false);
    };
    if !exact_working_git_finalization_snapshot(&current, finalization)
        || !working_git_history_is_safe(store, project_root, &current, starting_head)?
    {
        return Ok(false);
    }
    after_validation();
    cleanup_clt_atomic_task_temporaries(&board_dir)?;
    let mut linked = Vec::new();
    for status in [TaskStatus::Todo, TaskStatus::Doing] {
        for (index, entry) in read_task_entries(&board_dir, status)?
            .into_iter()
            .enumerate()
        {
            if codex_session_id_from_task_content(&entry.content)
                == Some(finalization.codex_session_id.as_str())
                && durable_task_identity(&entry.content).as_deref() == Some(task_identity)
            {
                linked.push((status, index + 1, entry));
            }
        }
    }
    match linked.as_slice() {
        [(TaskStatus::Doing, _, _)] => return Ok(true),
        [(TaskStatus::Todo, index, _)] => {
            move_task_without_reordering_after_lock(
                &board_dir,
                TaskStatus::Todo,
                TaskStatus::Doing,
                *index,
            )?;
            return Ok(true);
        }
        [(first_status, _, first), (second_status, _, second)]
            if [*first_status, *second_status].contains(&TaskStatus::Todo)
                && [*first_status, *second_status].contains(&TaskStatus::Doing)
                && first.content.trim_end() == second.content.trim_end()
                && [first, second].iter().all(|entry| {
                    matches!(
                        entry.source,
                        TaskSource::MarkdownLine { .. } | TaskSource::Path { is_dir: false, .. }
                    )
                }) =>
        {
            let (_, _, todo_duplicate) = linked
                .iter()
                .find(|(status, _, _)| *status == TaskStatus::Todo)
                .expect("one linked crash duplicate is in Todo");
            TaskBoard::new(&board_dir)
                .remove_entry_without_reordering(TaskStatus::Todo, todo_duplicate)?;
            return Ok(true);
        }
        [] => {}
        _ => return Ok(false),
    }
    let mut matches = Vec::new();
    for status in [TaskStatus::Todo, TaskStatus::Doing] {
        for (index, entry) in read_task_entries(&board_dir, status)?
            .into_iter()
            .enumerate()
        {
            if durable_task_identity(&entry.content).as_deref() == Some(task_identity) {
                matches.push((status, index + 1, entry));
            }
        }
    }
    let [(status, index, entry)] = matches.as_slice() else {
        return Ok(false);
    };
    attach_codex_session_to_task_after_lock(
        project_root,
        *status,
        entry,
        &finalization.codex_session_id,
        || {},
    )?;
    if *status == TaskStatus::Todo {
        move_task_without_reordering_after_lock(
            &board_dir,
            TaskStatus::Todo,
            TaskStatus::Doing,
            *index,
        )?;
    }
    Ok(true)
}

pub(super) fn working_git_history_is_safe(
    store: &agent::TursoAgentStore,
    project_root: &Path,
    finalization: &agent::GitFinalizationRecord,
    starting_head: &str,
) -> Result<bool> {
    let current_branch = git_optional_stdout(
        project_root,
        &["symbolic-ref", "-q", "HEAD"],
        &[1],
        "verify the Working task repair branch",
    )?;
    if current_branch.as_deref() != finalization.branch_ref.as_deref() {
        return Ok(false);
    }
    let current_head = resolve_git_commit(
        project_root,
        "HEAD",
        "verify the Working task repair history",
    )?;
    if !git_commit_is_ancestor(project_root, starting_head, &current_head)?
        || !agent_git_range_is_safe_before_manifest(
            AgentGitProofContext {
                store,
                project_id: finalization.project_id,
            },
            project_root,
            starting_head,
            &current_head,
            &finalization.codex_session_id,
        )?
    {
        return Ok(false);
    }
    Ok(true)
}

pub(super) fn configure_agent_git_identity(command: &mut Command, git_mode: AgentGitMode) {
    if git_mode == AgentGitMode::Off {
        return;
    }

    command
        .env("GIT_AUTHOR_NAME", AGENT_GIT_IDENTITY_NAME)
        .env("GIT_AUTHOR_EMAIL", AGENT_GIT_IDENTITY_EMAIL)
        .env("GIT_COMMITTER_NAME", AGENT_GIT_IDENTITY_NAME)
        .env("GIT_COMMITTER_EMAIL", AGENT_GIT_IDENTITY_EMAIL);
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AgentGitStartState {
    pub(super) starting_head: String,
    pub(super) branch_ref: Option<String>,
    pub(super) upstream_ref: Option<String>,
    pub(super) worktree_baseline: String,
}

#[derive(Clone, Copy)]
pub(super) struct AgentGitProofContext<'a> {
    pub(super) store: &'a agent::TursoAgentStore,
    pub(super) project_id: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AgentGitUpstreamDestination {
    remote: String,
    merge_ref: String,
    push_url: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AgentGitWorktreeBaseline {
    version: u64,
    tracked_patch_ids: BTreeMap<String, String>,
    untracked_blob_ids: BTreeMap<String, String>,
    require_clean: bool,
    staged_non_task_patch_ids: Option<BTreeMap<String, String>>,
    staged_index_tree: Option<String>,
    manifest_parent_head: Option<String>,
    upstream_remote: Option<String>,
    upstream_merge_ref: Option<String>,
    upstream_push_url: Option<String>,
}

impl Default for AgentGitWorktreeBaseline {
    fn default() -> Self {
        Self {
            version: 2,
            tracked_patch_ids: BTreeMap::new(),
            untracked_blob_ids: BTreeMap::new(),
            require_clean: false,
            staged_non_task_patch_ids: None,
            staged_index_tree: None,
            manifest_parent_head: None,
            upstream_remote: None,
            upstream_merge_ref: None,
            upstream_push_url: None,
        }
    }
}

impl AgentGitWorktreeBaseline {
    fn to_json(&self) -> Result<String> {
        serde_json::to_string(&serde_json::json!({
            "version": self.version,
            "tracked_patch_ids": self.tracked_patch_ids,
            "untracked_blob_ids": self.untracked_blob_ids,
            "require_clean": self.require_clean,
            "staged_non_task_patch_ids": self.staged_non_task_patch_ids,
            "staged_index_tree": self.staged_index_tree,
            "manifest_parent_head": self.manifest_parent_head,
            "upstream_remote": self.upstream_remote,
            "upstream_merge_ref": self.upstream_merge_ref,
            "upstream_push_url": self.upstream_push_url,
        }))
        .context("Failed to serialize the automated Git worktree baseline")
    }

    pub(super) fn from_json(raw: &str) -> Result<Self> {
        let value: serde_json::Value = serde_json::from_str(raw)
            .context("Failed to parse the automated Git worktree baseline")?;
        let version = value.get("version").and_then(serde_json::Value::as_u64);
        if !matches!(version, Some(1 | 2)) {
            anyhow::bail!("Unsupported automated Git worktree baseline version");
        }
        let parse_map = |field: &str| -> Result<BTreeMap<String, String>> {
            let object = value
                .get(field)
                .and_then(serde_json::Value::as_object)
                .with_context(|| format!("Git worktree baseline is missing {field}"))?;
            object
                .iter()
                .map(|(path, value)| {
                    value
                        .as_str()
                        .map(|value| (path.clone(), value.to_string()))
                        .with_context(|| {
                            format!("Git worktree baseline entry {field}.{path} is not text")
                        })
                })
                .collect()
        };
        Ok(Self {
            version: version.expect("supported baseline version"),
            tracked_patch_ids: parse_map("tracked_patch_ids")?,
            untracked_blob_ids: parse_map("untracked_blob_ids")?,
            require_clean: value
                .get("require_clean")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            staged_non_task_patch_ids: value
                .get("staged_non_task_patch_ids")
                .and_then(serde_json::Value::as_object)
                .map(|object| {
                    object
                        .iter()
                        .map(|(path, value)| {
                            value
                                .as_str()
                                .map(|value| (path.clone(), value.to_string()))
                                .with_context(|| {
                                    format!(
                                        "Git worktree baseline entry staged_non_task_patch_ids.{path} is not text"
                                    )
                                })
                        })
                        .collect::<Result<BTreeMap<_, _>>>()
                })
                .transpose()?,
            staged_index_tree: value
                .get("staged_index_tree")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            manifest_parent_head: value
                .get("manifest_parent_head")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            upstream_remote: value
                .get("upstream_remote")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            upstream_merge_ref: value
                .get("upstream_merge_ref")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            upstream_push_url: value
                .get("upstream_push_url")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
        })
    }
}

pub(super) fn ensure_agent_git_index_preflight(
    project: &agent::AgentProject,
    resuming_known_session: bool,
) -> Result<()> {
    if project.git_mode == AgentGitMode::Off || resuming_known_session {
        return Ok(());
    }

    let output = Command::new("git")
        .current_dir(&project.path)
        .args(["diff", "--cached", "--quiet", "--exit-code", "--"])
        .output()
        .with_context(|| {
            format!(
                "Failed to inspect the staged Git index in {}",
                project.path.display()
            )
        })?;
    match output.status.code() {
        Some(0) => Ok(()),
        Some(1) => anyhow::bail!(
            "Refusing to start a fresh automated Git task with pre-existing staged changes; preserve the index and resolve its ownership before retrying"
        ),
        _ => anyhow::bail!(
            "Failed to inspect the staged Git index in {}: {}",
            project.path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ),
    }
}

pub(super) fn prepare_agent_git_start_state_for_run(
    store: &agent::TursoAgentStore,
    project: &agent::AgentProject,
    task_selection: AgentTaskSelection,
    has_known_session: bool,
    has_existing_session_finalization: bool,
    run_token: &str,
) -> Result<Option<AgentGitStartState>> {
    if project.git_mode == AgentGitMode::Off {
        return Ok(None);
    }
    if has_known_session && !has_existing_session_finalization {
        anyhow::bail!(
            "Known Codex session has no frozen Git start journal; CLT will not reconstruct the task boundary from a later checkout"
        );
    }
    if has_existing_session_finalization {
        return Ok(None);
    }
    if task_selection != AgentTaskSelection::NextTodo {
        anyhow::bail!(
            "Git-enabled task recovery has no frozen start journal; only a fresh NextTodo run may establish a new task boundary"
        );
    }
    if store.has_other_git_launch_state_blocking(project.id, run_token)? {
        let (prior_run_token, prior_mode, prior_start) = store
            .git_launch_state_for_project_blocking(project.id)?
            .context("The prior Git launch boundary disappeared during recovery")?;
        let checkout_is_unchanged = prior_mode == project.git_mode
            && verify_agent_git_start_state_unchanged(&project.path, prior_mode, &prior_start)
                .is_ok();
        let reclaimed = checkout_is_unchanged
            && store.reclaim_unchanged_git_launch_state_blocking(
                project.id,
                &prior_run_token,
                prior_mode,
                &prior_start,
            )?;
        if !reclaimed {
            anyhow::bail!(
                "An earlier released Git-enabled run has an unconsumed launch boundary; its exact worker is not proven dead or the checkout changed, so CLT will not overwrite it or start another task"
            );
        }
    }
    if store
        .git_launch_state_blocking(project.id, run_token)?
        .is_some()
    {
        anyhow::bail!(
            "Automated run {run_token} already has an unconsumed Git launch boundary; CLT will not recapture it from a later checkout"
        );
    }
    let project_has_working_boundary = store
        .list_pending_git_finalizations_blocking(Some(project.id))?
        .into_iter()
        .any(|finalization| finalization.state == GitFinalizationState::Working);
    if task_selection == AgentTaskSelection::NextTodo && !project_has_working_boundary {
        synchronize_agent_git_checkout_before_launch(&project.path)?;
    }
    {
        let board_dir = get_tasks_dir(&project.path);
        let _mutation_lock = acquire_board_mutation_lock(&board_dir)?;
        cleanup_clt_atomic_task_temporaries(&board_dir)?;
        require_agent_git_board_storage_compatible(&project.path)?;
        checkpoint_agent_git_task_board_before_launch(&project.path)?;
    }
    let start = capture_agent_git_start_state(&project.path, project.git_mode)?;
    require_agent_git_todo_candidates_committed(&project.path, &start.starting_head)?;
    Ok(Some(start))
}

pub(super) fn checkpoint_agent_git_task_board_before_launch(
    project_root: &Path,
) -> Result<Option<String>> {
    require_agent_git_index_matches_head(project_root)?;
    let starting_head = resolve_git_commit(
        project_root,
        "HEAD",
        "resolve the parent for the automated task-board checkpoint",
    )?;
    let branch_ref = git_optional_stdout(
        project_root,
        &["symbolic-ref", "-q", "HEAD"],
        &[1],
        "resolve the branch for the automated task-board checkpoint",
    )?
    .context("Git-enabled automated tasks require an attached branch before CLT checkpoints the task board")?;
    let starting_tree = git_stdout(
        project_root,
        &[
            "rev-parse",
            "--verify",
            &format!("{starting_head}^{{tree}}"),
        ],
        "resolve the tree before the automated task-board checkpoint",
    )?;
    let (_projection, index_path, _) =
        create_agent_git_tree_projection(project_root, &starting_head)?;
    run_agent_git_projection_command(
        project_root,
        &index_path,
        None,
        &["add", "-A", "--", "tasks"],
        "stage the prelaunch task-board checkpoint",
    )?;
    let checkpoint_tree = run_agent_git_projection_command(
        project_root,
        &index_path,
        None,
        &["write-tree"],
        "snapshot the prelaunch task board",
    )?;
    if checkpoint_tree == starting_tree {
        return Ok(None);
    }

    require_agent_git_index_matches_head(project_root)?;
    let rechecked_head = resolve_git_commit(
        project_root,
        "HEAD",
        "recheck the task-board checkpoint parent",
    )?;
    let rechecked_branch = git_optional_stdout(
        project_root,
        &["symbolic-ref", "-q", "HEAD"],
        &[1],
        "recheck the task-board checkpoint branch",
    )?;
    run_agent_git_projection_command(
        project_root,
        &index_path,
        None,
        &["add", "-A", "--", "tasks"],
        "recheck the prelaunch task-board checkpoint",
    )?;
    let rechecked_tree = run_agent_git_projection_command(
        project_root,
        &index_path,
        None,
        &["write-tree"],
        "recheck the prelaunch task-board tree",
    )?;
    if rechecked_head != starting_head
        || rechecked_branch.as_deref() != Some(branch_ref.as_str())
        || rechecked_tree != checkpoint_tree
    {
        anyhow::bail!(
            "Git HEAD, branch, index, or task board changed while CLT was preparing its prelaunch checkpoint; retry"
        );
    }

    let output = Command::new("git")
        .current_dir(project_root)
        .env("GIT_AUTHOR_NAME", AGENT_GIT_IDENTITY_NAME)
        .env("GIT_AUTHOR_EMAIL", AGENT_GIT_IDENTITY_EMAIL)
        .env("GIT_COMMITTER_NAME", AGENT_GIT_IDENTITY_NAME)
        .env("GIT_COMMITTER_EMAIL", AGENT_GIT_IDENTITY_EMAIL)
        .args([
            "commit-tree",
            checkpoint_tree.as_str(),
            "-p",
            starting_head.as_str(),
            "-m",
            "Record CLT task board",
        ])
        .output()
        .with_context(|| {
            format!(
                "Failed to create the automated task-board checkpoint in {}",
                project_root.display()
            )
        })?;
    if !output.status.success() {
        anyhow::bail!(
            "Failed to create the automated task-board checkpoint in {}: {}",
            project_root.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let checkpoint_commit = String::from_utf8(output.stdout)
        .context("Git returned a non-UTF-8 task-board checkpoint commit")?
        .trim()
        .to_string();

    require_agent_git_index_matches_head(project_root)?;
    if resolve_git_commit(
        project_root,
        "HEAD",
        "recheck the task-board checkpoint parent",
    )? != starting_head
        || git_optional_stdout(
            project_root,
            &["symbolic-ref", "-q", "HEAD"],
            &[1],
            "recheck the task-board checkpoint branch",
        )?
        .as_deref()
            != Some(branch_ref.as_str())
    {
        anyhow::bail!(
            "Git HEAD, branch, or index changed before CLT could publish its task-board checkpoint; retry"
        );
    }
    git_stdout(
        project_root,
        &[
            "update-ref",
            branch_ref.as_str(),
            checkpoint_commit.as_str(),
            starting_head.as_str(),
        ],
        "publish the automated task-board checkpoint",
    )?;
    git_stdout(
        project_root,
        &[
            "reset",
            "--quiet",
            checkpoint_commit.as_str(),
            "--",
            "tasks",
        ],
        "align the task-board index with its automated checkpoint",
    )?;
    require_agent_git_index_matches_head(project_root)?;
    let remaining = capture_agent_git_worktree_baseline(project_root)?;
    if remaining
        .tracked_patch_ids
        .keys()
        .chain(remaining.untracked_blob_ids.keys())
        .any(|path| path == "tasks" || path.starts_with("tasks/"))
    {
        anyhow::bail!(
            "The task board changed while CLT was publishing its prelaunch checkpoint; retry"
        );
    }

    Ok(Some(checkpoint_commit))
}

pub(super) fn require_agent_git_todo_candidates_committed(
    project_root: &Path,
    starting_head: &str,
) -> Result<()> {
    let candidates = read_task_entries(&get_tasks_dir(project_root), TaskStatus::Todo)?
        .into_iter()
        .filter(|entry| !task_entry_is_blocked(entry))
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        anyhow::bail!("Fresh Git-enabled automation has no unblocked Todo task to start");
    }
    for candidate in candidates {
        let task_identity = durable_task_identity(&candidate.content)
            .context("A Todo candidate has no durable task identity")?;
        require_agent_git_start_task_identity(project_root, starting_head, &task_identity)
            .with_context(|| {
                format!(
                    "Todo candidate {:?} is not committed exactly once at the frozen task boundary",
                    candidate.summary
                )
            })?;
    }
    Ok(())
}

pub(super) fn require_agent_git_board_storage_compatible(project_root: &Path) -> Result<()> {
    let board_dir = get_tasks_dir(project_root);
    let todo_is_directory = matches!(
        get_status_store(&board_dir, TaskStatus::Todo)?,
        StatusStore::Directory(_)
    );
    let doing_is_directory = matches!(
        get_status_store(&board_dir, TaskStatus::Doing)?,
        StatusStore::Directory(_)
    );
    let done_is_directory = matches!(
        get_status_store(&board_dir, TaskStatus::Done)?,
        StatusStore::Directory(_)
    );
    if todo_is_directory && !doing_is_directory {
        anyhow::bail!(
            "Git-enabled automation requires folder-backed Doing storage when Todo is folder-backed; expand and commit the board layout before scheduling"
        );
    }
    if doing_is_directory && !done_is_directory {
        anyhow::bail!(
            "Git-enabled automation requires folder-backed Done storage when Doing is folder-backed; expand and commit the board layout before scheduling"
        );
    }
    Ok(())
}

pub(super) fn synchronize_agent_git_checkout_before_launch(project_root: &Path) -> Result<()> {
    let branch_ref = git_optional_stdout(
        project_root,
        &["symbolic-ref", "-q", "HEAD"],
        &[1],
        "resolve the branch for automated startup synchronization",
    )?;
    let Some(branch_ref) = branch_ref else {
        return Ok(());
    };
    let upstream_ref = resolve_agent_git_upstream(project_root, Some(&branch_ref))?;
    let Some(upstream_ref) = upstream_ref else {
        return Ok(());
    };
    let starting_head = resolve_git_commit(
        project_root,
        "HEAD",
        "resolve the commit before automated startup synchronization",
    )?;
    let mut command = Command::new("git");
    command
        .current_dir(project_root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(["pull", "--ff-only", "--no-rebase"]);
    let output = run_agent_git_remote_command(
        &mut command,
        &format!("fast-forward the automated checkout from {upstream_ref}"),
    )?;
    if !output.status.success() {
        anyhow::bail!(
            "Failed to fast-forward the automated checkout from {upstream_ref}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    require_agent_git_index_matches_head(project_root)?;
    let current_branch = git_optional_stdout(
        project_root,
        &["symbolic-ref", "-q", "HEAD"],
        &[1],
        "recheck the branch after automated startup synchronization",
    )?;
    let current_upstream = resolve_agent_git_upstream(project_root, current_branch.as_deref())?;
    let current_head = resolve_git_commit(
        project_root,
        "HEAD",
        "recheck the commit after automated startup synchronization",
    )?;
    if current_branch.as_deref() != Some(branch_ref.as_str())
        || current_upstream.as_deref() != Some(upstream_ref.as_str())
        || !git_commit_is_ancestor(project_root, &starting_head, &current_head)?
    {
        anyhow::bail!(
            "Git branch, upstream, or history changed incompatibly during automated startup synchronization"
        );
    }
    Ok(())
}

pub(super) fn git_nul_separated_paths(
    project_root: &Path,
    args: &[&str],
    operation: &str,
) -> Result<Vec<String>> {
    let output = Command::new("git")
        .current_dir(project_root)
        .args(args)
        .output()
        .with_context(|| format!("Failed to {operation} in {}", project_root.display()))?;
    if !output.status.success() {
        anyhow::bail!(
            "Failed to {operation} in {}: {}",
            project_root.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| {
            std::str::from_utf8(path)
                .with_context(|| {
                    format!("Git returned a non-UTF-8 path while trying to {operation}")
                })
                .map(str::to_string)
        })
        .collect()
}

#[cfg(test)]
mod tests;

pub(super) fn git_hash_stdin(project_root: &Path, bytes: &[u8], operation: &str) -> Result<String> {
    let mut child = Command::new("git")
        .current_dir(project_root)
        .args(["hash-object", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("Failed to start Git while trying to {operation}"))?;
    child
        .stdin
        .take()
        .context("git hash-object did not expose stdin")?
        .write_all(bytes)
        .with_context(|| format!("Failed to send content to Git while trying to {operation}"))?;
    let output = child
        .wait_with_output()
        .with_context(|| format!("Failed to finish Git while trying to {operation}"))?;
    if !output.status.success() {
        anyhow::bail!(
            "Failed to {operation}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    String::from_utf8(output.stdout)
        .with_context(|| format!("Git returned non-UTF-8 output while trying to {operation}"))
        .map(|value| value.trim().to_string())
}

pub(super) fn git_delta_id_for_path(
    project_root: &Path,
    diff_arguments: &[&str],
    path: &str,
) -> Result<String> {
    let diff = Command::new("git")
        .current_dir(project_root)
        .arg("diff")
        .args(diff_arguments)
        .args([
            "--no-ext-diff",
            "--binary",
            "--full-index",
            "--unified=0",
            "--",
            path,
        ])
        .output()
        .with_context(|| {
            format!(
                "Failed to read the Git delta for {path} in {}",
                project_root.display()
            )
        })?;
    if !diff.status.success() {
        anyhow::bail!(
            "Failed to read the Git delta for {path} in {}: {}",
            project_root.display(),
            String::from_utf8_lossy(&diff.stderr).trim()
        );
    }
    if diff.stdout.is_empty() {
        anyhow::bail!("Git state changed while CLT was capturing the delta for {path}");
    }

    let mut canonical = Vec::new();
    let mut in_binary_patch = false;
    let mut in_hunk = false;
    for line in diff.stdout.split_inclusive(|byte| *byte == b'\n') {
        if line.starts_with(b"GIT binary patch") {
            in_binary_patch = true;
        }
        if line.starts_with(b"@@") {
            in_hunk = true;
            continue;
        }
        if in_binary_patch
            || line.starts_with(b"+")
            || line.starts_with(b"-")
            || line.starts_with(b"old mode ")
            || line.starts_with(b"new mode ")
            || line.starts_with(b"new file mode ")
            || line.starts_with(b"deleted file mode ")
            || line.starts_with(b"rename from ")
            || line.starts_with(b"rename to ")
            || line.starts_with(b"copy from ")
            || line.starts_with(b"copy to ")
            || line.starts_with(b"\\ No newline at end of file")
        {
            if !in_hunk && (line.starts_with(b"--- ") || line.starts_with(b"+++ ")) {
                continue;
            }
            canonical.extend_from_slice(line);
        }
    }
    if canonical.is_empty() {
        anyhow::bail!("Git produced no canonical delta for {path}");
    }
    git_hash_stdin(
        project_root,
        &canonical,
        &format!("fingerprint the exact Git delta for {path}"),
    )
}

pub(super) fn git_worktree_delta_id_for_path(project_root: &Path, path: &str) -> Result<String> {
    git_delta_id_for_path(project_root, &[], path)
}

pub(super) fn git_untracked_blob_id(project_root: &Path, path: &str) -> Result<String> {
    git_stdout(
        project_root,
        &["hash-object", "--no-filters", "--", path],
        "fingerprint an untracked worktree file",
    )
}

pub(super) fn capture_agent_git_worktree_baseline(
    project_root: &Path,
) -> Result<AgentGitWorktreeBaseline> {
    capture_agent_git_worktree_state(project_root)
}

pub(super) fn is_agent_git_task_board_path(path: &str) -> bool {
    path == "tasks" || path.starts_with("tasks/")
}

pub(super) fn require_agent_git_index_matches_head(project_root: &Path) -> Result<()> {
    let cached = Command::new("git")
        .current_dir(project_root)
        .args(["diff", "--cached", "--quiet", "--exit-code", "--"])
        .output()
        .with_context(|| {
            format!(
                "Failed to verify the staged Git index in {}",
                project_root.display()
            )
        })?;
    match cached.status.code() {
        Some(0) => Ok(()),
        Some(1) => {
            anyhow::bail!("Automated Git finalization requires the staged index to match HEAD")
        }
        _ => anyhow::bail!(
            "Failed to verify the staged Git index in {}: {}",
            project_root.display(),
            String::from_utf8_lossy(&cached.stderr).trim()
        ),
    }
}

pub(super) fn capture_agent_git_worktree_state(
    project_root: &Path,
) -> Result<AgentGitWorktreeBaseline> {
    let tracked_paths = git_nul_separated_paths(
        project_root,
        &["diff", "--name-only", "-z", "--"],
        "list modified tracked files",
    )?;
    let untracked_paths = git_nul_separated_paths(
        project_root,
        &["ls-files", "--others", "--exclude-standard", "-z", "--"],
        "list untracked files",
    )?;
    let mut baseline = AgentGitWorktreeBaseline::default();
    for path in tracked_paths {
        baseline.tracked_patch_ids.insert(
            path.clone(),
            git_worktree_delta_id_for_path(project_root, &path)?,
        );
    }
    for path in untracked_paths {
        baseline
            .untracked_blob_ids
            .insert(path.clone(), git_untracked_blob_id(project_root, &path)?);
    }
    Ok(baseline)
}

pub(super) fn worktree_matches_agent_git_baseline(
    project_root: &Path,
    raw_baseline: &str,
) -> Result<bool> {
    let expected = AgentGitWorktreeBaseline::from_json(raw_baseline)?;
    let current = capture_agent_git_worktree_baseline(project_root)?;
    if expected.require_clean {
        return Ok(current.tracked_patch_ids.is_empty() && current.untracked_blob_ids.is_empty());
    }
    // The private staged-tree proof below owns task-board commit scope. Ignore raw
    // board changes here so a person can add another Todo while the agent works;
    // they remain outside the index and therefore outside the sealed task commit.
    let current_non_task_is_subset = current
        .tracked_patch_ids
        .iter()
        .filter(|(path, _)| !is_agent_git_task_board_path(path))
        .all(|(path, patch_id)| expected.tracked_patch_ids.get(path) == Some(patch_id))
        && current
            .untracked_blob_ids
            .iter()
            .filter(|(path, _)| !is_agent_git_task_board_path(path))
            .all(|(path, blob_id)| expected.untracked_blob_ids.get(path) == Some(blob_id));
    let non_task_baseline_preserved = expected
        .tracked_patch_ids
        .iter()
        .filter(|(path, _)| !is_agent_git_task_board_path(path))
        .all(|(path, patch_id)| current.tracked_patch_ids.get(path) == Some(patch_id))
        && expected
            .untracked_blob_ids
            .iter()
            .filter(|(path, _)| !is_agent_git_task_board_path(path))
            .all(|(path, blob_id)| current.untracked_blob_ids.get(path) == Some(blob_id));
    Ok(current_non_task_is_subset && non_task_baseline_preserved)
}

pub(super) fn git_ref_has_one_active_session_task(
    project_root: &Path,
    reference: &str,
    session_id: &str,
    task_identity: &str,
) -> Result<bool> {
    let entries = git_ref_task_entries(project_root, reference)?;
    let marker_count = entries
        .iter()
        .flat_map(|entry| codex_session_markers_in_task_content(&entry.content))
        .filter(|(_, _, candidate)| *candidate == session_id)
        .count();
    let active_count = entries
        .iter()
        .filter(|entry| {
            matches!(entry.status.as_str(), "todo" | "doing")
                && codex_session_id_from_task_content(&entry.content) == Some(session_id)
                && durable_task_identity(&entry.content).as_deref() == Some(task_identity)
        })
        .count();
    Ok(marker_count == 1 && active_count == 1)
}

pub(super) fn git_ref_has_one_completed_session_task(
    project_root: &Path,
    reference: &str,
    session_id: &str,
    task_identity: &str,
) -> Result<bool> {
    let entries = git_ref_task_entries(project_root, reference)?;
    let marker_count = entries
        .iter()
        .flat_map(|entry| codex_session_markers_in_task_content(&entry.content))
        .filter(|(_, _, candidate)| *candidate == session_id)
        .count();
    let completed_count = entries
        .iter()
        .filter(|entry| {
            entry.status == "done"
                && codex_session_id_from_task_content(&entry.content) == Some(session_id)
                && durable_task_identity(&entry.content).as_deref() == Some(task_identity)
                && task_content_has_completed_note(&entry.content)
        })
        .count();
    Ok(marker_count == 1 && completed_count == 1)
}

pub(super) struct AgentGitTreeProjection {
    root: PathBuf,
}

impl Drop for AgentGitTreeProjection {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

pub(super) fn run_agent_git_projection_command(
    project_root: &Path,
    index_path: &Path,
    worktree_path: Option<&Path>,
    args: &[&str],
    operation: &str,
) -> Result<String> {
    let mut command = Command::new("git");
    command
        .current_dir(project_root)
        .env("GIT_INDEX_FILE", index_path)
        .args(args);
    if let Some(worktree_path) = worktree_path {
        command.env("GIT_WORK_TREE", worktree_path);
    }
    let output = command.output().with_context(|| {
        format!(
            "Failed to {operation} while projecting the sealed task tree in {}",
            project_root.display()
        )
    })?;
    if !output.status.success() {
        anyhow::bail!(
            "Failed to {operation} while projecting the sealed task tree in {}: {}",
            project_root.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    String::from_utf8(output.stdout)
        .with_context(|| format!("Git returned non-UTF-8 output while trying to {operation}"))
        .map(|value| value.trim().to_string())
}

pub(super) fn materialize_agent_git_task_tree(
    project_root: &Path,
    index_path: &Path,
    staged_tree: &str,
    checkout_prefix: &str,
) -> Result<()> {
    let task_paths = Command::new("git")
        .current_dir(project_root)
        .args([
            "ls-tree",
            "-r",
            "-z",
            "--name-only",
            staged_tree,
            "--",
            "tasks",
        ])
        .output()
        .with_context(|| {
            format!(
                "Failed to list the staged task tree in {}",
                project_root.display()
            )
        })?;
    if !task_paths.status.success() {
        anyhow::bail!(
            "Failed to list the staged task tree in {}: {}",
            project_root.display(),
            String::from_utf8_lossy(&task_paths.stderr).trim()
        );
    }
    let checkout_argument = format!("--prefix={checkout_prefix}");
    let mut child = Command::new("git")
        .current_dir(project_root)
        .env("GIT_INDEX_FILE", index_path)
        .args([
            "checkout-index",
            "--force",
            "--stdin",
            "-z",
            &checkout_argument,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Failed to start Git while materializing the staged task tree")?;
    child
        .stdin
        .take()
        .context("git checkout-index did not expose stdin")?
        .write_all(&task_paths.stdout)
        .context("Failed to send staged task paths to git checkout-index")?;
    let output = child
        .wait_with_output()
        .context("Failed to finish materializing the staged task tree")?;
    if !output.status.success() {
        anyhow::bail!(
            "Failed to materialize the staged task tree in {}: {}",
            project_root.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

pub(super) fn create_agent_git_tree_projection(
    project_root: &Path,
    source_tree: &str,
) -> Result<(AgentGitTreeProjection, PathBuf, PathBuf)> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let projection_root = std::env::temp_dir().join(format!(
        "clt-git-finalization-{}-{nonce}",
        std::process::id()
    ));
    let mut projection_builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        projection_builder.mode(0o700);
    }
    projection_builder
        .create(&projection_root)
        .with_context(|| {
            format!(
                "Failed to create sealed task projection directory {:?}",
                projection_root
            )
        })?;
    let projection = AgentGitTreeProjection {
        root: projection_root,
    };
    let index_path = projection.root.join("index");
    let worktree_path = projection.root.join("worktree");
    fs::create_dir(&worktree_path).with_context(|| {
        format!(
            "Failed to create sealed task projection worktree {:?}",
            worktree_path
        )
    })?;
    run_agent_git_projection_command(
        project_root,
        &index_path,
        None,
        &["read-tree", source_tree],
        "load the staged task tree",
    )?;
    let mut checkout_prefix = worktree_path.as_os_str().to_os_string();
    checkout_prefix.push(std::path::MAIN_SEPARATOR.to_string());
    let checkout_prefix = checkout_prefix
        .to_str()
        .context("Sealed task projection path is not valid UTF-8")?;
    materialize_agent_git_task_tree(project_root, &index_path, source_tree, checkout_prefix)?;
    Ok((projection, index_path, worktree_path))
}

pub(super) fn git_task_subtree(project_root: &Path, root_tree: &str) -> Result<String> {
    let tasks_reference = format!("{root_tree}:tasks");
    git_stdout(
        project_root,
        &["rev-parse", "--verify", &tasks_reference],
        "resolve the exact task-board tree",
    )
}

pub(super) fn stage_projected_task_tree(
    project_root: &Path,
    index_path: &Path,
    worktree_path: &Path,
    operation: &str,
) -> Result<String> {
    run_agent_git_projection_command(
        project_root,
        index_path,
        Some(worktree_path),
        &["add", "-A", "--", "tasks"],
        operation,
    )?;
    run_agent_git_projection_command(
        project_root,
        index_path,
        Some(worktree_path),
        &["write-tree"],
        operation,
    )
}

pub(super) fn projected_task_entry(
    board_dir: &Path,
    statuses: &[TaskStatus],
    session_id: Option<&str>,
    task_identity: &str,
) -> Result<(TaskStatus, usize, TaskEntry)> {
    let mut selected = Vec::new();
    for status in statuses {
        for (index, entry) in read_task_entries(board_dir, *status)?
            .into_iter()
            .enumerate()
        {
            if session_id.is_none_or(|session_id| {
                codex_session_id_from_task_content(&entry.content) == Some(session_id)
            }) && durable_task_identity(&entry.content).as_deref() == Some(task_identity)
            {
                selected.push((*status, index + 1, entry));
            }
        }
    }
    let [selected] = selected.as_slice() else {
        anyhow::bail!("The task projection must contain exactly one matching task");
    };
    Ok(selected.clone())
}

pub(super) fn agent_git_task_scope_without_selected(
    project_root: &Path,
    source_tree: &str,
    task_identity: &str,
) -> Result<String> {
    let (_projection, index_path, worktree_path) =
        create_agent_git_tree_projection(project_root, source_tree)?;
    let board_dir = get_tasks_dir(&worktree_path);
    let mut selected = Vec::new();
    for status in [TaskStatus::Todo, TaskStatus::Doing] {
        for entry in read_task_entries(&board_dir, status)? {
            if durable_task_identity(&entry.content).as_deref() == Some(task_identity) {
                selected.push((status, entry));
            }
        }
    }
    match selected.as_slice() {
        [] => anyhow::bail!(
            "The selected Git-enabled task is not present in the checkpointed manifest parent"
        ),
        [(status, entry)] => remove_task_entry_without_reordering(&board_dir, *status, entry)?,
        _ => {
            anyhow::bail!(
                "The Git manifest parent contains more than one active task with the selected identity"
            )
        }
    }
    let sanitized_tree = stage_projected_task_tree(
        project_root,
        &index_path,
        &worktree_path,
        "sanitize the selected task from the manifest parent",
    )?;
    git_task_subtree(project_root, &sanitized_tree)
}

pub(super) fn agent_git_completed_scope_without_selected(
    project_root: &Path,
    completed_tree: &str,
    parent_tree: &str,
    session_id: &str,
    task_identity: &str,
) -> Result<String> {
    let (_projection, index_path, worktree_path) =
        create_agent_git_tree_projection(project_root, completed_tree)?;
    let board_dir = get_tasks_dir(&worktree_path);
    let (_, _, entry) = projected_task_entry(
        &board_dir,
        &[TaskStatus::Done],
        Some(session_id),
        task_identity,
    )?;
    remove_task_entry_without_reordering(&board_dir, TaskStatus::Done, &entry)?;
    // Only new, explicitly linked blocked follow-ups may accompany the selected
    // task. Removing them from this private projection must restore the exact
    // parent board; edits to existing tasks, headers, paths and attachments still fail.
    let parent_entries = git_ref_task_entries(project_root, parent_tree)?;
    let follow_ups = read_task_entries(&board_dir, TaskStatus::Doing)?
        .into_iter()
        .filter(|entry| {
            blocked_follow_up_session(&entry.content) == Some(session_id)
                && match &entry.source {
                    TaskSource::MarkdownLine { .. } => true,
                    TaskSource::Path {
                        path,
                        is_dir: false,
                    } => fs::symlink_metadata(path)
                        .is_ok_and(|metadata| metadata.file_type().is_file()),
                    TaskSource::Path { is_dir: true, .. } => false,
                }
                && !parent_entries.iter().any(|parent| {
                    parent.status == "doing"
                        && parent.content.trim_end() == entry.content.trim_end()
                })
        })
        .collect::<Vec<_>>();
    anyhow::ensure!(
        follow_ups.len() <= 1,
        "Only one new blocked follow-up may accompany a task commit"
    );
    for follow_up in follow_ups.iter().rev() {
        remove_task_entry_without_reordering(&board_dir, TaskStatus::Doing, follow_up)?;
    }
    let sanitized_tree = stage_projected_task_tree(
        project_root,
        &index_path,
        &worktree_path,
        "sanitize the selected task from the completed manifest",
    )?;
    git_task_subtree(project_root, &sanitized_tree)
}

pub(super) fn project_agent_git_completed_tree(
    project_root: &Path,
    staged_tree: &str,
    session_id: &str,
    task_identity: &str,
    parent_tree: &str,
    expected_task_scope_tree: &str,
) -> Result<String> {
    let (_projection, index_path, worktree_path) =
        create_agent_git_tree_projection(project_root, staged_tree)?;
    let board_dir = get_tasks_dir(&worktree_path);
    let (status, task_index, _) = projected_task_entry(
        &board_dir,
        &[TaskStatus::Todo, TaskStatus::Doing],
        Some(session_id),
        task_identity,
    )?;
    move_task_without_reordering_after_lock(&board_dir, status, TaskStatus::Done, task_index)?;
    let completed_tree = stage_projected_task_tree(
        project_root,
        &index_path,
        &worktree_path,
        "stage the projected Done transition",
    )?;
    if git_ref_completed_task_identity(project_root, &completed_tree, session_id)?.as_deref()
        != Some(task_identity)
    {
        anyhow::bail!("CLT could not project the selected task into an exact completed Git tree");
    }
    if agent_git_completed_scope_without_selected(
        project_root,
        &completed_tree,
        parent_tree,
        session_id,
        task_identity,
    )? != expected_task_scope_tree
    {
        anyhow::bail!(
            "The staged task board contains raw changes outside the selected task; leave unrelated task files, ordering, headers, archives, and attachments unstaged"
        );
    }
    Ok(completed_tree)
}

pub(super) fn capture_agent_git_staged_manifest(
    proof: AgentGitProofContext<'_>,
    project_root: &Path,
    raw_baseline: &str,
    session_id: &str,
    task_identity: &str,
    starting_head: &str,
    branch_ref: Option<&str>,
) -> Result<String> {
    let baseline = AgentGitWorktreeBaseline::from_json(raw_baseline)?;
    if !worktree_matches_agent_git_baseline(project_root, raw_baseline)? {
        anyhow::bail!(
            "Stage every task-owned change before `clt done`; remaining unstaged and untracked non-task work must match the pre-task baseline, while concurrent task-board edits must stay unstaged"
        );
    }
    let manifest_parent_head = resolve_git_commit(
        project_root,
        "HEAD",
        "freeze the staged task manifest parent",
    )?;
    if !git_commit_is_ancestor(project_root, starting_head, &manifest_parent_head)?
        || !agent_git_range_is_safe_before_manifest(
            proof,
            project_root,
            starting_head,
            &manifest_parent_head,
            session_id,
        )?
    {
        anyhow::bail!(
            "Automated Git completion found an unproven intervening commit before the sealed manifest; keep the implementation and Done transition in one task commit"
        );
    }
    let expected_task_scope_tree =
        agent_git_task_scope_without_selected(project_root, &manifest_parent_head, task_identity)?;
    let staged_paths = git_nul_separated_paths(
        project_root,
        &["diff", "--cached", "--name-only", "-z", "--"],
        "list staged task files",
    )?;
    if staged_paths.is_empty() {
        anyhow::bail!(
            "Automated Git completion requires the verified task changes to be staged before `clt done`"
        );
    }
    let mut staged_non_task_patch_ids = BTreeMap::new();
    for path in staged_paths
        .iter()
        .filter(|path| !path.starts_with("tasks/"))
    {
        staged_non_task_patch_ids.insert(
            path.clone(),
            git_delta_id_for_path(project_root, &["--cached"], path)?,
        );
    }
    let staged_index_tree = git_stdout(
        project_root,
        &["write-tree"],
        "snapshot the staged task manifest",
    )?;
    if !git_ref_has_one_active_session_task(
        project_root,
        &staged_index_tree,
        session_id,
        task_identity,
    )? {
        anyhow::bail!(
            "Stage the selected Doing task, including its terminal codex:{session_id} marker and COMPLETED note, before `clt done`"
        );
    }
    let sealed_commit_tree = project_agent_git_completed_tree(
        project_root,
        &staged_index_tree,
        session_id,
        task_identity,
        &manifest_parent_head,
        &expected_task_scope_tree,
    )?;
    let rechecked_parent =
        resolve_git_commit(project_root, "HEAD", "recheck the staged manifest parent")?;
    let rechecked_tree = git_stdout(
        project_root,
        &["write-tree"],
        "recheck the staged task manifest",
    )?;
    let rechecked_branch = git_optional_stdout(
        project_root,
        &["symbolic-ref", "-q", "HEAD"],
        &[1],
        "recheck the frozen task branch",
    )?;
    if rechecked_parent != manifest_parent_head
        || rechecked_tree != staged_index_tree
        || rechecked_branch.as_deref() != branch_ref
        || !git_commit_is_ancestor(project_root, starting_head, &rechecked_parent)?
        || !worktree_matches_agent_git_baseline(project_root, raw_baseline)?
    {
        anyhow::bail!(
            "Git HEAD, index, or unstaged work changed while CLT was sealing the task manifest; retry `clt done`"
        );
    }

    let mut baseline = baseline;
    baseline.staged_non_task_patch_ids = Some(staged_non_task_patch_ids);
    baseline.staged_index_tree = Some(sealed_commit_tree);
    baseline.manifest_parent_head = Some(manifest_parent_head);
    baseline.to_json()
}

pub(super) fn capture_agent_git_resealed_manifest(
    proof: AgentGitProofContext<'_>,
    project_root: &Path,
    raw_baseline: &str,
    session_id: &str,
    task_identity: &str,
    starting_head: &str,
    branch_ref: Option<&str>,
) -> Result<String> {
    let mut baseline = AgentGitWorktreeBaseline::from_json(raw_baseline)?;
    if baseline.version < 2 {
        anyhow::bail!("Legacy Git finalizations cannot be resealed");
    }
    if !worktree_matches_agent_git_baseline(project_root, raw_baseline)? {
        anyhow::bail!(
            "Stage every corrected task-owned change before resealing; remaining unstaged and untracked non-task work must match the pre-task baseline, while concurrent task-board edits must stay unstaged"
        );
    }
    let manifest_parent_head =
        resolve_git_commit(project_root, "HEAD", "freeze the corrected manifest parent")?;
    let current_branch = git_optional_stdout(
        project_root,
        &["symbolic-ref", "-q", "HEAD"],
        &[1],
        "verify the corrected manifest branch",
    )?;
    if current_branch.as_deref() != branch_ref
        || !git_commit_is_ancestor(project_root, starting_head, &manifest_parent_head)?
        || git_ref_contains_completed_task(project_root, &manifest_parent_head, session_id)?
        || !agent_git_range_is_safe_before_manifest(
            proof,
            project_root,
            starting_head,
            &manifest_parent_head,
            session_id,
        )?
    {
        anyhow::bail!(
            "The branch or history changed incompatibly before the provisional Done manifest could be resealed"
        );
    }
    let staged_paths = git_nul_separated_paths(
        project_root,
        &["diff", "--cached", "--name-only", "-z", "--"],
        "list corrected staged task files",
    )?;
    if staged_paths.is_empty() {
        anyhow::bail!("Resealing requires the complete corrected task commit to be staged");
    }
    let mut staged_non_task_patch_ids = BTreeMap::new();
    for path in staged_paths
        .iter()
        .filter(|path| !path.starts_with("tasks/"))
    {
        staged_non_task_patch_ids.insert(
            path.clone(),
            git_delta_id_for_path(project_root, &["--cached"], path)?,
        );
    }
    let sealed_commit_tree = git_stdout(
        project_root,
        &["write-tree"],
        "snapshot the corrected completed-task manifest",
    )?;
    if !git_ref_has_one_completed_session_task(
        project_root,
        &sealed_commit_tree,
        session_id,
        task_identity,
    )? {
        anyhow::bail!(
            "The corrected staged index must contain exactly one completed task for Codex session {session_id}"
        );
    }
    let expected_task_scope_tree =
        agent_git_task_scope_without_selected(project_root, &manifest_parent_head, task_identity)?;
    if agent_git_completed_scope_without_selected(
        project_root,
        &sealed_commit_tree,
        &manifest_parent_head,
        session_id,
        task_identity,
    )? != expected_task_scope_tree
    {
        anyhow::bail!("The corrected staged task board changes evidence outside the selected task");
    }
    let rechecked_parent = resolve_git_commit(
        project_root,
        "HEAD",
        "recheck the corrected manifest parent",
    )?;
    let rechecked_tree = git_stdout(
        project_root,
        &["write-tree"],
        "recheck the corrected completed-task manifest",
    )?;
    let rechecked_branch = git_optional_stdout(
        project_root,
        &["symbolic-ref", "-q", "HEAD"],
        &[1],
        "recheck the corrected manifest branch",
    )?;
    if rechecked_parent != manifest_parent_head
        || rechecked_tree != sealed_commit_tree
        || rechecked_branch.as_deref() != branch_ref
        || !worktree_matches_agent_git_baseline(project_root, raw_baseline)?
    {
        anyhow::bail!(
            "Git HEAD, branch, index, or unstaged work changed while CLT was resealing the task manifest; retry"
        );
    }
    baseline.staged_non_task_patch_ids = Some(staged_non_task_patch_ids);
    baseline.staged_index_tree = Some(sealed_commit_tree);
    baseline.manifest_parent_head = Some(manifest_parent_head);
    baseline.to_json()
}

pub(super) fn agent_git_range_is_safe_before_manifest(
    proof: AgentGitProofContext<'_>,
    project_root: &Path,
    starting_head: &str,
    manifest_parent: &str,
    current_session_id: &str,
) -> Result<bool> {
    if starting_head == manifest_parent {
        return Ok(true);
    }
    if !git_commit_is_first_parent_ancestor(project_root, starting_head, manifest_parent)? {
        return Ok(false);
    }
    let range = format!("{starting_head}..{manifest_parent}");
    let revisions = git_stdout(
        project_root,
        &["rev-list", "--first-parent", "--reverse", &range],
        "audit commits created before the task manifest",
    )?;
    for commit in revisions.lines().filter(|commit| !commit.is_empty()) {
        let is_proven_completed_task = git_commit_is_proven_completed_other_session(
            proof,
            project_root,
            commit,
            current_session_id,
        )?;
        if !is_proven_completed_task {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(super) fn git_commit_is_proven_completed_other_session(
    proof: AgentGitProofContext<'_>,
    project_root: &Path,
    commit_oid: &str,
    current_session_id: &str,
) -> Result<bool> {
    let trailers = git_commit_task_trailers(project_root, commit_oid)?;
    let [trailer] = trailers.as_slice() else {
        return Ok(false);
    };
    let Some(session_id) = trailer.strip_prefix(CODEX_TASK_SESSION_PREFIX) else {
        return Ok(false);
    };
    if session_id.is_empty()
        || session_id == current_session_id
        || !git_commit_uses_agent_identity(project_root, commit_oid)?
    {
        return Ok(false);
    }
    let Some(finalization) = proof
        .store
        .git_finalization_blocking(proof.project_id, session_id)?
    else {
        return Ok(false);
    };
    let Some(task_identity) = finalization.task_identity.as_deref() else {
        return Ok(false);
    };
    let baseline = AgentGitWorktreeBaseline::from_json(&finalization.worktree_baseline)?;
    if finalization.state != GitFinalizationState::Completed
        || baseline.version < 2
        || finalization.commit_oid.as_deref() != Some(commit_oid)
    {
        return Ok(false);
    }
    git_commit_matches_agent_staged_manifest(
        project_root,
        commit_oid,
        session_id,
        task_identity,
        &finalization.worktree_baseline,
        true,
    )
}

pub(super) fn git_commit_matches_agent_staged_manifest(
    project_root: &Path,
    commit_oid: &str,
    session_id: &str,
    task_identity: &str,
    raw_baseline: &str,
    require_manifest_parent: bool,
) -> Result<bool> {
    let baseline = AgentGitWorktreeBaseline::from_json(raw_baseline)?;
    let (Some(_expected_non_task), Some(sealed_commit_tree)) = (
        baseline.staged_non_task_patch_ids.as_ref(),
        baseline.staged_index_tree.as_deref(),
    ) else {
        // Version-one journals predate staged manifests. Their immutable tree,
        // identity, trailer, and author proof is still safe to adopt; new
        // journals always take the stronger manifest path.
        return Ok(baseline.version == 1);
    };
    let parents = git_stdout(
        project_root,
        &["show", "-s", "--format=%P", commit_oid],
        "read the task commit parent for manifest verification",
    )?;
    let parent_oids = parents.split_whitespace().collect::<Vec<_>>();
    if parent_oids.len() != 1 {
        return Ok(false);
    }
    let parent = parent_oids[0];
    if require_manifest_parent && baseline.manifest_parent_head.as_deref() != Some(parent) {
        return Ok(false);
    }
    let tree_reference = format!("{commit_oid}^{{tree}}");
    let committed_tree = git_stdout(
        project_root,
        &["rev-parse", "--verify", &tree_reference],
        "resolve the committed task tree",
    )?;
    if committed_tree != sealed_commit_tree
        || git_ref_completed_task_identity(project_root, commit_oid, session_id)?.as_deref()
            != Some(task_identity)
    {
        return Ok(false);
    }
    Ok(true)
}

pub(super) fn capture_agent_git_start_state(
    project_root: &Path,
    git_mode: AgentGitMode,
) -> Result<AgentGitStartState> {
    require_agent_git_index_matches_head(project_root)?;
    let starting_head = git_stdout(
        project_root,
        &["rev-parse", "--verify", "HEAD^{commit}"],
        "resolve the starting Git commit",
    )?;
    let branch_ref = git_optional_stdout(
        project_root,
        &["symbolic-ref", "-q", "HEAD"],
        &[1],
        "resolve the current Git branch",
    )?;
    if branch_ref.is_none() {
        anyhow::bail!(
            "Git-enabled automated tasks require an attached branch before CLT freezes the task boundary"
        );
    }
    let upstream_ref = git_optional_stdout(
        project_root,
        &[
            "rev-parse",
            "--symbolic-full-name",
            "--verify",
            "@{upstream}",
        ],
        &[1, 128],
        "resolve the current Git upstream",
    )?;
    if git_mode == AgentGitMode::CommitAndPush && upstream_ref.is_none() {
        anyhow::bail!(
            "Automated commit-and-push tasks require an attached branch with a configured upstream before entering Doing"
        );
    }
    let upstream_destination = if git_mode == AgentGitMode::CommitAndPush {
        Some(
            capture_agent_git_upstream_destination(project_root, branch_ref.as_deref())?
                .context("Automated commit-and-push tasks require one stable push destination")?,
        )
    } else {
        None
    };
    let mut baseline = capture_agent_git_worktree_baseline(project_root)?;
    if let Some(destination) = upstream_destination.as_ref() {
        baseline.upstream_remote = Some(destination.remote.clone());
        baseline.upstream_merge_ref = Some(destination.merge_ref.clone());
        baseline.upstream_push_url = destination.push_url.clone();
    }
    let worktree_baseline = baseline.to_json()?;
    require_agent_git_index_matches_head(project_root)?;
    let rechecked_head = resolve_git_commit(project_root, "HEAD", "recheck the task start commit")?;
    let rechecked_branch = git_optional_stdout(
        project_root,
        &["symbolic-ref", "-q", "HEAD"],
        &[1],
        "recheck the current Git branch",
    )?;
    let rechecked_upstream = resolve_agent_git_upstream(project_root, branch_ref.as_deref())?;
    let rechecked_destination = if git_mode == AgentGitMode::CommitAndPush {
        capture_agent_git_upstream_destination(project_root, branch_ref.as_deref())?
    } else {
        None
    };
    if rechecked_head != starting_head
        || rechecked_branch != branch_ref
        || rechecked_upstream != upstream_ref
        || rechecked_destination != upstream_destination
    {
        anyhow::bail!(
            "Git HEAD, branch, upstream, or index changed while CLT was freezing the task start state; retry the Todo-to-Doing move"
        );
    }

    Ok(AgentGitStartState {
        starting_head,
        branch_ref,
        upstream_ref,
        worktree_baseline,
    })
}

pub(super) fn verify_agent_git_start_state_unchanged(
    project_root: &Path,
    git_mode: AgentGitMode,
    start: &AgentGitStartState,
) -> Result<()> {
    require_agent_git_index_matches_head(project_root)?;
    let current_head = resolve_git_commit(project_root, "HEAD", "verify the prelaunch Git commit")?;
    let current_branch = git_optional_stdout(
        project_root,
        &["symbolic-ref", "-q", "HEAD"],
        &[1],
        "verify the prelaunch Git branch",
    )?;
    let current_upstream = resolve_agent_git_upstream(project_root, current_branch.as_deref())?;
    let expected_baseline = AgentGitWorktreeBaseline::from_json(&start.worktree_baseline)?;
    let current_baseline = capture_agent_git_worktree_baseline(project_root)?;
    let worktree_is_unchanged = current_baseline.tracked_patch_ids
        == expected_baseline.tracked_patch_ids
        && current_baseline.untracked_blob_ids == expected_baseline.untracked_blob_ids;
    let upstream_is_unchanged = if git_mode == AgentGitMode::CommitAndPush {
        capture_agent_git_upstream_destination(project_root, current_branch.as_deref())?
            == Some(AgentGitUpstreamDestination {
                remote: expected_baseline
                    .upstream_remote
                    .clone()
                    .unwrap_or_default(),
                merge_ref: expected_baseline
                    .upstream_merge_ref
                    .clone()
                    .unwrap_or_default(),
                push_url: expected_baseline.upstream_push_url.clone(),
            })
    } else {
        true
    };
    if current_head != start.starting_head
        || current_branch != start.branch_ref
        || current_upstream != start.upstream_ref
        || !worktree_is_unchanged
        || !upstream_is_unchanged
    {
        anyhow::bail!(
            "Git HEAD, branch, upstream, index, or worktree changed after CLT froze the automated run; start the task before making implementation changes or commits"
        );
    }
    Ok(())
}

pub(super) fn ensure_agent_git_working_record(
    store: &agent::TursoAgentStore,
    project: &agent::AgentProject,
    session_id: &str,
    run_token: &str,
    git_start_state: Option<&AgentGitStartState>,
) -> Result<()> {
    if project.git_mode == AgentGitMode::Off {
        return Ok(());
    }
    if let Some(existing) = store.git_finalization_blocking(project.id, session_id)? {
        if existing.state.is_terminal() {
            return Ok(());
        }
        if existing.git_mode != project.git_mode {
            anyhow::bail!(
                "Codex session {session_id} already has a Git journal with mode {}, not {}",
                existing.git_mode.label(),
                project.git_mode.label()
            );
        }
        return Ok(());
    }
    let git = git_start_state.with_context(|| {
        format!("Git start state was not captured for Codex session {session_id}")
    })?;
    if worktree_completed_task_identity(&project.path, session_id)?.is_some() {
        anyhow::bail!(
            "Codex session {session_id} has completed task evidence but no frozen Git start journal; CLT cannot safely reconstruct the exact-one-commit boundary"
        );
    }
    let created = store.create_git_finalization_blocking(agent::NewGitFinalization {
        project_id: project.id,
        codex_session_id: session_id,
        git_mode: project.git_mode,
        starting_head: Some(&git.starting_head),
        branch_ref: git.branch_ref.as_deref(),
        upstream_ref: git.upstream_ref.as_deref(),
        worktree_baseline: &git.worktree_baseline,
        task_identity: None,
        owner_run_token: Some(run_token),
        created_at: &agent_timestamp(),
    })?;
    if !created {
        anyhow::bail!(
            "Codex session {session_id} lost its running-session fence before CLT could record the Git start state"
        );
    }
    Ok(())
}

pub(super) fn bind_agent_git_working_task_identity(
    store: &agent::TursoAgentStore,
    project: &agent::AgentProject,
    session_id: &str,
    run_token: &str,
) -> Result<bool> {
    let Some(finalization) = store.git_finalization_blocking(project.id, session_id)? else {
        return Ok(false);
    };
    let Some((_, task)) =
        terminal_task_for_codex_session_in_board(&get_tasks_dir(&project.path), session_id)?
    else {
        return Ok(false);
    };
    let task_identity = durable_task_identity(&task.content)
        .context("CLT could not derive a durable identity for the session-linked task")?;
    let starting_head = finalization
        .starting_head
        .as_deref()
        .context("The Git finalization has no frozen starting commit")?;
    require_agent_git_start_task_identity(&project.path, starting_head, &task_identity)?;
    if let Some(bound_identity) = finalization.task_identity.as_deref() {
        if bound_identity != task_identity {
            anyhow::bail!(
                "Codex session {session_id} is attached to task content that no longer matches its frozen Git journal"
            );
        }
        if finalization.state == GitFinalizationState::Working
            && finalization.owner_run_token.as_deref() != Some(run_token)
            && !store.compare_and_set_git_finalization_with_identity_blocking(
                project.id,
                session_id,
                finalization.generation,
                GitFinalizationState::Working,
                &task_identity,
                Some(run_token),
                &agent_timestamp(),
            )?
        {
            anyhow::bail!(
                "Codex session {session_id} lost its running-session fence while rotating its Working Git journal to the resumed run"
            );
        }
        return Ok(true);
    }
    if finalization.state != GitFinalizationState::Working {
        anyhow::bail!(
            "Git finalization for Codex session {session_id} entered {} before its task identity was bound",
            finalization.state.database_value()
        );
    }
    let changed = store.compare_and_set_git_finalization_with_identity_blocking(
        project.id,
        session_id,
        finalization.generation,
        GitFinalizationState::Working,
        &task_identity,
        Some(run_token),
        &agent_timestamp(),
    )?;
    if !changed {
        anyhow::bail!(
            "Codex session {session_id} lost its running-session fence before CLT could bind its task identity"
        );
    }
    Ok(true)
}

pub(super) fn git_stdout(project_root: &Path, args: &[&str], operation: &str) -> Result<String> {
    let output = Command::new("git")
        .current_dir(project_root)
        .args(args)
        .output()
        .with_context(|| format!("Failed to {operation} in {}", project_root.display()))?;
    if !output.status.success() {
        anyhow::bail!(
            "Failed to {operation} in {}: {}",
            project_root.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    String::from_utf8(output.stdout)
        .with_context(|| format!("Git output for {operation} was not valid UTF-8"))
        .map(|value| value.trim().to_string())
}

pub(super) fn git_optional_stdout(
    project_root: &Path,
    args: &[&str],
    absent_exit_codes: &[i32],
    operation: &str,
) -> Result<Option<String>> {
    let output = Command::new("git")
        .current_dir(project_root)
        .args(args)
        .output()
        .with_context(|| format!("Failed to {operation} in {}", project_root.display()))?;
    if output.status.success() {
        return String::from_utf8(output.stdout)
            .with_context(|| format!("Git output for {operation} was not valid UTF-8"))
            .map(|value| Some(value.trim().to_string()));
    }
    if output
        .status
        .code()
        .is_some_and(|code| absent_exit_codes.contains(&code))
    {
        return Ok(None);
    }
    anyhow::bail!(
        "Failed to {operation} in {}: {}",
        project_root.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

pub(super) fn task_content_has_completed_note(content: &str) -> bool {
    !task_content_is_blocked(content)
        && content.lines().any(|line| {
            let uppercase = line.to_ascii_uppercase();
            uppercase
                .match_indices("COMPLETED ")
                .any(|(index, matched)| {
                    let has_word_boundary = uppercase[..index]
                        .chars()
                        .next_back()
                        .is_none_or(|ch| !ch.is_ascii_alphanumeric() && ch != '_');
                    has_word_boundary
                        && starts_with_task_note_date(
                            &uppercase.as_bytes()[index + matched.len()..],
                        )
                })
        })
}

pub(super) fn git_ref_contains_completed_task(
    project_root: &Path,
    reference: &str,
    session_id: &str,
) -> Result<bool> {
    Ok(git_ref_completed_task_identity(project_root, reference, session_id)?.is_some())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct GitTaskProofEntry {
    pub(super) status: String,
    pub(super) content: String,
}

pub(super) fn git_ref_task_entries(
    project_root: &Path,
    reference: &str,
) -> Result<Vec<GitTaskProofEntry>> {
    let output = Command::new("git")
        .current_dir(project_root)
        .args(["ls-tree", "-r", "-z", reference, "--", "tasks"])
        .output()
        .with_context(|| {
            format!(
                "Failed to list tasks at {reference} in {}",
                project_root.display()
            )
        })?;
    if !output.status.success() {
        anyhow::bail!(
            "Failed to list tasks at {reference} in {}: {}",
            project_root.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let mut paths = Vec::new();
    for raw in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|raw| !raw.is_empty())
    {
        let entry = std::str::from_utf8(raw)
            .context("Git returned a non-UTF-8 tree entry while proving finalization")?;
        let (metadata, path) = entry
            .split_once('\t')
            .context("Git returned an invalid tree entry while proving finalization")?;
        let mut metadata = metadata.split_whitespace();
        let mode = metadata.next().unwrap_or_default();
        let object_type = metadata.next().unwrap_or_default();
        if mode.starts_with("100") && object_type == "blob" {
            paths.push(path.to_string());
        }
    }
    let mut entries = Vec::new();
    collect_git_ref_board_task_entries(project_root, reference, &paths, "tasks", &mut entries)?;
    Ok(entries)
}

pub(super) fn git_tree_has_directory(paths: &[String], directory: &str) -> bool {
    let prefix = format!("{directory}/");
    paths.iter().any(|path| path.starts_with(&prefix))
}

pub(super) fn git_tree_board_has_any_status_store(paths: &[String], board_dir: &str) -> bool {
    TASK_STATUSES.iter().any(|status| {
        paths
            .iter()
            .any(|path| path == &format!("{board_dir}/{status}.md"))
            || git_tree_has_directory(paths, &format!("{board_dir}/{status}"))
    })
}

pub(super) fn git_ref_blob_content(
    project_root: &Path,
    reference: &str,
    path: &str,
) -> Result<String> {
    let object = format!("{reference}:{path}");
    git_stdout(
        project_root,
        &["cat-file", "blob", object.as_str()],
        "read a committed task",
    )
}

pub(super) fn collect_git_ref_board_task_entries(
    project_root: &Path,
    reference: &str,
    paths: &[String],
    board_dir: &str,
    entries: &mut Vec<GitTaskProofEntry>,
) -> Result<()> {
    for status in TASK_STATUSES {
        let status_dir = format!("{board_dir}/{status}");
        if git_tree_has_directory(paths, &status_dir) {
            let prefix = format!("{status_dir}/");
            let mut children = BTreeMap::<String, bool>::new();
            for path in paths.iter().filter(|path| path.starts_with(&prefix)) {
                let remainder = &path[prefix.len()..];
                let child = remainder.split('/').next().unwrap_or_default();
                if child.is_empty() || child.starts_with('.') {
                    continue;
                }
                let is_directory = remainder.len() > child.len();
                children
                    .entry(child.to_string())
                    .and_modify(|known_directory| *known_directory |= is_directory)
                    .or_insert(is_directory);
            }
            for (child, is_directory) in children {
                let task_path = format!("{status_dir}/{child}");
                let content = if is_directory {
                    let detail_path = TASK_DETAIL_FILES
                        .iter()
                        .map(|detail| format!("{task_path}/{detail}"))
                        .find(|detail_path| paths.iter().any(|path| path == detail_path));
                    match detail_path {
                        Some(detail_path) => {
                            git_ref_blob_content(project_root, reference, &detail_path)?
                        }
                        None => title_from_path(Path::new(&child)),
                    }
                } else {
                    git_ref_blob_content(project_root, reference, &task_path)?
                };
                entries.push(GitTaskProofEntry {
                    status: status.to_string(),
                    content,
                });
                if is_directory && git_tree_board_has_any_status_store(paths, &task_path) {
                    collect_git_ref_board_task_entries(
                        project_root,
                        reference,
                        paths,
                        &task_path,
                        entries,
                    )?;
                }
            }
        } else {
            let markdown_path = format!("{board_dir}/{status}.md");
            if paths.iter().any(|path| path == &markdown_path) {
                let content = git_ref_blob_content(project_root, reference, &markdown_path)?;
                entries.extend(content.lines().filter_map(|line| {
                    line.strip_prefix("- ").map(|content| GitTaskProofEntry {
                        status: status.to_string(),
                        content: content.to_string(),
                    })
                }));
            }
        }
    }
    Ok(())
}

pub(super) fn git_ref_completed_task_identity(
    project_root: &Path,
    reference: &str,
    session_id: &str,
) -> Result<Option<String>> {
    let entries = git_ref_task_entries(project_root, reference)?;
    let mut marker_count = 0;
    let mut completed_matches = 0;
    let mut completed_identity = None;
    for entry in entries {
        let matching_markers = codex_session_markers_in_task_content(&entry.content)
            .into_iter()
            .filter(|(_, _, candidate)| *candidate == session_id)
            .count();
        marker_count += matching_markers;
        if matching_markers == 1
            && codex_session_id_from_task_content(&entry.content) == Some(session_id)
            && entry.status == "done"
            && task_content_has_completed_note(&entry.content)
        {
            completed_matches += 1;
            if completed_matches == 1 {
                completed_identity = durable_task_identity(&entry.content);
            }
        }
    }
    Ok((marker_count == 1 && completed_matches == 1)
        .then_some(completed_identity)
        .flatten())
}

pub(super) fn git_ref_active_task_identity_count(
    project_root: &Path,
    reference: &str,
    task_identity: &str,
) -> Result<usize> {
    Ok(git_ref_task_entries(project_root, reference)?
        .into_iter()
        .filter(|entry| {
            matches!(entry.status.as_str(), "todo" | "doing")
                && durable_task_identity(&entry.content).as_deref() == Some(task_identity)
        })
        .count())
}

pub(super) fn require_agent_git_start_task_identity(
    project_root: &Path,
    starting_head: &str,
    task_identity: &str,
) -> Result<()> {
    let count = git_ref_active_task_identity_count(project_root, starting_head, task_identity)?;
    if count != 1 {
        anyhow::bail!(
            "Git-enabled automated work requires the selected task to be committed exactly once in Todo or Doing before the task starts (found {count})"
        );
    }
    Ok(())
}

pub(super) fn git_ref_contains_active_task_identity(
    project_root: &Path,
    reference: &str,
    task_identity: &str,
) -> Result<bool> {
    Ok(git_ref_task_entries(project_root, reference)?
        .into_iter()
        .any(|entry| {
            matches!(entry.status.as_str(), "todo" | "doing")
                && durable_task_identity(&entry.content).as_deref() == Some(task_identity)
        }))
}

pub(super) fn git_commit_has_task_trailer(
    project_root: &Path,
    commit_oid: &str,
    session_id: &str,
) -> Result<bool> {
    let values = git_commit_task_trailers(project_root, commit_oid)?;
    let expected = format!("{CODEX_TASK_SESSION_PREFIX}{session_id}");
    Ok(values == [expected])
}

pub(super) fn git_commit_task_trailers(
    project_root: &Path,
    commit_oid: &str,
) -> Result<Vec<String>> {
    Ok(git_stdout(
        project_root,
        &[
            "show",
            "-s",
            "--format=%(trailers:key=CLT-Task,valueonly)",
            commit_oid,
        ],
        "read the task commit trailer",
    )?
    .lines()
    .map(str::trim)
    .filter(|value| !value.is_empty())
    .map(str::to_string)
    .collect())
}

pub(super) fn git_commit_uses_agent_identity(
    project_root: &Path,
    commit_oid: &str,
) -> Result<bool> {
    let identity = git_stdout(
        project_root,
        &[
            "show",
            "-s",
            "--format=%an%x00%ae%x00%cn%x00%ce",
            commit_oid,
        ],
        "read commit identity",
    )?;
    let fields = identity.split('\0').collect::<Vec<_>>();
    Ok(fields
        == [
            AGENT_GIT_IDENTITY_NAME,
            AGENT_GIT_IDENTITY_EMAIL,
            AGENT_GIT_IDENTITY_NAME,
            AGENT_GIT_IDENTITY_EMAIL,
        ])
}

pub(super) fn git_commit_is_ancestor(
    project_root: &Path,
    ancestor: &str,
    descendant: &str,
) -> Result<bool> {
    let output = Command::new("git")
        .current_dir(project_root)
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .output()
        .with_context(|| {
            format!(
                "Failed to compare Git ancestry in {}",
                project_root.display()
            )
        })?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => anyhow::bail!(
            "Failed to compare Git ancestry in {}: {}",
            project_root.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ),
    }
}

pub(super) fn git_commit_is_first_parent_ancestor(
    project_root: &Path,
    ancestor: &str,
    descendant: &str,
) -> Result<bool> {
    if ancestor == descendant {
        return Ok(true);
    }
    Ok(git_stdout(
        project_root,
        &["rev-list", "--first-parent", descendant],
        "verify the first-parent task history",
    )?
    .lines()
    .any(|commit| commit == ancestor))
}

pub(super) fn resolve_git_commit(
    project_root: &Path,
    reference: &str,
    operation: &str,
) -> Result<String> {
    let commit_reference = format!("{reference}^{{commit}}");
    git_stdout(
        project_root,
        &["rev-parse", "--verify", commit_reference.as_str()],
        operation,
    )
}

#[cfg(test)]
pub(super) fn find_agent_git_task_commit(
    project_root: &Path,
    starting_head: &str,
    branch_ref: Option<&str>,
    session_id: &str,
    task_identity: &str,
) -> Result<Option<String>> {
    find_agent_git_task_commit_with_policy(
        project_root,
        starting_head,
        branch_ref,
        session_id,
        task_identity,
        true,
    )
}

pub(super) fn find_agent_git_task_commit_with_policy(
    project_root: &Path,
    starting_head: &str,
    branch_ref: Option<&str>,
    session_id: &str,
    task_identity: &str,
    legacy_identity_checks: bool,
) -> Result<Option<String>> {
    let branch_ref = branch_ref.unwrap_or("HEAD");
    let branch_tip = resolve_git_commit(project_root, branch_ref, "resolve the finalization tip")?;
    let starting_identity_count =
        git_ref_active_task_identity_count(project_root, starting_head, task_identity)?;
    if (legacy_identity_checks && starting_identity_count > 1)
        || !git_commit_is_first_parent_ancestor(project_root, starting_head, &branch_tip)?
        || git_ref_completed_task_identity(project_root, &branch_tip, session_id)?.as_deref()
            != Some(task_identity)
        || (legacy_identity_checks
            && git_ref_contains_active_task_identity(project_root, &branch_tip, task_identity)?)
    {
        return Ok(None);
    }
    if git_ref_contains_completed_task(project_root, starting_head, session_id)? {
        return Ok(None);
    }

    let range = format!("{starting_head}..{branch_tip}");
    let revisions = git_stdout(
        project_root,
        &["rev-list", "--first-parent", "--reverse", range.as_str()],
        "list task finalization commits",
    )?;
    let mut candidates = Vec::new();
    for commit in revisions.lines().filter(|line| !line.is_empty()) {
        if !git_commit_has_task_trailer(project_root, commit, session_id)?
            || !git_commit_uses_agent_identity(project_root, commit)?
            || git_ref_completed_task_identity(project_root, commit, session_id)?.as_deref()
                != Some(task_identity)
            || (legacy_identity_checks
                && git_ref_contains_active_task_identity(project_root, commit, task_identity)?)
        {
            continue;
        }
        let parents = git_stdout(
            project_root,
            &["show", "-s", "--format=%P", commit],
            "read task commit parents",
        )?;
        let parent_oids = parents.split_whitespace().collect::<Vec<_>>();
        let introduced_completion = parent_oids.len() == 1
            && !git_ref_contains_completed_task(project_root, parent_oids[0], session_id)?
            && (!legacy_identity_checks
                || git_ref_active_task_identity_count(
                    project_root,
                    parent_oids[0],
                    task_identity,
                )? == starting_identity_count);
        if introduced_completion {
            candidates.push(commit.to_string());
        }
    }
    if candidates.len() != 1 {
        return Ok(None);
    }
    let candidate = candidates.remove(0);

    for commit in revisions.lines().filter(|line| !line.is_empty()) {
        if commit != candidate {
            return Ok(None);
        }
    }

    if resolve_git_commit(project_root, branch_ref, "recheck the finalization tip")? != branch_tip {
        return Ok(None);
    }

    Ok(Some(candidate))
}

pub(super) fn resolve_agent_git_upstream(
    project_root: &Path,
    branch_ref: Option<&str>,
) -> Result<Option<String>> {
    let branch = branch_ref
        .and_then(|branch| branch.strip_prefix("refs/heads/"))
        .unwrap_or("HEAD");
    let upstream = format!("{branch}@{{upstream}}");
    git_optional_stdout(
        project_root,
        &[
            "rev-parse",
            "--symbolic-full-name",
            "--verify",
            upstream.as_str(),
        ],
        &[1, 128],
        "resolve the task finalization upstream",
    )
}

pub(super) fn capture_agent_git_upstream_destination(
    project_root: &Path,
    branch_ref: Option<&str>,
) -> Result<Option<AgentGitUpstreamDestination>> {
    let Some(branch_name) = branch_ref.and_then(|branch| branch.strip_prefix("refs/heads/")) else {
        return Ok(None);
    };
    let remote_key = format!("branch.{branch_name}.remote");
    let merge_key = format!("branch.{branch_name}.merge");
    let Some(upstream_remote) = git_optional_stdout(
        project_root,
        &["config", "--get", remote_key.as_str()],
        &[1],
        "resolve the configured upstream remote",
    )?
    else {
        return Ok(None);
    };
    let push_remote_key = format!("branch.{branch_name}.pushRemote");
    let branch_push_remote = git_optional_stdout(
        project_root,
        &["config", "--get", push_remote_key.as_str()],
        &[1],
        "resolve the configured branch push remote",
    )?;
    let default_push_remote = git_optional_stdout(
        project_root,
        &["config", "--get", "remote.pushDefault"],
        &[1],
        "resolve the configured default push remote",
    )?;
    let remote = branch_push_remote
        .or(default_push_remote)
        .unwrap_or(upstream_remote);
    if remote.is_empty() {
        anyhow::bail!("Automated commit-and-push tasks require a non-empty push remote");
    }
    let Some(merge_ref) = git_optional_stdout(
        project_root,
        &["config", "--get", merge_key.as_str()],
        &[1],
        "resolve the configured upstream branch",
    )?
    else {
        return Ok(None);
    };
    if !merge_ref.starts_with("refs/heads/") {
        anyhow::bail!(
            "Automated commit-and-push tasks require an upstream branch ref under refs/heads/"
        );
    }
    if remote == "." {
        anyhow::bail!(
            "Automated commit-and-push tasks require a named remote with one explicit push URL"
        );
    }
    let urls = git_stdout(
        project_root,
        &["remote", "get-url", "--push", "--all", &remote],
        "resolve the configured upstream push destination",
    )?;
    let urls = urls
        .lines()
        .filter(|url| !url.is_empty())
        .collect::<Vec<_>>();
    let [url] = urls.as_slice() else {
        anyhow::bail!(
            "Automated commit-and-push tasks require exactly one configured push URL for remote {remote}"
        );
    };
    let push_url = Some((*url).to_string());
    Ok(Some(AgentGitUpstreamDestination {
        remote,
        merge_ref,
        push_url,
    }))
}

pub(super) fn run_agent_git_remote_command(
    command: &mut Command,
    operation: &str,
) -> Result<std::process::Output> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    configure_agent_child_command(command);
    let mut child = command
        .spawn()
        .with_context(|| format!("Failed to start Git while trying to {operation}"))?;
    let started = Instant::now();
    loop {
        if child
            .try_wait()
            .with_context(|| format!("Failed to poll Git while trying to {operation}"))?
            .is_some()
        {
            return child.wait_with_output().with_context(|| {
                format!("Failed to collect Git output while trying to {operation}")
            });
        }
        if started.elapsed() >= Duration::from_secs(AGENT_GIT_REMOTE_TIMEOUT_SECONDS) {
            stop_agent_child_process(&mut child).with_context(|| {
                format!("Timed-out Git process could not be stopped while trying to {operation}")
            })?;
            anyhow::bail!(
                "Git timed out after {AGENT_GIT_REMOTE_TIMEOUT_SECONDS} seconds while trying to {operation}"
            );
        }
        thread::sleep(Duration::from_millis(25));
    }
}

pub(super) fn ensure_agent_git_finalization_fence(
    finalization_lease: Option<&AgentGitFinalizationLease>,
) -> Result<()> {
    if let Some(finalization_lease) = finalization_lease {
        finalization_lease.ensure_owned()?;
    }
    Ok(())
}

pub(super) fn push_agent_git_commit_to_frozen_destination(
    project_root: &Path,
    branch_ref: Option<&str>,
    expected_upstream_ref: Option<&str>,
    baseline: &AgentGitWorktreeBaseline,
    commit_oid: &str,
    finalization_lease: Option<&AgentGitFinalizationLease>,
) -> Result<()> {
    ensure_agent_git_finalization_fence(finalization_lease)?;
    let expected_destination = AgentGitUpstreamDestination {
        remote: baseline.upstream_remote.clone().unwrap_or_default(),
        merge_ref: baseline.upstream_merge_ref.clone().unwrap_or_default(),
        push_url: baseline.upstream_push_url.clone(),
    };
    let push_url = expected_destination
        .push_url
        .as_deref()
        .context("The frozen Git push destination has no explicit URL")?;
    if expected_destination.remote.is_empty()
        || expected_destination.merge_ref.is_empty()
        || resolve_agent_git_upstream(project_root, branch_ref)?.as_deref() != expected_upstream_ref
        || capture_agent_git_upstream_destination(project_root, branch_ref)?.as_ref()
            != Some(&expected_destination)
    {
        anyhow::bail!(
            "The Git upstream or push destination changed after CLT froze it; leaving the task PUSH-PENDING"
        );
    }

    let refspec = format!("{commit_oid}:{}", expected_destination.merge_ref);
    let mut command = Command::new("git");
    command
        .current_dir(project_root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .args([
            "-c",
            "push.followTags=false",
            "-c",
            "push.recurseSubmodules=no",
            "push",
            "--porcelain",
            "--no-follow-tags",
            "--recurse-submodules=no",
            "--",
            push_url,
            refspec.as_str(),
        ]);
    ensure_agent_git_finalization_fence(finalization_lease)?;
    let output = run_agent_git_remote_command(
        &mut command,
        &format!(
            "push sealed commit {commit_oid} to {}",
            expected_destination.merge_ref
        ),
    )?;
    ensure_agent_git_finalization_fence(finalization_lease)?;
    if !output.status.success() {
        anyhow::bail!(
            "Failed to push sealed commit {commit_oid} to {}: {}",
            expected_destination.merge_ref,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    if resolve_agent_git_upstream(project_root, branch_ref)?.as_deref() != expected_upstream_ref
        || capture_agent_git_upstream_destination(project_root, branch_ref)?.as_ref()
            != Some(&expected_destination)
    {
        anyhow::bail!(
            "The Git push destination changed while CLT was publishing; remote proof is required before completion"
        );
    }
    ensure_agent_git_finalization_fence(finalization_lease)?;
    Ok(())
}

fn tracking_tip_for_push_destination(
    project_root: &Path,
    branch_ref: Option<&str>,
    upstream_ref: Option<&str>,
    destination: &AgentGitUpstreamDestination,
) -> Result<Option<String>> {
    let Some(branch) = branch_ref.and_then(|value| value.strip_prefix("refs/heads/")) else {
        return Ok(None);
    };
    let Some(upstream_ref) = upstream_ref.filter(|value| value.starts_with("refs/remotes/")) else {
        return Ok(None);
    };
    // The branch can fetch from one repository and push to another. Only
    // refresh its tracking ref when it represents this exact destination.
    let remote = git_optional_stdout(
        project_root,
        &["config", "--get", &format!("branch.{branch}.remote")],
        &[1],
        "resolve the upstream fetch remote",
    )?;
    if remote.as_deref() != Some(destination.remote.as_str()) {
        return Ok(None);
    }
    let urls = git_stdout(
        project_root,
        &["remote", "get-url", "--all", &destination.remote],
        "resolve the upstream fetch URL",
    )?;
    if Some(urls.as_str()) != destination.push_url.as_deref() {
        return Ok(None);
    }
    git_optional_stdout(
        project_root,
        &["rev-parse", "--verify", upstream_ref],
        &[1, 128],
        "read the upstream tracking ref before publication proof",
    )
}

fn refresh_agent_git_tracking_ref(
    project_root: &Path,
    upstream_ref: &str,
    previous_tip: &str,
    verified_tip: &str,
) {
    // Explicit-URL pushes/fetches do not maintain the named remote's cache.
    // Compare-and-swap so a concurrent fetch cannot be overwritten. A cache
    // update failure must not turn a proven publication into a failed task.
    if previous_tip != verified_tip
        && let Err(error) = git_stdout(
            project_root,
            &[
                "update-ref",
                "--no-deref",
                "-m",
                "clt: verified task publication",
                upstream_ref,
                verified_tip,
                previous_tip,
            ],
            "refresh the upstream tracking ref",
        )
    {
        eprintln!(
            "CLT verified the remote publication but could not refresh {upstream_ref}: {error:#}"
        );
    }
}

pub(super) fn fetch_agent_git_upstream_tip(
    project_root: &Path,
    branch_ref: Option<&str>,
    expected_upstream_ref: Option<&str>,
    baseline: &AgentGitWorktreeBaseline,
    finalization_lease: Option<&AgentGitFinalizationLease>,
) -> Result<Option<String>> {
    ensure_agent_git_finalization_fence(finalization_lease)?;
    if resolve_agent_git_upstream(project_root, branch_ref)?.as_deref() != expected_upstream_ref {
        return Ok(None);
    }
    let expected_destination = AgentGitUpstreamDestination {
        remote: baseline.upstream_remote.clone().unwrap_or_default(),
        merge_ref: baseline.upstream_merge_ref.clone().unwrap_or_default(),
        push_url: baseline.upstream_push_url.clone(),
    };
    if expected_destination.remote.is_empty()
        || expected_destination.merge_ref.is_empty()
        || capture_agent_git_upstream_destination(project_root, branch_ref)?.as_ref()
            != Some(&expected_destination)
    {
        return Ok(None);
    }
    if expected_destination.remote == "." {
        let tip = resolve_git_commit(
            project_root,
            &expected_destination.merge_ref,
            "resolve the local upstream tip",
        )?;
        ensure_agent_git_finalization_fence(finalization_lease)?;
        return Ok(
            (resolve_agent_git_upstream(project_root, branch_ref)?.as_deref()
                == expected_upstream_ref
                && capture_agent_git_upstream_destination(project_root, branch_ref)?.as_ref()
                    == Some(&expected_destination))
            .then_some(tip),
        );
    }
    let Some(push_url) = expected_destination.push_url.as_deref() else {
        return Ok(None);
    };
    let previous_tracking_tip = tracking_tip_for_push_destination(
        project_root,
        branch_ref,
        expected_upstream_ref,
        &expected_destination,
    )?;

    let read_remote_tip = || -> Result<Option<String>> {
        ensure_agent_git_finalization_fence(finalization_lease)?;
        let mut command = Command::new("git");
        command
            .current_dir(project_root)
            .env("GIT_TERMINAL_PROMPT", "0")
            .args([
                "ls-remote",
                "--refs",
                push_url,
                &expected_destination.merge_ref,
            ]);
        let output = run_agent_git_remote_command(
            &mut command,
            &format!(
                "query the frozen Git push destination for {}",
                expected_destination.merge_ref
            ),
        )?;
        ensure_agent_git_finalization_fence(finalization_lease)?;
        if !output.status.success() {
            anyhow::bail!(
                "Failed to query the frozen Git push destination for {}: {}",
                expected_destination.merge_ref,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let stdout = String::from_utf8(output.stdout)
            .context("Configured Git remote returned non-UTF-8 output")?;
        let tips = stdout
            .lines()
            .filter_map(|line| line.split_whitespace().next())
            .collect::<Vec<_>>();
        Ok(match tips.as_slice() {
            [] => None,
            [tip] => Some((*tip).to_string()),
            _ => None,
        })
    };

    let Some(observed_tip) = read_remote_tip()? else {
        return Ok(None);
    };
    ensure_agent_git_finalization_fence(finalization_lease)?;
    let mut fetch_command = Command::new("git");
    fetch_command
        .current_dir(project_root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .args([
            "fetch",
            "--no-tags",
            "--quiet",
            push_url,
            &expected_destination.merge_ref,
        ]);
    let fetch = run_agent_git_remote_command(
        &mut fetch_command,
        &format!(
            "fetch the frozen Git push destination for {}",
            expected_destination.merge_ref
        ),
    )?;
    ensure_agent_git_finalization_fence(finalization_lease)?;
    if !fetch.status.success() {
        anyhow::bail!(
            "Failed to fetch the frozen Git push destination for {}: {}",
            expected_destination.merge_ref,
            String::from_utf8_lossy(&fetch.stderr).trim()
        );
    }
    let fetched_tip = resolve_git_commit(
        project_root,
        "FETCH_HEAD",
        "resolve the fetched upstream tip",
    )?;
    let rechecked_tip = read_remote_tip()?;
    if resolve_agent_git_upstream(project_root, branch_ref)?.as_deref() != expected_upstream_ref
        || capture_agent_git_upstream_destination(project_root, branch_ref)?.as_ref()
            != Some(&expected_destination)
    {
        return Ok(None);
    }
    ensure_agent_git_finalization_fence(finalization_lease)?;
    if Some(fetched_tip.clone()) != rechecked_tip || fetched_tip != observed_tip {
        return Ok(None);
    }
    if let (Some(upstream_ref), Some(previous_tip)) =
        (expected_upstream_ref, previous_tracking_tip.as_deref())
        && tracking_tip_for_push_destination(
            project_root,
            branch_ref,
            expected_upstream_ref,
            &expected_destination,
        )?
        .as_deref()
            == Some(previous_tip)
    {
        ensure_agent_git_finalization_fence(finalization_lease)?;
        refresh_agent_git_tracking_ref(project_root, upstream_ref, previous_tip, &fetched_tip);
    }
    ensure_agent_git_finalization_fence(finalization_lease)?;
    Ok(Some(fetched_tip))
}

pub(super) fn worktree_contains_completed_done_task(
    project_root: &Path,
    session_id: &str,
) -> Result<bool> {
    let Some((status, task)) =
        terminal_task_for_codex_session_in_board(&get_tasks_dir(project_root), session_id)?
    else {
        return Ok(false);
    };
    Ok(status == TaskStatus::Done && task_content_has_completed_note(&task.content))
}

pub(super) fn agent_git_upstream_tip_proves_task_commit(
    project_root: &Path,
    upstream_tip: &str,
    local_commit_oid: &str,
    session_id: &str,
    task_identity: &str,
    legacy_identity_checks: bool,
) -> Result<bool> {
    Ok(
        git_commit_is_ancestor(project_root, local_commit_oid, upstream_tip)?
            && git_ref_completed_task_identity(project_root, upstream_tip, session_id)?.as_deref()
                == Some(task_identity)
            && (!legacy_identity_checks
                || !git_ref_contains_active_task_identity(
                    project_root,
                    upstream_tip,
                    task_identity,
                )?),
    )
}

pub(super) fn agent_git_manifest_parent_is_current(
    project_root: &Path,
    finalization: &agent::GitFinalizationRecord,
) -> Result<bool> {
    if git_optional_stdout(
        project_root,
        &["symbolic-ref", "-q", "HEAD"],
        &[1],
        "verify the task-recovery branch",
    )?
    .as_deref()
        != finalization.branch_ref.as_deref()
    {
        return Ok(false);
    }
    let current_head = resolve_git_commit(project_root, "HEAD", "verify the task-recovery commit")?;
    let baseline = AgentGitWorktreeBaseline::from_json(&finalization.worktree_baseline)?;
    if baseline.version >= 2 {
        return Ok(baseline.manifest_parent_head.as_deref() == Some(current_head.as_str()));
    }
    let Some(starting_head) = finalization.starting_head.as_deref() else {
        return Ok(false);
    };
    git_commit_is_ancestor(project_root, starting_head, &current_head)
}

pub(super) fn local_agent_git_task_commit_is_retained(
    project_root: &Path,
    branch_ref: Option<&str>,
    commit_oid: &str,
    session_id: &str,
    task_identity: &str,
) -> Result<bool> {
    let Some(branch_ref) = branch_ref else {
        return Ok(false);
    };
    if git_optional_stdout(
        project_root,
        &["symbolic-ref", "-q", "HEAD"],
        &[1],
        "verify the frozen push branch",
    )?
    .as_deref()
        != Some(branch_ref)
        || worktree_completed_task_identity(project_root, session_id)?.as_deref()
            != Some(task_identity)
    {
        return Ok(false);
    }
    let branch_tip =
        resolve_git_commit(project_root, branch_ref, "resolve the frozen push branch")?;
    if !git_commit_is_ancestor(project_root, commit_oid, &branch_tip)?
        || git_ref_completed_task_identity(project_root, &branch_tip, session_id)?.as_deref()
            != Some(task_identity)
        || git_ref_contains_active_task_identity(project_root, &branch_tip, task_identity)?
    {
        return Ok(false);
    }
    Ok(
        resolve_git_commit(project_root, branch_ref, "recheck the frozen push branch")?
            == branch_tip,
    )
}

pub(super) fn matching_agent_session_tasks(
    board_dir: &Path,
    status: TaskStatus,
    session_id: &str,
    task_identity: &str,
) -> Result<Vec<TaskEntry>> {
    Ok(read_task_entries(board_dir, status)?
        .into_iter()
        .filter(|entry| {
            codex_session_id_from_task_content(&entry.content) == Some(session_id)
                && durable_task_identity(&entry.content).as_deref() == Some(task_identity)
                && task_content_has_completed_note(&entry.content)
        })
        .collect())
}

pub(super) fn repair_tracking_agent_git_board(
    project_root: &Path,
    session_id: &str,
    task_identity: &str,
) -> Result<bool> {
    let board_dir = get_tasks_dir(project_root);
    let _mutation_lock = acquire_board_mutation_lock(&board_dir)?;
    cleanup_clt_atomic_task_temporaries(&board_dir)?;
    if [TaskStatus::Backlog]
        .into_iter()
        .map(|status| matching_agent_session_tasks(&board_dir, status, session_id, task_identity))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .any(|entries| !entries.is_empty())
    {
        return Ok(false);
    }

    let mut done =
        matching_agent_session_tasks(&board_dir, TaskStatus::Done, session_id, task_identity)?;
    let mut active = Vec::new();
    for status in [TaskStatus::Todo, TaskStatus::Doing] {
        for entry in matching_agent_session_tasks(&board_dir, status, session_id, task_identity)? {
            active.push((status, entry));
        }
    }
    if done.is_empty() {
        let [(status, entry)] = active.as_slice() else {
            return Ok(false);
        };
        let task_index = read_task_entries(&board_dir, *status)?
            .iter()
            .position(|candidate| candidate.source == entry.source)
            .map(|index| index + 1)
            .context("The tracked Git task changed while CLT was repairing its Done move")?;
        move_task_without_reordering_after_lock(&board_dir, *status, TaskStatus::Done, task_index)?;
        return Ok(true);
    }

    let canonical_content = done[0].content.trim_end().to_string();
    let crash_duplicates_are_safe_entries = done
        .iter()
        .chain(active.iter().map(|(_, entry)| entry))
        .all(|entry| {
            matches!(
                entry.source,
                TaskSource::MarkdownLine { .. } | TaskSource::Path { is_dir: false, .. }
            ) && entry.content.trim_end() == canonical_content
        });
    if (!active.is_empty() || done.len() > 1) && !crash_duplicates_are_safe_entries {
        return Ok(false);
    }
    while done.len() > 1 {
        let duplicate = done.pop().expect("Done duplicate exists");
        remove_task_entry_without_reordering(&board_dir, TaskStatus::Done, &duplicate)?;
        done =
            matching_agent_session_tasks(&board_dir, TaskStatus::Done, session_id, task_identity)?;
    }
    for status in [TaskStatus::Todo, TaskStatus::Doing] {
        loop {
            let mut duplicates =
                matching_agent_session_tasks(&board_dir, status, session_id, task_identity)?;
            let Some(duplicate) = duplicates.pop() else {
                break;
            };
            remove_task_entry_without_reordering(&board_dir, status, &duplicate)?;
        }
    }
    Ok(true)
}

pub(super) fn worktree_completed_task_identity(
    project_root: &Path,
    session_id: &str,
) -> Result<Option<String>> {
    let Some((status, task)) =
        terminal_task_for_codex_session_in_board(&get_tasks_dir(project_root), session_id)?
    else {
        return Ok(None);
    };
    if status != TaskStatus::Done || !task_content_has_completed_note(&task.content) {
        return Ok(None);
    }
    Ok(durable_task_identity(&task.content))
}

pub(super) fn reconcile_agent_git_finalization(
    store: &agent::TursoAgentStore,
    project_root: &Path,
    mut finalization: agent::GitFinalizationRecord,
    owner_run_token: Option<&str>,
    finalization_lease: Option<&AgentGitFinalizationLease>,
) -> Result<agent::GitFinalizationRecord> {
    for _ in 0..6 {
        ensure_agent_git_finalization_fence(finalization_lease)?;
        let effective_owner = owner_run_token.or(finalization.owner_run_token.as_deref());
        match finalization.state {
            GitFinalizationState::Working => {
                if let Some(lease) = finalization_lease
                    && cancel_orphaned_working_git_finalization(
                        store,
                        project_root,
                        &finalization,
                        lease,
                    )?
                {
                    return store
                        .git_finalization_blocking(
                            finalization.project_id,
                            &finalization.codex_session_id,
                        )?
                        .context("Retired orphan Git journal disappeared");
                }
                let Some(task_identity) = finalization.task_identity.as_deref() else {
                    return Ok(finalization);
                };
                let baseline =
                    AgentGitWorktreeBaseline::from_json(&finalization.worktree_baseline)?;
                if baseline.version >= 2
                    && (baseline.staged_non_task_patch_ids.is_none()
                        || baseline.staged_index_tree.is_none()
                        || baseline.manifest_parent_head.is_none())
                {
                    return Ok(finalization);
                }
                if !worktree_contains_completed_done_task(
                    project_root,
                    &finalization.codex_session_id,
                )? || !agent_git_manifest_parent_is_current(project_root, &finalization)?
                {
                    return Ok(finalization);
                }
                ensure_agent_git_finalization_fence(finalization_lease)?;
                let changed = store.recover_git_finalization_intent_blocking(
                    finalization.project_id,
                    &finalization.codex_session_id,
                    finalization.generation,
                    task_identity,
                    effective_owner,
                    &agent_timestamp(),
                )?;
                if !changed {
                    finalization = store
                        .git_finalization_blocking(
                            finalization.project_id,
                            &finalization.codex_session_id,
                        )?
                        .context("Git journal disappeared during completion-intent recovery")?;
                    continue;
                }
            }
            GitFinalizationState::Tracking => {
                let Some(task_identity) = finalization.task_identity.as_deref() else {
                    return Ok(finalization);
                };
                if !agent_git_manifest_parent_is_current(project_root, &finalization)? {
                    return Ok(finalization);
                }
                ensure_agent_git_finalization_fence(finalization_lease)?;
                if !repair_tracking_agent_git_board(
                    project_root,
                    &finalization.codex_session_id,
                    task_identity,
                )? {
                    return Ok(finalization);
                }
                ensure_agent_git_finalization_fence(finalization_lease)?;
                if !worktree_contains_completed_done_task(
                    project_root,
                    &finalization.codex_session_id,
                )? {
                    return Ok(finalization);
                }
                ensure_agent_git_finalization_fence(finalization_lease)?;
                let changed = store.compare_and_set_git_finalization_blocking(
                    finalization.project_id,
                    &finalization.codex_session_id,
                    finalization.generation,
                    GitFinalizationState::CommitPending,
                    effective_owner,
                    None,
                    None,
                    &agent_timestamp(),
                )?;
                if !changed {
                    finalization = store
                        .git_finalization_blocking(
                            finalization.project_id,
                            &finalization.codex_session_id,
                        )?
                        .context("Git finalization disappeared during task-move reconciliation")?;
                    continue;
                }
            }
            GitFinalizationState::CommitPending => {
                let Some(starting_head) = finalization.starting_head.as_deref() else {
                    return Ok(finalization);
                };
                let Some(task_identity) = finalization.task_identity.as_deref() else {
                    return Ok(finalization);
                };
                let baseline =
                    AgentGitWorktreeBaseline::from_json(&finalization.worktree_baseline)?;
                let proof_start = if baseline.version >= 2 {
                    baseline
                        .manifest_parent_head
                        .as_deref()
                        .unwrap_or(starting_head)
                } else {
                    starting_head
                };
                let Some(commit_oid) = find_agent_git_task_commit_with_policy(
                    project_root,
                    proof_start,
                    finalization.branch_ref.as_deref(),
                    &finalization.codex_session_id,
                    task_identity,
                    baseline.version == 1,
                )?
                else {
                    return Ok(finalization);
                };
                if !git_commit_matches_agent_staged_manifest(
                    project_root,
                    &commit_oid,
                    &finalization.codex_session_id,
                    task_identity,
                    &finalization.worktree_baseline,
                    true,
                )? || !local_agent_git_task_commit_is_retained(
                    project_root,
                    finalization.branch_ref.as_deref(),
                    &commit_oid,
                    &finalization.codex_session_id,
                    task_identity,
                )? {
                    return Ok(finalization);
                }
                ensure_agent_git_finalization_fence(finalization_lease)?;
                let next_state = match finalization.git_mode {
                    AgentGitMode::Off => {
                        anyhow::bail!("Pending Git finalization unexpectedly has Git mode off")
                    }
                    AgentGitMode::Commit => GitFinalizationState::Completed,
                    AgentGitMode::CommitAndPush => GitFinalizationState::PushPending,
                };
                let changed = store.compare_and_set_git_finalization_blocking(
                    finalization.project_id,
                    &finalization.codex_session_id,
                    finalization.generation,
                    next_state,
                    effective_owner,
                    Some(&commit_oid),
                    None,
                    &agent_timestamp(),
                )?;
                if !changed {
                    finalization = store
                        .git_finalization_blocking(
                            finalization.project_id,
                            &finalization.codex_session_id,
                        )?
                        .context("Git finalization disappeared during commit reconciliation")?;
                    continue;
                }
            }
            GitFinalizationState::PushPending => {
                let Some(task_identity) = finalization.task_identity.as_deref() else {
                    return Ok(finalization);
                };
                let baseline =
                    AgentGitWorktreeBaseline::from_json(&finalization.worktree_baseline)?;
                let Some(local_commit_oid) = finalization.commit_oid.as_deref() else {
                    return Ok(finalization);
                };
                if !git_commit_matches_agent_staged_manifest(
                    project_root,
                    local_commit_oid,
                    &finalization.codex_session_id,
                    task_identity,
                    &finalization.worktree_baseline,
                    true,
                )? || !local_agent_git_task_commit_is_retained(
                    project_root,
                    finalization.branch_ref.as_deref(),
                    local_commit_oid,
                    &finalization.codex_session_id,
                    task_identity,
                )? {
                    return Ok(finalization);
                }
                let mut upstream_tip = fetch_agent_git_upstream_tip(
                    project_root,
                    finalization.branch_ref.as_deref(),
                    finalization.upstream_ref.as_deref(),
                    &baseline,
                    finalization_lease,
                )?;
                let already_published = match upstream_tip.as_deref() {
                    Some(upstream_tip) => agent_git_upstream_tip_proves_task_commit(
                        project_root,
                        upstream_tip,
                        local_commit_oid,
                        &finalization.codex_session_id,
                        task_identity,
                        baseline.version == 1,
                    )?,
                    None => false,
                };
                if !already_published {
                    push_agent_git_commit_to_frozen_destination(
                        project_root,
                        finalization.branch_ref.as_deref(),
                        finalization.upstream_ref.as_deref(),
                        &baseline,
                        local_commit_oid,
                        finalization_lease,
                    )?;
                    upstream_tip = fetch_agent_git_upstream_tip(
                        project_root,
                        finalization.branch_ref.as_deref(),
                        finalization.upstream_ref.as_deref(),
                        &baseline,
                        finalization_lease,
                    )?;
                }
                let Some(upstream_tip) = upstream_tip else {
                    return Ok(finalization);
                };
                if !agent_git_upstream_tip_proves_task_commit(
                    project_root,
                    &upstream_tip,
                    local_commit_oid,
                    &finalization.codex_session_id,
                    task_identity,
                    baseline.version == 1,
                )? || !local_agent_git_task_commit_is_retained(
                    project_root,
                    finalization.branch_ref.as_deref(),
                    local_commit_oid,
                    &finalization.codex_session_id,
                    task_identity,
                )? {
                    return Ok(finalization);
                }
                ensure_agent_git_finalization_fence(finalization_lease)?;
                let changed = store.compare_and_set_git_finalization_blocking(
                    finalization.project_id,
                    &finalization.codex_session_id,
                    finalization.generation,
                    GitFinalizationState::Completed,
                    effective_owner,
                    Some(local_commit_oid),
                    None,
                    &agent_timestamp(),
                )?;
                if !changed {
                    finalization = store
                        .git_finalization_blocking(
                            finalization.project_id,
                            &finalization.codex_session_id,
                        )?
                        .context("Git finalization disappeared during push reconciliation")?;
                    continue;
                }
            }
            GitFinalizationState::Completed | GitFinalizationState::Cancelled => {
                return Ok(finalization);
            }
        }

        finalization = store
            .git_finalization_blocking(finalization.project_id, &finalization.codex_session_id)?
            .context("Git finalization disappeared after a successful reconciliation step")?;
    }

    anyhow::bail!(
        "Git finalization for session {} changed too many times during reconciliation",
        finalization.codex_session_id
    )
}

pub(super) fn reconcile_pending_agent_git_finalizations(
    state_dir: &Path,
    project: &agent::AgentProject,
    finalization_lease: Option<&AgentGitFinalizationLease>,
) -> Result<Vec<agent::GitFinalizationRecord>> {
    let store = open_agent_store_at(state_dir)?;
    store
        .list_pending_git_finalizations_blocking(Some(project.id))?
        .into_iter()
        .map(|finalization| {
            reconcile_agent_git_finalization(
                &store,
                &project.path,
                finalization,
                None,
                finalization_lease,
            )
        })
        .collect()
}
