use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use turso::{Connection, Database, params, transaction::TransactionBehavior};

use super::RepositoryDatabase;
use crate::{
    agent::{
        AgentGitMode, AgentModelDefaults, AgentModelProvider, AgentModelTarget, AgentProject,
        TursoAgentStore, query_count, row_integer, row_optional_text, row_text,
    },
    application::AgentLeaseHolderLiveness,
    runner::{agent_timestamp, agent_timestamp_seconds},
    scheduler::agent_lease_holder_liveness,
};

/// Persistence for registered projects, provider configuration, and models.
pub(in crate::agent) struct ProjectsModelsRepository(RepositoryDatabase);

impl ProjectsModelsRepository {
    pub(in crate::agent) fn new(db: &Database) -> Self {
        Self(RepositoryDatabase::new(db))
    }

    pub(in crate::agent) async fn connect(&self) -> Result<Connection> {
        self.0.connect().await
    }
}

impl TursoAgentStore {
    pub(crate) fn set_project_enabled_blocking(
        &self,
        project_id: i64,
        enabled: bool,
    ) -> Result<bool> {
        self.blocking
            .block_on(self.set_project_enabled(project_id, enabled))
    }

    async fn set_project_enabled(&self, project_id: i64, enabled: bool) -> Result<bool> {
        let conn = self.repositories.projects_models.connect().await?;
        let changed = conn
            .execute(
                "UPDATE projects SET enabled = ?1, updated_at = ?2 WHERE id = ?3",
                params![
                    if enabled { 1_i64 } else { 0_i64 },
                    agent_timestamp(),
                    project_id
                ],
            )
            .await
            .with_context(|| format!("Failed to set project {} enabled state", project_id))?;

        Ok(changed > 0)
    }

    pub(crate) fn set_project_enabled_for_path_blocking(
        &self,
        project_root: &Path,
        enabled: bool,
    ) -> Result<bool> {
        self.blocking
            .block_on(self.set_project_enabled_for_path(project_root, enabled))
    }

    async fn set_project_enabled_for_path(
        &self,
        project_root: &Path,
        enabled: bool,
    ) -> Result<bool> {
        let conn = self.repositories.projects_models.connect().await?;
        let path = project_root.display().to_string();
        let changed = conn
            .execute(
                "UPDATE projects SET enabled = ?1, updated_at = ?2 WHERE path = ?3",
                params![
                    if enabled { 1_i64 } else { 0_i64 },
                    agent_timestamp(),
                    path.as_str()
                ],
            )
            .await
            .with_context(|| format!("Failed to set project {} enabled state", path))?;

        Ok(changed > 0)
    }

    pub(crate) fn clear_project_failure_backoff_for_path_blocking(
        &self,
        project_root: &Path,
    ) -> Result<bool> {
        self.blocking.block_on(async {
            let mut conn = self.repositories.projects_models.connect().await?;
            let path = project_root.display().to_string();
            let transaction = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .await
                .with_context(|| format!("Failed to begin retrying registered project {path}"))?;
            if query_count(
                &transaction,
                "SELECT COUNT(*) FROM agent_workers
                      WHERE project_id = (SELECT id FROM projects WHERE path = ?1)
                        AND state IN ('dispatching', 'running', 'finalizing')",
                [path.as_str()],
            )
            .await?
                > 0
                || query_count(
                    &transaction,
                    "SELECT COUNT(*) FROM leases
                          WHERE project_id = (SELECT id FROM projects WHERE path = ?1)
                            AND CAST(expires_at AS INTEGER) > CAST(?2 AS INTEGER)",
                    params![path.as_str(), agent_timestamp()],
                )
                .await?
                    > 0
            {
                anyhow::bail!(
                    "Cannot retry project {path} while its agent worker or lease is active"
                );
            }
            let changed = transaction
                .execute(
                    "UPDATE projects SET failure_count = 0, updated_at = ?1 WHERE path = ?2",
                    params![agent_timestamp(), path.as_str()],
                )
                .await
                .with_context(|| format!("Failed to clear project {path} failure cooldown"))?;
            transaction
                .commit()
                .await
                .with_context(|| format!("Failed to commit project {path} immediate retry"))?;
            Ok(changed > 0)
        })
    }

