use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use turso::{Connection, Database, params, transaction::TransactionBehavior};

use super::RepositoryDatabase;
use crate::{
    agent::{
        AGENT_GIT_FINALIZATION_RESUME_TOKEN_PREFIX, AgentDaemonCheckin, AgentGitMode,
        AgentKnownSessionRegistration, AgentRunOutcome, AgentRunRecord, AgentSessionControlRecord,
        AgentSessionControlState, TursoAgentStore, query_count, row_integer, row_optional_integer,
        row_optional_text, row_text, update_project_after_run,
    },
    application::AgentCleanSummary,
    runner::{agent_timestamp, agent_timestamp_after},
    session_control::{InteractiveGuardianDisposition, is_stopped_shared_interactive_holder},
};

/// Persistence for Codex session controls, runs, and daemon check-ins.
pub(in crate::agent) struct SessionsRunsRepository(RepositoryDatabase);

impl SessionsRunsRepository {
    pub(in crate::agent) fn new(db: &Database) -> Self {
        Self(RepositoryDatabase::new(db))
    }

    pub(in crate::agent) async fn connect(&self) -> Result<Connection> {
        self.0.connect().await
    }
}

impl TursoAgentStore {
    pub(crate) fn record_run_outcome_blocking(&self, outcome: AgentRunOutcome<'_>) -> Result<i64> {
        self.blocking.block_on(self.record_run_outcome(outcome))
    }

    async fn record_run_outcome(&self, outcome: AgentRunOutcome<'_>) -> Result<i64> {
        let conn = self.repositories.sessions_runs.connect().await?;

        conn.execute(
            "INSERT INTO runs (
                project_id, status, started_at, finished_at, exit_code,
                log_dir, stdout_path, stderr_path, summary, codex_session_id
             )
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                outcome.project_id,
                outcome.status,
                outcome.started_at,
                outcome.finished_at,
                outcome.exit_code,
                outcome.log_dir,
                outcome.stdout_path,
                outcome.stderr_path,
                outcome.summary,
                outcome.codex_session_id
            ],
        )
        .await
        .with_context(|| format!("Failed to record run for project {}", outcome.project_id))?;

        let run_id = query_count(&conn, "SELECT last_insert_rowid()", ()).await?;
        update_project_after_run(&conn, &outcome).await?;

        Ok(run_id)
    }

    pub(crate) fn list_recent_runs_blocking(&self, limit: i64) -> Result<Vec<AgentRunRecord>> {
        self.blocking.block_on(self.list_recent_runs(limit))
    }

    async fn list_recent_runs(&self, limit: i64) -> Result<Vec<AgentRunRecord>> {
        let conn = self.repositories.sessions_runs.connect().await?;
        let mut rows = conn
            .query(
                "SELECT r.id, r.project_id, p.name, p.path, r.status, r.started_at,
                        r.finished_at, r.exit_code, r.stdout_path, r.stderr_path, r.summary,
                        r.codex_session_id
                 FROM runs r
                 JOIN projects p ON p.id = r.project_id
                 ORDER BY r.id DESC
                 LIMIT ?1",
                params![limit],
            )
            .await
            .context("Failed to list recent agent runs")?;
        let mut runs = Vec::new();

        while let Some(row) = rows.next().await.context("Failed to read run row")? {
            runs.push(AgentRunRecord {
                id: row_integer(&row, 0, "id")?,
                project_id: row_integer(&row, 1, "project_id")?,
                project_name: row_text(&row, 2, "name")?,
                project_path: PathBuf::from(row_text(&row, 3, "path")?),
                status: row_text(&row, 4, "status")?,
                started_at: row_text(&row, 5, "started_at")?,
                finished_at: row_optional_text(&row, 6, "finished_at")?,
                exit_code: row_optional_integer(&row, 7, "exit_code")?,
                stdout_path: row_optional_text(&row, 8, "stdout_path")?,
                stderr_path: row_optional_text(&row, 9, "stderr_path")?,
                summary: row_optional_text(&row, 10, "summary")?,
                codex_session_id: row_optional_text(&row, 11, "codex_session_id")?,
            });
        }

        Ok(runs)
    }

    pub(crate) fn latest_run_for_project_blocking(
        &self,
        project_id: i64,
    ) -> Result<Option<AgentRunRecord>> {
        self.blocking
            .block_on(self.latest_run_for_project(project_id))
    }

    async fn latest_run_for_project(&self, project_id: i64) -> Result<Option<AgentRunRecord>> {
        let conn = self.repositories.sessions_runs.connect().await?;
        let mut rows = conn
            .query(
                "SELECT r.id, r.project_id, p.name, p.path, r.status, r.started_at,
                        r.finished_at, r.exit_code, r.stdout_path, r.stderr_path, r.summary,
                        r.codex_session_id
                 FROM runs r
                 JOIN projects p ON p.id = r.project_id
                 WHERE r.project_id = ?1
                 ORDER BY r.id DESC
                 LIMIT 1",
                [project_id],
            )
            .await
            .with_context(|| format!("Failed to find latest run for project {project_id}"))?;

        let Some(row) = rows
            .next()
            .await
            .context("Failed to read latest agent run row")?
        else {
            return Ok(None);
        };

        Ok(Some(AgentRunRecord {
            id: row_integer(&row, 0, "id")?,
            project_id: row_integer(&row, 1, "project_id")?,
            project_name: row_text(&row, 2, "name")?,
            project_path: PathBuf::from(row_text(&row, 3, "path")?),
            status: row_text(&row, 4, "status")?,
            started_at: row_text(&row, 5, "started_at")?,
            finished_at: row_optional_text(&row, 6, "finished_at")?,
            exit_code: row_optional_integer(&row, 7, "exit_code")?,
            stdout_path: row_optional_text(&row, 8, "stdout_path")?,
            stderr_path: row_optional_text(&row, 9, "stderr_path")?,
            summary: row_optional_text(&row, 10, "summary")?,
            codex_session_id: row_optional_text(&row, 11, "codex_session_id")?,
        }))
    }

    pub(crate) fn latest_run_for_codex_session_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
    ) -> Result<Option<AgentRunRecord>> {
        self.blocking
            .block_on(self.latest_run_for_codex_session(project_id, codex_session_id))
    }

    async fn latest_run_for_codex_session(
        &self,
        project_id: i64,
        codex_session_id: &str,
    ) -> Result<Option<AgentRunRecord>> {
        let conn = self.repositories.sessions_runs.connect().await?;
        let mut rows = conn
            .query(
                "SELECT r.id, r.project_id, p.name, p.path, r.status, r.started_at,
                        r.finished_at, r.exit_code, r.stdout_path, r.stderr_path, r.summary,
                        r.codex_session_id
                 FROM runs r
                 JOIN projects p ON p.id = r.project_id
                 WHERE r.project_id = ?1 AND r.codex_session_id = ?2
                 ORDER BY r.id DESC
                 LIMIT 1",
                params![project_id, codex_session_id],
            )
            .await
            .with_context(|| {
                format!(
                    "Failed to find run for project {project_id} and Codex session {codex_session_id}"
                )
            })?;

        let Some(row) = rows
            .next()
            .await
            .context("Failed to read Codex session run row")?
        else {
            return Ok(None);
        };

        Ok(Some(AgentRunRecord {
            id: row_integer(&row, 0, "id")?,
            project_id: row_integer(&row, 1, "project_id")?,
            project_name: row_text(&row, 2, "name")?,
            project_path: PathBuf::from(row_text(&row, 3, "path")?),
            status: row_text(&row, 4, "status")?,
            started_at: row_text(&row, 5, "started_at")?,
            finished_at: row_optional_text(&row, 6, "finished_at")?,
            exit_code: row_optional_integer(&row, 7, "exit_code")?,
            stdout_path: row_optional_text(&row, 8, "stdout_path")?,
            stderr_path: row_optional_text(&row, 9, "stderr_path")?,
            summary: row_optional_text(&row, 10, "summary")?,
            codex_session_id: row_optional_text(&row, 11, "codex_session_id")?,
        }))
    }

    pub(crate) fn record_daemon_checkin_blocking(
        &self,
        holder: &str,
        mode: &str,
        started_at: &str,
        checked_in_at: &str,
        expires_at: &str,
    ) -> Result<()> {
        self.blocking.block_on(self.record_daemon_checkin(
            holder,
            mode,
            started_at,
            checked_in_at,
            expires_at,
        ))
    }

    async fn record_daemon_checkin(
        &self,
        holder: &str,
        mode: &str,
        started_at: &str,
        checked_in_at: &str,
        expires_at: &str,
    ) -> Result<()> {
        let conn = self.repositories.sessions_runs.connect().await?;

        conn.execute(
            "INSERT INTO daemon_checkins (holder, mode, started_at, checked_in_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(holder) DO UPDATE SET
                mode = excluded.mode,
                started_at = excluded.started_at,
                checked_in_at = excluded.checked_in_at,
                expires_at = excluded.expires_at",
            params![holder, mode, started_at, checked_in_at, expires_at],
        )
        .await
        .with_context(|| format!("Failed to record daemon check-in for {holder}"))?;

        Ok(())
    }

    pub(crate) fn clear_daemon_checkin_blocking(&self, holder: &str) -> Result<bool> {
        self.blocking.block_on(self.clear_daemon_checkin(holder))
    }

    async fn clear_daemon_checkin(&self, holder: &str) -> Result<bool> {
        let conn = self.repositories.sessions_runs.connect().await?;
        let removed = conn
            .execute("DELETE FROM daemon_checkins WHERE holder = ?1", [holder])
            .await
            .with_context(|| format!("Failed to clear daemon check-in for {holder}"))?;

        Ok(removed > 0)
    }

    pub(crate) fn list_daemon_checkins_blocking(&self) -> Result<Vec<AgentDaemonCheckin>> {
        self.blocking.block_on(self.list_daemon_checkins())
    }

    async fn list_daemon_checkins(&self) -> Result<Vec<AgentDaemonCheckin>> {
        let conn = self.repositories.sessions_runs.connect().await?;
        let mut rows = conn
            .query(
                "SELECT holder, mode, started_at, checked_in_at, expires_at
                 FROM daemon_checkins
                 ORDER BY CAST(checked_in_at AS INTEGER) DESC, holder",
                (),
            )
            .await
            .context("Failed to list daemon check-ins")?;
        let mut checkins = Vec::new();

        while let Some(row) = rows
            .next()
            .await
            .context("Failed to read daemon check-in row")?
        {
            checkins.push(AgentDaemonCheckin {
                holder: row_text(&row, 0, "holder")?,
                mode: row_text(&row, 1, "mode")?,
                started_at: row_text(&row, 2, "started_at")?,
                checked_in_at: row_text(&row, 3, "checked_in_at")?,
                expires_at: row_text(&row, 4, "expires_at")?,
            });
        }

        Ok(checkins)
    }

    pub(crate) fn clean_agent_history_blocking(
        &self,
        cleaned_at: &str,
    ) -> Result<AgentCleanSummary> {
        self.blocking.block_on(self.clean_agent_history(cleaned_at))
    }

    async fn clean_agent_history(&self, cleaned_at: &str) -> Result<AgentCleanSummary> {
        let mut conn = self.repositories.sessions_runs.connect().await?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .context("Failed to begin cleaning agent history")?;
        if query_count(
            &transaction,
            "SELECT COUNT(*) FROM agent_workers
              WHERE state IN ('dispatching', 'running', 'finalizing')",
            (),
        )
        .await?
            > 0
        {
            anyhow::bail!("Cannot clean agent history while independent workers are active");
        }
        if query_count(
            &transaction,
            "SELECT COUNT(*) FROM git_finalizations
              WHERE state IN ('working', 'tracking', 'commit_pending', 'push_pending')",
            (),
        )
        .await?
            > 0
        {
            anyhow::bail!("Cannot clean agent history while Git finalization is pending");
        }
        if query_count(
            &transaction,
            "SELECT COUNT(*) FROM agent_git_launch_states",
            (),
        )
        .await?
            > 0
        {
            anyhow::bail!(
                "Cannot clean agent history while an unconsumed Git launch boundary remains"
            );
        }
        if query_count(
            &transaction,
            "SELECT COUNT(*) FROM leases
              WHERE CAST(expires_at AS INTEGER) > CAST(?1 AS INTEGER)",
            [cleaned_at],
        )
        .await?
            > 0
        {
            anyhow::bail!("Cannot clean agent history while project leases are active");
        }

        let projects_reset = transaction
            .execute(
                "UPDATE projects
                 SET failure_count = 0,
                     last_failure_at = NULL,
                     last_blocked_recovery_at = NULL,
                     updated_at = ?1
                 WHERE failure_count <> 0
                    OR last_failure_at IS NOT NULL
                    OR last_blocked_recovery_at IS NOT NULL",
                [cleaned_at],
            )
            .await
            .context("Failed to reset agent project failure state")?;
        transaction
            .execute("DELETE FROM agent_workers", ())
            .await
            .context("Failed to delete terminal agent worker records")?;
        transaction
            .execute("DELETE FROM git_finalizations", ())
            .await
            .context("Failed to delete terminal Git finalization records")?;
        transaction
            .execute("DELETE FROM agent_git_launch_states", ())
            .await
            .context("Failed to delete stale prelaunch Git states")?;
        let runs_deleted = transaction
            .execute("DELETE FROM runs", ())
            .await
            .context("Failed to delete agent run records")?;
        let leases_deleted = transaction
            .execute("DELETE FROM leases", ())
            .await
            .context("Failed to delete agent leases")?;
        let daemon_checkins_deleted = transaction
            .execute("DELETE FROM daemon_checkins", ())
            .await
            .context("Failed to delete agent daemon check-ins")?;
        transaction
            .commit()
            .await
            .context("Failed to commit cleaned agent history")?;

        Ok(AgentCleanSummary {
            projects_reset,
            runs_deleted,
            leases_deleted,
            daemon_checkins_deleted,
            run_log_dirs_removed: 0,
            service_logs_truncated: 0,
        })
    }
}

