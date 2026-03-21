use ingot_domain::commit_oid::CommitOid;
use ingot_domain::ids::{ItemId, ItemRevisionId};
use ingot_domain::ports::{RepositoryError, RevisionContextRepository, RevisionRepository};
use ingot_domain::revision::{AuthoringBaseSeed, ItemRevision};
use ingot_domain::revision_context::RevisionContext;
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use super::helpers::{db_err, db_write_err, json_err, parse_json};
use crate::db::Database;

impl Database {
    pub async fn list_revisions_by_item(
        &self,
        item_id: ItemId,
    ) -> Result<Vec<ItemRevision>, RepositoryError> {
        let rows =
            sqlx::query("SELECT * FROM item_revisions WHERE item_id = ? ORDER BY revision_no DESC")
                .bind(item_id)
                .fetch_all(&self.pool)
                .await
                .map_err(db_err)?;

        rows.iter().map(map_revision).collect()
    }

    pub async fn get_revision(
        &self,
        revision_id: ItemRevisionId,
    ) -> Result<ItemRevision, RepositoryError> {
        let row = sqlx::query("SELECT * FROM item_revisions WHERE id = ?")
            .bind(revision_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;

        row.as_ref()
            .map(map_revision)
            .transpose()?
            .ok_or(RepositoryError::NotFound)
    }

    pub async fn create_revision(&self, revision: &ItemRevision) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO item_revisions (
                id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
                approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid,
                seed_target_commit_oid, supersedes_revision_id, created_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(revision.id)
        .bind(revision.item_id)
        .bind(revision.revision_no as i64)
        .bind(&revision.title)
        .bind(&revision.description)
        .bind(&revision.acceptance_criteria)
        .bind(&revision.target_ref)
        .bind(revision.approval_policy)
        .bind(serde_json::to_string(&revision.policy_snapshot).map_err(json_err)?)
        .bind(serde_json::to_string(&revision.template_map_snapshot).map_err(json_err)?)
        .bind(revision.seed.seed_commit_oid().cloned())
        .bind(revision.seed.seed_target_commit_oid().clone())
        .bind(revision.supersedes_revision_id)
        .bind(revision.created_at)
        .execute(&self.pool)
        .await
        .map_err(db_write_err)?;

        Ok(())
    }

    pub async fn get_revision_context(
        &self,
        revision_id: ItemRevisionId,
    ) -> Result<Option<RevisionContext>, RepositoryError> {
        let row = sqlx::query("SELECT * FROM revision_contexts WHERE item_revision_id = ?")
            .bind(revision_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;

        row.as_ref().map(map_revision_context).transpose()
    }

    pub async fn upsert_revision_context(
        &self,
        context: &RevisionContext,
    ) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO revision_contexts (
                item_revision_id, schema_version, payload, updated_from_job_id, updated_at
             ) VALUES (?, ?, ?, ?, ?)
             ON CONFLICT(item_revision_id) DO UPDATE SET
                schema_version = excluded.schema_version,
                payload = excluded.payload,
                updated_from_job_id = excluded.updated_from_job_id,
                updated_at = excluded.updated_at",
        )
        .bind(context.item_revision_id)
        .bind(&context.schema_version)
        .bind(serde_json::to_string(&context.payload).map_err(json_err)?)
        .bind(context.updated_from_job_id)
        .bind(context.updated_at)
        .execute(&self.pool)
        .await
        .map_err(db_write_err)?;

        Ok(())
    }
}

impl RevisionRepository for Database {
    async fn list_by_item(&self, item_id: ItemId) -> Result<Vec<ItemRevision>, RepositoryError> {
        self.list_revisions_by_item(item_id).await
    }
    async fn get(&self, id: ItemRevisionId) -> Result<ItemRevision, RepositoryError> {
        self.get_revision(id).await
    }
    async fn create(&self, revision: &ItemRevision) -> Result<(), RepositoryError> {
        self.create_revision(revision).await
    }
}

impl RevisionContextRepository for Database {
    async fn get(
        &self,
        revision_id: ItemRevisionId,
    ) -> Result<Option<RevisionContext>, RepositoryError> {
        self.get_revision_context(revision_id).await
    }
    async fn upsert(&self, context: &RevisionContext) -> Result<(), RepositoryError> {
        self.upsert_revision_context(context).await
    }
}

fn map_revision(row: &SqliteRow) -> Result<ItemRevision, RepositoryError> {
    let seed_commit_oid: Option<CommitOid> = row.try_get("seed_commit_oid").map_err(db_err)?;
    let seed_target_commit_oid: CommitOid = row
        .try_get::<Option<CommitOid>, _>("seed_target_commit_oid")
        .map_err(db_err)?
        .ok_or_else(|| {
            RepositoryError::Conflict("seed_target_commit_oid must not be NULL".into())
        })?;
    let seed = AuthoringBaseSeed::from_parts(seed_commit_oid, seed_target_commit_oid);

    Ok(ItemRevision {
        id: row.try_get("id").map_err(db_err)?,
        item_id: row.try_get("item_id").map_err(db_err)?,
        revision_no: row.try_get::<i64, _>("revision_no").map_err(db_err)? as u32,
        title: row.try_get("title").map_err(db_err)?,
        description: row.try_get("description").map_err(db_err)?,
        acceptance_criteria: row.try_get("acceptance_criteria").map_err(db_err)?,
        target_ref: row.try_get("target_ref").map_err(db_err)?,
        approval_policy: row.try_get("approval_policy").map_err(db_err)?,
        policy_snapshot: parse_json(row.try_get("policy_snapshot").map_err(db_err)?)?,
        template_map_snapshot: parse_json(row.try_get("template_map_snapshot").map_err(db_err)?)?,
        seed,
        supersedes_revision_id: row
            .try_get("supersedes_revision_id")
            .map_err(db_err)?,
        created_at: row.try_get("created_at").map_err(db_err)?,
    })
}

fn map_revision_context(row: &SqliteRow) -> Result<RevisionContext, RepositoryError> {
    Ok(RevisionContext {
        item_revision_id: row.try_get("item_revision_id").map_err(db_err)?,
        schema_version: row.try_get("schema_version").map_err(db_err)?,
        payload: parse_json(row.try_get("payload").map_err(db_err)?)?,
        updated_from_job_id: row
            .try_get("updated_from_job_id")
            .map_err(db_err)?,
        updated_at: row.try_get("updated_at").map_err(db_err)?,
    })
}