    pub(crate) fn set_project_git_mode_blocking(
        &self,
        project_id: i64,
        mode: AgentGitMode,
    ) -> Result<bool> {
        self.blocking
            .block_on(self.set_project_git_mode(project_id, mode))
    }

    async fn set_project_git_mode(&self, project_id: i64, mode: AgentGitMode) -> Result<bool> {
        let mut conn = self.repositories.projects_models.connect().await?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .with_context(|| format!("Failed to begin setting project {project_id} Git mode"))?;
        if query_count(
            &transaction,
            "SELECT COUNT(*) FROM git_finalizations
              WHERE project_id = ?1
                AND state IN ('working', 'tracking', 'commit_pending', 'push_pending')",
            [project_id],
        )
        .await?
            > 0
            || query_count(
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
                "SELECT COUNT(*) FROM leases WHERE project_id = ?1",
                [project_id],
            )
            .await?
                > 0
        {
            anyhow::bail!(
                "Cannot change project {project_id} Git mode while an agent run or Git journal is active"
            );
        }
        let changed = transaction
            .execute(
                "UPDATE projects SET git_mode = ?1, updated_at = ?2 WHERE id = ?3",
                params![mode.database_value(), agent_timestamp(), project_id],
            )
            .await
            .with_context(|| format!("Failed to set project {} Git mode", project_id))?;
        transaction
            .commit()
            .await
            .with_context(|| format!("Failed to commit project {project_id} Git mode"))?;

        Ok(changed > 0)
    }

    pub(crate) fn set_project_git_mode_for_path_blocking(
        &self,
        project_root: &Path,
        mode: AgentGitMode,
    ) -> Result<bool> {
        self.blocking
            .block_on(self.set_project_git_mode_for_path(project_root, mode))
    }

    async fn set_project_git_mode_for_path(
        &self,
        project_root: &Path,
        mode: AgentGitMode,
    ) -> Result<bool> {
        let mut conn = self.repositories.projects_models.connect().await?;
        let path = project_root.display().to_string();
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .with_context(|| format!("Failed to begin setting project {path} Git mode"))?;
        if query_count(
            &transaction,
            "SELECT COUNT(*) FROM git_finalizations
              WHERE project_id = (SELECT id FROM projects WHERE path = ?1)
                AND state IN ('working', 'tracking', 'commit_pending', 'push_pending')",
            [path.as_str()],
        )
        .await?
            > 0
            || query_count(
                &transaction,
                "SELECT COUNT(*) FROM agent_workers
                  WHERE project_id = (SELECT id FROM projects WHERE path = ?1)
                    AND state IN ('dispatching', 'running', 'finalizing')",
                [path.as_str()],
            )
            .await?
                > 0
            || query_count(
                &transaction,
                "SELECT COUNT(*) FROM leases
                  WHERE project_id = (SELECT id FROM projects WHERE path = ?1)",
                [path.as_str()],
            )
            .await?
                > 0
        {
            anyhow::bail!(
                "Cannot change project {path} Git mode while an agent run or Git journal is active"
            );
        }
        let changed = transaction
            .execute(
                "UPDATE projects SET git_mode = ?1, updated_at = ?2 WHERE path = ?3",
                params![mode.database_value(), agent_timestamp(), path.as_str()],
            )
            .await
            .with_context(|| format!("Failed to set project {} Git mode", path))?;
        transaction
            .commit()
            .await
            .with_context(|| format!("Failed to commit project {path} Git mode"))?;

        Ok(changed > 0)
    }

