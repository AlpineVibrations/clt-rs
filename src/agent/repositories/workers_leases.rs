use std::{ffi::OsString, path::PathBuf};

use anyhow::{Context, Result};
use turso::{Connection, Database, params, transaction::TransactionBehavior};

use super::RepositoryDatabase;
use crate::agent::{
    AGENT_GIT_FINALIZATION_RESUME_TOKEN_PREFIX, AgentLeaseRecord, AgentRunOutcome,
    AgentWorkerAbandonment, AgentWorkerFinalization, AgentWorkerRecord, AgentWorkerReservation,
    TursoAgentStore, query_count, row_integer, row_optional_integer, row_optional_text, row_text,
    update_project_after_run, worker_lease_holder,
};

/// Persistence for independent workers and project leases.
pub(in crate::agent) struct WorkersLeasesRepository(RepositoryDatabase);

impl WorkersLeasesRepository {
    pub(in crate::agent) fn new(db: &Database) -> Self {
        Self(RepositoryDatabase::new(db))
    }

    pub(in crate::agent) async fn connect(&self) -> Result<Connection> {
        self.0.connect().await
    }
}

impl TursoAgentStore {
    pub(crate) fn lease_for_project_blocking(
        &self,
        project_id: i64,
    ) -> Result<Option<AgentLeaseRecord>> {
        self.blocking.block_on(self.lease_for_project(project_id))
    }

    async fn lease_for_project(&self, project_id: i64) -> Result<Option<AgentLeaseRecord>> {
        let conn = self.repositories.workers_leases.connect().await?;
        let mut rows = conn
            .query(
                "SELECT l.project_id, p.name, p.path, l.holder, l.acquired_at, l.expires_at
                 FROM leases l
                 JOIN projects p ON p.id = l.project_id
                 WHERE l.project_id = ?1",
                [project_id],
            )
            .await
            .with_context(|| format!("Failed to read lease for project {project_id}"))?;

        let Some(row) = rows
            .next()
            .await
            .context("Failed to read agent lease row")?
        else {
            return Ok(None);
        };

        Ok(Some(AgentLeaseRecord {
            project_id: row_integer(&row, 0, "project_id")?,
            project_name: row_text(&row, 1, "name")?,
            project_path: PathBuf::from(row_text(&row, 2, "path")?),
            holder: row_text(&row, 3, "holder")?,
            acquired_at: row_text(&row, 4, "acquired_at")?,
            expires_at: row_text(&row, 5, "expires_at")?,
        }))
    }

    pub(crate) fn list_active_leases_blocking(&self, now: &str) -> Result<Vec<AgentLeaseRecord>> {
        self.blocking.block_on(self.list_active_leases(now))
    }

    async fn list_active_leases(&self, now: &str) -> Result<Vec<AgentLeaseRecord>> {
        let conn = self.repositories.workers_leases.connect().await?;
        let mut rows = conn
            .query(
                "SELECT l.project_id, p.name, p.path, l.holder, l.acquired_at, l.expires_at
                 FROM leases l
                 JOIN projects p ON p.id = l.project_id
                 WHERE CAST(l.expires_at AS INTEGER) > CAST(?1 AS INTEGER)
                 ORDER BY CAST(l.expires_at AS INTEGER), p.name COLLATE NOCASE",
                [now],
            )
            .await
            .context("Failed to list active agent leases")?;
        let mut leases = Vec::new();

        while let Some(row) = rows.next().await.context("Failed to read lease row")? {
            leases.push(AgentLeaseRecord {
                project_id: row_integer(&row, 0, "project_id")?,
                project_name: row_text(&row, 1, "name")?,
                project_path: PathBuf::from(row_text(&row, 2, "path")?),
                holder: row_text(&row, 3, "holder")?,
                acquired_at: row_text(&row, 4, "acquired_at")?,
                expires_at: row_text(&row, 5, "expires_at")?,
            });
        }

        Ok(leases)
    }
}

impl TursoAgentStore {
    pub(crate) fn try_acquire_lease_blocking(
        &self,
        project_id: i64,
        holder: &str,
        acquired_at: &str,
        expires_at: &str,
    ) -> Result<bool> {
        self.blocking
            .block_on(self.try_acquire_lease(project_id, holder, acquired_at, expires_at))
    }

