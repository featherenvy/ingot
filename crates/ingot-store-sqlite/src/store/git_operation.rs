use ingot_domain::git_operation::GitOperation;
use ingot_domain::ports::{GitOperationRepository, RepositoryError};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use super::helpers::{
    db_err, db_write_err, encode_enum, json_err, parse_enum, parse_id, parse_json,
};
use crate::db::Database;

impl Database {
    pub async fn create_git_operation(
        &self,
        operation: &GitOperation,
    ) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO git_operations (
                id, project_id, operation_kind, entity_type, entity_id, workspace_id, ref_name,
                expected_old_oid, new_oid, commit_oid, status, metadata, created_at, completed_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(operation.id.to_string())
        .bind(operation.project_id.to_string())
        .bind(encode_enum(&operation.operation_kind)?)
        .bind(encode_enum(&operation.entity_type)?)
        .bind(&operation.entity_id)
        .bind(operation.workspace_id.map(|id| id.to_string()))
        .bind(operation.ref_name.as_deref())
        .bind(operation.expected_old_oid.as_deref())
        .bind(operation.new_oid.as_deref())
        .bind(operation.commit_oid.as_deref())
        .bind(encode_enum(&operation.status)?)
        .bind(
            operation
                .metadata
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(json_err)?,
        )
        .bind(operation.created_at)
        .bind(operation.completed_at)
        .execute(&self.pool)
        .await
        .map_err(db_write_err)?;

        Ok(())
    }

    pub async fn update_git_operation(
        &self,
        operation: &GitOperation,
    ) -> Result<(), RepositoryError> {
        let result = sqlx::query(
            "UPDATE git_operations
             SET workspace_id = ?, ref_name = ?, expected_old_oid = ?, new_oid = ?, commit_oid = ?,
                 status = ?, metadata = ?, completed_at = ?
             WHERE id = ?",
        )
        .bind(operation.workspace_id.map(|id| id.to_string()))
        .bind(operation.ref_name.as_deref())
        .bind(operation.expected_old_oid.as_deref())
        .bind(operation.new_oid.as_deref())
        .bind(operation.commit_oid.as_deref())
        .bind(encode_enum(&operation.status)?)
        .bind(
            operation
                .metadata
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(json_err)?,
        )
        .bind(operation.completed_at)
        .bind(operation.id.to_string())
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
}

#[allow(dead_code)]
fn map_git_operation(row: &SqliteRow) -> Result<GitOperation, RepositoryError> {
    Ok(GitOperation {
        id: parse_id(row.try_get("id").map_err(db_err)?)?,
        project_id: parse_id(row.try_get("project_id").map_err(db_err)?)?,
        operation_kind: parse_enum(row.try_get("operation_kind").map_err(db_err)?)?,
        entity_type: parse_enum(row.try_get("entity_type").map_err(db_err)?)?,
        entity_id: row.try_get("entity_id").map_err(db_err)?,
        workspace_id: row
            .try_get::<Option<String>, _>("workspace_id")
            .map_err(db_err)?
            .map(parse_id)
            .transpose()?,
        ref_name: row.try_get("ref_name").map_err(db_err)?,
        expected_old_oid: row.try_get("expected_old_oid").map_err(db_err)?,
        new_oid: row.try_get("new_oid").map_err(db_err)?,
        commit_oid: row.try_get("commit_oid").map_err(db_err)?,
        status: parse_enum(row.try_get("status").map_err(db_err)?)?,
        metadata: row
            .try_get::<Option<String>, _>("metadata")
            .map_err(db_err)?
            .map(parse_json)
            .transpose()?,
        created_at: row.try_get("created_at").map_err(db_err)?,
        completed_at: row.try_get("completed_at").map_err(db_err)?,
    })
}