impl TursoAgentStore {
    pub(crate) fn mark_session_running_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        child_pid: u32,
        run_token: &str,
        stdout_path: &Path,
        stderr_path: &Path,
    ) -> Result<()> {
        self.blocking.block_on(self.mark_session_running(
            project_id,
            codex_session_id,
            child_pid,
            run_token,
            stdout_path,
            stderr_path,
            None,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn mark_session_running_with_git_finalization_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        child_pid: u32,
        run_token: &str,
        stdout_path: &Path,
        stderr_path: &Path,
        git_mode: AgentGitMode,
    ) -> Result<()> {
        self.blocking.block_on(self.mark_session_running(
            project_id,
            codex_session_id,
            child_pid,
            run_token,
            stdout_path,
            stderr_path,
            Some(git_mode),
        ))
    }

    #[allow(clippy::too_many_arguments)]
    async fn mark_session_running(
        &self,
        project_id: i64,
        codex_session_id: &str,
        child_pid: u32,
        run_token: &str,
        stdout_path: &Path,
        stderr_path: &Path,
        git_mode: Option<AgentGitMode>,
    ) -> Result<()> {
        let mut conn = self.repositories.sessions_runs.connect().await?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .with_context(|| {
                format!(
                    "Failed to begin registering Codex session {codex_session_id} for project {project_id}"
                )
            })?;
        let known_worker = query_count(
            &transaction,
            "SELECT COUNT(*) FROM agent_workers WHERE worker_token = ?1",
            [run_token],
        )
        .await?
            == 1;
        let fenced_worker = query_count(
            &transaction,
            "SELECT COUNT(*)
               FROM agent_workers w
               JOIN leases l
                 ON l.project_id = w.project_id AND l.holder = w.lease_holder
              WHERE w.worker_token = ?1 AND w.project_id = ?2
                AND w.state IN ('dispatching', 'running', 'finalizing')",
            params![run_token, project_id],
        )
        .await?
            == 1;
        if known_worker && !fenced_worker {
            anyhow::bail!(
                "Codex session {codex_session_id} worker generation no longer owns its lease"
            );
        }
        if fenced_worker
            && query_count(
                &transaction,
                "SELECT COUNT(*) FROM session_controls
                  WHERE project_id = ?1 AND codex_session_id = ?2
                    AND run_token IS NOT NULL AND run_token <> ?3",
                params![project_id, codex_session_id, run_token],
            )
            .await?
                > 0
        {
            anyhow::bail!(
                "Codex session {codex_session_id} belongs to a different active run generation"
            );
        }
        let changed = transaction.execute(
            "INSERT INTO session_controls (
                project_id, codex_session_id, state, child_pid, run_token,
                interactive_holder, stdout_path, stderr_path, updated_at
             ) VALUES (?1, ?2, 'running', ?3, ?4, NULL, ?5, ?6, ?7)
             ON CONFLICT(project_id, codex_session_id) DO UPDATE SET
                state = CASE
                    WHEN session_controls.run_token = excluded.run_token
                     AND session_controls.state IN ('stop_requested', 'interrupt_requested')
                        THEN session_controls.state
                    ELSE 'running'
                END,
                child_pid = excluded.child_pid,
                run_token = excluded.run_token,
                interactive_holder = CASE
                    WHEN session_controls.run_token = excluded.run_token
                     AND session_controls.state = 'interrupt_requested'
                        THEN session_controls.interactive_holder
                    ELSE NULL
                END,
                stdout_path = excluded.stdout_path,
                stderr_path = excluded.stderr_path,
                updated_at = excluded.updated_at",
            params![
                project_id,
                codex_session_id,
                i64::from(child_pid),
                run_token,
                stdout_path.to_string_lossy().as_ref(),
                stderr_path.to_string_lossy().as_ref(),
                agent_timestamp()
            ],
        )
        .await
        .with_context(|| {
            format!(
                "Failed to mark Codex session {codex_session_id} running for project {project_id}"
            )
        })?;
        if changed != 1 {
            anyhow::bail!(
                "Codex session {codex_session_id} belongs to a different active run generation"
            );
        }
        if let Some(git_mode) = git_mode {
            if git_mode == AgentGitMode::Off {
                anyhow::bail!("An atomic Git session registration cannot use Git mode off");
            }
            let created_at = agent_timestamp();
            let inserted = transaction
                .execute(
                    "INSERT OR IGNORE INTO git_finalizations (
                        project_id, codex_session_id, state, git_mode, starting_head,
                        branch_ref, upstream_ref, worktree_baseline, task_identity,
                        owner_run_token, commit_oid, generation, last_error,
                        created_at, updated_at, completed_at
                     )
                     SELECT launch.project_id, ?2, 'working', launch.git_mode,
                            launch.starting_head, launch.branch_ref, launch.upstream_ref,
                            launch.worktree_baseline, NULL, launch.run_token, NULL, 0, NULL,
                            ?5, ?5, NULL
                       FROM agent_git_launch_states launch
                      WHERE launch.project_id = ?1 AND launch.run_token = ?3
                        AND launch.git_mode = ?4",
                    params![
                        project_id,
                        codex_session_id,
                        run_token,
                        git_mode.database_value(),
                        created_at.as_str(),
                    ],
                )
                .await
                .with_context(|| {
                    format!(
                        "Failed to atomically create the Git journal for Codex session {codex_session_id}"
                    )
                })?;
            let compatible_journal = query_count(
                &transaction,
                "SELECT COUNT(*)
                   FROM git_finalizations
                  WHERE project_id = ?1 AND codex_session_id = ?2
                    AND state = 'working' AND git_mode = ?3
                    AND owner_run_token = ?4",
                params![
                    project_id,
                    codex_session_id,
                    git_mode.database_value(),
                    run_token,
                ],
            )
            .await?
                == 1;
            if !compatible_journal
                || (inserted == 0
                    && query_count(
                        &transaction,
                        "SELECT COUNT(*)
                       FROM git_finalizations finalization
                       LEFT JOIN agent_git_launch_states launch
                         ON launch.project_id = finalization.project_id
                        AND launch.run_token = finalization.owner_run_token
                      WHERE finalization.project_id = ?1
                        AND finalization.codex_session_id = ?2
                        AND finalization.state = 'working'
                        AND finalization.git_mode = ?3
                        AND finalization.owner_run_token = ?4
                        AND (launch.run_token IS NULL OR (
                            finalization.git_mode = launch.git_mode
                            AND finalization.starting_head = launch.starting_head
                            AND finalization.branch_ref IS launch.branch_ref
                            AND finalization.upstream_ref IS launch.upstream_ref
                            AND finalization.worktree_baseline = launch.worktree_baseline
                        ))",
                        params![
                            project_id,
                            codex_session_id,
                            git_mode.database_value(),
                            run_token,
                        ],
                    )
                    .await?
                        != 1)
            {
                anyhow::bail!(
                    "Automated run {run_token} has no compatible scheduler-owned Git launch state for Codex session {codex_session_id}"
                );
            }
            if inserted == 1 {
                let deleted = transaction
                    .execute(
                        "DELETE FROM agent_git_launch_states
                          WHERE project_id = ?1 AND run_token = ?2",
                        params![project_id, run_token],
                    )
                    .await
                    .context("Failed to consume the scheduler-owned Git launch state")?;
                if deleted != 1 {
                    anyhow::bail!(
                        "Automated run {run_token} lost its Git launch state while registering Codex session {codex_session_id}"
                    );
                }
            }
        }
        transaction.commit().await.with_context(|| {
            format!(
                "Failed to commit Codex session {codex_session_id} registration for project {project_id}"
            )
        })?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn set_session_control_state_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        state: AgentSessionControlState,
    ) -> Result<()> {
        self.blocking
            .block_on(self.set_session_control_state(project_id, codex_session_id, state))
    }

    #[cfg(test)]
    async fn set_session_control_state(
        &self,
        project_id: i64,
        codex_session_id: &str,
        state: AgentSessionControlState,
    ) -> Result<()> {
        let conn = self.repositories.sessions_runs.connect().await?;
        conn.execute(
            "INSERT INTO session_controls (
                project_id, codex_session_id, state, child_pid, updated_at
             ) VALUES (?1, ?2, ?3, NULL, ?4)
             ON CONFLICT(project_id, codex_session_id) DO UPDATE SET
                state = excluded.state,
                updated_at = excluded.updated_at",
            params![
                project_id,
                codex_session_id,
                state.database_value(),
                agent_timestamp()
            ],
        )
        .await
        .with_context(|| {
            format!(
                "Failed to set Codex session {codex_session_id} state to {} for project {project_id}",
                state.database_value()
            )
        })?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn set_session_control_recovery_token_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        run_token: &str,
    ) -> Result<()> {
        self.blocking.block_on(async {
                let conn = self.repositories.sessions_runs.connect().await?;
                conn.execute(
                    "INSERT INTO session_controls (
                        project_id, codex_session_id, state, child_pid, run_token,
                        interactive_holder, interactive_launch_token, updated_at
                     ) VALUES (?1, ?2, 'resume_requested', NULL, ?3, NULL, NULL, ?4)
                     ON CONFLICT(project_id, codex_session_id) DO UPDATE SET
                        state = 'resume_requested', child_pid = NULL, run_token = excluded.run_token,
                        interactive_holder = NULL, interactive_launch_token = NULL,
                        updated_at = excluded.updated_at",
                    params![project_id, codex_session_id, run_token, agent_timestamp()],
                )
                .await
                .with_context(|| {
                    format!(
                        "Failed to set the test recovery token for Codex session {codex_session_id}"
                    )
                })?;
                Ok(())
            })
    }

    pub(crate) fn request_session_interrupt_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        expected_child_pid: u32,
        expected_run_token: &str,
        interactive_holder: &str,
    ) -> Result<bool> {
        self.blocking.block_on(async {
        let conn = self.repositories.sessions_runs.connect().await?;
                let changed = conn.execute(
                    "UPDATE session_controls
                        SET state = 'interrupt_requested',
                            interactive_holder = ?1,
                            updated_at = ?2
                      WHERE project_id = ?3 AND codex_session_id = ?4
                        AND state = 'running' AND child_pid = ?5
                        AND run_token = ?6",
                    params![
                        interactive_holder,
                        agent_timestamp(),
                        project_id,
                        codex_session_id,
                        i64::from(expected_child_pid),
                        expected_run_token
                    ],
                )
                .await
                .with_context(|| {
                    format!(
                        "Failed to request interactive interruption for Codex session {codex_session_id}"
                    )
                })?;
                Ok(changed > 0)
            })
    }

    pub(crate) fn cancel_session_interrupt_handoff_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        interactive_holder: &str,
    ) -> Result<bool> {
        self.blocking.block_on(async {
            let conn = self.repositories.sessions_runs.connect().await?;
            let changed = conn
                .execute(
                    "UPDATE session_controls
                            SET state = CASE
                                    WHEN state = 'interrupt_requested' THEN 'running'
                                    ELSE 'resume_requested'
                                END,
                                child_pid = CASE
                                    WHEN state = 'interrupt_requested' THEN child_pid
                                    ELSE NULL
                                END,
                                interactive_holder = NULL,
                                interactive_launch_token = NULL,
                                updated_at = ?1
                          WHERE project_id = ?2 AND codex_session_id = ?3
                            AND state IN ('interrupt_requested', 'ready_interactive')
                            AND interactive_holder = ?4",
                    params![
                        agent_timestamp(),
                        project_id,
                        codex_session_id,
                        interactive_holder
                    ],
                )
                .await
                .with_context(|| {
                    format!(
                        "Failed to cancel interactive handoff for Codex session {codex_session_id}"
                    )
                })?;
            Ok(changed > 0)
        })
    }

    pub(crate) fn reserve_idle_session_interactive_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        interactive_holder: &str,
        expected_stopped_run_token: Option<&str>,
    ) -> Result<bool> {
        self.blocking.block_on(async {
            let conn = self.repositories.sessions_runs.connect().await?;
            let restore_stopped = interactive_holder.starts_with("clt-stopped-interactive-");
            let changed = if restore_stopped {
                conn.execute(
                    "UPDATE session_controls
                            SET state = 'ready_interactive', child_pid = NULL,
                                interactive_holder = ?1,
                                interactive_launch_token = NULL, updated_at = ?2
                          WHERE project_id = ?3 AND codex_session_id = ?4
                            AND state = 'stopped'
                            AND (
                                run_token = ?5
                                OR (run_token IS NULL AND ?5 IS NULL)
                            )
                            AND NOT EXISTS (
                                SELECT 1 FROM session_controls
                                 WHERE project_id = ?3
                                   AND codex_session_id <> ?4
                                   AND state <> 'stopped'
                            )",
                    params![
                        interactive_holder,
                        agent_timestamp(),
                        project_id,
                        codex_session_id,
                        expected_stopped_run_token
                    ],
                )
                .await
            } else {
                conn.execute(
                    "INSERT INTO session_controls (
                            project_id, codex_session_id, state, child_pid,
                            interactive_holder, updated_at
                         )
                         SELECT ?1, ?2, 'ready_interactive', NULL, ?3, ?4
                          WHERE NOT EXISTS (
                            SELECT 1 FROM session_controls
                             WHERE project_id = ?1 AND codex_session_id = ?2
                          )
                            AND NOT EXISTS (
                            SELECT 1 FROM session_controls
                             WHERE project_id = ?1 AND state <> 'stopped'
                          )
                         ON CONFLICT(project_id, codex_session_id) DO NOTHING",
                    params![
                        project_id,
                        codex_session_id,
                        interactive_holder,
                        agent_timestamp()
                    ],
                )
                .await
            }
            .with_context(|| {
                format!(
                    "Failed to reserve idle Codex session {codex_session_id} for interactive use"
                )
            })?;
            Ok(changed > 0)
        })
    }

    pub(crate) fn reserve_shared_session_interactive_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        interactive_holder: &str,
        expected_stopped_run_token: Option<&str>,
    ) -> Result<bool> {
        self.blocking.block_on(async {
                let mut conn = self.repositories.sessions_runs.connect().await?;
                let transaction = conn
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to begin reserving shared interactive session {codex_session_id}"
                        )
                    })?;
                let git_boundary_conflict = query_count(
                    &transaction,
                    "SELECT COUNT(*)
                       FROM projects p
                      WHERE p.id = ?1
                        AND (
                            EXISTS (
                                SELECT 1 FROM agent_git_launch_states launch
                                 WHERE launch.project_id = p.id
                            )
                            OR EXISTS (
                                SELECT 1 FROM git_finalizations finalization
                                 WHERE finalization.project_id = p.id
                                   AND finalization.state NOT IN ('completed', 'cancelled')
                            )
                            OR (
                                p.git_mode <> 'off'
                                AND EXISTS (
                                    SELECT 1 FROM leases
                                     WHERE leases.project_id = p.id
                                )
                            )
                        )",
                    [project_id],
                )
                .await?
                    != 0;
                if git_boundary_conflict {
                    transaction.commit().await.with_context(|| {
                        format!(
                            "Failed to finish rejecting unsafe shared interactive session {codex_session_id}"
                        )
                    })?;
                    return Ok(false);
                }
                let restore_stopped =
                    is_stopped_shared_interactive_holder(interactive_holder);
                let changed = if restore_stopped {
                    transaction.execute(
                        "UPDATE session_controls
                            SET state = 'ready_interactive', child_pid = NULL,
                                interactive_holder = ?1,
                                interactive_launch_token = NULL, updated_at = ?2
                          WHERE project_id = ?3 AND codex_session_id = ?4
                            AND state = 'stopped'
                            AND (
                                run_token = ?5
                                OR (run_token IS NULL AND ?5 IS NULL)
                            )
                            AND (
                                EXISTS (
                                    SELECT 1 FROM leases WHERE project_id = ?3
                                )
                                OR EXISTS (
                                    SELECT 1 FROM session_controls
                                     WHERE project_id = ?3
                                       AND codex_session_id <> ?4
                                       AND state <> 'stopped'
                                )
                            )",
                        params![
                            interactive_holder,
                            agent_timestamp(),
                            project_id,
                            codex_session_id,
                            expected_stopped_run_token
                        ],
                    )
                    .await
                } else {
                    transaction.execute(
                        "INSERT INTO session_controls (
                            project_id, codex_session_id, state, child_pid,
                            interactive_holder, updated_at
                         )
                         SELECT ?1, ?2, 'ready_interactive', NULL, ?3, ?4
                          WHERE NOT EXISTS (
                            SELECT 1 FROM session_controls
                             WHERE project_id = ?1 AND codex_session_id = ?2
                          )
                            AND (
                                EXISTS (
                                    SELECT 1 FROM leases WHERE project_id = ?1
                                )
                                OR EXISTS (
                                    SELECT 1 FROM session_controls
                                     WHERE project_id = ?1
                                       AND codex_session_id <> ?2
                                       AND state <> 'stopped'
                                )
                            )
                         ON CONFLICT(project_id, codex_session_id) DO NOTHING",
                        params![
                            project_id,
                            codex_session_id,
                            interactive_holder,
                            agent_timestamp()
                        ],
                    )
                    .await
                }
                .with_context(|| {
                    format!(
                        "Failed to reserve Codex session {codex_session_id} for shared interactive use"
                    )
                })?;
                transaction.commit().await.with_context(|| {
                    format!(
                        "Failed to commit shared interactive session {codex_session_id} reservation"
                    )
                })?;
                Ok(changed > 0)
            })
    }

    pub(crate) fn cancel_idle_session_interactive_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        interactive_holder: &str,
    ) -> Result<bool> {
        self.blocking.block_on(async {
                let conn = self.repositories.sessions_runs.connect().await?;
                let restore_stopped = interactive_holder
                    .starts_with("clt-stopped-interactive-")
                    || is_stopped_shared_interactive_holder(interactive_holder);
                let changed = if restore_stopped {
                    conn.execute(
                        "UPDATE session_controls
                            SET state = 'stopped', child_pid = NULL,
                                interactive_holder = NULL,
                                interactive_launch_token = NULL, updated_at = ?1
                          WHERE project_id = ?2 AND codex_session_id = ?3
                            AND state = 'ready_interactive'
                            AND interactive_holder = ?4",
                        params![
                            agent_timestamp(),
                            project_id,
                            codex_session_id,
                            interactive_holder
                        ],
                    )
                    .await
                } else {
                    conn.execute(
                        "DELETE FROM session_controls
                          WHERE project_id = ?1 AND codex_session_id = ?2
                            AND state = 'ready_interactive'
                            AND interactive_holder = ?3",
                        params![project_id, codex_session_id, interactive_holder],
                    )
                    .await
                }
                    .with_context(|| {
                        format!(
                            "Failed to cancel idle interactive reservation for Codex session {codex_session_id}"
                        )
                    })?;
                Ok(changed > 0)
            })
    }

    pub(crate) fn request_session_stop_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        expected_child_pid: u32,
        expected_run_token: &str,
    ) -> Result<bool> {
        self.blocking.block_on(async {
            let conn = self.repositories.sessions_runs.connect().await?;
            let changed = conn
                .execute(
                    "UPDATE session_controls
                            SET state = 'stop_requested', updated_at = ?1
                          WHERE project_id = ?2 AND codex_session_id = ?3
                            AND state = 'running' AND child_pid = ?4
                            AND run_token = ?5",
                    params![
                        agent_timestamp(),
                        project_id,
                        codex_session_id,
                        i64::from(expected_child_pid),
                        expected_run_token
                    ],
                )
                .await
                .with_context(|| {
                    format!("Failed to request stop for Codex session {codex_session_id}")
                })?;
            Ok(changed > 0)
        })
    }

    pub(crate) fn request_interactive_session_stop_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        expected_child_pid: u32,
        expected_interactive_holder: &str,
    ) -> Result<bool> {
        self.blocking.block_on(async {
            let conn = self.repositories.sessions_runs.connect().await?;
            let changed = conn
                .execute(
                    "UPDATE session_controls
                            SET state = 'stop_requested', updated_at = ?1
                          WHERE project_id = ?2 AND codex_session_id = ?3
                            AND state = 'interactive' AND child_pid = ?4
                            AND interactive_holder = ?5
                            AND interactive_launch_token = ?5",
                    params![
                        agent_timestamp(),
                        project_id,
                        codex_session_id,
                        i64::from(expected_child_pid),
                        expected_interactive_holder
                    ],
                )
                .await
                .with_context(|| {
                    format!(
                        "Failed to request stop for interactive Codex session {codex_session_id}"
                    )
                })?;
            Ok(changed > 0)
        })
    }

    pub(crate) fn request_stopped_session_resume_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        expected_run_token: Option<&str>,
    ) -> Result<bool> {
        self.blocking.block_on(async {
            let conn = self.repositories.sessions_runs.connect().await?;
            let changed = conn
                .execute(
                    "UPDATE session_controls
                            SET state = 'resume_requested', child_pid = NULL,
                                interactive_holder = NULL,
                                interactive_launch_token = NULL, updated_at = ?1
                          WHERE project_id = ?2 AND codex_session_id = ?3
                            AND state = 'stopped'
                            AND (
                                run_token = ?4
                                OR (run_token IS NULL AND ?4 IS NULL)
                            )",
                    params![
                        agent_timestamp(),
                        project_id,
                        codex_session_id,
                        expected_run_token
                    ],
                )
                .await
                .with_context(|| {
                    format!("Failed to request resume for stopped Codex session {codex_session_id}")
                })?;
            Ok(changed > 0)
        })
    }

    pub(crate) fn transition_session_control_state_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        from: AgentSessionControlState,
        to: AgentSessionControlState,
    ) -> Result<bool> {
        self.blocking.block_on(async {
            let conn = self.repositories.sessions_runs.connect().await?;
            let changed = conn
                .execute(
                    "UPDATE session_controls
                            SET state = ?1,
                                child_pid = CASE WHEN ?1 = 'running' THEN child_pid ELSE NULL END,
                                interactive_launch_token = NULL,
                                updated_at = ?2
                          WHERE project_id = ?3 AND codex_session_id = ?4 AND state = ?5",
                    params![
                        to.database_value(),
                        agent_timestamp(),
                        project_id,
                        codex_session_id,
                        from.database_value()
                    ],
                )
                .await
                .with_context(|| {
                    format!(
                        "Failed to transition Codex session {codex_session_id} from {} to {}",
                        from.database_value(),
                        to.database_value()
                    )
                })?;
            Ok(changed > 0)
        })
    }

    pub(crate) fn recover_stale_automated_session_control_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        from: AgentSessionControlState,
        to: AgentSessionControlState,
        expected_child_pid: u32,
        expected_run_token: Option<&str>,
    ) -> Result<bool> {
        self.blocking.block_on(async {
            let conn = self.repositories.sessions_runs.connect().await?;
            let changed = conn
                .execute(
                    "UPDATE session_controls
                            SET state = ?1, child_pid = NULL,
                                interactive_holder = NULL,
                                interactive_launch_token = NULL, updated_at = ?2
                          WHERE project_id = ?3 AND codex_session_id = ?4
                            AND state = ?5 AND child_pid = ?6
                            AND (
                                run_token = ?7
                                OR (run_token IS NULL AND ?7 IS NULL)
                            )",
                    params![
                        to.database_value(),
                        agent_timestamp(),
                        project_id,
                        codex_session_id,
                        from.database_value(),
                        i64::from(expected_child_pid),
                        expected_run_token
                    ],
                )
                .await
                .with_context(|| {
                    format!(
                        "Failed to recover stale Codex session {codex_session_id} from {} to {}",
                        from.database_value(),
                        to.database_value()
                    )
                })?;
            Ok(changed > 0)
        })
    }

    pub(crate) fn finalize_reaped_automated_session_blocking(
        &self,
        project_id: i64,
        expected_child_pid: u32,
        expected_run_token: &str,
        lease_holder: &str,
        lease_timeout_seconds: u64,
    ) -> Result<bool> {
        self.blocking.block_on(async {
                let mut conn = self.repositories.sessions_runs.connect().await?;
                let transaction = conn.transaction().await.with_context(|| {
                    format!(
                        "Failed to begin reaped automated-session finalization for project {project_id}"
                    )
                })?;
                let (codex_session_id, state, interactive_holder) = {
                    let mut rows = transaction
                        .query(
                            "SELECT codex_session_id, state, interactive_holder
                               FROM session_controls
                              WHERE project_id = ?1 AND child_pid = ?2 AND run_token = ?3",
                            params![
                                project_id,
                                i64::from(expected_child_pid),
                                expected_run_token
                            ],
                        )
                        .await
                        .context("Failed to read the reaped automated session generation")?;
                    let Some(row) = rows
                        .next()
                        .await
                        .context("Failed to read the reaped automated session row")?
                    else {
                        return Ok(false);
                    };
                    (
                        row_text(&row, 0, "codex_session_id")?,
                        AgentSessionControlState::from_database(&row_text(&row, 1, "state")?)?,
                        row_optional_text(&row, 2, "interactive_holder")?,
                    )
                };

                let terminal_state = match state {
                    AgentSessionControlState::Running => {
                        AgentSessionControlState::ResumeRequested
                    }
                    AgentSessionControlState::StopRequested => {
                        AgentSessionControlState::Stopped
                    }
                    AgentSessionControlState::InterruptRequested => {
                        AgentSessionControlState::ReadyInteractive
                    }
                    _ => return Ok(false),
                };

                if terminal_state == AgentSessionControlState::ReadyInteractive {
                    let Some(interactive_holder) = interactive_holder.as_deref() else {
                        return Ok(false);
                    };
                    let acquired_at = agent_timestamp();
                    let expires_at = agent_timestamp_after(lease_timeout_seconds);
                    let transferred = transaction
                        .execute(
                            "UPDATE leases
                                SET holder = ?1, acquired_at = ?2, expires_at = ?3
                              WHERE project_id = ?4 AND holder = ?5",
                            params![
                                interactive_holder,
                                acquired_at.as_str(),
                                expires_at.as_str(),
                                project_id,
                                lease_holder
                            ],
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "Failed to transfer the reaped project {project_id} lease for interactive handoff"
                            )
                        })?;
                    if transferred == 0 {
                        let inserted = transaction
                            .execute(
                                "INSERT OR IGNORE INTO leases (
                                    project_id, holder, acquired_at, expires_at
                                 ) VALUES (?1, ?2, ?3, ?4)",
                                params![
                                    project_id,
                                    interactive_holder,
                                    acquired_at.as_str(),
                                    expires_at.as_str()
                                ],
                            )
                            .await
                            .with_context(|| {
                                format!(
                                    "Failed to acquire the reaped project {project_id} lease for interactive handoff"
                                )
                            })?;
                        if inserted == 0 {
                            let existing_holder = {
                                let mut rows = transaction
                                    .query(
                                        "SELECT holder FROM leases WHERE project_id = ?1",
                                        [project_id],
                                    )
                                    .await
                                    .with_context(|| {
                                        format!(
                                            "Failed to inspect the reaped project {project_id} lease"
                                        )
                                    })?;
                                rows.next()
                                    .await
                                    .context("Failed to read the reaped project lease")?
                                    .map(|row| row_text(&row, 0, "holder"))
                                    .transpose()?
                            };
                            if existing_holder.as_deref() != Some(interactive_holder) {
                                return Ok(false);
                            }
                        }
                    }
                }

                let changed = transaction
                    .execute(
                        "UPDATE session_controls
                            SET state = ?1, child_pid = NULL,
                                interactive_holder = CASE
                                    WHEN ?1 = 'ready_interactive' THEN interactive_holder
                                    ELSE NULL
                                END,
                                updated_at = ?2
                          WHERE project_id = ?3 AND codex_session_id = ?4
                            AND state = ?5 AND child_pid = ?6 AND run_token = ?7",
                        params![
                            terminal_state.database_value(),
                            agent_timestamp(),
                            project_id,
                            codex_session_id.as_str(),
                            state.database_value(),
                            i64::from(expected_child_pid),
                            expected_run_token
                        ],
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to finalize reaped Codex session {codex_session_id}"
                        )
                    })?;
                if changed != 1 {
                    return Ok(false);
                }
                if terminal_state != AgentSessionControlState::ReadyInteractive {
                    transaction
                        .execute(
                            "DELETE FROM leases WHERE project_id = ?1 AND holder = ?2",
                            params![project_id, lease_holder],
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "Failed to release the reaped project {project_id} lease"
                            )
                        })?;
                }
                transaction.commit().await.with_context(|| {
                    format!(
                        "Failed to commit reaped automated-session finalization for project {project_id}"
                    )
                })?;
                Ok(true)
            })
    }

    pub(crate) fn recover_stale_interactive_session_control_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        from: AgentSessionControlState,
        to: AgentSessionControlState,
        expected_interactive_holder: Option<&str>,
    ) -> Result<bool> {
        self.blocking.block_on(async {
                let conn = self.repositories.sessions_runs.connect().await?;
                let changed = conn
                    .execute(
                        "UPDATE session_controls
                            SET state = ?1, child_pid = NULL,
                                interactive_holder = NULL,
                                interactive_launch_token = NULL, updated_at = ?2
                          WHERE project_id = ?3 AND codex_session_id = ?4
                            AND state = ?5
                            AND (
                                interactive_holder = ?6
                                OR (interactive_holder IS NULL AND ?6 IS NULL)
                            )",
                        params![
                            to.database_value(),
                            agent_timestamp(),
                            project_id,
                            codex_session_id,
                            from.database_value(),
                            expected_interactive_holder
                        ],
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to recover abandoned Codex interactive handoff {codex_session_id} from {} to {}",
                            from.database_value(),
                            to.database_value()
                        )
                    })?;
                Ok(changed > 0)
            })
    }

    pub(crate) fn begin_stopped_session_interactive_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        interactive_holder: &str,
        expected_run_token: Option<&str>,
    ) -> Result<bool> {
        self.blocking.block_on(async {
            let conn = self.repositories.sessions_runs.connect().await?;
            let changed = conn
                .execute(
                    "UPDATE session_controls
                            SET state = 'ready_interactive', child_pid = NULL,
                                interactive_holder = ?1,
                                interactive_launch_token = NULL, updated_at = ?2
                          WHERE project_id = ?3 AND codex_session_id = ?4
                            AND state = 'stopped'
                            AND (
                                run_token = ?5
                                OR (run_token IS NULL AND ?5 IS NULL)
                            )",
                    params![
                        interactive_holder,
                        agent_timestamp(),
                        project_id,
                        codex_session_id,
                        expected_run_token
                    ],
                )
                .await
                .with_context(|| {
                    format!("Failed to open stopped Codex session {codex_session_id} interactively")
                })?;
            Ok(changed > 0)
        })
    }

    pub(crate) fn adopt_interactive_guardian_blocking(
        &self,
        project_id: i64,
        codex_session_id: Option<&str>,
        from_holder: &str,
        guardian_holder: &str,
        lease_timeout_seconds: u64,
    ) -> Result<bool> {
        self.blocking.block_on(async {
                let disposition = InteractiveGuardianDisposition::from_guardian_holder(
                    guardian_holder,
                )
                .context("Invalid interactive guardian holder")?;
                let mut conn = self.repositories.sessions_runs.connect().await?;
                let transaction = conn.transaction().await.with_context(|| {
                    format!("Failed to begin interactive guardian for project {project_id}")
                })?;
                if disposition.holds_project_lease() {
                    let acquired_at = agent_timestamp();
                    let expires_at = agent_timestamp_after(lease_timeout_seconds);
                    let transferred = transaction
                        .execute(
                            "UPDATE leases
                                SET holder = ?1, acquired_at = ?2, expires_at = ?3
                              WHERE project_id = ?4 AND holder = ?5",
                            params![
                                guardian_holder,
                                acquired_at,
                                expires_at,
                                project_id,
                                from_holder
                            ],
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "Failed to transfer project {project_id} lease to its interactive guardian"
                            )
                        })?;
                    if transferred == 0 {
                        return Ok(false);
                    }
                }
                if let Some(codex_session_id) = codex_session_id {
                    let changed = transaction
                        .execute(
                            "UPDATE session_controls
                                SET state = 'interactive', interactive_holder = ?1,
                                    interactive_launch_token = ?1, child_pid = NULL,
                                    updated_at = ?2
                              WHERE project_id = ?3 AND codex_session_id = ?4
                                AND state = 'ready_interactive'
                                AND interactive_holder = ?5",
                            params![
                                guardian_holder,
                                agent_timestamp(),
                                project_id,
                                codex_session_id,
                                from_holder
                            ],
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "Failed to transfer Codex session {codex_session_id} to its interactive guardian"
                            )
                        })?;
                    if changed == 0 {
                        return Ok(false);
                    }
                }
                transaction.commit().await.with_context(|| {
                    format!(
                        "Failed to commit project {project_id} interactive guardian transfer"
                    )
                })?;
                Ok(true)
            })
    }

    pub(crate) fn register_interactive_guardian_child_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        guardian_holder: &str,
        child_pid: u32,
        lease_timeout_seconds: u64,
    ) -> Result<bool> {
        self.blocking.block_on(async {
                let disposition = InteractiveGuardianDisposition::from_guardian_holder(
                    guardian_holder,
                )
                .context("Invalid interactive guardian holder")?;
                let mut conn = self.repositories.sessions_runs.connect().await?;
                let now = agent_timestamp();
                let transaction = conn.transaction().await.with_context(|| {
                    format!(
                        "Failed to begin interactive child registration for project {project_id}"
                    )
                })?;
                if disposition.holds_project_lease() {
                    let fresh_expiry = agent_timestamp_after(lease_timeout_seconds);
                    let lease_changed = transaction
                        .execute(
                            "UPDATE leases SET expires_at = ?1
                              WHERE project_id = ?2 AND holder = ?3
                                AND expires_at > ?4",
                            params![
                                fresh_expiry.as_str(),
                                project_id,
                                guardian_holder,
                                now.as_str()
                            ],
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "Failed to renew interactive guardian lease for project {project_id}"
                            )
                        })?;
                    if lease_changed != 1 {
                        return Ok(false);
                    }
                }
                let control_changed = transaction
                    .execute(
                        "UPDATE session_controls
                            SET child_pid = ?1, updated_at = ?2
                          WHERE project_id = ?3 AND codex_session_id = ?4
                            AND state = 'interactive'
                            AND interactive_holder = ?5
                            AND interactive_launch_token = ?5
                            AND child_pid IS NULL",
                        params![
                            i64::from(child_pid),
                            now.as_str(),
                            project_id,
                            codex_session_id,
                            guardian_holder
                        ],
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to register interactive Codex child {child_pid} for session {codex_session_id}"
                        )
                    })?;
                if control_changed != 1 {
                    return Ok(false);
                }
                transaction.commit().await.with_context(|| {
                    format!(
                        "Failed to commit interactive Codex child registration for project {project_id}"
                    )
                })?;
                Ok(true)
            })
    }

    pub(crate) fn finish_interactive_guardian_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        guardian_holder: &str,
        disposition: InteractiveGuardianDisposition,
    ) -> Result<bool> {
        self.blocking.block_on(async {
                let mut conn = self.repositories.sessions_runs.connect().await?;
                let transaction = conn.transaction().await.with_context(|| {
                    format!("Failed to finish interactive guardian for project {project_id}")
                })?;
                let changed = match disposition {
                    InteractiveGuardianDisposition::ResumeExec => transaction
                        .execute(
                            "UPDATE session_controls
                                SET state = CASE
                                        WHEN state = 'stop_requested' THEN 'stopped'
                                        ELSE 'resume_requested'
                                    END,
                                    child_pid = NULL,
                                    interactive_holder = NULL,
                                    interactive_launch_token = NULL, updated_at = ?1
                              WHERE project_id = ?2 AND codex_session_id = ?3
                                AND state IN ('interactive', 'stop_requested')
                                AND interactive_holder = ?4",
                            params![
                                agent_timestamp(),
                                project_id,
                                codex_session_id,
                                guardian_holder
                            ],
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "Failed to hand Codex session {codex_session_id} back to exec mode"
                            )
                        })?,
                    InteractiveGuardianDisposition::PreserveIdleSession
                    | InteractiveGuardianDisposition::PreserveSharedSession
                    | InteractiveGuardianDisposition::RestoreStopped
                    | InteractiveGuardianDisposition::RestoreStoppedShared => {
                        transaction.execute(
                            "UPDATE session_controls
                                SET state = 'stopped', child_pid = NULL,
                                    interactive_holder = NULL,
                                    interactive_launch_token = NULL, updated_at = ?1
                              WHERE project_id = ?2 AND codex_session_id = ?3
                                AND state IN ('interactive', 'stop_requested')
                                AND interactive_holder = ?4",
                            params![
                                agent_timestamp(),
                                project_id,
                                codex_session_id,
                                guardian_holder
                            ],
                        )
                        .await.with_context(|| {
                            format!(
                                "Failed to preserve Codex session {codex_session_id} after interactive use"
                            )
                        })?
                    }
                };
                if changed == 0 {
                    return Ok(false);
                }
                if disposition.holds_project_lease() {
                    let _released = transaction
                        .execute(
                            "DELETE FROM leases WHERE project_id = ?1 AND holder = ?2",
                            params![project_id, guardian_holder],
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "Failed to release project {project_id} interactive guardian lease"
                            )
                        })?;
                }
                // The child is already reaped before this transaction begins. A
                // missing exact-holder lease can only mean it expired or was
                // independently cleared; the generation-scoped control CAS above
                // is the authority for both an `i` handback and a completed `c`
                // reservation. Releasing an already-gone lease is complete.
                transaction.commit().await.with_context(|| {
                    format!(
                        "Failed to commit project {project_id} interactive guardian completion"
                    )
                })?;
                Ok(true)
            })
    }

    pub(crate) fn recover_stale_interactive_guardian_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        guardian_holder: &str,
        expected_child_pid: Option<u32>,
        disposition: InteractiveGuardianDisposition,
    ) -> Result<bool> {
        self.blocking.block_on(async {
                let mut conn = self.repositories.sessions_runs.connect().await?;
                let transaction = conn.transaction().await.with_context(|| {
                    format!(
                        "Failed to begin stale interactive guardian recovery for project {project_id}"
                    )
                })?;
                let changed = match disposition {
                    InteractiveGuardianDisposition::ResumeExec => transaction
                        .execute(
                            "UPDATE session_controls
                                SET state = CASE
                                        WHEN state = 'stop_requested' THEN 'stopped'
                                        ELSE 'resume_requested'
                                    END,
                                    child_pid = NULL,
                                    interactive_holder = NULL,
                                    interactive_launch_token = NULL, updated_at = ?1
                              WHERE project_id = ?2 AND codex_session_id = ?3
                                AND state IN ('interactive', 'stop_requested')
                                AND interactive_holder = ?4
                                AND interactive_launch_token = ?4
                                AND (
                                    child_pid = ?5
                                    OR (child_pid IS NULL AND ?5 IS NULL)
                                )",
                            params![
                                agent_timestamp(),
                                project_id,
                                codex_session_id,
                                guardian_holder,
                                expected_child_pid.map(i64::from)
                            ],
                        )
                        .await,
                    InteractiveGuardianDisposition::PreserveIdleSession
                    | InteractiveGuardianDisposition::PreserveSharedSession
                    | InteractiveGuardianDisposition::RestoreStopped
                    | InteractiveGuardianDisposition::RestoreStoppedShared => {
                        transaction.execute(
                            "UPDATE session_controls
                                SET state = 'stopped', child_pid = NULL,
                                    interactive_holder = NULL,
                                    interactive_launch_token = NULL, updated_at = ?1
                              WHERE project_id = ?2 AND codex_session_id = ?3
                                AND state IN ('interactive', 'stop_requested')
                                AND interactive_holder = ?4
                                AND interactive_launch_token = ?4
                                AND (
                                    child_pid = ?5
                                    OR (child_pid IS NULL AND ?5 IS NULL)
                                )",
                            params![
                                agent_timestamp(),
                                project_id,
                                codex_session_id,
                                guardian_holder,
                                expected_child_pid.map(i64::from)
                            ],
                        )
                        .await
                    }
                }
                .with_context(|| {
                    format!(
                        "Failed to recover stale interactive guardian for Codex session {codex_session_id}"
                    )
                })?;
                if changed != 1 {
                    return Ok(false);
                }
                if disposition.holds_project_lease() {
                    let _ = transaction
                        .execute(
                            "DELETE FROM leases WHERE project_id = ?1 AND holder = ?2",
                            params![project_id, guardian_holder],
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "Failed to release stale interactive guardian lease for project {project_id}"
                            )
                        })?;
                }
                transaction.commit().await.with_context(|| {
                    format!(
                        "Failed to commit stale interactive guardian recovery for project {project_id}"
                    )
                })?;
                Ok(true)
            })
    }

    pub(crate) fn complete_session_stop_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        run_token: &str,
    ) -> Result<bool> {
        self.blocking.block_on(async {
            let conn = self.repositories.sessions_runs.connect().await?;
            let changed = conn
                .execute(
                    "UPDATE session_controls
                            SET state = 'stopped', child_pid = NULL,
                                interactive_holder = NULL,
                                interactive_launch_token = NULL, updated_at = ?1
                          WHERE project_id = ?2 AND codex_session_id = ?3
                            AND state = 'stop_requested' AND run_token = ?4
                            AND (
                                NOT EXISTS (
                                    SELECT 1 FROM agent_workers WHERE worker_token = ?4
                                )
                                OR EXISTS (
                                    SELECT 1 FROM agent_workers w
                                    JOIN leases l ON l.project_id = w.project_id
                                                 AND l.holder = w.lease_holder
                                    WHERE w.worker_token = ?4 AND w.project_id = ?2
                                      AND w.state IN ('dispatching', 'running', 'finalizing')
                                )
                            )",
                    params![agent_timestamp(), project_id, codex_session_id, run_token],
                )
                .await
                .with_context(|| {
                    format!("Failed to finish stopping Codex session {codex_session_id}")
                })?;
            Ok(changed > 0)
        })
    }

    pub(crate) fn complete_session_interrupt_handoff_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        run_token: &str,
        from_holder: &str,
        lease_timeout_seconds: u64,
    ) -> Result<Option<String>> {
        self.blocking.block_on(async {
                let mut conn = self.repositories.sessions_runs.connect().await?;
                let transaction = conn.transaction().await.with_context(|| {
                    format!(
                        "Failed to begin interactive handoff for Codex session {codex_session_id}"
                    )
                })?;
                let interactive_holder = {
                    let mut rows = transaction
                        .query(
                            "SELECT interactive_holder
                               FROM session_controls
                              WHERE project_id = ?1 AND codex_session_id = ?2
                                AND state = 'interrupt_requested' AND run_token = ?3",
                            params![project_id, codex_session_id, run_token],
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "Failed to read interactive handoff for Codex session {codex_session_id}"
                            )
                        })?;
                    let Some(row) = rows
                        .next()
                        .await
                        .context("Failed to read interactive handoff row")?
                    else {
                        return Ok(None);
                    };
                    row_optional_text(&row, 0, "interactive_holder")?
                };
                let Some(interactive_holder) = interactive_holder else {
                    return Ok(None);
                };
                let acquired_at = agent_timestamp();
                let expires_at = agent_timestamp_after(lease_timeout_seconds);
                let transferred = transaction
                    .execute(
                        "UPDATE leases
                            SET holder = ?1, acquired_at = ?2, expires_at = ?3
                          WHERE project_id = ?4 AND holder = ?5",
                        params![
                            interactive_holder.as_str(),
                            acquired_at,
                            expires_at,
                            project_id,
                            from_holder
                        ],
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to transfer project {project_id} lease for interactive handoff"
                        )
                    })?;
                if transferred == 0 {
                    return Ok(None);
                }
                let changed = transaction
                    .execute(
                        "UPDATE session_controls
                            SET state = 'ready_interactive', child_pid = NULL,
                                interactive_launch_token = NULL, updated_at = ?1
                          WHERE project_id = ?2 AND codex_session_id = ?3
                            AND state = 'interrupt_requested' AND run_token = ?4",
                        params![agent_timestamp(), project_id, codex_session_id, run_token],
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to mark Codex session {codex_session_id} ready for interactive handoff"
                        )
                    })?;
                if changed == 0 {
                    anyhow::bail!(
                        "Codex session {codex_session_id} changed during interactive handoff"
                    );
                }
                transaction.commit().await.with_context(|| {
                    format!(
                        "Failed to commit interactive handoff for Codex session {codex_session_id}"
                    )
                })?;
                Ok(Some(interactive_holder))
            })
    }

    pub(crate) fn session_control_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
    ) -> Result<Option<AgentSessionControlRecord>> {
        self.blocking
            .block_on(self.session_control(project_id, codex_session_id))
    }

    async fn session_control(
        &self,
        project_id: i64,
        codex_session_id: &str,
    ) -> Result<Option<AgentSessionControlRecord>> {
        let conn = self.repositories.sessions_runs.connect().await?;
        let mut rows = conn
            .query(
                "SELECT project_id, codex_session_id, state, child_pid, run_token,
                        interactive_holder, interactive_launch_token,
                        stdout_path, stderr_path, updated_at
                   FROM session_controls
                  WHERE project_id = ?1 AND codex_session_id = ?2",
                params![project_id, codex_session_id],
            )
            .await
            .with_context(|| {
                format!(
                    "Failed to read Codex session {codex_session_id} control for project {project_id}"
                )
            })?;
        let Some(row) = rows
            .next()
            .await
            .context("Failed to read Codex session control row")?
        else {
            return Ok(None);
        };
        let child_pid = row_optional_integer(&row, 3, "child_pid")?
            .map(u32::try_from)
            .transpose()
            .context("Codex session child PID is outside the supported range")?;
        Ok(Some(AgentSessionControlRecord {
            project_id: row_integer(&row, 0, "project_id")?,
            codex_session_id: row_text(&row, 1, "codex_session_id")?,
            state: AgentSessionControlState::from_database(&row_text(&row, 2, "state")?)?,
            child_pid,
            run_token: row_optional_text(&row, 4, "run_token")?,
            interactive_holder: row_optional_text(&row, 5, "interactive_holder")?,
            interactive_launch_token: row_optional_text(&row, 6, "interactive_launch_token")?,
            stdout_path: row_optional_text(&row, 7, "stdout_path")?,
            stderr_path: row_optional_text(&row, 8, "stderr_path")?,
            updated_at: row_text(&row, 9, "updated_at")?,
        }))
    }

    pub(crate) fn session_controls_for_project_blocking(
        &self,
        project_id: i64,
    ) -> Result<Vec<AgentSessionControlRecord>> {
        self.blocking.block_on(async {
            let conn = self.repositories.sessions_runs.connect().await?;
            let mut rows = conn
                .query(
                    "SELECT project_id, codex_session_id, state, child_pid, run_token,
                                interactive_holder, interactive_launch_token,
                                stdout_path, stderr_path, updated_at
                           FROM session_controls
                          WHERE project_id = ?1
                          ORDER BY updated_at, codex_session_id",
                    [project_id],
                )
                .await
                .with_context(|| {
                    format!("Failed to list Codex session controls for project {project_id}")
                })?;
            let mut controls = Vec::new();
            while let Some(row) = rows
                .next()
                .await
                .context("Failed to read Codex session control row")?
            {
                let child_pid = row_optional_integer(&row, 3, "child_pid")?
                    .map(u32::try_from)
                    .transpose()
                    .context("Codex session child PID is outside the supported range")?;
                controls.push(AgentSessionControlRecord {
                    project_id: row_integer(&row, 0, "project_id")?,
                    codex_session_id: row_text(&row, 1, "codex_session_id")?,
                    state: AgentSessionControlState::from_database(&row_text(&row, 2, "state")?)?,
                    child_pid,
                    run_token: row_optional_text(&row, 4, "run_token")?,
                    interactive_holder: row_optional_text(&row, 5, "interactive_holder")?,
                    interactive_launch_token: row_optional_text(
                        &row,
                        6,
                        "interactive_launch_token",
                    )?,
                    stdout_path: row_optional_text(&row, 7, "stdout_path")?,
                    stderr_path: row_optional_text(&row, 8, "stderr_path")?,
                    updated_at: row_text(&row, 9, "updated_at")?,
                });
            }
            Ok(controls)
        })
    }

    pub(crate) fn suspending_session_project_ids_blocking(&self) -> Result<HashSet<i64>> {
        self.blocking.block_on(async {
            let conn = self.repositories.sessions_runs.connect().await?;
            let mut rows = conn
                .query(
                    "SELECT DISTINCT project_id
                           FROM session_controls
                          WHERE state != 'stopped'",
                    (),
                )
                .await
                .context("Failed to list projects suspended by Codex session controls")?;
            let mut project_ids = HashSet::new();
            while let Some(row) = rows
                .next()
                .await
                .context("Failed to read suspended Codex session project")?
            {
                project_ids.insert(row_integer(&row, 0, "project_id")?);
            }
            Ok(project_ids)
        })
    }

    pub(crate) fn resume_requested_session_blocking(
        &self,
        project_id: i64,
    ) -> Result<Option<String>> {
        self.blocking.block_on(async {
            let conn = self.repositories.sessions_runs.connect().await?;
            let mut rows = conn
                .query(
                    "SELECT codex_session_id
                           FROM session_controls
                          WHERE project_id = ?1 AND state = 'resume_requested'
                          ORDER BY updated_at, codex_session_id
                          LIMIT 1",
                    [project_id],
                )
                .await
                .with_context(|| {
                    format!(
                        "Failed to find a requested Codex session resume for project {project_id}"
                    )
                })?;
            let Some(row) = rows
                .next()
                .await
                .context("Failed to read requested Codex session resume")?
            else {
                return Ok(None);
            };
            Ok(Some(row_text(&row, 0, "codex_session_id")?))
        })
    }

    pub(crate) fn ensure_pending_git_finalization_resume_requested_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
    ) -> Result<bool> {
        self.blocking.block_on(async {
                let mut conn = self.repositories.sessions_runs.connect().await?;
                let transaction = conn
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to begin restoring the exact Git finalization session {codex_session_id}"
                        )
                    })?;
                let generation = {
                    let mut rows = transaction
                        .query(
                            "SELECT generation FROM git_finalizations
                              WHERE project_id = ?1 AND codex_session_id = ?2
                                AND state IN ('working', 'tracking', 'commit_pending', 'push_pending')",
                            params![project_id, codex_session_id],
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "Failed to inspect pending Git finalization {codex_session_id}"
                            )
                        })?;
                    rows.next()
                        .await
                        .context("Failed to read pending Git finalization generation")?
                        .map(|row| row_integer(&row, 0, "generation"))
                        .transpose()?
                };
                let Some(generation) = generation else {
                    transaction.commit().await.with_context(|| {
                        format!(
                            "Failed to finish restoring absent Git finalization session {codex_session_id}"
                        )
                    })?;
                    return Ok(false);
                };
                let recovery_token = format!(
                    "{AGENT_GIT_FINALIZATION_RESUME_TOKEN_PREFIX}{generation}"
                );
                transaction
                    .execute(
                        "UPDATE session_controls
                            SET run_token = ?1, updated_at = ?2
                          WHERE project_id = ?3 AND codex_session_id = ?4
                            AND state = 'resume_requested' AND child_pid IS NULL
                            AND interactive_holder IS NULL",
                        params![
                            recovery_token.as_str(),
                            agent_timestamp(),
                            project_id,
                            codex_session_id,
                        ],
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to tag Git finalization recovery session {codex_session_id}"
                        )
                    })?;
                transaction
                    .execute(
                        "INSERT OR IGNORE INTO session_controls (
                            project_id, codex_session_id, state, child_pid,
                            run_token, interactive_holder, interactive_launch_token, updated_at
                         ) VALUES (?1, ?2, 'resume_requested', NULL, ?3, NULL, NULL, ?4)",
                        params![
                            project_id,
                            codex_session_id,
                            recovery_token.as_str(),
                            agent_timestamp()
                        ],
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to restore Git finalization session {codex_session_id}"
                        )
                    })?;
                let ready = query_count(
                    &transaction,
                    "SELECT COUNT(*) FROM session_controls
                      WHERE project_id = ?1 AND codex_session_id = ?2
                        AND state = 'resume_requested' AND child_pid IS NULL
                        AND interactive_holder IS NULL",
                    params![project_id, codex_session_id],
                )
                .await?
                    == 1;
                transaction.commit().await.with_context(|| {
                    format!(
                        "Failed to commit restored Git finalization session {codex_session_id}"
                    )
                })?;
                Ok(ready)
            })
    }

    pub(crate) fn clear_orphaned_resume_requested_session_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        now: u64,
    ) -> Result<bool> {
        self.blocking.block_on(async {
                let mut conn = self.repositories.sessions_runs.connect().await?;
                let transaction = conn
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to begin clearing orphaned Codex session {codex_session_id} for project {project_id}"
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
                    || query_count(
                        &transaction,
                        "SELECT COUNT(*) FROM leases
                          WHERE project_id = ?1
                            AND CAST(expires_at AS INTEGER) > CAST(?2 AS INTEGER)",
                        params![project_id, now.to_string()],
                    )
                    .await?
                        > 0
                {
                    return Ok(false);
                }
                let removed = transaction
                    .execute(
                        "DELETE FROM session_controls
                          WHERE project_id = ?1 AND codex_session_id = ?2
                            AND state = 'resume_requested' AND child_pid IS NULL
                            AND interactive_holder IS NULL",
                        params![project_id, codex_session_id],
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to clear orphaned Codex session {codex_session_id} for project {project_id}"
                        )
                    })?;
                transaction.commit().await.with_context(|| {
                    format!(
                        "Failed to commit clearing orphaned Codex session {codex_session_id} for project {project_id}"
                    )
                })?;
                Ok(removed > 0)
            })
    }

    pub(crate) fn register_known_session_with_child_blocking(
        &self,
        registration: AgentKnownSessionRegistration<'_>,
    ) -> Result<bool> {
        let AgentKnownSessionRegistration {
            project_id,
            codex_session_id,
            child_pid,
            run_token,
            stdout_path,
            stderr_path,
            lease_holder,
            lease_timeout_seconds,
            claim_requested_resume,
        } = registration;
        self.blocking.block_on(async {
            let mut conn = self.repositories.sessions_runs.connect().await?;
            let now = agent_timestamp();
            let fresh_expiry = agent_timestamp_after(lease_timeout_seconds);
            let transaction = conn.transaction().await.with_context(|| {
                format!("Failed to begin known-session registration for project {project_id}")
            })?;
            let lease_changed = transaction
                .execute(
                    "UPDATE leases SET expires_at = ?1
                          WHERE project_id = ?2 AND holder = ?3
                            AND expires_at > ?4",
                    params![
                        fresh_expiry.as_str(),
                        project_id,
                        lease_holder,
                        now.as_str()
                    ],
                )
                .await
                .with_context(|| {
                    format!("Failed to renew known-session lease for project {project_id}")
                })?;
            if lease_changed != 1 {
                return Ok(false);
            }
            let control_changed = if claim_requested_resume {
                transaction
                    .execute(
                        "UPDATE session_controls
                                SET state = 'running', child_pid = ?1, run_token = ?2,
                                    interactive_holder = NULL,
                                    interactive_launch_token = NULL, stdout_path = ?3,
                                    stderr_path = ?4, updated_at = ?5
                              WHERE project_id = ?6 AND codex_session_id = ?7
                                AND state = 'resume_requested'",
                        params![
                            i64::from(child_pid),
                            run_token,
                            stdout_path.to_string_lossy().as_ref(),
                            stderr_path.to_string_lossy().as_ref(),
                            now.as_str(),
                            project_id,
                            codex_session_id,
                        ],
                    )
                    .await
            } else {
                transaction
                    .execute(
                        "INSERT OR IGNORE INTO session_controls (
                                project_id, codex_session_id, state, child_pid, run_token,
                                interactive_holder, stdout_path, stderr_path, updated_at
                             ) VALUES (?1, ?2, 'running', ?3, ?4, NULL, ?5, ?6, ?7)",
                        params![
                            project_id,
                            codex_session_id,
                            i64::from(child_pid),
                            run_token,
                            stdout_path.to_string_lossy().as_ref(),
                            stderr_path.to_string_lossy().as_ref(),
                            now.as_str(),
                        ],
                    )
                    .await
            }
            .with_context(|| {
                format!("Failed to register known Codex session {codex_session_id} before launch")
            })?;
            if control_changed != 1 {
                return Ok(false);
            }
            transaction.commit().await.with_context(|| {
                format!("Failed to commit known-session registration for project {project_id}")
            })?;
            Ok(true)
        })
    }

    pub(crate) fn clear_running_session_control_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        run_token: Option<&str>,
    ) -> Result<bool> {
        self.blocking.block_on(async {
                let mut conn = self.repositories.sessions_runs.connect().await?;
                let transaction = conn
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to begin clearing Codex session {codex_session_id} for project {project_id}"
                        )
                    })?;
                if run_token.is_none()
                    && query_count(
                        &transaction,
                        "SELECT COUNT(*) FROM agent_workers
                          WHERE project_id = ?1
                            AND state IN ('dispatching', 'running', 'finalizing')",
                        [project_id],
                    )
                    .await?
                        > 0
                {
                    return Ok(false);
                }
                if let Some(run_token) = run_token {
                    let known_worker = query_count(
                        &transaction,
                        "SELECT COUNT(*) FROM agent_workers WHERE worker_token = ?1",
                        [run_token],
                    )
                    .await?
                        == 1;
                    let fenced_worker = query_count(
                        &transaction,
                        "SELECT COUNT(*) FROM agent_workers w
                          JOIN leases l ON l.project_id = w.project_id
                                       AND l.holder = w.lease_holder
                         WHERE w.worker_token = ?1 AND w.project_id = ?2
                           AND w.state IN ('dispatching', 'running', 'finalizing')",
                        params![run_token, project_id],
                    )
                    .await?
                        == 1;
                    if known_worker && !fenced_worker {
                        return Ok(false);
                    }
                }
                let removed = transaction
                    .execute(
                        "DELETE FROM session_controls
                          WHERE project_id = ?1 AND codex_session_id = ?2 AND state = 'running'
                            AND (?3 IS NULL OR run_token = ?3)",
                        params![project_id, codex_session_id, run_token],
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to clear running Codex session {codex_session_id} for project {project_id}"
                        )
                    })?;
                transaction.commit().await.with_context(|| {
                    format!(
                        "Failed to commit clearing Codex session {codex_session_id} for project {project_id}"
                    )
                })?;
                Ok(removed > 0)
            })
    }

    pub(crate) fn clear_autonomous_push_resume_request_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
    ) -> Result<bool> {
        self.blocking.block_on(async {
                let conn = self.repositories.sessions_runs.connect().await?;
                let removed = conn
                    .execute(
                        "DELETE FROM session_controls
                          WHERE project_id = ?1 AND codex_session_id = ?2
                            AND state = 'resume_requested' AND child_pid IS NULL
                            AND EXISTS (
                                SELECT 1 FROM git_finalizations
                                 WHERE project_id = ?1 AND codex_session_id = ?2
                                   AND state = 'push_pending'
                            )
                            AND NOT EXISTS (
                                SELECT 1 FROM agent_workers
                                 WHERE project_id = ?1
                                   AND state IN ('dispatching', 'running', 'finalizing')
                            )",
                        params![project_id, codex_session_id],
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to clear autonomous PushPending resume request for Codex session {codex_session_id}"
                        )
                    })?;
                Ok(removed == 1)
            })
    }
}