    pub(crate) fn set_project_codex_settings_blocking(
        &self,
        project_id: i64,
        provider: Option<&str>,
        model: Option<&str>,
        reasoning_effort: Option<&str>,
        fast_enabled: bool,
    ) -> Result<bool> {
        self.blocking.block_on(self.set_project_codex_settings(
            project_id,
            provider,
            model,
            reasoning_effort,
            fast_enabled,
        ))
    }

    async fn set_project_codex_settings(
        &self,
        project_id: i64,
        provider: Option<&str>,
        model: Option<&str>,
        reasoning_effort: Option<&str>,
        fast_enabled: bool,
    ) -> Result<bool> {
        let conn = self.repositories.projects_models.connect().await?;
        let changed = conn
            .execute(
                "UPDATE projects
                 SET codex_provider = ?1,
                     codex_model = ?2,
                     codex_reasoning_effort = ?3,
                     codex_fast_enabled = ?4,
                     updated_at = ?5
                 WHERE id = ?6",
                params![
                    provider,
                    model,
                    reasoning_effort,
                    if fast_enabled { 1_i64 } else { 0_i64 },
                    agent_timestamp(),
                    project_id
                ],
            )
            .await
            .with_context(|| format!("Failed to set project {} Codex settings", project_id))?;

        Ok(changed > 0)
    }

    pub(crate) fn list_model_providers_blocking(&self) -> Result<Vec<AgentModelProvider>> {
        self.blocking.block_on(async {
            let conn = self.repositories.projects_models.connect().await?;
            let mut rows = conn
                .query(
                    "SELECT provider_id, name, base_url, env_key, built_in, enabled
                         FROM model_providers
                         ORDER BY built_in DESC, name COLLATE NOCASE, provider_id COLLATE NOCASE",
                    (),
                )
                .await
                .context("Failed to list model providers")?;
            let mut providers = Vec::new();
            while let Some(row) = rows.next().await.context("Failed to read model provider")? {
                providers.push(AgentModelProvider {
                    id: row_text(&row, 0, "provider_id")?,
                    name: row_text(&row, 1, "name")?,
                    base_url: row_optional_text(&row, 2, "base_url")?,
                    env_key: row_optional_text(&row, 3, "env_key")?,
                    built_in: row_integer(&row, 4, "built_in")? != 0,
                    enabled: row_integer(&row, 5, "enabled")? != 0,
                });
            }
            Ok(providers)
        })
    }

    pub(crate) fn list_model_targets_blocking(
        &self,
        provider_id: Option<&str>,
    ) -> Result<Vec<AgentModelTarget>> {
        self.blocking.block_on(async {
            let conn = self.repositories.projects_models.connect().await?;
            let mut rows = conn
                .query(
                    "SELECT provider_id, model_id, label, enabled, favorite, reasoning_effort
                         FROM model_targets
                         WHERE (?1 IS NULL OR provider_id = ?1)
                         ORDER BY favorite DESC, label COLLATE NOCASE, model_id COLLATE NOCASE",
                    [provider_id],
                )
                .await
                .context("Failed to list model targets")?;
            let mut targets = Vec::new();
            while let Some(row) = rows.next().await.context("Failed to read model target")? {
                targets.push(AgentModelTarget {
                    provider_id: row_text(&row, 0, "provider_id")?,
                    model_id: row_text(&row, 1, "model_id")?,
                    label: row_text(&row, 2, "label")?,
                    enabled: row_integer(&row, 3, "enabled")? != 0,
                    favorite: row_integer(&row, 4, "favorite")? != 0,
                    reasoning_effort: row_optional_text(&row, 5, "reasoning_effort")?,
                });
            }
            Ok(targets)
        })
    }