    async fn try_acquire_lease(
        &self,
        project_id: i64,
        holder: &str,
        acquired_at: &str,
        expires_at: &str,
    ) -> Result<bool> {
        let conn = self.repositories.workers_leases.connect().await?;

        conn.execute(
            "DELETE FROM leases
              WHERE project_id = ?1 AND expires_at <= ?2
                AND NOT EXISTS (
                    SELECT 1 FROM agent_workers w
                     WHERE w.project_id = leases.project_id
                       AND w.lease_holder = leases.holder
                       AND w.state IN ('dispatching', 'running', 'finalizing')
                )",
            params![project_id, acquired_at],
        )
        .await
        .with_context(|| format!("Failed to clear expired lease for project {}", project_id))?;

        let inserted = conn
            .execute(
                "INSERT OR IGNORE INTO leases (project_id, holder, acquired_at, expires_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![project_id, holder, acquired_at, expires_at],
            )
            .await
            .with_context(|| format!("Failed to acquire lease for project {}", project_id))?;

        Ok(inserted > 0)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn try_acquire_git_finalization_lease_blocking(
        &self,
        project_id: i64,
        holder: &str,
        acquired_at: &str,
        expires_at: &str,
        reclaim_holder: Option<&str>,
    ) -> Result<bool> {
        self.blocking
            .block_on(self.try_acquire_git_finalization_lease(
                project_id,
                holder,
                acquired_at,
                expires_at,
                reclaim_holder,
            ))
    }

    #[allow(clippy::too_many_arguments)]
    async fn try_acquire_git_finalization_lease(
        &self,
        project_id: i64,
        holder: &str,
        acquired_at: &str,
        expires_at: &str,
        reclaim_holder: Option<&str>,
    ) -> Result<bool> {
        let mut conn = self.repositories.workers_leases.connect().await?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .with_context(|| {
                format!(
                    "Failed to begin acquiring the Git finalization lease for project {project_id}"
                )
            })?;
        transaction
            .execute(
                "UPDATE session_controls
                    SET run_token = ?1 || (
                            SELECT CAST(g.generation AS TEXT)
                              FROM git_finalizations g
                             WHERE g.project_id = session_controls.project_id
                               AND g.codex_session_id = session_controls.codex_session_id
                               AND g.state IN ('working', 'tracking', 'commit_pending', 'push_pending')
                        ),
                        updated_at = ?2
                  WHERE project_id = ?3 AND state = 'resume_requested'
                    AND child_pid IS NULL AND interactive_holder IS NULL
                    AND interactive_launch_token IS NULL
                    AND run_token LIKE ?1 || '%'
                    AND EXISTS (
                        SELECT 1 FROM git_finalizations g
                         WHERE g.project_id = session_controls.project_id
                           AND g.codex_session_id = session_controls.codex_session_id
                           AND g.state IN ('working', 'tracking', 'commit_pending', 'push_pending')
                    )",
                params![
                    AGENT_GIT_FINALIZATION_RESUME_TOKEN_PREFIX,
                    acquired_at,
                    project_id
                ],
            )
            .await
            .with_context(|| {
                format!(
                    "Failed to repair stale Git finalization recovery fences for project {project_id}"
                )
            })?;
        let controls_allow_finalization = "NOT EXISTS (
            SELECT 1 FROM session_controls sc
             WHERE sc.project_id = ?1
               AND NOT (
                   sc.state = 'stopped'
                   OR (sc.state = 'resume_requested'
                       AND sc.child_pid IS NULL
                       AND sc.interactive_holder IS NULL
                       AND sc.interactive_launch_token IS NULL
                       AND EXISTS (
                           SELECT 1 FROM git_finalizations g
                            WHERE g.project_id = sc.project_id
                              AND g.codex_session_id = sc.codex_session_id
                              AND g.state IN ('working', 'tracking', 'commit_pending', 'push_pending')
                              AND sc.run_token = ?4 || CAST(g.generation AS TEXT)
                       ))
               )
        )";
        transaction
            .execute(
                &format!(
                    "DELETE FROM leases
                      WHERE project_id = ?1
                        AND (CAST(expires_at AS INTEGER) <= CAST(?2 AS INTEGER)
                             OR (?3 IS NOT NULL AND holder = ?3))
                        AND NOT EXISTS (
                            SELECT 1 FROM agent_workers w
                             WHERE w.project_id = leases.project_id
                               AND w.state IN ('dispatching', 'running', 'finalizing')
                        )
                        AND {controls_allow_finalization}"
                ),
                params![
                    project_id,
                    acquired_at,
                    reclaim_holder,
                    AGENT_GIT_FINALIZATION_RESUME_TOKEN_PREFIX
                ],
            )
            .await
            .with_context(|| {
                format!(
                    "Failed to clear a reclaimable lease before Git finalization for project {project_id}"
                )
            })?;
        let inserted = transaction
            .execute(
                &format!(
                    "INSERT OR IGNORE INTO leases (project_id, holder, acquired_at, expires_at)
                     SELECT ?1, ?2, ?3, ?5
                      WHERE EXISTS (
                          SELECT 1 FROM git_finalizations g
                           WHERE g.project_id = ?1
                             AND g.state IN ('working', 'tracking', 'commit_pending', 'push_pending')
                      )
                        AND NOT EXISTS (
                            SELECT 1 FROM agent_workers w
                             WHERE w.project_id = ?1
                               AND w.state IN ('dispatching', 'running', 'finalizing')
                        )
                        AND {controls_allow_finalization}"
                ),
                params![
                    project_id,
                    holder,
                    acquired_at,
                    AGENT_GIT_FINALIZATION_RESUME_TOKEN_PREFIX,
                    expires_at
                ],
            )
            .await
            .with_context(|| {
                format!(
                    "Failed to acquire the guarded Git finalization lease for project {project_id}"
                )
            })?;
        transaction.commit().await.with_context(|| {
            format!("Failed to commit Git finalization lease acquisition for project {project_id}")
        })?;
        Ok(inserted == 1)
    }

