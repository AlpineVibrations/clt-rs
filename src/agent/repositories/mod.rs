use anyhow::{Context, Result};
use turso::{Connection, Database};

use super::configure_agent_connection;

mod git_journals;
mod projects_models;
mod sessions_runs;
mod workers_leases;

pub(super) use git_journals::GitJournalsRepository;
pub(super) use projects_models::ProjectsModelsRepository;
pub(super) use sessions_runs::SessionsRunsRepository;
pub(super) use workers_leases::WorkersLeasesRepository;

#[derive(Clone)]
struct RepositoryDatabase(Database);

impl RepositoryDatabase {
    fn new(db: &Database) -> Self {
        Self(db.clone())
    }

    async fn connect(&self) -> Result<Connection> {
        let connection = self
            .0
            .connect()
            .context("Failed to connect to agent database")?;
        configure_agent_connection(&connection).await?;
        Ok(connection)
    }
}

pub(super) struct AgentRepositories {
    pub(super) projects_models: ProjectsModelsRepository,
    pub(super) workers_leases: WorkersLeasesRepository,
    pub(super) sessions_runs: SessionsRunsRepository,
    pub(super) git_journals: GitJournalsRepository,
}

impl AgentRepositories {
    pub(super) fn new(db: &Database) -> Self {
        Self {
            projects_models: ProjectsModelsRepository::new(db),
            workers_leases: WorkersLeasesRepository::new(db),
            sessions_runs: SessionsRunsRepository::new(db),
            git_journals: GitJournalsRepository::new(db),
        }
    }
}