    pub(crate) fn list_enabled_model_targets_blocking(&self) -> Result<Vec<AgentModelTarget>> {
        self.blocking.block_on(async {
            let conn = self.repositories.projects_models.connect().await?;
            let mut rows = conn
                .query(
                    "SELECT t.provider_id, t.model_id, t.label, t.enabled, t.favorite,
                                t.reasoning_effort
                         FROM model_targets t
                         JOIN model_providers p ON p.provider_id = t.provider_id
                         WHERE p.enabled != 0 AND t.enabled != 0
                         ORDER BY t.favorite DESC, p.name COLLATE NOCASE,
                                  t.label COLLATE NOCASE, t.model_id COLLATE NOCASE",
                    (),
                )
                .await
                .context("Failed to list enabled model targets")?;
            let mut targets = Vec::new();
            while let Some(row) = rows.next().await.context("Failed to read model target")? {
                targets.push(AgentModelTarget {
                    provider_id: row_text(&row, 0, "provider_id")?,
                    model_id: row_text(&row, 1, "model_id")?,
                    label: row_text(&row, 2, "label")?,
                    enabled: row_integer(&row, 3, "enabled")? != 0,
                    favorite: row_integer(&row, 4, "favorite")? != 0,
                    reasoning_effort: row_optional_text(&row, 5, "reasoning_effort")?,
                });
            }
            Ok(targets)
        })
    }

    pub(crate) fn model_defaults_blocking(&self) -> Result<AgentModelDefaults> {
        self.blocking.block_on(async {
            let conn = self.repositories.projects_models.connect().await?;
            let mut rows = conn
                .query(
                    "SELECT default_provider, default_model FROM agent_settings WHERE id = 1",
                    (),
                )
                .await
                .context("Failed to read model defaults")?;
            let Some(row) = rows.next().await.context("Failed to read model defaults")? else {
                return Ok(AgentModelDefaults::default());
            };
            Ok(AgentModelDefaults {
                provider_id: row_optional_text(&row, 0, "default_provider")?,
                model_id: row_optional_text(&row, 1, "default_model")?,
            })
        })
    }

    pub(crate) fn resolve_model_target_blocking(
        &self,
        project: &AgentProject,
    ) -> Result<AgentModelDefaults> {
        if let Some(model_id) = project.codex_model.as_ref() {
            return Ok(AgentModelDefaults {
                provider_id: Some(
                    project
                        .codex_provider
                        .clone()
                        .unwrap_or_else(|| "openai".to_string()),
                ),
                model_id: Some(model_id.clone()),
            });
        }
        self.model_defaults_blocking()
    }

    pub(crate) fn model_target_reasoning_blocking(
        &self,
        provider_id: &str,
        model_id: &str,
    ) -> Result<Option<String>> {
        self.blocking.block_on(async {
            let conn = self.repositories.projects_models.connect().await?;
            let mut rows = conn
                .query(
                    "SELECT reasoning_effort
                         FROM model_targets
                         WHERE provider_id = ?1 AND model_id = ?2",
                    params![provider_id, model_id],
                )
                .await
                .with_context(|| {
                    format!("Failed to read model {provider_id}/{model_id} reasoning")
                })?;
            let Some(row) = rows
                .next()
                .await
                .context("Failed to read model reasoning")?
            else {
                return Ok(None);
            };
            row_optional_text(&row, 0, "reasoning_effort")
        })
    }

    pub(crate) fn upsert_model_provider_blocking(
        &self,
        provider: &AgentModelProvider,
    ) -> Result<()> {
        self.blocking.block_on(async {
            let conn = self.repositories.projects_models.connect().await?;
            conn.execute(
                "INSERT INTO model_providers (
                        provider_id, name, base_url, env_key, built_in, enabled,
                        created_at, updated_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'), datetime('now'))
                     ON CONFLICT(provider_id) DO UPDATE SET
                        name = excluded.name,
                        base_url = excluded.base_url,
                        env_key = excluded.env_key,
                        built_in = excluded.built_in,
                        enabled = excluded.enabled,
                        updated_at = datetime('now')",
                params![
                    provider.id.as_str(),
                    provider.name.as_str(),
                    provider.base_url.as_deref(),
                    provider.env_key.as_deref(),
                    if provider.built_in { 1_i64 } else { 0_i64 },
                    if provider.enabled { 1_i64 } else { 0_i64 },
                ],
            )
            .await
            .with_context(|| format!("Failed to save model provider {}", provider.id))?;
            Ok(())
        })
    }

