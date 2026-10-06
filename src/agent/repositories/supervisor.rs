use anyhow::{Context, Result};
use clt_database::turso::params;

use crate::{
    agent::{TursoAgentStore, row_text},
    runner::agent_timestamp,
    supervisor::{SupervisorReview, SupervisorSettings},
};

impl TursoAgentStore {
    pub(crate) fn supervisor_settings_blocking(&self) -> Result<SupervisorSettings> {
        if self
            .pending_migration_version()
            .is_some_and(|version| version <= 20)
        {
            return Ok(SupervisorSettings::default());
        }
        self.blocking.block_on(async {
            let conn = self.repositories.projects_models.connect().await?;
            let mut rows = conn
                .query(
                    "SELECT supervisor_settings FROM agent_settings WHERE id = 1",
                    (),
                )
                .await?;
            let Some(row) = rows.next().await? else {
                return Ok(SupervisorSettings::default());
            };
            let settings = crate::agent::row_optional_text(&row, 0, "supervisor_settings")?;
            settings
                .map(|text| serde_json::from_str(&text).context("Invalid supervisor settings"))
                .unwrap_or_else(|| Ok(SupervisorSettings::default()))
        })
    }

    pub(crate) fn set_supervisor_settings_blocking(
        &self,
        settings: &SupervisorSettings,
    ) -> Result<()> {
        anyhow::ensure!(
            self.pending_migration_version().is_none(),
            "Supervisor upgrade is waiting for older workers to finish"
        );
        let json = serde_json::to_string(settings)?;
        self.blocking.block_on_persist(async {
            let conn = self.repositories.projects_models.connect().await?;
            conn.execute(
                "UPDATE agent_settings SET supervisor_settings = ?1, updated_at = ?2 WHERE id = 1",
                params![json, agent_timestamp()],
            )
            .await?;
            Ok(())
        })
    }

    pub(crate) fn supervisor_review_blocking(
        &self,
        project_id: i64,
    ) -> Result<Option<SupervisorReview>> {
        if self
            .pending_migration_version()
            .is_some_and(|version| version <= 20)
        {
            return Ok(None);
        }
        self.blocking.block_on(async {
            let conn = self.repositories.projects_models.connect().await?;
            let mut rows = conn
                .query(
                    "SELECT record FROM supervisor_reviews WHERE project_id = ?1",
                    [project_id],
                )
                .await?;
            rows.next()
                .await?
                .map(|row| {
                    serde_json::from_str(&row_text(&row, 0, "record")?)
                        .context("Invalid supervisor review")
                })
                .transpose()
        })
    }

    pub(crate) fn save_supervisor_review_blocking(
        &self,
        project_id: i64,
        review: &SupervisorReview,
    ) -> Result<()> {
        let json = serde_json::to_string(review)?;
        self.blocking.block_on_persist(async {
            let conn = self.repositories.projects_models.connect().await?;
            conn.execute("INSERT INTO supervisor_reviews (project_id, evidence, record, updated_at)
                VALUES (?1, ?2, ?3, ?4) ON CONFLICT(project_id) DO UPDATE SET
                evidence = excluded.evidence, record = excluded.record, updated_at = excluded.updated_at",
                params![project_id, review.evidence.as_str(), json, agent_timestamp()]).await?;
            Ok(())
        })
    }

    pub(crate) fn clear_supervisor_review_blocking(&self, project_id: i64) -> Result<()> {
        if self
            .pending_migration_version()
            .is_some_and(|version| version <= 20)
        {
            return Ok(());
        }
        self.blocking.block_on_persist(async {
            let conn = self.repositories.projects_models.connect().await?;
            conn.execute(
                "DELETE FROM supervisor_reviews WHERE project_id = ?1",
                [project_id],
            )
            .await?;
            Ok(())
        })
    }
}
