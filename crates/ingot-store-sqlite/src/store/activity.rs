use ingot_domain::activity::Activity;
use ingot_domain::ids::ProjectId;
use ingot_domain::ports::{ActivityRepository, RepositoryError};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use super::helpers::{
    db_err, db_write_err, encode_enum, json_err, parse_enum, parse_id, parse_json,
};
use crate::db::Database;

impl Database {
    pub async fn append_activity(&self, activity: &Activity) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO activity (
                id, project_id, event_type, entity_type, entity_id, payload, created_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(activity.id.to_string())
        .bind(activity.project_id.to_string())
        .bind(encode_enum(&activity.event_type)?)
        .bind(&activity.entity_type)
        .bind(&activity.entity_id)
        .bind(serde_json::to_string(&activity.payload).map_err(json_err)?)
        .bind(activity.created_at)
        .execute(&self.pool)
        .await
        .map_err(db_write_err)?;

        Ok(())
    }

    pub async fn list_activity_by_project(
        &self,
        project_id: ProjectId,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<Activity>, RepositoryError> {
        let rows = sqlx::query(
            "SELECT id, project_id, event_type, entity_type, entity_id, payload, created_at
             FROM activity
             WHERE project_id = ?
             ORDER BY created_at DESC
             LIMIT ? OFFSET ?",
        )
        .bind(project_id.to_string())
        .bind(limit as i64)
        .bind(offset as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;

        rows.iter().map(map_activity).collect()
    }
}

impl ActivityRepository for Database {
    async fn append(&self, activity: &Activity) -> Result<(), RepositoryError> {
        self.append_activity(activity).await
    }
    async fn list_by_project(
        &self,
        project_id: ProjectId,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<Activity>, RepositoryError> {
        self.list_activity_by_project(project_id, limit, offset)
            .await
    }
}

fn map_activity(row: &SqliteRow) -> Result<Activity, RepositoryError> {
    Ok(Activity {
        id: parse_id(row.try_get("id").map_err(db_err)?)?,
        project_id: parse_id(row.try_get("project_id").map_err(db_err)?)?,
        event_type: parse_enum(row.try_get("event_type").map_err(db_err)?)?,
        entity_type: row.try_get("entity_type").map_err(db_err)?,
        entity_id: row.try_get("entity_id").map_err(db_err)?,
        payload: parse_json(row.try_get("payload").map_err(db_err)?)?,
        created_at: row.try_get("created_at").map_err(db_err)?,
    })
}
