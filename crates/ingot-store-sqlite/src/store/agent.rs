use std::path::PathBuf;

use ingot_domain::agent::Agent;
use ingot_domain::ids::AgentId;
use ingot_domain::ports::{AgentRepository, RepositoryError};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use super::helpers::{db_err, db_write_err, json_err, parse_json};
use crate::db::Database;

impl Database {
    pub async fn list_agents(&self) -> Result<Vec<Agent>, RepositoryError> {
        let rows = sqlx::query(
            "SELECT id, slug, name, adapter_kind, provider, model, cli_path, capabilities,
                    health_check, status
             FROM agents
             ORDER BY slug ASC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;

        rows.iter().map(map_agent).collect()
    }

    pub async fn get_agent(&self, agent_id: AgentId) -> Result<Agent, RepositoryError> {
        let row = sqlx::query(
            "SELECT id, slug, name, adapter_kind, provider, model, cli_path, capabilities,
                    health_check, status
             FROM agents
             WHERE id = ?",
        )
        .bind(agent_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;

        row.as_ref()
            .map(map_agent)
            .transpose()?
            .ok_or(RepositoryError::NotFound)
    }

    pub async fn create_agent(&self, agent: &Agent) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO agents (
                id, slug, name, adapter_kind, provider, model, cli_path, capabilities,
                health_check, status
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(agent.id)
        .bind(&agent.slug)
        .bind(&agent.name)
        .bind(agent.adapter_kind)
        .bind(&agent.provider)
        .bind(&agent.model)
        .bind(agent.cli_path.to_string_lossy().as_ref())
        .bind(serde_json::to_string(&agent.capabilities).map_err(json_err)?)
        .bind(agent.health_check.as_deref())
        .bind(agent.status)
        .execute(&self.pool)
        .await
        .map_err(db_write_err)?;

        Ok(())
    }

    pub async fn update_agent(&self, agent: &Agent) -> Result<(), RepositoryError> {
        let result = sqlx::query(
            "UPDATE agents
             SET slug = ?, name = ?, adapter_kind = ?, provider = ?, model = ?, cli_path = ?,
                 capabilities = ?, health_check = ?, status = ?
             WHERE id = ?",
        )
        .bind(&agent.slug)
        .bind(&agent.name)
        .bind(agent.adapter_kind)
        .bind(&agent.provider)
        .bind(&agent.model)
        .bind(agent.cli_path.to_string_lossy().as_ref())
        .bind(serde_json::to_string(&agent.capabilities).map_err(json_err)?)
        .bind(agent.health_check.as_deref())
        .bind(agent.status)
        .bind(agent.id)
        .execute(&self.pool)
        .await
        .map_err(db_write_err)?;

        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound);
        }

        Ok(())
    }

    pub async fn delete_agent(&self, agent_id: AgentId) -> Result<(), RepositoryError> {
        let result = sqlx::query("DELETE FROM agents WHERE id = ?")
            .bind(agent_id)
            .execute(&self.pool)
            .await
            .map_err(db_write_err)?;

        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound);
        }

        Ok(())
    }
}

impl AgentRepository for Database {
    async fn list(&self) -> Result<Vec<Agent>, RepositoryError> {
        self.list_agents().await
    }
    async fn get(&self, id: AgentId) -> Result<Agent, RepositoryError> {
        self.get_agent(id).await
    }
    async fn create(&self, agent: &Agent) -> Result<(), RepositoryError> {
        self.create_agent(agent).await
    }
    async fn update(&self, agent: &Agent) -> Result<(), RepositoryError> {
        self.update_agent(agent).await
    }
    async fn delete(&self, id: AgentId) -> Result<(), RepositoryError> {
        self.delete_agent(id).await
    }
}

fn map_agent(row: &SqliteRow) -> Result<Agent, RepositoryError> {
    Ok(Agent {
        id: row.try_get("id").map_err(db_err)?,
        slug: row.try_get("slug").map_err(db_err)?,
        name: row.try_get("name").map_err(db_err)?,
        adapter_kind: row.try_get("adapter_kind").map_err(db_err)?,
        provider: row.try_get("provider").map_err(db_err)?,
        model: row.try_get("model").map_err(db_err)?,
        cli_path: PathBuf::from(row.try_get::<String, _>("cli_path").map_err(db_err)?),
        capabilities: parse_json(row.try_get("capabilities").map_err(db_err)?)?,
        health_check: row.try_get("health_check").map_err(db_err)?,
        status: row.try_get("status").map_err(db_err)?,
    })
}
