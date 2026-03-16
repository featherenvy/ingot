use ingot_domain::ids::{ItemId, ProjectId};
use ingot_domain::item::{Escalation, Item, Lifecycle, Origin};
use ingot_domain::ports::{ItemRepository, RepositoryError};
use ingot_domain::revision::ItemRevision;
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use super::helpers::{
    db_err, db_write_err, encode_enum, json_err, parse_enum, parse_id, parse_json,
};
use crate::db::Database;

type SqliteQuery<'a> = sqlx::query::Query<'a, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'a>>;

impl Database {
    pub async fn list_items_by_project(
        &self,
        project_id: ProjectId,
    ) -> Result<Vec<Item>, RepositoryError> {
        let rows = sqlx::query("SELECT * FROM items WHERE project_id = ? ORDER BY created_at DESC")
            .bind(project_id.to_string())
            .fetch_all(&self.pool)
            .await
            .map_err(db_err)?;

        rows.iter().map(map_item).collect()
    }

    pub async fn get_item(&self, item_id: ItemId) -> Result<Item, RepositoryError> {
        let row = sqlx::query("SELECT * FROM items WHERE id = ?")
            .bind(item_id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;

        row.as_ref()
            .map(map_item)
            .transpose()?
            .ok_or(RepositoryError::NotFound)
    }

    pub async fn update_item(&self, item: &Item) -> Result<(), RepositoryError> {
        let result = sqlx::query(
            "UPDATE items
             SET classification = ?, workflow_version = ?, lifecycle_state = ?, parking_state = ?,
                 done_reason = ?, resolution_source = ?, approval_state = ?, escalation_state = ?,
                 escalation_reason = ?, current_revision_id = ?, origin_kind = ?, origin_finding_id = ?,
                 priority = ?, labels = ?, operator_notes = ?, updated_at = ?, closed_at = ?
             WHERE id = ?",
        )
        .bind(encode_enum(&item.classification)?)
        .bind(&item.workflow_version)
        .bind(item.lifecycle.as_db_str())
        .bind(encode_enum(&item.parking_state)?)
        .bind(item.lifecycle.done_reason().as_ref().map(encode_enum).transpose()?)
        .bind(item.lifecycle.resolution_source().as_ref().map(encode_enum).transpose()?)
        .bind(encode_enum(&item.approval_state)?)
        .bind(item.escalation.as_db_str())
        .bind(item.escalation.reason().as_ref().map(encode_enum).transpose()?)
        .bind(item.current_revision_id.to_string())
        .bind(item.origin.as_db_str())
        .bind(item.origin.finding_id().map(|id| id.to_string()))
        .bind(encode_enum(&item.priority)?)
        .bind(serde_json::to_string(&item.labels).map_err(json_err)?)
        .bind(item.operator_notes.as_deref())
        .bind(item.updated_at)
        .bind(item.lifecycle.closed_at())
        .bind(item.id.to_string())
        .execute(&self.pool)
        .await
        .map_err(db_write_err)?;

        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound);
        }

        Ok(())
    }

    pub async fn create_item_with_revision(
        &self,
        item: &Item,
        revision: &ItemRevision,
    ) -> Result<(), RepositoryError> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;

        insert_item_query(item)?
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;

        sqlx::query(
            "INSERT INTO item_revisions (
                id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
                approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid,
                seed_target_commit_oid, supersedes_revision_id, created_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(revision.id.to_string())
        .bind(revision.item_id.to_string())
        .bind(revision.revision_no as i64)
        .bind(&revision.title)
        .bind(&revision.description)
        .bind(&revision.acceptance_criteria)
        .bind(&revision.target_ref)
        .bind(encode_enum(&revision.approval_policy)?)
        .bind(serde_json::to_string(&revision.policy_snapshot).map_err(json_err)?)
        .bind(serde_json::to_string(&revision.template_map_snapshot).map_err(json_err)?)
        .bind(revision.seed_commit_oid.as_deref())
        .bind(revision.seed_target_commit_oid.as_deref())
        .bind(revision.supersedes_revision_id.map(|id| id.to_string()))
        .bind(revision.created_at)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;

        tx.commit().await.map_err(db_err)?;
        Ok(())
    }

    pub async fn create_item(&self, item: &Item) -> Result<(), RepositoryError> {
        insert_item_query(item)?
            .execute(&self.pool)
            .await
            .map_err(db_write_err)?;

        Ok(())
    }
}

impl ItemRepository for Database {
    async fn list_by_project(&self, project_id: ProjectId) -> Result<Vec<Item>, RepositoryError> {
        self.list_items_by_project(project_id).await
    }
    async fn get(&self, id: ItemId) -> Result<Item, RepositoryError> {
        self.get_item(id).await
    }
    async fn create(&self, item: &Item) -> Result<(), RepositoryError> {
        self.create_item(item).await
    }
    async fn update(&self, item: &Item) -> Result<(), RepositoryError> {
        self.update_item(item).await
    }
    async fn create_with_revision(
        &self,
        item: &Item,
        revision: &ItemRevision,
    ) -> Result<(), RepositoryError> {
        self.create_item_with_revision(item, revision).await
    }
}

