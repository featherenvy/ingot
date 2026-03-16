use ingot_domain::ids::{ItemId, ItemRevisionId, ProjectId, WorkspaceId};
use ingot_domain::ports::{RepositoryError, WorkspaceRepository};
use ingot_domain::workspace::{Workspace, WorkspaceCommitState, WorkspaceState, WorkspaceStatus};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use super::helpers::{db_err, db_write_err, encode_enum, parse_enum, parse_id};
use crate::db::Database;

impl Database {
    pub async fn get_workspace(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Workspace, RepositoryError> {
        let row = sqlx::query("SELECT * FROM workspaces WHERE id = ?")
            .bind(workspace_id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;

        row.as_ref()
            .map(map_workspace)
            .transpose()?
            .ok_or(RepositoryError::NotFound)
    }

    pub async fn create_workspace(&self, workspace: &Workspace) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO workspaces (
                id, project_id, kind, strategy, path, created_for_revision_id, parent_workspace_id,
                target_ref, workspace_ref, base_commit_oid, head_commit_oid, retention_policy,
                status, current_job_id, created_at, updated_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(workspace.id.to_string())
        .bind(workspace.project_id.to_string())
        .bind(encode_enum(&workspace.kind)?)
        .bind(encode_enum(&workspace.strategy)?)
        .bind(&workspace.path)
        .bind(workspace.created_for_revision_id.map(|id| id.to_string()))
        .bind(workspace.parent_workspace_id.map(|id| id.to_string()))
        .bind(workspace.target_ref.as_deref())
        .bind(workspace.workspace_ref.as_deref())
        .bind(workspace.state.base_commit_oid())
        .bind(workspace.state.head_commit_oid())
        .bind(encode_enum(&workspace.retention_policy)?)
        .bind(encode_enum(&workspace.state.status())?)
        .bind(workspace.state.current_job_id().map(|id| id.to_string()))
        .bind(workspace.created_at)
        .bind(workspace.updated_at)
        .execute(&self.pool)
        .await
        .map_err(db_write_err)?;

        Ok(())
    }

    pub async fn update_workspace(&self, workspace: &Workspace) -> Result<(), RepositoryError> {
        let result = sqlx::query(
            "UPDATE workspaces
             SET path = ?, target_ref = ?, workspace_ref = ?, base_commit_oid = ?, head_commit_oid = ?,
                 retention_policy = ?, status = ?, current_job_id = ?, updated_at = ?
             WHERE id = ?",
        )
        .bind(&workspace.path)
        .bind(workspace.target_ref.as_deref())
        .bind(workspace.workspace_ref.as_deref())
        .bind(workspace.state.base_commit_oid())
        .bind(workspace.state.head_commit_oid())
        .bind(encode_enum(&workspace.retention_policy)?)
        .bind(encode_enum(&workspace.state.status())?)
        .bind(workspace.state.current_job_id().map(|id| id.to_string()))
        .bind(workspace.updated_at)
        .bind(workspace.id.to_string())
        .execute(&self.pool)
        .await
        .map_err(db_write_err)?;

        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound);
        }

        Ok(())
    }

    pub async fn find_authoring_workspace_for_revision(
        &self,
        revision_id: ItemRevisionId,
    ) -> Result<Option<Workspace>, RepositoryError> {
        let row = sqlx::query(
            "SELECT *
             FROM workspaces
             WHERE created_for_revision_id = ?
               AND kind = 'authoring'
             ORDER BY created_at DESC
             LIMIT 1",
        )
        .bind(revision_id.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;

        row.as_ref().map(map_workspace).transpose()
    }

    pub async fn list_workspaces_by_item(
        &self,
        item_id: ItemId,
    ) -> Result<Vec<Workspace>, RepositoryError> {
        let rows = sqlx::query(
            "SELECT w.*
             FROM workspaces w
             JOIN item_revisions r ON r.id = w.created_for_revision_id
             WHERE r.item_id = ?
             ORDER BY w.created_at DESC",
        )
        .bind(item_id.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;

        rows.iter().map(map_workspace).collect()
    }

    pub async fn list_workspaces_by_project(
        &self,
        project_id: ProjectId,
    ) -> Result<Vec<Workspace>, RepositoryError> {
        let rows = sqlx::query(
            "SELECT *
             FROM workspaces
             WHERE project_id = ?
             ORDER BY created_at DESC",
        )
        .bind(project_id.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;

        rows.iter().map(map_workspace).collect()
    }
}

impl WorkspaceRepository for Database {
    async fn list_by_project(
        &self,
        project_id: ProjectId,
    ) -> Result<Vec<Workspace>, RepositoryError> {
        self.list_workspaces_by_project(project_id).await
    }
    async fn get(&self, id: WorkspaceId) -> Result<Workspace, RepositoryError> {
        self.get_workspace(id).await
    }
    async fn create(&self, workspace: &Workspace) -> Result<(), RepositoryError> {
        self.create_workspace(workspace).await
    }
    async fn update(&self, workspace: &Workspace) -> Result<(), RepositoryError> {
        self.update_workspace(workspace).await
    }
    async fn find_authoring_for_revision(
        &self,
        revision_id: ItemRevisionId,
    ) -> Result<Option<Workspace>, RepositoryError> {
        self.find_authoring_workspace_for_revision(revision_id)
            .await
    }
    async fn list_by_item(&self, item_id: ItemId) -> Result<Vec<Workspace>, RepositoryError> {
        self.list_workspaces_by_item(item_id).await
    }
}

fn map_workspace(row: &SqliteRow) -> Result<Workspace, RepositoryError> {
    let status: WorkspaceStatus = parse_enum(row.try_get("status").map_err(db_err)?)?;
    let current_job_id = row
        .try_get::<Option<String>, _>("current_job_id")
        .map_err(db_err)?
        .map(parse_id)
        .transpose()?;
    let state = WorkspaceState::from_parts(
        status,
        WorkspaceCommitState::from_option_parts(
            row.try_get("base_commit_oid").map_err(db_err)?,
            row.try_get("head_commit_oid").map_err(db_err)?,
        ),
        current_job_id,
    )
    .map_err(|error| db_err(std::io::Error::other(error)))?;

    Ok(Workspace {
        id: parse_id(row.try_get("id").map_err(db_err)?)?,
        project_id: parse_id(row.try_get("project_id").map_err(db_err)?)?,
        kind: parse_enum(row.try_get("kind").map_err(db_err)?)?,
        strategy: parse_enum(row.try_get("strategy").map_err(db_err)?)?,
        path: row.try_get("path").map_err(db_err)?,
        created_for_revision_id: row
            .try_get::<Option<String>, _>("created_for_revision_id")
            .map_err(db_err)?
            .map(parse_id)
            .transpose()?,
        parent_workspace_id: row
            .try_get::<Option<String>, _>("parent_workspace_id")
            .map_err(db_err)?
            .map(parse_id)
            .transpose()?,
        target_ref: row.try_get("target_ref").map_err(db_err)?,
        workspace_ref: row.try_get("workspace_ref").map_err(db_err)?,
        retention_policy: parse_enum(row.try_get("retention_policy").map_err(db_err)?)?,
        created_at: row.try_get("created_at").map_err(db_err)?,
        updated_at: row.try_get("updated_at").map_err(db_err)?,
        state,
    })
}