    pub(crate) fn renew_git_finalization_lease_blocking(
        &self,
        project_id: i64,
        holder: &str,
        expires_at: &str,
    ) -> Result<bool> {
        self.blocking.block_on(async {
                let conn = self.repositories.workers_leases.connect().await?;
                let changed = conn
                    .execute(
                        "UPDATE leases
                            SET expires_at = ?1
                          WHERE project_id = ?2 AND holder = ?3
                            AND NOT EXISTS (
                                SELECT 1 FROM agent_workers w
                                 WHERE w.project_id = ?2
                                   AND w.state IN ('dispatching', 'running', 'finalizing')
                            )
                            AND NOT EXISTS (
                                SELECT 1 FROM session_controls sc
                                 WHERE sc.project_id = ?2
                                   AND NOT (
                                       sc.state = 'stopped'
                                       OR (sc.state = 'resume_requested'
                                           AND sc.child_pid IS NULL
                                           AND sc.interactive_holder IS NULL
                                           AND sc.interactive_launch_token IS NULL
                                           AND EXISTS (
                                               SELECT 1 FROM git_finalizations g
                                                WHERE g.project_id = sc.project_id
                                                  AND g.codex_session_id = sc.codex_session_id
                                                  AND g.state IN ('working', 'tracking', 'commit_pending', 'push_pending')
                                                  AND sc.run_token = ?4 || CAST(g.generation AS TEXT)
                                           ))
                                   )
                            )",
                        params![
                            expires_at,
                            project_id,
                            holder,
                            AGENT_GIT_FINALIZATION_RESUME_TOKEN_PREFIX
                        ],
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to renew the guarded Git finalization lease for project {project_id}"
                        )
                    })?;
                Ok(changed == 1)
            })
    }

    pub(crate) fn git_finalization_lease_is_owned_blocking(
        &self,
        project_id: i64,
        holder: &str,
        now: &str,
    ) -> Result<bool> {
        self.blocking.block_on(async {
                let conn = self.repositories.workers_leases.connect().await?;
                Ok(query_count(
                    &conn,
                    "SELECT COUNT(*) FROM leases l
                      WHERE l.project_id = ?1 AND l.holder = ?2
                        AND CAST(l.expires_at AS INTEGER) > CAST(?3 AS INTEGER)
                        AND NOT EXISTS (
                            SELECT 1 FROM agent_workers w
                             WHERE w.project_id = ?1
                               AND w.state IN ('dispatching', 'running', 'finalizing')
                        )
                        AND NOT EXISTS (
                            SELECT 1 FROM session_controls sc
                             WHERE sc.project_id = ?1
                               AND NOT (
                                   sc.state = 'stopped'
                                   OR (sc.state = 'resume_requested'
                                       AND sc.child_pid IS NULL
                                       AND sc.interactive_holder IS NULL
                                       AND sc.interactive_launch_token IS NULL
                                       AND EXISTS (
                                           SELECT 1 FROM git_finalizations g
                                            WHERE g.project_id = sc.project_id
                                              AND g.codex_session_id = sc.codex_session_id
                                              AND g.state IN ('working', 'tracking', 'commit_pending', 'push_pending')
                                              AND sc.run_token = ?4 || CAST(g.generation AS TEXT)
                                       ))
                               )
                        )",
                    params![
                        project_id,
                        holder,
                        now,
                        AGENT_GIT_FINALIZATION_RESUME_TOKEN_PREFIX
                    ],
                )
                .await? == 1)
            })
    }

    pub(crate) fn renew_lease_blocking(
        &self,
        project_id: i64,
        holder: &str,
        expires_at: &str,
    ) -> Result<bool> {
        self.blocking
            .block_on(self.renew_lease(project_id, holder, expires_at))
    }

    async fn renew_lease(&self, project_id: i64, holder: &str, expires_at: &str) -> Result<bool> {
        let conn = self.repositories.workers_leases.connect().await?;
        let changed = conn
            .execute(
                "UPDATE leases SET expires_at = ?1 WHERE project_id = ?2 AND holder = ?3",
                params![expires_at, project_id, holder],
            )
            .await
            .with_context(|| format!("Failed to renew lease for project {project_id}"))?;

        if changed > 0 {
            return Ok(true);
        }

        Ok(query_count(
            &conn,
            "SELECT COUNT(*) FROM leases WHERE project_id = ?1 AND holder = ?2",
            params![project_id, holder],
        )
        .await?
            > 0)
    }

    pub(crate) fn release_lease_blocking(&self, project_id: i64, holder: &str) -> Result<bool> {
        self.blocking
            .block_on(self.release_lease(project_id, holder))
    }

    async fn release_lease(&self, project_id: i64, holder: &str) -> Result<bool> {
        let conn = self.repositories.workers_leases.connect().await?;
        let removed = conn
            .execute(
                "DELETE FROM leases WHERE project_id = ?1 AND holder = ?2",
                params![project_id, holder],
            )
            .await
            .with_context(|| format!("Failed to release lease for project {}", project_id))?;

        Ok(removed > 0)
    }

    pub(crate) fn reserve_worker_blocking(
        &self,
        reservation: AgentWorkerReservation<'_>,
    ) -> Result<bool> {
        self.blocking
            .block_on(self.reserve_worker(reservation, None))
    }

    pub(crate) fn reserve_and_claim_worker_blocking(
        &self,
        reservation: AgentWorkerReservation<'_>,
        worker_pid: u32,
        started_at: &str,
    ) -> Result<bool> {
        self.blocking
            .block_on(self.reserve_worker(reservation, Some((worker_pid, started_at))))
    }

    async fn reserve_worker(
        &self,
        reservation: AgentWorkerReservation<'_>,
        initial_claim: Option<(u32, &str)>,
    ) -> Result<bool> {
        let AgentWorkerReservation {
            project_id,
            worker_token,
            expected_lease_holder,
            max_active_workers,
            protocol_version,
            service_label,
            binary_path,
            command_arguments,
            path_env,
            codex_path,
            task_selection,
            resume_session_id,
            created_at,
        } = reservation;
        let lease_holder = worker_lease_holder(worker_token);
        let codex_path = codex_path.map(|path| path.to_string_lossy().into_owned());
        let mut conn = self.repositories.workers_leases.connect().await?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .with_context(|| {
                format!("Failed to begin worker reservation for project {project_id}")
            })?;

        let max_active_workers = i64::try_from(max_active_workers)
            .context("Maximum active worker count is outside the supported range")?;
        if max_active_workers <= 0
            || query_count(
                &transaction,
                "SELECT COUNT(*) FROM agent_workers
                  WHERE state IN ('dispatching', 'running', 'finalizing')",
                (),
            )
            .await?
                >= max_active_workers
        {
            return Ok(false);
        }

        let inserted = transaction
            .execute(
                "INSERT OR IGNORE INTO agent_workers (
                    worker_token, project_id, state, protocol_version, lease_holder, service_label,
                    binary_path, command_arguments, path_env, codex_path,
                    task_selection, resume_session_id, worker_pid,
                    created_at, started_at, heartbeat_at, finished_at, run_id, error,
                    service_cleaned_at
                 ) VALUES (?1, ?2, 'dispatching', ?3, ?4, ?5, ?6, ?7, ?8, ?9,
                           ?10, ?11, NULL, ?12, NULL, ?12, NULL, NULL, NULL, NULL)",
                params![
                    worker_token,
                    project_id,
                    protocol_version,
                    lease_holder.as_str(),
                    service_label,
                    binary_path.to_string_lossy().as_ref(),
                    command_arguments,
                    path_env.to_string_lossy().as_ref(),
                    codex_path.as_deref(),
                    task_selection,
                    resume_session_id,
                    created_at,
                ],
            )
            .await
            .with_context(|| {
                format!("Failed to reserve worker {worker_token} for project {project_id}")
            })?;
        if inserted != 1 {
            return Ok(false);
        }

        let transferred = transaction
            .execute(
                "UPDATE leases
                    SET holder = ?1
                  WHERE project_id = ?2 AND holder = ?3",
                params![lease_holder.as_str(), project_id, expected_lease_holder],
            )
            .await
            .with_context(|| {
                format!("Failed to transfer project {project_id} lease to worker {worker_token}")
            })?;
        if transferred != 1 {
            return Ok(false);
        }

        transaction
            .execute(
                "UPDATE agent_workers
                    SET state = 'superseded'
                  WHERE project_id = ?1 AND state = 'abandoned'
                    AND worker_token <> ?2",
                params![project_id, worker_token],
            )
            .await
            .with_context(|| {
                format!("Failed to supersede earlier abandoned workers for project {project_id}")
            })?;

        if let Some((worker_pid, started_at)) = initial_claim {
            let claimed = transaction
                .execute(
                    "UPDATE agent_workers
                        SET state = 'running', worker_pid = ?1, started_at = ?2,
                            heartbeat_at = ?2, error = NULL
                      WHERE worker_token = ?3 AND state = 'dispatching'
                        AND EXISTS (
                            SELECT 1 FROM leases
                             WHERE leases.project_id = agent_workers.project_id
                               AND leases.holder = agent_workers.lease_holder
                        )",
                    params![i64::from(worker_pid), started_at, worker_token],
                )
                .await
                .with_context(|| {
                    format!("Failed to atomically claim inline worker {worker_token}")
                })?;
            if claimed != 1 {
                return Ok(false);
            }
        }

        transaction
            .commit()
            .await
            .with_context(|| format!("Failed to commit worker {worker_token} reservation"))?;
        Ok(true)
    }

    pub(crate) fn claim_worker_blocking(
        &self,
        worker_token: &str,
        worker_pid: u32,
        started_at: &str,
    ) -> Result<bool> {
        self.blocking
            .block_on(self.claim_worker(worker_token, worker_pid, started_at))
    }

    async fn claim_worker(
        &self,
        worker_token: &str,
        worker_pid: u32,
        started_at: &str,
    ) -> Result<bool> {
        let mut conn = self.repositories.workers_leases.connect().await?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .with_context(|| format!("Failed to begin worker {worker_token} claim"))?;
        let changed = transaction
            .execute(
                "UPDATE agent_workers
                    SET state = 'running', worker_pid = ?1, started_at = ?2,
                        heartbeat_at = ?2, error = NULL
                  WHERE worker_token = ?3 AND state = 'dispatching'
                    AND EXISTS (
                        SELECT 1 FROM leases
                         WHERE leases.project_id = agent_workers.project_id
                           AND leases.holder = agent_workers.lease_holder
                    )",
                params![i64::from(worker_pid), started_at, worker_token],
            )
            .await
            .with_context(|| format!("Failed to claim worker {worker_token}"))?;
        if changed == 1 {
            transaction
                .commit()
                .await
                .with_context(|| format!("Failed to commit worker {worker_token} claim"))?;
            return Ok(true);
        }

        let already_claimed = query_count(
            &transaction,
            "SELECT COUNT(*)
               FROM agent_workers w
               JOIN leases l
                 ON l.project_id = w.project_id AND l.holder = w.lease_holder
              WHERE w.worker_token = ?1 AND w.state = 'running' AND w.worker_pid = ?2",
            params![worker_token, i64::from(worker_pid)],
        )
        .await?
            == 1;
        transaction
            .commit()
            .await
            .with_context(|| format!("Failed to finish idempotent worker {worker_token} claim"))?;
        Ok(already_claimed)
    }

    pub(crate) fn renew_worker_blocking(
        &self,
        worker_token: &str,
        worker_pid: u32,
        heartbeat_at: &str,
        lease_expires_at: &str,
    ) -> Result<bool> {
        self.blocking.block_on(self.renew_worker(
            worker_token,
            worker_pid,
            heartbeat_at,
            lease_expires_at,
        ))
    }

    pub(crate) fn worker_fence_snapshot_blocking(
        &self,
        worker_token: &str,
        expected_worker_pid: u32,
    ) -> Result<String> {
        self.blocking.block_on(async {
                let conn = self.repositories.workers_leases.connect().await?;
                let mut rows = conn
                    .query(
                        "SELECT project_id, state, worker_pid, lease_holder, heartbeat_at
                           FROM agent_workers WHERE worker_token = ?1",
                        [worker_token],
                    )
                    .await
                    .with_context(|| {
                        format!("Failed to inspect worker {worker_token} ownership fence")
                    })?;
                let Some(row) = rows
                    .next()
                    .await
                    .context("Failed to read worker ownership fence row")?
                else {
                    return Ok(format!(
                        "worker=missing expected_pid={expected_worker_pid}"
                    ));
                };
                let project_id = row_integer(&row, 0, "project_id")?;
                let state = row_text(&row, 1, "state")?;
                let worker_pid = row_optional_integer(&row, 2, "worker_pid")?;
                let worker_lease_holder = row_text(&row, 3, "lease_holder")?;
                let heartbeat_at = row_optional_text(&row, 4, "heartbeat_at")?;
                drop(rows);
                let mut lease_rows = conn
                    .query(
                        "SELECT holder, expires_at FROM leases WHERE project_id = ?1",
                        [project_id],
                    )
                    .await
                    .with_context(|| {
                        format!("Failed to inspect worker {worker_token} project lease")
                    })?;
                let lease = lease_rows
                    .next()
                    .await
                    .context("Failed to read worker project lease row")?
                    .map(|row| {
                        Ok::<String, anyhow::Error>(format!(
                            "{}@{}",
                            row_text(&row, 0, "holder")?,
                            row_text(&row, 1, "expires_at")?
                        ))
                    })
                    .transpose()?
                    .unwrap_or_else(|| "missing".to_string());
                Ok(format!(
                    "state={state} worker_pid={} expected_pid={expected_worker_pid} worker_lease_holder={worker_lease_holder} project_lease={lease} heartbeat_at={}",
                    worker_pid
                        .map(|pid| pid.to_string())
                        .unwrap_or_else(|| "missing".to_string()),
                    heartbeat_at.as_deref().unwrap_or("missing")
                ))
            })
    }

    async fn renew_worker(
        &self,
        worker_token: &str,
        worker_pid: u32,
        heartbeat_at: &str,
        lease_expires_at: &str,
    ) -> Result<bool> {
        let mut conn = self.repositories.workers_leases.connect().await?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .with_context(|| format!("Failed to begin worker {worker_token} heartbeat"))?;
        let (project_id, lease_holder) = {
            let mut rows = transaction
                .query(
                    "SELECT w.project_id, w.lease_holder
                       FROM agent_workers w
                       JOIN leases l
                         ON l.project_id = w.project_id AND l.holder = w.lease_holder
                      WHERE w.worker_token = ?1 AND w.state = 'running'
                        AND w.worker_pid = ?2",
                    params![worker_token, i64::from(worker_pid)],
                )
                .await
                .with_context(|| {
                    format!("Failed to verify worker {worker_token} heartbeat ownership")
                })?;
            let Some(row) = rows
                .next()
                .await
                .context("Failed to read worker heartbeat ownership row")?
            else {
                return Ok(false);
            };
            (
                row_integer(&row, 0, "project_id")?,
                row_text(&row, 1, "lease_holder")?,
            )
        };
        let worker_changed = transaction
            .execute(
                "UPDATE agent_workers
                    SET heartbeat_at = ?1
                  WHERE worker_token = ?2 AND state = 'running' AND worker_pid = ?3",
                params![heartbeat_at, worker_token, i64::from(worker_pid)],
            )
            .await
            .with_context(|| format!("Failed to update worker {worker_token} heartbeat"))?;
        if worker_changed != 1
            && query_count(
                &transaction,
                "SELECT COUNT(*) FROM agent_workers
                  WHERE worker_token = ?1 AND state = 'running' AND worker_pid = ?2
                    AND heartbeat_at = ?3",
                params![worker_token, i64::from(worker_pid), heartbeat_at],
            )
            .await?
                != 1
        {
            return Ok(false);
        }

        let lease_changed = transaction
            .execute(
                "UPDATE leases
                    SET expires_at = ?1
                  WHERE project_id = ?2 AND holder = ?3",
                params![lease_expires_at, project_id, lease_holder.as_str()],
            )
            .await
            .with_context(|| format!("Failed to renew worker {worker_token} lease"))?;
        if lease_changed != 1
            && query_count(
                &transaction,
                "SELECT COUNT(*) FROM leases
                  WHERE project_id = ?1 AND holder = ?2 AND expires_at = ?3",
                params![project_id, lease_holder.as_str(), lease_expires_at],
            )
            .await?
                != 1
        {
            return Ok(false);
        }

        transaction
            .commit()
            .await
            .with_context(|| format!("Failed to commit worker {worker_token} heartbeat"))?;
        Ok(true)
    }

    pub(crate) fn list_active_workers_blocking(&self) -> Result<Vec<AgentWorkerRecord>> {
        self.blocking.block_on(self.list_active_workers())
    }

    async fn list_active_workers(&self) -> Result<Vec<AgentWorkerRecord>> {
        self.list_workers_by_terminal_state(false).await
    }

    pub(crate) fn list_terminal_workers_blocking(&self) -> Result<Vec<AgentWorkerRecord>> {
        self.blocking.block_on(self.list_terminal_workers())
    }

    pub(crate) fn supersede_abandoned_workers_for_lease_blocking(
        &self,
        project_id: i64,
        expected_lease_holder: &str,
    ) -> Result<u64> {
        self.blocking.block_on(async {
            let conn = self.repositories.workers_leases.connect().await?;
            conn.execute(
                "UPDATE agent_workers
                        SET state = 'superseded'
                      WHERE project_id = ?1 AND state = 'abandoned'
                        AND EXISTS (
                            SELECT 1 FROM leases
                             WHERE leases.project_id = agent_workers.project_id
                               AND leases.holder = ?2
                        )",
                params![project_id, expected_lease_holder],
            )
            .await
            .with_context(|| {
                format!("Failed to supersede abandoned workers for project {project_id}")
            })
        })
    }

    async fn list_terminal_workers(&self) -> Result<Vec<AgentWorkerRecord>> {
        self.list_workers_by_terminal_state(true).await
    }

    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn mark_worker_service_cleaned_blocking(
        &self,
        worker_token: &str,
        cleaned_at: &str,
    ) -> Result<bool> {
        self.blocking.block_on(async {
            let conn = self.repositories.workers_leases.connect().await?;
            let changed = conn
                .execute(
                    "UPDATE agent_workers
                            SET service_cleaned_at = ?1
                          WHERE worker_token = ?2
                            AND state NOT IN ('dispatching', 'running', 'finalizing')",
                    params![cleaned_at, worker_token],
                )
                .await
                .with_context(|| {
                    format!("Failed to mark worker {worker_token} service metadata cleaned")
                })?;
            Ok(changed == 1)
        })
    }

    async fn list_workers_by_terminal_state(
        &self,
        terminal: bool,
    ) -> Result<Vec<AgentWorkerRecord>> {
        let conn = self.repositories.workers_leases.connect().await?;
        let states = if terminal {
            "NOT IN ('dispatching', 'running', 'finalizing')"
        } else {
            "IN ('dispatching', 'running', 'finalizing')"
        };
        let sql = format!(
            "SELECT w.worker_token, w.project_id, p.name, p.path, w.state,
                    w.protocol_version, w.lease_holder, w.service_label, w.binary_path,
                    w.command_arguments, w.path_env, w.codex_path, w.task_selection,
                    w.resume_session_id, w.worker_pid, w.created_at, w.started_at,
                    w.heartbeat_at, w.finished_at, w.run_id, w.error, w.service_cleaned_at
               FROM agent_workers w
               JOIN projects p ON p.id = w.project_id
              WHERE w.state {states}
              ORDER BY CAST(w.created_at AS INTEGER), w.worker_token"
        );
        let mut rows = conn.query(&sql, ()).await.with_context(|| {
            if terminal {
                "Failed to list terminal agent workers"
            } else {
                "Failed to list active agent workers"
            }
        })?;
        let mut workers = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .context("Failed to read agent worker row")?
        {
            let worker_pid = row_optional_integer(&row, 14, "worker_pid")?
                .map(u32::try_from)
                .transpose()
                .context("Agent worker PID is outside the supported range")?;
            workers.push(AgentWorkerRecord {
                worker_token: row_text(&row, 0, "worker_token")?,
                project_id: row_integer(&row, 1, "project_id")?,
                project_name: row_text(&row, 2, "name")?,
                project_path: PathBuf::from(row_text(&row, 3, "path")?),
                state: row_text(&row, 4, "state")?,
                protocol_version: row_integer(&row, 5, "protocol_version")?,
                lease_holder: row_text(&row, 6, "lease_holder")?,
                service_label: row_text(&row, 7, "service_label")?,
                binary_path: PathBuf::from(row_text(&row, 8, "binary_path")?),
                command_arguments: row_text(&row, 9, "command_arguments")?,
                path_env: OsString::from(row_text(&row, 10, "path_env")?),
                codex_path: row_optional_text(&row, 11, "codex_path")?.map(PathBuf::from),
                task_selection: row_text(&row, 12, "task_selection")?,
                resume_session_id: row_optional_text(&row, 13, "resume_session_id")?,
                worker_pid,
                created_at: row_text(&row, 15, "created_at")?,
                started_at: row_optional_text(&row, 16, "started_at")?,
                heartbeat_at: row_optional_text(&row, 17, "heartbeat_at")?,
                finished_at: row_optional_text(&row, 18, "finished_at")?,
                run_id: row_optional_integer(&row, 19, "run_id")?,
                error: row_optional_text(&row, 20, "error")?,
                service_cleaned_at: row_optional_text(&row, 21, "service_cleaned_at")?,
            });
        }
        Ok(workers)
    }

    pub(crate) fn abandon_worker_blocking(
        &self,
        abandonment: AgentWorkerAbandonment<'_>,
    ) -> Result<bool> {
        self.blocking.block_on(self.abandon_worker(abandonment))
    }

    async fn abandon_worker(&self, abandonment: AgentWorkerAbandonment<'_>) -> Result<bool> {
        let AgentWorkerAbandonment {
            worker_token,
            expected_state,
            expected_worker_pid,
            expected_heartbeat_at,
            finished_at,
            error,
            permitted_successor_holder,
        } = abandonment;
        let mut conn = self.repositories.workers_leases.connect().await?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .with_context(|| format!("Failed to begin abandoning worker {worker_token}"))?;

        let (project_id, created_at, started_at, worker_lease_holder) = {
            let mut rows = transaction
                .query(
                    "SELECT project_id, created_at, started_at, lease_holder
                       FROM agent_workers
                      WHERE worker_token = ?1 AND state = ?2
                        AND (worker_pid = ?3 OR (worker_pid IS NULL AND ?3 IS NULL))
                        AND (heartbeat_at = ?4 OR (heartbeat_at IS NULL AND ?4 IS NULL))",
                    params![
                        worker_token,
                        expected_state,
                        expected_worker_pid.map(i64::from),
                        expected_heartbeat_at,
                    ],
                )
                .await
                .with_context(|| format!("Failed to inspect worker {worker_token}"))?;
            let Some(row) = rows
                .next()
                .await
                .context("Failed to read worker abandonment row")?
            else {
                return Ok(false);
            };
            (
                row_integer(&row, 0, "project_id")?,
                row_text(&row, 1, "created_at")?,
                row_optional_text(&row, 2, "started_at")?,
                row_text(&row, 3, "lease_holder")?,
            )
        };
        let observed_lease_holder = {
            let mut rows = transaction
                .query(
                    "SELECT holder FROM leases WHERE project_id = ?1",
                    [project_id],
                )
                .await
                .with_context(|| {
                    format!("Failed to inspect worker {worker_token} project lease")
                })?;
            rows.next()
                .await
                .context("Failed to read worker project lease")?
                .map(|row| row_text(&row, 0, "holder"))
                .transpose()?
        };
        let preserve_successor_lease = match observed_lease_holder.as_deref() {
            None => false,
            Some(holder) if holder == worker_lease_holder => false,
            Some(holder) if permitted_successor_holder == Some(holder) => true,
            Some(_) => return Ok(false),
        };
        if observed_lease_holder.is_some()
            && observed_lease_holder.as_deref() != Some(worker_lease_holder.as_str())
            && !preserve_successor_lease
        {
            return Ok(false);
        }

        let run_started_at = started_at.as_deref().unwrap_or(created_at.as_str());
        let outcome = AgentRunOutcome {
            project_id,
            status: "failure",
            started_at: run_started_at,
            finished_at: Some(finished_at),
            exit_code: None,
            log_dir: None,
            stdout_path: None,
            stderr_path: None,
            summary: Some(error),
            codex_session_id: None,
        };
        let inserted = transaction
            .execute(
                "INSERT OR IGNORE INTO runs (
                    project_id, status, started_at, finished_at, exit_code,
                    log_dir, stdout_path, stderr_path, summary, codex_session_id,
                    worker_token
                 ) VALUES (?1, ?2, ?3, ?4, NULL, NULL, NULL, NULL, ?5, NULL, ?6)",
                params![
                    project_id,
                    outcome.status,
                    outcome.started_at,
                    outcome.finished_at,
                    error,
                    worker_token,
                ],
            )
            .await
            .with_context(|| format!("Failed to record abandoned worker {worker_token}"))?;
        let run_id = if inserted == 1 {
            let run_id = query_count(&transaction, "SELECT last_insert_rowid()", ()).await?;
            update_project_after_run(&transaction, &outcome).await?;
            run_id
        } else {
            query_count(
                &transaction,
                "SELECT id FROM runs WHERE worker_token = ?1 AND project_id = ?2",
                params![worker_token, project_id],
            )
            .await?
        };
        let changed = transaction
            .execute(
                "UPDATE agent_workers
                    SET state = 'abandoned', finished_at = ?1, error = ?2, run_id = ?3
                  WHERE worker_token = ?4 AND state = ?5
                    AND (worker_pid = ?6 OR (worker_pid IS NULL AND ?6 IS NULL))
                    AND (
                        heartbeat_at = ?7
                        OR (heartbeat_at IS NULL AND ?7 IS NULL)
                    )",
                params![
                    finished_at,
                    error,
                    run_id,
                    worker_token,
                    expected_state,
                    expected_worker_pid.map(i64::from),
                    expected_heartbeat_at,
                ],
            )
            .await
            .with_context(|| format!("Failed to abandon worker {worker_token}"))?;
        if changed != 1 {
            return Ok(false);
        }

        if observed_lease_holder.as_deref() == Some(worker_lease_holder.as_str()) {
            let released = transaction
                .execute(
                    "DELETE FROM leases WHERE project_id = ?1 AND holder = ?2",
                    params![project_id, worker_lease_holder.as_str()],
                )
                .await
                .with_context(|| {
                    format!("Failed to release abandoned worker {worker_token} lease")
                })?;
            if released != 1 {
                return Ok(false);
            }
        }

        transaction
            .commit()
            .await
            .with_context(|| format!("Failed to commit abandoned worker {worker_token}"))?;
        Ok(true)
    }

    pub(crate) fn finalize_worker_blocking(
        &self,
        finalization: AgentWorkerFinalization<'_>,
    ) -> Result<Option<i64>> {
        self.blocking.block_on(self.finalize_worker(finalization))
    }

    async fn finalize_worker(
        &self,
        finalization: AgentWorkerFinalization<'_>,
    ) -> Result<Option<i64>> {
        let AgentWorkerFinalization {
            worker_token,
            expected_worker_pid,
            expected_lease_holder,
            status,
            finished_at,
            exit_code,
            log_dir,
            stdout_path,
            stderr_path,
            summary,
            codex_session_id,
            error,
        } = finalization;
        let mut conn = self.repositories.workers_leases.connect().await?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .with_context(|| format!("Failed to begin finalizing worker {worker_token}"))?;
        let (project_id, state, observed_worker_pid, created_at, started_at, existing_run_id) = {
            let mut rows = transaction
                .query(
                    "SELECT project_id, state, worker_pid, created_at, started_at, run_id
                       FROM agent_workers
                      WHERE worker_token = ?1",
                    [worker_token],
                )
                .await
                .with_context(|| {
                    format!("Failed to read worker {worker_token} for finalization")
                })?;
            let Some(row) = rows
                .next()
                .await
                .context("Failed to read worker finalization row")?
            else {
                return Ok(None);
            };
            (
                row_integer(&row, 0, "project_id")?,
                row_text(&row, 1, "state")?,
                row_optional_integer(&row, 2, "worker_pid")?,
                row_text(&row, 3, "created_at")?,
                row_optional_text(&row, 4, "started_at")?,
                row_optional_integer(&row, 5, "run_id")?,
            )
        };
        if state == "completed" {
            transaction.commit().await.with_context(|| {
                format!("Failed to finish idempotent worker {worker_token} finalization")
            })?;
            return existing_run_id
                .map(Some)
                .context("Completed agent worker is missing its run ID");
        }
        if !matches!(state.as_str(), "dispatching" | "running" | "finalizing")
            || observed_worker_pid != expected_worker_pid.map(i64::from)
        {
            return Ok(None);
        }
        if query_count(
            &transaction,
            "SELECT COUNT(*) FROM leases WHERE project_id = ?1 AND holder = ?2",
            params![project_id, expected_lease_holder],
        )
        .await?
            != 1
        {
            return Ok(None);
        }

        let claimed = transaction
            .execute(
                "UPDATE agent_workers
                    SET state = 'finalizing'
                  WHERE worker_token = ?1 AND state = ?2
                    AND (worker_pid = ?3 OR (worker_pid IS NULL AND ?3 IS NULL))",
                params![
                    worker_token,
                    state.as_str(),
                    expected_worker_pid.map(i64::from)
                ],
            )
            .await
            .with_context(|| format!("Failed to claim worker {worker_token} finalization"))?;
        if claimed != 1 {
            return Ok(None);
        }

        let run_started_at = started_at.as_deref().unwrap_or(created_at.as_str());
        let outcome = AgentRunOutcome {
            project_id,
            status,
            started_at: run_started_at,
            finished_at: Some(finished_at),
            exit_code,
            log_dir,
            stdout_path,
            stderr_path,
            summary,
            codex_session_id,
        };
        let inserted = transaction
            .execute(
                "INSERT OR IGNORE INTO runs (
                    project_id, status, started_at, finished_at, exit_code,
                    log_dir, stdout_path, stderr_path, summary, codex_session_id,
                    worker_token
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
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
                    outcome.codex_session_id,
                    worker_token,
                ],
            )
            .await
            .with_context(|| format!("Failed to record run for worker {worker_token}"))?;
        let run_id = if inserted == 1 {
            let run_id = query_count(&transaction, "SELECT last_insert_rowid()", ()).await?;
            update_project_after_run(&transaction, &outcome).await?;
            run_id
        } else {
            query_count(
                &transaction,
                "SELECT id FROM runs WHERE worker_token = ?1 AND project_id = ?2",
                params![worker_token, project_id],
            )
            .await
            .with_context(|| {
                format!("Failed to reuse the existing run for worker {worker_token}")
            })?
        };

        let completed = transaction
            .execute(
                "UPDATE agent_workers
                    SET state = 'completed', finished_at = ?1, run_id = ?2, error = ?3
                  WHERE worker_token = ?4 AND state = 'finalizing'",
                params![finished_at, run_id, error, worker_token],
            )
            .await
            .with_context(|| format!("Failed to complete worker {worker_token}"))?;
        if completed != 1 {
            return Ok(None);
        }
        transaction
            .execute(
                "DELETE FROM leases
                  WHERE project_id = ?1
                    AND holder = (
                            SELECT lease_holder FROM agent_workers WHERE worker_token = ?2
                        )",
                params![project_id, worker_token],
            )
            .await
            .with_context(|| format!("Failed to release completed worker {worker_token} lease"))?;
        transaction
            .commit()
            .await
            .with_context(|| format!("Failed to commit worker {worker_token} finalization"))?;
        Ok(Some(run_id))
    }
}