fn insert_item_query<'a>(item: &'a Item) -> Result<SqliteQuery<'a>, RepositoryError> {
    Ok(sqlx::query(
        "INSERT INTO items (
            id, project_id, classification, workflow_version, lifecycle_state, parking_state,
            done_reason, resolution_source, approval_state, escalation_state, escalation_reason,
            current_revision_id, origin_kind, origin_finding_id, priority, labels, operator_notes,
            created_at, updated_at, closed_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(item.id.to_string())
    .bind(item.project_id.to_string())
    .bind(encode_enum(&item.classification)?)
    .bind(&item.workflow_version)
    .bind(item.lifecycle.as_db_str())
    .bind(encode_enum(&item.parking_state)?)
    .bind(
        item.lifecycle
            .done_reason()
            .as_ref()
            .map(encode_enum)
            .transpose()?,
    )
    .bind(
        item.lifecycle
            .resolution_source()
            .as_ref()
            .map(encode_enum)
            .transpose()?,
    )
    .bind(encode_enum(&item.approval_state)?)
    .bind(item.escalation.as_db_str())
    .bind(
        item.escalation
            .reason()
            .as_ref()
            .map(encode_enum)
            .transpose()?,
    )
    .bind(item.current_revision_id.to_string())
    .bind(item.origin.as_db_str())
    .bind(item.origin.finding_id().map(|id| id.to_string()))
    .bind(encode_enum(&item.priority)?)
    .bind(serde_json::to_string(&item.labels).map_err(json_err)?)
    .bind(item.operator_notes.as_deref())
    .bind(item.created_at)
    .bind(item.updated_at)
    .bind(item.lifecycle.closed_at()))
}

fn map_item(row: &SqliteRow) -> Result<Item, RepositoryError> {
    Ok(Item {
        id: parse_id(row.try_get("id").map_err(db_err)?)?,
        project_id: parse_id(row.try_get("project_id").map_err(db_err)?)?,
        classification: parse_enum(row.try_get("classification").map_err(db_err)?)?,
        workflow_version: row.try_get("workflow_version").map_err(db_err)?,
        lifecycle: parse_lifecycle(row)?,
        parking_state: parse_enum(row.try_get("parking_state").map_err(db_err)?)?,
        approval_state: parse_enum(row.try_get("approval_state").map_err(db_err)?)?,
        escalation: parse_escalation(row)?,
        current_revision_id: parse_id(row.try_get("current_revision_id").map_err(db_err)?)?,
        origin: parse_origin(row)?,
        priority: parse_enum(row.try_get("priority").map_err(db_err)?)?,
        labels: parse_json(row.try_get("labels").map_err(db_err)?)?,
        operator_notes: row.try_get("operator_notes").map_err(db_err)?,
        created_at: row.try_get("created_at").map_err(db_err)?,
        updated_at: row.try_get("updated_at").map_err(db_err)?,
    })
}

fn parse_lifecycle(row: &SqliteRow) -> Result<Lifecycle, RepositoryError> {
    match row
        .try_get::<String, _>("lifecycle_state")
        .map_err(db_err)?
        .as_str()
    {
        "open" => Ok(Lifecycle::Open),
        "done" => Ok(Lifecycle::Done {
            reason: parse_enum(row.try_get::<String, _>("done_reason").map_err(db_err)?)?,
            source: parse_enum(
                row.try_get::<String, _>("resolution_source")
                    .map_err(db_err)?,
            )?,
            closed_at: row.try_get("closed_at").map_err(db_err)?,
        }),
        other => invalid_state("lifecycle_state", other),
    }
}

fn parse_escalation(row: &SqliteRow) -> Result<Escalation, RepositoryError> {
    match row
        .try_get::<String, _>("escalation_state")
        .map_err(db_err)?
        .as_str()
    {
        "none" => Ok(Escalation::None),
        "operator_required" => Ok(Escalation::OperatorRequired {
            reason: parse_enum(
                row.try_get::<String, _>("escalation_reason")
                    .map_err(db_err)?,
            )?,
        }),
        other => invalid_state("escalation_state", other),
    }
}

fn parse_origin(row: &SqliteRow) -> Result<Origin, RepositoryError> {
    match row
        .try_get::<String, _>("origin_kind")
        .map_err(db_err)?
        .as_str()
    {
        "manual" => Ok(Origin::Manual),
        "promoted_finding" => Ok(Origin::PromotedFinding {
            finding_id: parse_id(
                row.try_get::<String, _>("origin_finding_id")
                    .map_err(db_err)?,
            )?,
        }),
        other => invalid_state("origin_kind", other),
    }
}

fn invalid_state<T>(field: &str, value: &str) -> Result<T, RepositoryError> {
    Err(RepositoryError::Database(
        format!("unknown {field}: {value}").into(),
    ))
}
