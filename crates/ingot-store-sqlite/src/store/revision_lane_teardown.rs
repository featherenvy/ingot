use chrono::Utc;
use ingot_domain::git_operation::GitOperationWire;
use ingot_domain::ports::{
    ConflictKind, RepositoryError, RevisionLaneTeardownMutation, RevisionLaneTeardownRepository,
};

use super::helpers::{db_err, db_write_err, item_revision_is_stale, json_err};
use crate::db::Database;

impl Database {
    pub async fn apply_revision_lane_teardown(
        &self,
        mutation: RevisionLaneTeardownMutation,
    ) -> Result<(), RepositoryError> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;

        // 1. Job cancellations
        for cancellation in &mutation.job_cancellations {
            let params = &cancellation.params;
            let result = sqlx::query(
                "UPDATE jobs
                 SET status = ?,
                     outcome_class = ?,
                     result_schema_version = NULL,
                     result_payload = NULL,
                     output_commit_oid = NULL,
                     error_code = ?,
                     error_message = ?,
                     ended_at = ?
                 WHERE id = ?
                   AND status IN ('queued', 'assigned', 'running')
                   AND EXISTS (
                       SELECT 1
                       FROM items
                       WHERE id = ?
                         AND current_revision_id = ?
                   )",
            )
            .bind(params.status)
            .bind(params.outcome_class)
            .bind(params.error_code.as_deref())
            .bind(params.error_message.as_deref())
            .bind(Utc::now())
            .bind(params.job_id)
            .bind(params.item_id)
            .bind(params.expected_item_revision_id)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;

            if result.rows_affected() != 1 {
                if item_revision_is_stale(&mut tx, params.item_id, params.expected_item_revision_id)
                    .await?
                {
                    return Err(RepositoryError::Conflict(ConflictKind::JobRevisionStale));
                }

                let job_is_active: Option<String> = sqlx::query_scalar(
                    "SELECT id FROM jobs WHERE id = ? AND status IN ('queued', 'assigned', 'running')",
                )
                .bind(params.job_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_err)?;

                if job_is_active.is_none() {
                    return Err(RepositoryError::Conflict(ConflictKind::JobNotActive));
                }

                return Err(RepositoryError::Conflict(ConflictKind::JobUpdateConflict));
            }

            // Update workspace if present
            if let Some(workspace) = &cancellation.workspace_update {
                sqlx::query(
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
            }

            // Insert activity
            let activity = &cancellation.activity;
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
        }

        // 2. Convergence updates
        for convergence in &mutation.convergence_updates {
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
        }

        // 3. Workspace abandonments
        for workspace in &mutation.workspace_abandonments {
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

        // 4. Queue entry update
        if let Some(queue_entry) = &mutation.queue_entry_update {
            let result = sqlx::query(
                "UPDATE convergence_queue_entries
                 SET status = ?, head_acquired_at = ?, updated_at = ?, released_at = ?
                 WHERE id = ?",
            )
            .bind(queue_entry.status)
            .bind(queue_entry.head_acquired_at)
            .bind(queue_entry.updated_at)
            .bind(queue_entry.released_at)
            .bind(queue_entry.id)
            .execute(&mut *tx)
            .await
            .map_err(db_write_err)?;

            if result.rows_affected() == 0 {
                return Err(RepositoryError::NotFound);
            }
        }

        // 5. Git operation updates
        for operation in &mutation.git_operation_updates {
            let wire = GitOperationWire::from(operation);
            let result = sqlx::query(
                "UPDATE git_operations
                 SET workspace_id = ?, ref_name = ?, expected_old_oid = ?, new_oid = ?,
                     commit_oid = ?, status = ?, metadata = ?, completed_at = ?
                 WHERE id = ?",
            )
            .bind(wire.workspace_id)
            .bind(wire.ref_name.clone())
            .bind(wire.expected_old_oid.clone())
            .bind(wire.new_oid.clone())
            .bind(wire.commit_oid.clone())
            .bind(wire.status)
            .bind(
                wire.metadata
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()
                    .map_err(json_err)?,
            )
            .bind(wire.completed_at)
            .bind(wire.id)
            .execute(&mut *tx)
            .await
            .map_err(db_write_err)?;

            if result.rows_affected() == 0 {
                return Err(RepositoryError::NotFound);
            }
        }

        // 6. Git operation activities
        for activity in &mutation.git_operation_activities {
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
        }

        tx.commit().await.map_err(db_err)?;
        Ok(())
    }
}

impl RevisionLaneTeardownRepository for Database {
    async fn apply_revision_lane_teardown(
        &self,
        mutation: RevisionLaneTeardownMutation,
    ) -> Result<(), RepositoryError> {
        Database::apply_revision_lane_teardown(self, mutation).await
    }
}