    pub(crate) fn delete_model_provider_blocking(&self, provider_id: &str) -> Result<bool> {
        self.blocking.block_on(async {
            let mut conn = self.repositories.projects_models.connect().await?;
            let transaction = conn
                .transaction()
                .await
                .with_context(|| format!("Failed to begin deleting provider {provider_id}"))?;
            transaction
                .execute(
                    "UPDATE projects
                         SET codex_provider = NULL, codex_model = NULL,
                             updated_at = datetime('now')
                         WHERE codex_provider = ?1",
                    [provider_id],
                )
                .await
                .with_context(|| {
                    format!("Failed to clear project settings for provider {provider_id}")
                })?;
            transaction
                .execute(
                    "UPDATE agent_settings
                         SET default_provider = NULL, default_model = NULL,
                             updated_at = datetime('now')
                         WHERE default_provider = ?1",
                    [provider_id],
                )
                .await
                .with_context(|| {
                    format!("Failed to clear the default for provider {provider_id}")
                })?;
            transaction
                .execute(
                    "DELETE FROM model_targets WHERE provider_id = ?1",
                    [provider_id],
                )
                .await
                .with_context(|| format!("Failed to delete models for provider {provider_id}"))?;
            let deleted = transaction
                .execute(
                    "DELETE FROM model_providers WHERE provider_id = ?1",
                    [provider_id],
                )
                .await
                .with_context(|| format!("Failed to delete provider {provider_id}"))?;
            transaction
                .commit()
                .await
                .with_context(|| format!("Failed to commit deleting provider {provider_id}"))?;
            Ok(deleted > 0)
        })
    }

    pub(crate) fn upsert_model_target_blocking(&self, target: &AgentModelTarget) -> Result<()> {
        self.blocking.block_on(async {
            let conn = self.repositories.projects_models.connect().await?;
            conn.execute(
                "INSERT INTO model_targets (
                        provider_id, model_id, label, enabled, favorite, reasoning_effort,
                        created_at, updated_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'), datetime('now'))
                     ON CONFLICT(provider_id, model_id) DO UPDATE SET
                        label = excluded.label,
                        enabled = excluded.enabled,
                        favorite = excluded.favorite,
                        reasoning_effort = excluded.reasoning_effort,
                        updated_at = datetime('now')",
                params![
                    target.provider_id.as_str(),
                    target.model_id.as_str(),
                    target.label.as_str(),
                    if target.enabled { 1_i64 } else { 0_i64 },
                    if target.favorite { 1_i64 } else { 0_i64 },
                    target.reasoning_effort.as_deref(),
                ],
            )
            .await
            .with_context(|| {
                format!(
                    "Failed to save model target {}/{}",
                    target.provider_id, target.model_id
                )
            })?;
            Ok(())
        })
    }

    pub(crate) fn set_model_provider_enabled_blocking(
        &self,
        provider_id: &str,
        enabled: bool,
    ) -> Result<bool> {
        self.blocking.block_on(async {
            let conn = self.repositories.projects_models.connect().await?;
            let changed = conn
                .execute(
                    "UPDATE model_providers SET enabled = ?1, updated_at = datetime('now')
                         WHERE provider_id = ?2",
                    params![if enabled { 1_i64 } else { 0_i64 }, provider_id],
                )
                .await
                .with_context(|| format!("Failed to update provider {provider_id}"))?;
            Ok(changed > 0)
        })
    }

