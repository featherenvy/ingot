use ingot_domain::git_operation::{GitOperation, GitOperationWire};
use ingot_domain::ids::ConvergenceId;
use ingot_domain::ports::{ConflictKind, GitOperationRepository, RepositoryError};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use super::helpers::{db_err, db_write_err, json_err, parse_json};
use crate::db::Database;

impl Database {
    pub async fn create_git_operation(
        &self,
        operation: &GitOperation,
    ) -> Result<(), RepositoryError> {
        let wire = GitOperationWire::from(operation);
        sqlx::query(
            "INSERT INTO git_operations (
                id, project_id, operation_kind, entity_type, entity_id, workspace_id, ref_name,
                expected_old_oid, new_oid, commit_oid, status, metadata, created_at, completed_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(wire.id)
        .bind(wire.project_id)
        .bind(wire.operation_kind)
        .bind(wire.entity_type)
        .bind(&wire.entity_id)
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
        .bind(wire.created_at)
        .bind(wire.completed_at)
        .execute(&self.pool)
        .await
        .map_err(db_write_err)?;

        Ok(())
    }

    pub async fn update_git_operation(
        &self,
        operation: &GitOperation,
    ) -> Result<(), RepositoryError> {
        let wire = GitOperationWire::from(operation);
        let result = sqlx::query(
            "UPDATE git_operations
             SET workspace_id = ?, ref_name = ?, expected_old_oid = ?, new_oid = ?, commit_oid = ?,
                 status = ?, metadata = ?, completed_at = ?
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
        .execute(&self.pool)
        .await
        .map_err(db_write_err)?;

        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound);
        }

        Ok(())
    }

    pub async fn list_unresolved_git_operations(
        &self,
    ) -> Result<Vec<GitOperation>, RepositoryError> {
        let rows = sqlx::query(
            "SELECT id, project_id, operation_kind, entity_type, entity_id, workspace_id, ref_name,
                    expected_old_oid, new_oid, commit_oid, status, metadata, created_at, completed_at
             FROM git_operations
             WHERE status IN ('planned', 'applied')
             ORDER BY created_at ASC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;

        rows.iter().map(map_git_operation).collect()
    }

    pub async fn find_unresolved_finalize_for_convergence(
        &self,
        convergence_id: ConvergenceId,
    ) -> Result<Option<GitOperation>, RepositoryError> {
        let row = sqlx::query(
            "SELECT id, project_id, operation_kind, entity_type, entity_id, workspace_id, ref_name,
                    expected_old_oid, new_oid, commit_oid, status, metadata, created_at, completed_at
             FROM git_operations
             WHERE operation_kind = 'finalize_target_ref'
               AND entity_type = 'convergence'
               AND entity_id = ?
               AND status IN ('planned', 'applied')
             ORDER BY created_at ASC
             LIMIT 1",
        )
        .bind(convergence_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;

        row.as_ref().map(map_git_operation).transpose()
    }
}

impl GitOperationRepository for Database {
    async fn create(&self, operation: &GitOperation) -> Result<(), RepositoryError> {
        self.create_git_operation(operation).await
    }
    async fn update(&self, operation: &GitOperation) -> Result<(), RepositoryError> {
        self.update_git_operation(operation).await
    }
    async fn find_unresolved(&self) -> Result<Vec<GitOperation>, RepositoryError> {
        self.list_unresolved_git_operations().await
    }
    async fn find_unresolved_finalize_for_convergence(
        &self,
        convergence_id: ConvergenceId,
    ) -> Result<Option<GitOperation>, RepositoryError> {
        Database::find_unresolved_finalize_for_convergence(self, convergence_id).await
    }
}

fn map_git_operation(row: &SqliteRow) -> Result<GitOperation, RepositoryError> {
    let wire = GitOperationWire {
        id: row.try_get("id").map_err(db_err)?,
        project_id: row.try_get("project_id").map_err(db_err)?,
        operation_kind: row.try_get("operation_kind").map_err(db_err)?,
        entity_type: row.try_get("entity_type").map_err(db_err)?,
        entity_id: row.try_get("entity_id").map_err(db_err)?,
        workspace_id: row.try_get("workspace_id").map_err(db_err)?,
        ref_name: row.try_get("ref_name").map_err(db_err)?,
        expected_old_oid: row.try_get("expected_old_oid").map_err(db_err)?,
        new_oid: row.try_get("new_oid").map_err(db_err)?,
        commit_oid: row.try_get("commit_oid").map_err(db_err)?,
        status: row.try_get("status").map_err(db_err)?,
        metadata: row
            .try_get::<Option<String>, _>("metadata")
            .map_err(db_err)?
            .map(parse_json)
            .transpose()?,
        created_at: row.try_get("created_at").map_err(db_err)?,
        completed_at: row.try_get("completed_at").map_err(db_err)?,
    };
    GitOperation::try_from(wire).map_err(|e| {
        RepositoryError::Conflict(ConflictKind::Other(format!("invalid git operation: {e}")))
    })
}
