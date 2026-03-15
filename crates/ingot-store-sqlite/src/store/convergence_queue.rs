use ingot_domain::convergence_queue::ConvergenceQueueEntry;
use ingot_domain::ids::{ConvergenceQueueEntryId, ItemId, ItemRevisionId, ProjectId};
use ingot_domain::ports::{ConvergenceQueueRepository, RepositoryError};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use super::helpers::{db_err, db_write_err, encode_enum, parse_enum, parse_id};
use crate::db::Database;

impl Database {
    pub async fn list_queue_entries_by_item(
        &self,
        item_id: ItemId,
    ) -> Result<Vec<ConvergenceQueueEntry>, RepositoryError> {
        let rows = sqlx::query(
            "SELECT *
             FROM convergence_queue_entries
             WHERE item_id = ?
             ORDER BY created_at ASC, id ASC",
        )
        .bind(item_id.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;

        rows.iter().map(map_convergence_queue_entry).collect()
    }

    pub async fn get_queue_entry(
        &self,
        queue_entry_id: ConvergenceQueueEntryId,
    ) -> Result<ConvergenceQueueEntry, RepositoryError> {
        let row = sqlx::query("SELECT * FROM convergence_queue_entries WHERE id = ?")
            .bind(queue_entry_id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;

        row.as_ref()
            .map(map_convergence_queue_entry)
            .transpose()?
            .ok_or(RepositoryError::NotFound)
    }

    pub async fn find_active_queue_entry_for_revision(
        &self,
        revision_id: ItemRevisionId,
    ) -> Result<Option<ConvergenceQueueEntry>, RepositoryError> {
        let row = sqlx::query(
            "SELECT *
             FROM convergence_queue_entries
             WHERE item_revision_id = ?
               AND status IN ('queued', 'head')
             ORDER BY created_at ASC, id ASC
             LIMIT 1",
        )
        .bind(revision_id.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;

        row.as_ref().map(map_convergence_queue_entry).transpose()
    }

    pub async fn find_queue_head(
        &self,
        project_id: ProjectId,
        target_ref: &str,
    ) -> Result<Option<ConvergenceQueueEntry>, RepositoryError> {
        let row = sqlx::query(
            "SELECT *
             FROM convergence_queue_entries
             WHERE project_id = ?
               AND target_ref = ?
               AND status = 'head'
             LIMIT 1",
        )
        .bind(project_id.to_string())
        .bind(target_ref)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;

        row.as_ref().map(map_convergence_queue_entry).transpose()
    }

    pub async fn find_next_queued_entry(
        &self,
        project_id: ProjectId,
        target_ref: &str,
    ) -> Result<Option<ConvergenceQueueEntry>, RepositoryError> {
        let row = sqlx::query(
            "SELECT *
             FROM convergence_queue_entries
             WHERE project_id = ?
               AND target_ref = ?
               AND status = 'queued'
             ORDER BY created_at ASC, id ASC
             LIMIT 1",
        )
        .bind(project_id.to_string())
        .bind(target_ref)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;

        row.as_ref().map(map_convergence_queue_entry).transpose()
    }

    pub async fn list_active_queue_entries_for_lane(
        &self,
        project_id: ProjectId,
        target_ref: &str,
    ) -> Result<Vec<ConvergenceQueueEntry>, RepositoryError> {
        let rows = sqlx::query(
            "SELECT *
             FROM convergence_queue_entries
             WHERE project_id = ?
               AND target_ref = ?
               AND status IN ('queued', 'head')
             ORDER BY created_at ASC, id ASC",
        )
        .bind(project_id.to_string())
        .bind(target_ref)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;

        rows.iter().map(map_convergence_queue_entry).collect()
    }

    pub async fn list_active_queue_entries_by_project(
        &self,
        project_id: ProjectId,
    ) -> Result<Vec<ConvergenceQueueEntry>, RepositoryError> {
        let rows = sqlx::query(
            "SELECT *
             FROM convergence_queue_entries
             WHERE project_id = ?
               AND status IN ('queued', 'head')
             ORDER BY target_ref ASC, created_at ASC, id ASC",
        )
        .bind(project_id.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;

        rows.iter().map(map_convergence_queue_entry).collect()
    }

    pub async fn create_queue_entry(
        &self,
        queue_entry: &ConvergenceQueueEntry,
    ) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO convergence_queue_entries (
                id, project_id, item_id, item_revision_id, target_ref, status, head_acquired_at,
                created_at, updated_at, released_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(queue_entry.id.to_string())
        .bind(queue_entry.project_id.to_string())
        .bind(queue_entry.item_id.to_string())
        .bind(queue_entry.item_revision_id.to_string())
        .bind(&queue_entry.target_ref)
        .bind(encode_enum(&queue_entry.status)?)
        .bind(queue_entry.head_acquired_at)
        .bind(queue_entry.created_at)
        .bind(queue_entry.updated_at)
        .bind(queue_entry.released_at)
        .execute(&self.pool)
        .await
        .map_err(db_write_err)?;

        Ok(())
    }

    pub async fn update_queue_entry(
        &self,
        queue_entry: &ConvergenceQueueEntry,
    ) -> Result<(), RepositoryError> {
        let result = sqlx::query(
            "UPDATE convergence_queue_entries
             SET status = ?, head_acquired_at = ?, updated_at = ?, released_at = ?
             WHERE id = ?",
        )
        .bind(encode_enum(&queue_entry.status)?)
        .bind(queue_entry.head_acquired_at)
        .bind(queue_entry.updated_at)
        .bind(queue_entry.released_at)
        .bind(queue_entry.id.to_string())
        .execute(&self.pool)
        .await
        .map_err(db_write_err)?;

        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound);
        }

        Ok(())
    }
}

impl ConvergenceQueueRepository for Database {
    async fn list_by_item(
        &self,
        item_id: ItemId,
    ) -> Result<Vec<ConvergenceQueueEntry>, RepositoryError> {
        self.list_queue_entries_by_item(item_id).await
    }
    async fn get(
        &self,
        id: ConvergenceQueueEntryId,
    ) -> Result<ConvergenceQueueEntry, RepositoryError> {
        self.get_queue_entry(id).await
    }
    async fn find_active_for_revision(
        &self,
        revision_id: ItemRevisionId,
    ) -> Result<Option<ConvergenceQueueEntry>, RepositoryError> {
        self.find_active_queue_entry_for_revision(revision_id).await
    }
    async fn find_head(
        &self,
        project_id: ProjectId,
        target_ref: &str,
    ) -> Result<Option<ConvergenceQueueEntry>, RepositoryError> {
        self.find_queue_head(project_id, target_ref).await
    }
    async fn find_next_queued(
        &self,
        project_id: ProjectId,
        target_ref: &str,
    ) -> Result<Option<ConvergenceQueueEntry>, RepositoryError> {
        self.find_next_queued_entry(project_id, target_ref).await
    }
    async fn list_active_by_project(
        &self,
        project_id: ProjectId,
    ) -> Result<Vec<ConvergenceQueueEntry>, RepositoryError> {
        self.list_active_queue_entries_by_project(project_id).await
    }
    async fn list_active_for_lane(
        &self,
        project_id: ProjectId,
        target_ref: &str,
    ) -> Result<Vec<ConvergenceQueueEntry>, RepositoryError> {
        self.list_active_queue_entries_for_lane(project_id, target_ref)
            .await
    }
    async fn create(&self, entry: &ConvergenceQueueEntry) -> Result<(), RepositoryError> {
        self.create_queue_entry(entry).await
    }
    async fn update(&self, entry: &ConvergenceQueueEntry) -> Result<(), RepositoryError> {
        self.update_queue_entry(entry).await
    }
}

fn map_convergence_queue_entry(row: &SqliteRow) -> Result<ConvergenceQueueEntry, RepositoryError> {
    Ok(ConvergenceQueueEntry {
        id: parse_id(row.try_get("id").map_err(db_err)?)?,
        project_id: parse_id(row.try_get("project_id").map_err(db_err)?)?,
        item_id: parse_id(row.try_get("item_id").map_err(db_err)?)?,
        item_revision_id: parse_id(row.try_get("item_revision_id").map_err(db_err)?)?,
        target_ref: row.try_get("target_ref").map_err(db_err)?,
        status: parse_enum(row.try_get("status").map_err(db_err)?)?,
        head_acquired_at: row.try_get("head_acquired_at").map_err(db_err)?,
        created_at: row.try_get("created_at").map_err(db_err)?,
        updated_at: row.try_get("updated_at").map_err(db_err)?,
        released_at: row.try_get("released_at").map_err(db_err)?,
    })
}
