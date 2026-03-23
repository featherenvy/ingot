use ingot_domain::ports::{
    InvalidatePreparedConvergenceMutation, InvalidatePreparedConvergenceRepository, RepositoryError,
};

use super::helpers::{db_err, db_write_err, json_err};
use crate::db::Database;

impl Database {
    pub async fn apply_invalidate_prepared_convergence(
        &self,
        mutation: InvalidatePreparedConvergenceMutation,
    ) -> Result<(), RepositoryError> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;

        // 1. Update convergence (mark as failed)
        let convergence = &mutation.convergence;
        let state = &convergence.state;
        let result = sqlx::query(
            "UPDATE convergences
             SET integration_workspace_id = ?, source_head_commit_oid = ?, target_ref = ?,
                 strategy = ?, status = ?, input_target_commit_oid = ?,
                 prepared_commit_oid = ?, final_target_commit_oid = ?,
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
        .execute(&mut *tx)
        .await
        .map_err(db_write_err)?;

        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound);
        }

        // 2. Update workspace (mark as stale) if present
        if let Some(workspace) = &mutation.workspace_update {
            let result = sqlx::query(
                "UPDATE workspaces
                 SET path = ?, target_ref = ?, workspace_ref = ?, base_commit_oid = ?,
                     head_commit_oid = ?, retention_policy = ?, status = ?,
                     current_job_id = ?, updated_at = ?
                 WHERE id = ?",
            )
            .bind(workspace.path.to_string_lossy().as_ref())
            .bind(workspace.target_ref.clone())
            .bind(workspace.workspace_ref.clone())
            .bind(workspace.state.base_commit_oid().cloned())
            .bind(workspace.state.head_commit_oid().cloned())
            .bind(workspace.retention_policy)
            .bind(workspace.state.status())
            .bind(workspace.state.current_job_id())
            .bind(workspace.updated_at)
            .bind(workspace.id)
            .execute(&mut *tx)
            .await
            .map_err(db_write_err)?;

            if result.rows_affected() == 0 {
                return Err(RepositoryError::NotFound);
            }
        }

        // 3. Update item (reset approval state)
        let item = &mutation.item;
        let result = sqlx::query(
            "UPDATE items
             SET approval_state = ?, updated_at = ?
             WHERE id = ?",
        )
        .bind(item.approval_state)
        .bind(item.updated_at)
        .bind(item.id)
        .execute(&mut *tx)
        .await
        .map_err(db_write_err)?;

        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound);
        }

        // 4. Append activity
        let activity = &mutation.activity;
        sqlx::query(
            "INSERT INTO activity (
                id, project_id, event_type, entity_type, entity_id, payload, created_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(activity.id)
        .bind(activity.project_id)
        .bind(activity.event_type)
        .bind(activity.subject.entity_type())
        .bind(activity.subject.entity_id_string())
        .bind(serde_json::to_string(&activity.payload).map_err(json_err)?)
        .bind(activity.created_at)
        .execute(&mut *tx)
        .await
        .map_err(db_write_err)?;

        tx.commit().await.map_err(db_err)?;
        Ok(())
    }
}

impl InvalidatePreparedConvergenceRepository for Database {
    async fn apply_invalidate_prepared_convergence(
        &self,
        mutation: InvalidatePreparedConvergenceMutation,
    ) -> Result<(), RepositoryError> {
        Database::apply_invalidate_prepared_convergence(self, mutation).await
    }
}
