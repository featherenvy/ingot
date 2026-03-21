use std::path::PathBuf;

use ingot_domain::ids::ProjectId;
use ingot_domain::ports::{ProjectRepository, RepositoryError};
use ingot_domain::project::Project;
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use super::helpers::{db_err, db_write_err};
use crate::db::Database;

impl Database {
    pub async fn list_projects(&self) -> Result<Vec<Project>, RepositoryError> {
        let rows = sqlx::query(
            "SELECT id, name, path, default_branch, color, created_at, updated_at
             FROM projects
             ORDER BY name ASC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;

        rows.iter().map(map_project).collect()
    }

    pub async fn get_project(&self, project_id: ProjectId) -> Result<Project, RepositoryError> {
        let row = sqlx::query(
            "SELECT id, name, path, default_branch, color, created_at, updated_at
             FROM projects
             WHERE id = ?",
        )
        .bind(project_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;

        row.as_ref()
            .map(map_project)
            .transpose()?
            .ok_or(RepositoryError::NotFound)
    }

    pub async fn create_project(&self, project: &Project) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO projects (id, name, path, default_branch, color, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(project.id)
        .bind(&project.name)
        .bind(project.path.to_str().unwrap_or_default())
        .bind(&project.default_branch)
        .bind(&project.color)
        .bind(project.created_at)
        .bind(project.updated_at)
        .execute(&self.pool)
        .await
        .map_err(db_write_err)?;

        Ok(())
    }

    pub async fn update_project(&self, project: &Project) -> Result<(), RepositoryError> {
        let result = sqlx::query(
            "UPDATE projects
             SET name = ?, path = ?, default_branch = ?, color = ?, updated_at = ?
             WHERE id = ?",
        )
        .bind(&project.name)
        .bind(project.path.to_str().unwrap_or_default())
        .bind(&project.default_branch)
        .bind(&project.color)
        .bind(project.updated_at)
        .bind(project.id)
        .execute(&self.pool)
        .await
        .map_err(db_write_err)?;

        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound);
        }

        Ok(())
    }

    pub async fn delete_project(&self, project_id: ProjectId) -> Result<(), RepositoryError> {
        let result = sqlx::query("DELETE FROM projects WHERE id = ?")
            .bind(project_id)
            .execute(&self.pool)
            .await
            .map_err(db_write_err)?;

        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound);
        }

        Ok(())
    }
}

impl ProjectRepository for Database {
    async fn list(&self) -> Result<Vec<Project>, RepositoryError> {
        self.list_projects().await
    }
    async fn get(&self, id: ProjectId) -> Result<Project, RepositoryError> {
        self.get_project(id).await
    }
    async fn create(&self, project: &Project) -> Result<(), RepositoryError> {
        self.create_project(project).await
    }
    async fn update(&self, project: &Project) -> Result<(), RepositoryError> {
        self.update_project(project).await
    }
    async fn delete(&self, id: ProjectId) -> Result<(), RepositoryError> {
        self.delete_project(id).await
    }
}

fn map_project(row: &SqliteRow) -> Result<Project, RepositoryError> {
    Ok(Project {
        id: row.try_get("id").map_err(db_err)?,
        name: row.try_get("name").map_err(db_err)?,
        path: PathBuf::from(row.try_get::<String, _>("path").map_err(db_err)?),
        default_branch: row.try_get("default_branch").map_err(db_err)?,
        color: row.try_get("color").map_err(db_err)?,
        created_at: row.try_get("created_at").map_err(db_err)?,
        updated_at: row.try_get("updated_at").map_err(db_err)?,
    })
}
