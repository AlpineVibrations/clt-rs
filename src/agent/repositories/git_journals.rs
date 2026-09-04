use anyhow::{Context, Result};
use turso::{Connection, Database, params, transaction::TransactionBehavior};

use super::RepositoryDatabase;
use crate::{
    agent::{
        AGENT_EXTERNAL_COMPLETION_REASON, AGENT_GIT_FINALIZATION_RESUME_TOKEN_PREFIX, AgentGitMode,
        AgentRunOutcome, GitFinalizationRecord, GitFinalizationState, NewGitFinalization,
        TursoAgentStore, git_finalization_record_from_row, query_count, row_integer,
        row_optional_integer, row_optional_text, row_text, update_project_after_run,
    },
    managed_git::AgentGitStartState,
    runner::agent_timestamp,
};

/// Persistence for launch boundaries and managed Git finalization journals.
pub(in crate::agent) struct GitJournalsRepository(RepositoryDatabase);

impl GitJournalsRepository {
    pub(in crate::agent) fn new(db: &Database) -> Self {
        Self(RepositoryDatabase::new(db))
    }

    pub(in crate::agent) async fn connect(&self) -> Result<Connection> {
        self.0.connect().await
    }
}

impl TursoAgentStore {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn accept_external_git_completion_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        expected_generation: i64,
        task_identity: &str,
        lease_holder: &str,
        acquired_at: &str,
        expires_at: &str,
    ) -> Result<bool> {
        self.blocking
            .block_on_persist(self.accept_external_git_completion(
                project_id,
                codex_session_id,
                expected_generation,
                task_identity,
                lease_holder,
                acquired_at,
                expires_at,
            ))
    }

    #[allow(clippy::too_many_arguments)]
    async fn accept_external_git_completion(
        &self,
        project_id: i64,
        codex_session_id: &str,
        expected_generation: i64,
        task_identity: &str,
        lease_holder: &str,
        acquired_at: &str,
        expires_at: &str,
    ) -> Result<bool> {
        let mut conn = self.repositories.git_journals.connect().await?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .with_context(|| {
                format!(
                    "Failed to begin accepting external completion for Codex session {codex_session_id}"
                )
            })?;

        if query_count(
            &transaction,
            "SELECT COUNT(*) FROM agent_workers
              WHERE project_id = ?1
                AND state IN ('dispatching', 'running', 'finalizing')",
            [project_id],
        )
        .await?
            > 0
        {
            anyhow::bail!(
                "Task {codex_session_id} still has an active agent worker; stop it before moving the task to Done as an external completion"
            );
        }

        if query_count(
            &transaction,
            "SELECT COUNT(*) FROM session_controls
              WHERE project_id = ?1 AND codex_session_id = ?2
                AND NOT (
                    state IN ('stopped', 'resume_requested')
                    AND child_pid IS NULL
                    AND interactive_holder IS NULL
                    AND interactive_launch_token IS NULL
                )",
            params![project_id, codex_session_id],
        )
        .await?
            > 0
        {
            anyhow::bail!(
                "Codex session {codex_session_id} is still active; stop it before moving the task to Done as an external completion"
            );
        }

        transaction
            .execute(
                "DELETE FROM leases
                  WHERE project_id = ?1
                    AND CAST(expires_at AS INTEGER) <= CAST(?2 AS INTEGER)",
                params![project_id, acquired_at],
            )
            .await
            .with_context(|| {
                format!(
                    "Failed to clear an expired project lease before accepting external completion for {codex_session_id}"
                )
            })?;
        if query_count(
            &transaction,
            "SELECT COUNT(*) FROM leases WHERE project_id = ?1",
            [project_id],
        )
        .await?
            > 0
        {
            anyhow::bail!(
                "Task {codex_session_id} still has an active project lease; stop its agent session before moving the task to Done as an external completion"
            );
        }
        let lease_inserted = transaction
            .execute(
                "INSERT OR IGNORE INTO leases (project_id, holder, acquired_at, expires_at)
                 SELECT ?1, ?2, ?3, ?4
                  WHERE EXISTS (
                      SELECT 1 FROM git_finalizations
                       WHERE project_id = ?1 AND codex_session_id = ?5
                         AND state = 'working' AND generation = ?6
                         AND task_identity = ?7
                  )",
                params![
                    project_id,
                    lease_holder,
                    acquired_at,
                    expires_at,
                    codex_session_id,
                    expected_generation,
                    task_identity,
                ],
            )
            .await
            .with_context(|| {
                format!(
                    "Failed to fence the project while accepting external completion for {codex_session_id}"
                )
            })?;
        if lease_inserted != 1 {
            transaction.commit().await.with_context(|| {
                format!("Failed to finish a rejected external completion for {codex_session_id}")
            })?;
            return Ok(false);
        }

        let changed = transaction
            .execute(
                "UPDATE git_finalizations
                    SET state = 'cancelled', owner_run_token = NULL,
                        generation = generation + 1, last_error = ?1,
                        updated_at = ?2, completed_at = ?2
                  WHERE project_id = ?3 AND codex_session_id = ?4
                    AND state = 'working' AND generation = ?5
                    AND task_identity = ?6",
                params![
                    AGENT_EXTERNAL_COMPLETION_REASON,
                    acquired_at,
                    project_id,
                    codex_session_id,
                    expected_generation,
                    task_identity,
                ],
            )
            .await
            .with_context(|| {
                format!(
                    "Failed to cancel the managed Git journal for externally completed task {codex_session_id}"
                )
            })?;
        if changed != 1 {
            return Ok(false);
        }

        transaction
            .execute(
                "DELETE FROM session_controls
                  WHERE project_id = ?1 AND codex_session_id = ?2
                    AND state IN ('stopped', 'resume_requested')
                    AND child_pid IS NULL
                    AND interactive_holder IS NULL
                    AND interactive_launch_token IS NULL",
                params![project_id, codex_session_id],
            )
            .await
            .with_context(|| {
                format!(
                    "Failed to clear the idle resume state for externally completed task {codex_session_id}"
                )
            })?;

        transaction.commit().await.with_context(|| {
            format!("Failed to commit external completion for Codex session {codex_session_id}")
        })?;
        Ok(true)
    }

    pub(crate) fn record_git_launch_state_blocking(
        &self,
        project_id: i64,
        run_token: &str,
        git_mode: AgentGitMode,
        start: &AgentGitStartState,
        created_at: &str,
    ) -> Result<bool> {
        self.blocking.block_on_persist(async {
                if git_mode == AgentGitMode::Off {
                    anyhow::bail!("A Git launch state cannot use Git mode off");
                }
                let mut conn = self.repositories.git_journals.connect().await?;
                let transaction = conn
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .await
                    .context("Failed to begin recording the prelaunch Git state")?;
                if query_count(
                    &transaction,
                    "SELECT COUNT(*) FROM agent_git_launch_states
                      WHERE project_id = ?1 AND run_token <> ?2",
                    params![project_id, run_token],
                )
                .await?
                    != 0
                {
                    anyhow::bail!(
                        "A prior automated run has an unconsumed Git launch boundary for project {project_id}; refusing to replace it"
                    );
                }
                let inserted = transaction
                    .execute(
                        "INSERT OR IGNORE INTO agent_git_launch_states (
                            project_id, run_token, git_mode, starting_head, branch_ref,
                            upstream_ref, worktree_baseline, created_at
                         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                        params![
                            project_id,
                            run_token,
                            git_mode.database_value(),
                            start.starting_head.as_str(),
                            start.branch_ref.as_deref(),
                            start.upstream_ref.as_deref(),
                            start.worktree_baseline.as_str(),
                            created_at,
                        ],
                    )
                    .await
                    .context("Failed to persist the prelaunch Git state")?;
                if inserted == 0
                    && query_count(
                        &transaction,
                        "SELECT COUNT(*) FROM agent_git_launch_states
                          WHERE project_id = ?1 AND run_token = ?2
                            AND git_mode = ?3 AND starting_head = ?4
                            AND branch_ref IS ?5 AND upstream_ref IS ?6
                            AND worktree_baseline = ?7",
                        params![
                            project_id,
                            run_token,
                            git_mode.database_value(),
                            start.starting_head.as_str(),
                            start.branch_ref.as_deref(),
                            start.upstream_ref.as_deref(),
                            start.worktree_baseline.as_str(),
                        ],
                    )
                    .await?
                        != 1
                {
                    anyhow::bail!(
                        "Automated run {run_token} already has a different immutable Git launch boundary"
                    );
                }
                transaction
                    .commit()
                    .await
                    .context("Failed to commit the prelaunch Git state")?;
                Ok(inserted == 1)
            })
    }

    pub(crate) fn has_other_git_launch_state_blocking(
        &self,
        project_id: i64,
        run_token: &str,
    ) -> Result<bool> {
        self.blocking.block_on(async {
            let conn = self.repositories.git_journals.connect().await?;
            Ok(query_count(
                &conn,
                "SELECT COUNT(*) FROM agent_git_launch_states
                      WHERE project_id = ?1 AND run_token <> ?2",
                params![project_id, run_token],
            )
            .await?
                != 0)
        })
    }

    pub(crate) fn git_launch_state_for_project_blocking(
        &self,
        project_id: i64,
    ) -> Result<Option<(String, AgentGitMode, AgentGitStartState)>> {
        self.blocking.block_on(async {
            let conn = self.repositories.git_journals.connect().await?;
            let mut rows = conn
                .query(
                    "SELECT run_token, git_mode, starting_head, branch_ref,
                                upstream_ref, worktree_baseline
                           FROM agent_git_launch_states
                          WHERE project_id = ?1
                          ORDER BY created_at, run_token",
                    [project_id],
                )
                .await
                .context("Failed to read project Git launch states")?;
            let Some(row) = rows
                .next()
                .await
                .context("Failed to read project Git launch-state row")?
            else {
                return Ok(None);
            };
            let launch = (
                row_text(&row, 0, "run_token")?,
                AgentGitMode::from_database(&row_text(&row, 1, "git_mode")?)?,
                AgentGitStartState {
                    starting_head: row_text(&row, 2, "starting_head")?,
                    branch_ref: row_optional_text(&row, 3, "branch_ref")?,
                    upstream_ref: row_optional_text(&row, 4, "upstream_ref")?,
                    worktree_baseline: row_text(&row, 5, "worktree_baseline")?,
                },
            );
            if rows
                .next()
                .await
                .context("Failed to check for duplicate project Git launch states")?
                .is_some()
            {
                anyhow::bail!(
                    "Project {project_id} has more than one unconsumed Git launch boundary"
                );
            }
            Ok(Some(launch))
        })
    }

    pub(crate) fn reclaim_unchanged_git_launch_state_blocking(
        &self,
        project_id: i64,
        run_token: &str,
        git_mode: AgentGitMode,
        start: &AgentGitStartState,
    ) -> Result<bool> {
        self.blocking.block_on_persist(async {
            let mut conn = self.repositories.git_journals.connect().await?;
            let transaction = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .await
                .context("Failed to begin reclaiming an unchanged Git launch state")?;
            let terminal_worker = query_count(
                &transaction,
                "SELECT COUNT(*) FROM agent_workers
                      WHERE worker_token = ?1 AND project_id = ?2
                        AND state IN ('completed', 'abandoned', 'superseded')",
                params![run_token, project_id],
            )
            .await?
                == 1;
            let any_session = query_count(
                &transaction,
                "SELECT COUNT(*) FROM session_controls
                      WHERE project_id = ?1 AND run_token = ?2",
                params![project_id, run_token],
            )
            .await?
                != 0;
            if !terminal_worker || any_session {
                transaction
                    .commit()
                    .await
                    .context("Failed to finish checking an unreclaimable Git launch state")?;
                return Ok(false);
            }
            let deleted = transaction
                .execute(
                    "DELETE FROM agent_git_launch_states
                          WHERE project_id = ?1 AND run_token = ?2
                            AND git_mode = ?3 AND starting_head = ?4
                            AND branch_ref IS ?5 AND upstream_ref IS ?6
                            AND worktree_baseline = ?7",
                    params![
                        project_id,
                        run_token,
                        git_mode.database_value(),
                        start.starting_head.as_str(),
                        start.branch_ref.as_deref(),
                        start.upstream_ref.as_deref(),
                        start.worktree_baseline.as_str(),
                    ],
                )
                .await
                .context("Failed to delete the proven-unchanged Git launch state")?;
            transaction
                .commit()
                .await
                .context("Failed to commit Git launch-state reclamation")?;
            Ok(deleted == 1)
        })
    }

    pub(crate) fn git_launch_state_blocking(
        &self,
        project_id: i64,
        run_token: &str,
    ) -> Result<Option<(AgentGitMode, AgentGitStartState)>> {
        self.blocking.block_on(async {
            let conn = self.repositories.git_journals.connect().await?;
            let mut rows = conn
                .query(
                    "SELECT git_mode, starting_head, branch_ref, upstream_ref,
                                worktree_baseline
                           FROM agent_git_launch_states
                          WHERE project_id = ?1 AND run_token = ?2",
                    params![project_id, run_token],
                )
                .await
                .context("Failed to read the prelaunch Git state")?;
            let Some(row) = rows
                .next()
                .await
                .context("Failed to read the prelaunch Git state row")?
            else {
                return Ok(None);
            };
            Ok(Some((
                AgentGitMode::from_database(&row_text(&row, 0, "git_mode")?)?,
                AgentGitStartState {
                    starting_head: row_text(&row, 1, "starting_head")?,
                    branch_ref: row_optional_text(&row, 2, "branch_ref")?,
                    upstream_ref: row_optional_text(&row, 3, "upstream_ref")?,
                    worktree_baseline: row_text(&row, 4, "worktree_baseline")?,
                },
            )))
        })
    }

    #[cfg_attr(unix, allow(dead_code))]
    pub(crate) fn delete_git_launch_state_blocking(
        &self,
        project_id: i64,
        run_token: &str,
    ) -> Result<bool> {
        self.blocking.block_on_persist(async {
            let conn = self.repositories.git_journals.connect().await?;
            Ok(conn
                .execute(
                    "DELETE FROM agent_git_launch_states
                          WHERE project_id = ?1 AND run_token = ?2",
                    params![project_id, run_token],
                )
                .await
                .context("Failed to delete the prelaunch Git state")?
                == 1)
        })
    }

    pub(crate) fn create_git_finalization_blocking(
        &self,
        finalization: NewGitFinalization<'_>,
    ) -> Result<bool> {
        self.blocking
            .block_on_persist(self.create_git_finalization(finalization))
    }

    async fn create_git_finalization(&self, finalization: NewGitFinalization<'_>) -> Result<bool> {
        if finalization.codex_session_id.is_empty() {
            anyhow::bail!("Git finalization requires a Codex session ID");
        }
        if finalization.git_mode == AgentGitMode::Off {
            anyhow::bail!("Git finalization cannot be created when Git automation is off");
        }
        let mut conn = self.repositories.git_journals.connect().await?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .with_context(|| {
                format!(
                    "Failed to begin creating Git finalization for project {} and Codex session {}",
                    finalization.project_id, finalization.codex_session_id
                )
            })?;
        let inserted = if let Some(owner_run_token) = finalization.owner_run_token {
            transaction
                .execute(
                    "INSERT OR IGNORE INTO git_finalizations (
                        project_id, codex_session_id, state, git_mode, starting_head,
                        branch_ref, upstream_ref, worktree_baseline, task_identity,
                        owner_run_token, commit_oid, generation,
                        last_error, created_at, updated_at, completed_at
                     ) SELECT ?1, ?2, 'working', ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, 0,
                              NULL, ?10, ?10, NULL
                       WHERE EXISTS (
                           SELECT 1 FROM session_controls
                            WHERE project_id = ?1 AND codex_session_id = ?2
                              AND state = 'running' AND run_token = ?9
                       )",
                    params![
                        finalization.project_id,
                        finalization.codex_session_id,
                        finalization.git_mode.database_value(),
                        finalization.starting_head,
                        finalization.branch_ref,
                        finalization.upstream_ref,
                        finalization.worktree_baseline,
                        finalization.task_identity,
                        owner_run_token,
                        finalization.created_at,
                    ],
                )
                .await
        } else {
            transaction
                .execute(
                    "INSERT OR IGNORE INTO git_finalizations (
                        project_id, codex_session_id, state, git_mode, starting_head,
                        branch_ref, upstream_ref, worktree_baseline, task_identity,
                        owner_run_token, commit_oid, generation,
                        last_error, created_at, updated_at, completed_at
                     ) VALUES (?1, ?2, 'working', ?3, ?4, ?5, ?6, ?7, ?8, NULL, NULL, 0,
                               NULL, ?9, ?9, NULL)",
                    params![
                        finalization.project_id,
                        finalization.codex_session_id,
                        finalization.git_mode.database_value(),
                        finalization.starting_head,
                        finalization.branch_ref,
                        finalization.upstream_ref,
                        finalization.worktree_baseline,
                        finalization.task_identity,
                        finalization.created_at,
                    ],
                )
                .await
        }
        .with_context(|| {
            format!(
                "Failed to create Git finalization for project {} and Codex session {}",
                finalization.project_id, finalization.codex_session_id
            )
        })?;
        transaction.commit().await.with_context(|| {
            format!(
                "Failed to commit Git finalization creation for project {} and Codex session {}",
                finalization.project_id, finalization.codex_session_id
            )
        })?;
        Ok(inserted == 1)
    }

    pub(crate) fn git_finalization_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
    ) -> Result<Option<GitFinalizationRecord>> {
        self.blocking
            .block_on(self.git_finalization(project_id, codex_session_id))
    }

    async fn git_finalization(
        &self,
        project_id: i64,
        codex_session_id: &str,
    ) -> Result<Option<GitFinalizationRecord>> {
        let conn = self.repositories.git_journals.connect().await?;
        let mut rows = conn
            .query(
                "SELECT project_id, codex_session_id, state, git_mode, starting_head,
                        branch_ref, upstream_ref, worktree_baseline, task_identity,
                        owner_run_token, commit_oid, generation, last_error, created_at,
                        updated_at, completed_at, acknowledged_at, acknowledged_run_id
                   FROM git_finalizations
                  WHERE project_id = ?1 AND codex_session_id = ?2",
                params![project_id, codex_session_id],
            )
            .await
            .with_context(|| {
                format!(
                    "Failed to read Git finalization for project {project_id} and Codex session {codex_session_id}"
                )
            })?;
        rows.next()
            .await
            .context("Failed to read Git finalization row")?
            .map(|row| git_finalization_record_from_row(&row))
            .transpose()
    }

    pub(crate) fn list_pending_git_finalizations_blocking(
        &self,
        project_id: Option<i64>,
    ) -> Result<Vec<GitFinalizationRecord>> {
        self.blocking
            .block_on(self.list_pending_git_finalizations(project_id))
    }

    async fn list_pending_git_finalizations(
        &self,
        project_id: Option<i64>,
    ) -> Result<Vec<GitFinalizationRecord>> {
        let conn = self.repositories.git_journals.connect().await?;
        let mut rows = conn
            .query(
                "SELECT project_id, codex_session_id, state, git_mode, starting_head,
                        branch_ref, upstream_ref, worktree_baseline, task_identity,
                        owner_run_token, commit_oid, generation, last_error, created_at,
                        updated_at, completed_at, acknowledged_at, acknowledged_run_id
                   FROM git_finalizations
                  WHERE state IN ('working', 'tracking', 'commit_pending', 'push_pending')
                    AND (?1 IS NULL OR project_id = ?1)
                  ORDER BY CAST(updated_at AS INTEGER), project_id, codex_session_id",
                params![project_id],
            )
            .await
            .context("Failed to list pending Git finalizations")?;
        let mut finalizations = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .context("Failed to read pending Git finalization row")?
        {
            finalizations.push(git_finalization_record_from_row(&row)?);
        }
        Ok(finalizations)
    }

    pub(crate) fn list_unacknowledged_completed_git_finalizations_blocking(
        &self,
        project_id: Option<i64>,
    ) -> Result<Vec<GitFinalizationRecord>> {
        self.blocking
            .block_on(self.list_unacknowledged_completed_git_finalizations(project_id))
    }

    async fn list_unacknowledged_completed_git_finalizations(
        &self,
        project_id: Option<i64>,
    ) -> Result<Vec<GitFinalizationRecord>> {
        let conn = self.repositories.git_journals.connect().await?;
        let mut rows = conn
            .query(
                "SELECT project_id, codex_session_id, state, git_mode, starting_head,
                        branch_ref, upstream_ref, worktree_baseline, task_identity,
                        owner_run_token, commit_oid, generation, last_error, created_at,
                        updated_at, completed_at, acknowledged_at, acknowledged_run_id
                   FROM git_finalizations
                  WHERE state = 'completed' AND acknowledged_at IS NULL
                    AND (?1 IS NULL OR project_id = ?1)
                  ORDER BY CAST(completed_at AS INTEGER), project_id, codex_session_id",
                params![project_id],
            )
            .await
            .context("Failed to list unacknowledged completed Git finalizations")?;
        let mut finalizations = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .context("Failed to read an unacknowledged Git finalization row")?
        {
            finalizations.push(git_finalization_record_from_row(&row)?);
        }
        Ok(finalizations)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn compare_and_set_git_finalization_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        expected_generation: i64,
        next_state: GitFinalizationState,
        owner_run_token: Option<&str>,
        commit_oid: Option<&str>,
        last_error: Option<&str>,
        updated_at: &str,
    ) -> Result<bool> {
        self.blocking
            .block_on_persist(self.compare_and_set_git_finalization(
                project_id,
                codex_session_id,
                expected_generation,
                next_state,
                None,
                None,
                false,
                owner_run_token,
                commit_oid,
                last_error,
                updated_at,
            ))
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn compare_and_set_owned_git_finalization_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        expected_generation: i64,
        next_state: GitFinalizationState,
        owner_run_token: &str,
        commit_oid: Option<&str>,
        last_error: Option<&str>,
        updated_at: &str,
    ) -> Result<bool> {
        self.blocking
            .block_on_persist(self.compare_and_set_git_finalization(
                project_id,
                codex_session_id,
                expected_generation,
                next_state,
                None,
                None,
                true,
                Some(owner_run_token),
                commit_oid,
                last_error,
                updated_at,
            ))
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn compare_and_set_git_finalization_with_identity_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        expected_generation: i64,
        next_state: GitFinalizationState,
        task_identity: &str,
        owner_run_token: Option<&str>,
        updated_at: &str,
    ) -> Result<bool> {
        self.blocking
            .block_on_persist(self.compare_and_set_git_finalization(
                project_id,
                codex_session_id,
                expected_generation,
                next_state,
                Some(task_identity),
                None,
                true,
                owner_run_token,
                None,
                None,
                updated_at,
            ))
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn track_git_finalization_with_manifest_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        expected_generation: i64,
        task_identity: &str,
        worktree_baseline: &str,
        owner_run_token: &str,
        updated_at: &str,
    ) -> Result<bool> {
        self.blocking
            .block_on_persist(self.compare_and_set_git_finalization(
                project_id,
                codex_session_id,
                expected_generation,
                GitFinalizationState::Tracking,
                Some(task_identity),
                Some(worktree_baseline),
                true,
                Some(owner_run_token),
                None,
                None,
                updated_at,
            ))
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn reseal_git_finalization_manifest_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        expected_generation: i64,
        task_identity: &str,
        worktree_baseline: &str,
        owner_run_token: &str,
        updated_at: &str,
    ) -> Result<bool> {
        self.blocking
            .block_on_persist(self.compare_and_set_git_finalization(
                project_id,
                codex_session_id,
                expected_generation,
                GitFinalizationState::CommitPending,
                Some(task_identity),
                Some(worktree_baseline),
                true,
                Some(owner_run_token),
                None,
                None,
                updated_at,
            ))
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn recover_git_finalization_intent_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        expected_generation: i64,
        task_identity: &str,
        owner_run_token: Option<&str>,
        updated_at: &str,
    ) -> Result<bool> {
        self.blocking
            .block_on_persist(self.compare_and_set_git_finalization(
                project_id,
                codex_session_id,
                expected_generation,
                GitFinalizationState::Tracking,
                Some(task_identity),
                None,
                false,
                owner_run_token,
                None,
                None,
                updated_at,
            ))
    }

    #[allow(clippy::too_many_arguments)]
    async fn compare_and_set_git_finalization(
        &self,
        project_id: i64,
        codex_session_id: &str,
        expected_generation: i64,
        next_state: GitFinalizationState,
        task_identity: Option<&str>,
        worktree_baseline: Option<&str>,
        require_running_owner: bool,
        owner_run_token: Option<&str>,
        commit_oid: Option<&str>,
        last_error: Option<&str>,
        updated_at: &str,
    ) -> Result<bool> {
        let mut conn = self.repositories.git_journals.connect().await?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .with_context(|| {
                format!(
                    "Failed to begin updating Git finalization for project {project_id} and Codex session {codex_session_id}"
                )
            })?;
        let current = {
            let mut rows = transaction
                .query(
                    "SELECT project_id, codex_session_id, state, git_mode, starting_head,
                            branch_ref, upstream_ref, worktree_baseline, task_identity,
                            owner_run_token, commit_oid, generation, last_error, created_at,
                            updated_at, completed_at, acknowledged_at, acknowledged_run_id
                       FROM git_finalizations
                      WHERE project_id = ?1 AND codex_session_id = ?2",
                    params![project_id, codex_session_id],
                )
                .await
                .with_context(|| {
                    format!(
                        "Failed to inspect Git finalization for project {project_id} and Codex session {codex_session_id}"
                    )
                })?;
            rows.next()
                .await
                .context("Failed to read Git finalization compare-and-set row")?
                .map(|row| git_finalization_record_from_row(&row))
                .transpose()?
        };
        let Some(current) = current else {
            transaction
                .commit()
                .await
                .context("Failed to finish compare-and-set for a missing Git finalization")?;
            return Ok(false);
        };
        if current.generation != expected_generation {
            transaction
                .commit()
                .await
                .context("Failed to finish compare-and-set for a changed Git finalization")?;
            return Ok(false);
        }
        if require_running_owner {
            let Some(owner_run_token) = owner_run_token else {
                anyhow::bail!("Git completion intent requires a running owner token");
            };
            if query_count(
                &transaction,
                "SELECT COUNT(*) FROM session_controls
                  WHERE project_id = ?1 AND codex_session_id = ?2
                    AND state = 'running' AND run_token = ?3",
                params![project_id, codex_session_id, owner_run_token],
            )
            .await?
                != 1
            {
                transaction.commit().await.with_context(|| {
                    format!(
                        "Failed to finish fenced Git completion intent for project {project_id} and Codex session {codex_session_id}"
                    )
                })?;
                return Ok(false);
            }
        }
        if !current.state.can_transition_to(next_state) {
            anyhow::bail!(
                "Invalid Git finalization transition from {} to {}",
                current.state.database_value(),
                next_state.database_value()
            );
        }
        if next_state == GitFinalizationState::PushPending
            && current.git_mode != AgentGitMode::CommitAndPush
        {
            anyhow::bail!("Only commit-and-push finalizations may enter push_pending");
        }
        if let (Some(current_identity), Some(next_identity)) =
            (current.task_identity.as_deref(), task_identity)
            && current_identity != next_identity
        {
            anyhow::bail!(
                "Git finalization task identity cannot change after completion intent is recorded"
            );
        }
        let effective_task_identity = task_identity.or(current.task_identity.as_deref());
        if next_state.is_finalizing() && effective_task_identity.is_none() {
            anyhow::bail!(
                "Git finalization cannot enter {} without a task identity",
                next_state.database_value()
            );
        }
        let effective_commit_oid = commit_oid.or(current.commit_oid.as_deref());
        if let (Some(current_oid), Some(next_oid)) = (current.commit_oid.as_deref(), commit_oid)
            && current_oid != next_oid
        {
            anyhow::bail!("Git finalization commit OID cannot change once recorded");
        }
        if matches!(
            next_state,
            GitFinalizationState::PushPending | GitFinalizationState::Completed
        ) && effective_commit_oid.is_none()
        {
            anyhow::bail!(
                "Git finalization cannot enter {} without a commit OID",
                next_state.database_value()
            );
        }
        if next_state == GitFinalizationState::Completed
            && ((current.git_mode == AgentGitMode::Commit
                && current.state != GitFinalizationState::CommitPending)
                || (current.git_mode == AgentGitMode::CommitAndPush
                    && current.state != GitFinalizationState::PushPending))
        {
            anyhow::bail!(
                "Git finalization cannot complete before its configured commit or push step"
            );
        }

        let completed_at = next_state.is_terminal().then_some(updated_at);
        let changed = transaction
            .execute(
                "UPDATE git_finalizations
                    SET state = ?1,
                        task_identity = COALESCE(task_identity, ?2),
                        worktree_baseline = COALESCE(?3, worktree_baseline),
                        owner_run_token = ?4,
                        commit_oid = CASE WHEN ?5 IS NULL THEN commit_oid ELSE ?5 END,
                        generation = generation + 1,
                        last_error = ?6,
                        updated_at = ?7,
                        completed_at = ?8
                  WHERE project_id = ?9 AND codex_session_id = ?10 AND generation = ?11",
                params![
                    next_state.database_value(),
                    task_identity,
                    worktree_baseline,
                    owner_run_token,
                    commit_oid,
                    last_error,
                    updated_at,
                    completed_at,
                    project_id,
                    codex_session_id,
                    expected_generation,
                ],
            )
            .await
            .with_context(|| {
                format!(
                    "Failed to update Git finalization for project {project_id} and Codex session {codex_session_id}"
                )
            })?;
        if changed == 1 {
            let next_generation = expected_generation
                .checked_add(1)
                .context("Git finalization generation overflowed")?;
            if next_state.is_terminal() {
                transaction
                    .execute(
                        "DELETE FROM session_controls
                          WHERE project_id = ?1 AND codex_session_id = ?2
                            AND state = 'resume_requested' AND child_pid IS NULL
                            AND interactive_holder IS NULL AND interactive_launch_token IS NULL
                            AND run_token = ?3 || CAST(?4 AS TEXT)",
                        params![
                            project_id,
                            codex_session_id,
                            AGENT_GIT_FINALIZATION_RESUME_TOKEN_PREFIX,
                            expected_generation
                        ],
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to clear the terminal Git finalization recovery fence for session {codex_session_id}"
                        )
                    })?;
            } else {
                transaction
                    .execute(
                    "UPDATE session_controls
                        SET run_token = ?1 || CAST(?2 AS TEXT), updated_at = ?3
                      WHERE project_id = ?4 AND codex_session_id = ?5
                        AND state = 'resume_requested' AND child_pid IS NULL
                        AND interactive_holder IS NULL AND interactive_launch_token IS NULL
                        AND run_token = ?1 || CAST(?6 AS TEXT)",
                    params![
                        AGENT_GIT_FINALIZATION_RESUME_TOKEN_PREFIX,
                        next_generation,
                        updated_at,
                        project_id,
                        codex_session_id,
                        expected_generation
                    ],
                )
                .await
                .with_context(|| {
                    format!(
                        "Failed to advance the Git finalization recovery fence for session {codex_session_id}"
                    )
                })?;
            }
        }
        transaction.commit().await.with_context(|| {
            format!(
                "Failed to commit Git finalization update for project {project_id} and Codex session {codex_session_id}"
            )
        })?;
        Ok(changed == 1)
    }

    #[cfg(test)]
    pub(crate) fn delete_terminal_git_finalization_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
    ) -> Result<bool> {
        self.blocking.block_on_persist(async {
                let conn = self.repositories.git_journals.connect().await?;
                let removed = conn
                    .execute(
                        "DELETE FROM git_finalizations
                          WHERE project_id = ?1 AND codex_session_id = ?2
                            AND state IN ('completed', 'cancelled')",
                        params![project_id, codex_session_id],
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to delete terminal Git finalization for project {project_id} and Codex session {codex_session_id}"
                        )
                    })?;
                Ok(removed == 1)
            })
    }

    pub(crate) fn acknowledge_completed_git_finalization_session_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
    ) -> Result<bool> {
        self.blocking.block_on_persist(async {
                let mut conn = self.repositories.git_journals.connect().await?;
                let acknowledged_at = agent_timestamp();
                let transaction = conn
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to begin acknowledging completed Git finalization for project {project_id} and Codex session {codex_session_id}"
                        )
                    })?;
                let completed = {
                    let mut rows = transaction
                        .query(
                            "SELECT completed_at, commit_oid, acknowledged_at,
                                    acknowledged_run_id
                               FROM git_finalizations
                              WHERE project_id = ?1 AND codex_session_id = ?2
                                AND state = 'completed'",
                            params![project_id, codex_session_id],
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "Failed to inspect completed Git finalization for project {project_id} and Codex session {codex_session_id}"
                            )
                        })?;
                    rows
                        .next()
                        .await
                        .context("Failed to read completed Git finalization acknowledgement")?
                        .map(|row| {
                            Ok::<_, anyhow::Error>((
                                row_text(&row, 0, "completed_at")?,
                                row_optional_text(&row, 1, "commit_oid")?,
                                row_optional_text(&row, 2, "acknowledged_at")?,
                                row_optional_integer(&row, 3, "acknowledged_run_id")?,
                            ))
                        })
                        .transpose()?
                };
                let Some((completed_at, commit_oid, prior_acknowledgement, _)) = completed
                else {
                    transaction.commit().await.with_context(|| {
                        format!(
                            "Failed to finish acknowledging absent Git finalization for project {project_id} and Codex session {codex_session_id}"
                        )
                    })?;
                    return Ok(false);
                };

                if prior_acknowledgement.is_some() {
                    transaction
                        .execute(
                            "DELETE FROM session_controls
                              WHERE project_id = ?1 AND codex_session_id = ?2
                                AND state = 'resume_requested' AND child_pid IS NULL
                                AND interactive_holder IS NULL
                                AND run_token LIKE ?3",
                            params![
                                project_id,
                                codex_session_id,
                                format!("{AGENT_GIT_FINALIZATION_RESUME_TOKEN_PREFIX}%")
                            ],
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "Failed to clear a late resume request for completed Git finalization {codex_session_id}"
                            )
                        })?;
                    transaction.commit().await.with_context(|| {
                        format!(
                            "Failed to commit idempotent Git finalization acknowledgement for {codex_session_id}"
                        )
                    })?;
                    return Ok(true);
                }

                let latest_session_run = {
                    let mut rows = transaction
                        .query(
                            "SELECT id, status, finished_at
                               FROM runs
                              WHERE project_id = ?1 AND codex_session_id = ?2
                              ORDER BY id DESC
                              LIMIT 1",
                            params![project_id, codex_session_id],
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "Failed to find an existing successful run for Git finalization {codex_session_id}"
                            )
                        })?;
                    rows
                        .next()
                        .await
                        .context("Failed to read an existing Git finalization run")?
                        .map(|row| {
                            Ok::<_, anyhow::Error>((
                                row_integer(&row, 0, "id")?,
                                row_text(&row, 1, "status")?,
                                row_optional_text(&row, 2, "finished_at")?,
                            ))
                        })
                        .transpose()?
                };
                let short_commit = commit_oid
                    .as_deref()
                    .map(|oid| &oid[..oid.len().min(12)])
                    .unwrap_or("unknown");
                let summary = format!(
                    "CLT recovered the proven Git finalization at commit {short_commit} after an interrupted run acknowledgement."
                );
                let acknowledged_run_id = match latest_session_run {
                    Some((run_id, status, finished_at))
                        if matches!(status.as_str(), "success" | "idle")
                            && finished_at
                                .as_deref()
                                .and_then(|value| value.parse::<u64>().ok())
                                >= completed_at.parse::<u64>().ok() =>
                    {
                        run_id
                    }
                    _ => {
                    transaction
                        .execute(
                            "INSERT INTO runs (
                                project_id, status, started_at, finished_at, summary,
                                codex_session_id
                             ) VALUES (?1, 'success', ?2, ?2, ?3, ?4)",
                            params![
                                project_id,
                                completed_at.as_str(),
                                summary.as_str(),
                                codex_session_id,
                            ],
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "Failed to record recovered success for Git finalization {codex_session_id}"
                            )
                        })?;
                    query_count(&transaction, "SELECT last_insert_rowid()", ()).await?
                    }
                };

                let latest_project_run_id =
                    query_count(&transaction, "SELECT COALESCE(MAX(id), 0) FROM runs WHERE project_id = ?1", [project_id]).await?;
                if latest_project_run_id == acknowledged_run_id {
                    update_project_after_run(
                        &transaction,
                        &AgentRunOutcome {
                            project_id,
                            status: "success",
                            started_at: &completed_at,
                            finished_at: Some(&completed_at),
                            exit_code: None,
                            log_dir: None,
                            stdout_path: None,
                            stderr_path: None,
                            summary: Some(&summary),
                            codex_session_id: Some(codex_session_id),
                        },
                    )
                    .await?;
                }

                let marked = transaction
                    .execute(
                        "UPDATE git_finalizations
                            SET acknowledged_at = ?1, acknowledged_run_id = ?2
                          WHERE project_id = ?3 AND codex_session_id = ?4
                            AND state = 'completed' AND acknowledged_at IS NULL",
                        params![
                            acknowledged_at.as_str(),
                            acknowledged_run_id,
                            project_id,
                            codex_session_id,
                        ],
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to mark Git finalization {codex_session_id} acknowledged"
                        )
                    })?;
                if marked != 1 {
                    anyhow::bail!(
                        "Git finalization acknowledgement for {codex_session_id} changed inside its exclusive transaction"
                    );
                }
                transaction
                    .execute(
                        "DELETE FROM session_controls
                          WHERE project_id = ?1 AND codex_session_id = ?2
                            AND state = 'resume_requested' AND child_pid IS NULL
                            AND interactive_holder IS NULL
                            AND run_token LIKE ?3
                            AND EXISTS (
                                SELECT 1 FROM git_finalizations
                                 WHERE project_id = ?1 AND codex_session_id = ?2
                                   AND state = 'completed'
                            )",
                        params![
                            project_id,
                            codex_session_id,
                            format!("{AGENT_GIT_FINALIZATION_RESUME_TOKEN_PREFIX}%")
                        ],
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to acknowledge completed Git finalization for project {project_id} and Codex session {codex_session_id}"
                        )
                    })?;
                transaction.commit().await.with_context(|| {
                    format!(
                        "Failed to commit completed Git finalization acknowledgement for project {project_id} and Codex session {codex_session_id}"
                    )
                })?;
                Ok(true)
            })
    }
}