    pub(crate) fn set_model_target_flags_blocking(
        &self,
        provider_id: &str,
        model_id: &str,
        enabled: bool,
        favorite: bool,
    ) -> Result<bool> {
        self.blocking.block_on(async {
            let conn = self.repositories.projects_models.connect().await?;
            let changed = conn
                .execute(
                    "UPDATE model_targets
                         SET enabled = ?1, favorite = ?2, updated_at = datetime('now')
                         WHERE provider_id = ?3 AND model_id = ?4",
                    params![
                        if enabled { 1_i64 } else { 0_i64 },
                        if favorite { 1_i64 } else { 0_i64 },
                        provider_id,
                        model_id,
                    ],
                )
                .await
                .with_context(|| format!("Failed to update model {provider_id}/{model_id}"))?;
            Ok(changed > 0)
        })
    }

    pub(crate) fn set_model_target_reasoning_blocking(
        &self,
        provider_id: &str,
        model_id: &str,
        reasoning_effort: Option<&str>,
    ) -> Result<bool> {
        self.blocking.block_on(async {
            let conn = self.repositories.projects_models.connect().await?;
            let changed = conn
                .execute(
                    "UPDATE model_targets
                         SET reasoning_effort = ?1, updated_at = datetime('now')
                         WHERE provider_id = ?2 AND model_id = ?3",
                    params![reasoning_effort, provider_id, model_id],
                )
                .await
                .with_context(|| {
                    format!("Failed to update model {provider_id}/{model_id} reasoning")
                })?;
            Ok(changed > 0)
        })
    }

    pub(crate) fn set_model_default_blocking(
        &self,
        provider_id: &str,
        model_id: &str,
    ) -> Result<()> {
        self.blocking.block_on(async {
            let conn = self.repositories.projects_models.connect().await?;
            conn.execute(
                "UPDATE agent_settings
                     SET default_provider = ?1, default_model = ?2, updated_at = datetime('now')
                     WHERE id = 1",
                params![provider_id, model_id],
            )
            .await
            .context("Failed to set CLT default model")?;
            Ok(())
        })
    }
}

impl TursoAgentStore {
    pub(crate) fn record_project_scan_blocking(&self, project_id: i64) -> Result<String> {
        self.blocking.block_on(self.record_project_scan(project_id))
    }

    async fn record_project_scan(&self, project_id: i64) -> Result<String> {
        let conn = self.repositories.projects_models.connect().await?;
        let scanned_at = agent_timestamp();

        conn.execute(
            "UPDATE projects
             SET last_scan_at = ?1, updated_at = ?1
             WHERE id = ?2",
            params![scanned_at.as_str(), project_id],
        )
        .await
        .with_context(|| format!("Failed to record agent project scan {}", project_id))?;

        Ok(scanned_at)
    }

    pub(crate) fn record_project_daemon_scan_blocking(
        &self,
        project_id: i64,
        status: &str,
        error: Option<&str>,
    ) -> Result<String> {
        self.blocking
            .block_on(self.record_project_daemon_scan(project_id, status, error))
    }

    async fn record_project_daemon_scan(
        &self,
        project_id: i64,
        status: &str,
        error: Option<&str>,
    ) -> Result<String> {
        let conn = self.repositories.projects_models.connect().await?;
        let scanned_at = agent_timestamp();

        conn.execute(
            "UPDATE projects
             SET last_scan_at = ?1,
                 last_daemon_scan_status = ?2,
                 last_daemon_scan_error = ?3,
                 updated_at = ?1
             WHERE id = ?4",
            params![scanned_at.as_str(), status, error, project_id],
        )
        .await
        .with_context(|| format!("Failed to record daemon project scan {project_id}"))?;

        Ok(scanned_at)
    }
}

impl TursoAgentStore {
    pub(crate) fn register_project_blocking(
        &self,
        project_root: &Path,
        name: &str,
    ) -> Result<bool> {
        self.blocking
            .block_on(self.register_project(project_root, name))
    }

