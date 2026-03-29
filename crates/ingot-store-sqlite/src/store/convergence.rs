use ingot_domain::commit_oid::CommitOid;
use ingot_domain::convergence::{
    Convergence, ConvergenceState, ConvergenceStateParts, ConvergenceStatus,
};
use ingot_domain::ids::{ConvergenceId, ItemId, ItemRevisionId};
use ingot_domain::ports::{ConvergenceRepository, RepositoryError};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use super::helpers::{db_err, db_write_err};
use crate::db::Database;

impl Database {
    pub async fn list_convergences_by_item(
        &self,
        item_id: ItemId,
    ) -> Result<Vec<Convergence>, RepositoryError> {
        let rows =
            sqlx::query("SELECT * FROM convergences WHERE item_id = ? ORDER BY created_at DESC")
                .bind(item_id)
                .fetch_all(&self.pool)
                .await
                .map_err(db_err)?;

        rows.iter().map(map_convergence).collect()
    }

    pub async fn get_convergence(
        &self,
        convergence_id: ConvergenceId,
    ) -> Result<Convergence, RepositoryError> {
        let row = sqlx::query("SELECT * FROM convergences WHERE id = ?")
            .bind(convergence_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;

        row.as_ref()
            .map(map_convergence)
            .transpose()?
            .ok_or(RepositoryError::NotFound)
    }

    pub async fn list_active_convergences(&self) -> Result<Vec<Convergence>, RepositoryError> {
        let rows = sqlx::query(
            "SELECT *
             FROM convergences
             WHERE status IN ('queued', 'running')
             ORDER BY created_at ASC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;

        rows.iter().map(map_convergence).collect()
    }

    pub async fn create_convergence(
        &self,
        convergence: &Convergence,
    ) -> Result<(), RepositoryError> {
        let state = &convergence.state;

        sqlx::query(
            "INSERT INTO convergences (
                id, project_id, item_id, item_revision_id, source_workspace_id, integration_workspace_id,
                source_head_commit_oid, target_ref, strategy, status, input_target_commit_oid,
                prepared_commit_oid, final_target_commit_oid, conflict_summary, created_at, completed_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(convergence.id)
        .bind(convergence.project_id)
        .bind(convergence.item_id)
        .bind(convergence.item_revision_id)
        .bind(convergence.source_workspace_id)
        .bind(state.integration_workspace_id())
        .bind(convergence.source_head_commit_oid.clone())
        .bind(&convergence.target_ref)
        .bind(convergence.strategy)
        .bind(state.status())
        .bind(state.input_target_commit_oid().cloned())
        .bind(state.prepared_commit_oid().cloned())
        .bind(state.final_target_commit_oid().cloned())
        .bind(state.conflict_summary())
        .bind(convergence.created_at)
        .bind(state.completed_at())
        .execute(&self.pool)
        .await
        .map_err(db_write_err)?;

        Ok(())
    }

    pub async fn update_convergence(
        &self,
        convergence: &Convergence,
    ) -> Result<(), RepositoryError> {
        let state = &convergence.state;

        let result = sqlx::query(
            "UPDATE convergences
             SET integration_workspace_id = ?, source_head_commit_oid = ?, target_ref = ?, strategy = ?,
                 status = ?, input_target_commit_oid = ?, prepared_commit_oid = ?, final_target_commit_oid = ?,
                 conflict_summary = ?, completed_at = ?
             WHERE id = ?",
        )
        .bind(state.integration_workspace_id())
        .bind(convergence.source_head_commit_oid.clone())
        .bind(&convergence.target_ref)
        .bind(convergence.strategy)
        .bind(state.status())
        .bind(state.input_target_commit_oid().cloned())
        .bind(state.prepared_commit_oid().cloned())
        .bind(state.final_target_commit_oid().cloned())
        .bind(state.conflict_summary())
        .bind(state.completed_at())
        .bind(convergence.id)
        .execute(&self.pool)
        .await
        .map_err(db_write_err)?;

        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound);
        }

        Ok(())
    }

    pub async fn list_convergences_by_revision(
        &self,
        revision_id: ItemRevisionId,
    ) -> Result<Vec<Convergence>, RepositoryError> {
        let rows = sqlx::query(
            "SELECT * FROM convergences WHERE item_revision_id = ? ORDER BY created_at DESC",
        )
        .bind(revision_id)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;

        rows.iter().map(map_convergence).collect()
    }

    pub async fn find_active_convergence_for_revision(
        &self,
        revision_id: ItemRevisionId,
    ) -> Result<Option<Convergence>, RepositoryError> {
        let row = sqlx::query(
            "SELECT *
             FROM convergences
             WHERE item_revision_id = ?
               AND status IN ('queued', 'running')
             ORDER BY created_at DESC
             LIMIT 1",
        )
        .bind(revision_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;

        row.as_ref().map(map_convergence).transpose()
    }

    pub async fn find_prepared_convergence_for_revision(
        &self,
        revision_id: ItemRevisionId,
    ) -> Result<Option<Convergence>, RepositoryError> {
        let row = sqlx::query(
            "SELECT *
             FROM convergences
             WHERE item_revision_id = ?
               AND status = 'prepared'
             ORDER BY created_at DESC
             LIMIT 1",
        )
        .bind(revision_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;

        row.as_ref().map(map_convergence).transpose()
    }
}

impl ConvergenceRepository for Database {
    async fn list_by_revision(
        &self,
        revision_id: ItemRevisionId,
    ) -> Result<Vec<Convergence>, RepositoryError> {
        self.list_convergences_by_revision(revision_id).await
    }
    async fn get(&self, id: ConvergenceId) -> Result<Convergence, RepositoryError> {
        self.get_convergence(id).await
    }
    async fn create(&self, convergence: &Convergence) -> Result<(), RepositoryError> {
        self.create_convergence(convergence).await
    }
    async fn update(&self, convergence: &Convergence) -> Result<(), RepositoryError> {
        self.update_convergence(convergence).await
    }
    async fn find_active_for_revision(
        &self,
        revision_id: ItemRevisionId,
    ) -> Result<Option<Convergence>, RepositoryError> {
        self.find_active_convergence_for_revision(revision_id).await
    }
    async fn find_prepared_for_revision(
        &self,
        revision_id: ItemRevisionId,
    ) -> Result<Option<Convergence>, RepositoryError> {
        self.find_prepared_convergence_for_revision(revision_id)
            .await
    }
    async fn list_by_item(&self, item_id: ItemId) -> Result<Vec<Convergence>, RepositoryError> {
        self.list_convergences_by_item(item_id).await
    }
    async fn list_active(&self) -> Result<Vec<Convergence>, RepositoryError> {
        self.list_active_convergences().await
    }
}

fn map_convergence(row: &SqliteRow) -> Result<Convergence, RepositoryError> {
    let status: ConvergenceStatus = row.try_get("status").map_err(db_err)?;

    let integration_workspace_id: Option<ingot_domain::ids::WorkspaceId> =
        row.try_get("integration_workspace_id").map_err(db_err)?;
    let input_target_commit_oid: Option<CommitOid> =
        row.try_get("input_target_commit_oid").map_err(db_err)?;
    let prepared_commit_oid: Option<CommitOid> =
        row.try_get("prepared_commit_oid").map_err(db_err)?;
    let final_target_commit_oid: Option<CommitOid> =
        row.try_get("final_target_commit_oid").map_err(db_err)?;
    let conflict_summary: Option<String> = row.try_get("conflict_summary").map_err(db_err)?;
    let completed_at: Option<chrono::DateTime<chrono::Utc>> =
        row.try_get("completed_at").map_err(db_err)?;

    let state = ConvergenceState::from_parts(
        status,
        ConvergenceStateParts {
            integration_workspace_id,
            input_target_commit_oid,
            prepared_commit_oid,
            final_target_commit_oid,
            conflict_summary,
            completed_at,
        },
    )
    .map_err(|error| RepositoryError::Database(error.into()))?;

    Ok(Convergence {
        id: row.try_get("id").map_err(db_err)?,
        project_id: row.try_get("project_id").map_err(db_err)?,
        item_id: row.try_get("item_id").map_err(db_err)?,
        item_revision_id: row.try_get("item_revision_id").map_err(db_err)?,
        source_workspace_id: row.try_get("source_workspace_id").map_err(db_err)?,
        source_head_commit_oid: row.try_get("source_head_commit_oid").map_err(db_err)?,
        target_ref: row.try_get("target_ref").map_err(db_err)?,
        strategy: row.try_get("strategy").map_err(db_err)?,
        target_head_valid: None,
        created_at: row.try_get("created_at").map_err(db_err)?,
        state,
    })
}