    async fn register_project(&self, project_root: &Path, name: &str) -> Result<bool> {
        let conn = self.repositories.projects_models.connect().await?;
        let path = project_root.display().to_string();
        let exists = query_count(
            &conn,
            "SELECT COUNT(*) FROM projects WHERE path = ?1",
            [path.as_str()],
        )
        .await?
            > 0;

        if exists {
            conn.execute(
                "UPDATE projects
                 SET name = ?1, enabled = 1, updated_at = datetime('now')
                 WHERE path = ?2",
                params![name, path.as_str()],
            )
            .await
            .with_context(|| format!("Failed to update registered project {}", path))?;
        } else {
            conn.execute(
                "INSERT INTO projects (path, name, registered_at, updated_at)
                 VALUES (?1, ?2, datetime('now'), datetime('now'))",
                params![path.as_str(), name],
            )
            .await
            .with_context(|| format!("Failed to register project {}", path))?;
        }

        Ok(!exists)
    }

    pub(crate) fn unregister_project_blocking(&self, project_root: &Path) -> Result<bool> {
        self.blocking
            .block_on(self.unregister_project(project_root))
    }

    async fn unregister_project(&self, project_root: &Path) -> Result<bool> {
        let mut conn = self.repositories.projects_models.connect().await?;
        let path = project_root.display().to_string();
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .with_context(|| format!("Failed to begin unregistering project {}", path))?;
        let active_workers = query_count(
            &transaction,
            "SELECT COUNT(*)
               FROM agent_workers
              WHERE project_id = (SELECT id FROM projects WHERE path = ?1)
                AND state IN ('dispatching', 'running', 'finalizing')",
            [path.as_str()],
        )
        .await?;
        if active_workers > 0 {
            anyhow::bail!(
                "Cannot unregister project {path} while {active_workers} independent worker(s) are active"
            );
        }
        let lease = {
            let mut rows = transaction
                .query(
                    "SELECT holder, expires_at
                       FROM leases
                      WHERE project_id = (SELECT id FROM projects WHERE path = ?1)",
                    [path.as_str()],
                )
                .await
                .with_context(|| format!("Failed to read agent lease for project {path}"))?;
            match rows
                .next()
                .await
                .context("Failed to read agent lease row while unregistering project")?
            {
                Some(row) => Some((
                    row_text(&row, 0, "holder")?,
                    row_text(&row, 1, "expires_at")?,
                )),
                None => None,
            }
        };
        if let Some((holder, expires_at)) = lease {
            let reclaimable = expires_at
                .parse::<u64>()
                .is_ok_and(|expires_at| expires_at <= agent_timestamp_seconds())
                || agent_lease_holder_liveness(&holder) == AgentLeaseHolderLiveness::Dead;
            if !reclaimable {
                anyhow::bail!("Cannot unregister project {path} while its agent lease is active");
            }
            transaction
                .execute(
                    "DELETE FROM leases
                      WHERE project_id = (SELECT id FROM projects WHERE path = ?1)
                        AND holder = ?2",
                    params![path.as_str(), holder],
                )
                .await
                .with_context(|| {
                    format!("Failed to reclaim stale agent lease for project {path}")
                })?;
        }
        let pending_git_finalizations = query_count(
            &transaction,
            "SELECT COUNT(*) FROM git_finalizations
              WHERE project_id = (SELECT id FROM projects WHERE path = ?1)
                AND state IN ('working', 'tracking', 'commit_pending', 'push_pending')",
            [path.as_str()],
        )
        .await?;
        if pending_git_finalizations > 0 {
            anyhow::bail!(
                "Cannot unregister project {path} while {pending_git_finalizations} Git finalization(s) are nonterminal"
            );
        }
        let unconsumed_git_launches = query_count(
            &transaction,
            "SELECT COUNT(*) FROM agent_git_launch_states
              WHERE project_id = (SELECT id FROM projects WHERE path = ?1)",
            [path.as_str()],
        )
        .await?;
        if unconsumed_git_launches > 0 {
            anyhow::bail!(
                "Cannot unregister project {path} while {unconsumed_git_launches} Git launch boundary record(s) remain unconsumed"
            );
        }
        transaction
            .execute(
                "DELETE FROM agent_workers
                 WHERE project_id = (SELECT id FROM projects WHERE path = ?1)",
                [path.as_str()],
            )
            .await
            .with_context(|| format!("Failed to remove worker history for project {path}"))?;
        transaction
            .execute(
                "DELETE FROM git_finalizations
                 WHERE project_id = (SELECT id FROM projects WHERE path = ?1)",
                [path.as_str()],
            )
            .await
            .with_context(|| {
                format!("Failed to remove Git finalization history for project {path}")
            })?;
        transaction
            .execute(
                "DELETE FROM runs
                 WHERE project_id = (SELECT id FROM projects WHERE path = ?1)",
                [path.as_str()],
            )
            .await
            .with_context(|| format!("Failed to remove run history for project {}", path))?;
        let removed = transaction
            .execute("DELETE FROM projects WHERE path = ?1", [path.as_str()])
            .await
            .with_context(|| format!("Failed to unregister project {}", path))?;
        transaction
            .commit()
            .await
            .with_context(|| format!("Failed to commit unregistering project {}", path))?;

        Ok(removed > 0)
    }

    pub(crate) fn list_projects_blocking(&self) -> Result<Vec<AgentProject>> {
        self.blocking.block_on(self.list_projects())
    }

    async fn list_projects(&self) -> Result<Vec<AgentProject>> {
        let conn = self.repositories.projects_models.connect().await?;
        let mut rows = conn
            .query(
                "SELECT id, path, name, enabled, git_mode, codex_provider, codex_model,
                        codex_reasoning_effort, codex_fast_enabled, last_scan_at,
                        last_daemon_scan_status, last_daemon_scan_error, last_run_at,
                        last_success_at, last_failure_at, last_blocked_recovery_at, failure_count
                 FROM projects
                 ORDER BY name COLLATE NOCASE, path COLLATE NOCASE",
                (),
            )
            .await
            .context("Failed to list registered projects")?;
        let mut projects = Vec::new();

        while let Some(row) = rows
            .next()
            .await
            .context("Failed to read registered project row")?
        {
            let id = row_integer(&row, 0, "id")?;
            let path = PathBuf::from(row_text(&row, 1, "path")?);
            let name = row_text(&row, 2, "name")?;
            let enabled = row_integer(&row, 3, "enabled")? != 0;
            let git_mode = AgentGitMode::from_database(&row_text(&row, 4, "git_mode")?)?;
            let codex_provider = row_optional_text(&row, 5, "codex_provider")?;
            let codex_model = row_optional_text(&row, 6, "codex_model")?;
            let codex_reasoning_effort = row_optional_text(&row, 7, "codex_reasoning_effort")?;
            let codex_fast_enabled = row_integer(&row, 8, "codex_fast_enabled")? != 0;
            let last_scan_at = row_optional_text(&row, 9, "last_scan_at")?;
            let last_daemon_scan_status = row_optional_text(&row, 10, "last_daemon_scan_status")?;
            let last_daemon_scan_error = row_optional_text(&row, 11, "last_daemon_scan_error")?;
            let last_run_at = row_optional_text(&row, 12, "last_run_at")?;
            let last_success_at = row_optional_text(&row, 13, "last_success_at")?;
            let last_failure_at = row_optional_text(&row, 14, "last_failure_at")?;
            let last_blocked_recovery_at = row_optional_text(&row, 15, "last_blocked_recovery_at")?;
            let failure_count = row_integer(&row, 16, "failure_count")?;

            projects.push(AgentProject {
                id,
                path,
                name,
                enabled,
                git_mode,
                codex_provider,
                codex_model,
                codex_reasoning_effort,
                codex_fast_enabled,
                last_scan_at,
                last_daemon_scan_status,
                last_daemon_scan_error,
                last_run_at,
                last_success_at,
                last_failure_at,
                last_blocked_recovery_at,
                failure_count,
            });
        }

        Ok(projects)
    }
}
