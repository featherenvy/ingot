use chrono::Utc;
use ingot_domain::commit_oid::CommitOid;
use ingot_domain::finding::{Finding, FindingTriage, FindingTriageState};
use ingot_domain::ids::{FindingId, ItemId, JobId};
use ingot_domain::item::Item;
use ingot_domain::ports::{FindingRepository, RepositoryError};
use ingot_domain::revision::ItemRevision;
use sqlx::Row;
use sqlx::sqlite::SqliteRow;
use sqlx::{Sqlite, Transaction};

use super::helpers::{
    db_err, db_write_err, encode_enum, json_err, parse_enum, parse_id, parse_json,
};
use crate::db::Database;

impl Database {
    pub async fn list_findings_by_item(
        &self,
        item_id: ItemId,
    ) -> Result<Vec<Finding>, RepositoryError> {
        let rows =
            sqlx::query("SELECT * FROM findings WHERE source_item_id = ? ORDER BY created_at DESC")
                .bind(item_id.to_string())
                .fetch_all(&self.pool)
                .await
                .map_err(db_err)?;

        rows.iter().map(map_finding).collect()
    }

    pub async fn get_finding(&self, finding_id: FindingId) -> Result<Finding, RepositoryError> {
        let row = sqlx::query("SELECT * FROM findings WHERE id = ?")
            .bind(finding_id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;

        row.as_ref()
            .map(map_finding)
            .transpose()?
            .ok_or(RepositoryError::NotFound)
    }

    pub async fn create_finding(&self, finding: &Finding) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO findings (
                id, project_id, source_item_id, source_item_revision_id, source_job_id, source_step_id,
                source_report_schema_version, source_finding_key, source_subject_kind,
                source_subject_base_commit_oid, source_subject_head_commit_oid, code, severity, summary,
                paths, evidence, triage_state, linked_item_id, triage_note, created_at, triaged_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(finding.id.to_string())
        .bind(finding.project_id.to_string())
        .bind(finding.source_item_id.to_string())
        .bind(finding.source_item_revision_id.to_string())
        .bind(finding.source_job_id.to_string())
        .bind(&finding.source_step_id)
        .bind(&finding.source_report_schema_version)
        .bind(&finding.source_finding_key)
        .bind(encode_enum(&finding.source_subject_kind)?)
        .bind(finding.source_subject_base_commit_oid.as_ref().map(CommitOid::as_str))
        .bind(finding.source_subject_head_commit_oid.as_str())
        .bind(&finding.code)
        .bind(encode_enum(&finding.severity)?)
        .bind(&finding.summary)
        .bind(serde_json::to_string(&finding.paths).map_err(json_err)?)
        .bind(serde_json::to_string(&finding.evidence).map_err(json_err)?)
        .bind(encode_enum(&finding.triage.state())?)
        .bind(finding.triage.linked_item_id().map(|id| id.to_string()))
        .bind(finding.triage.triage_note())
        .bind(finding.created_at)
        .bind(finding.triage.triaged_at())
        .execute(&self.pool)
        .await
        .map_err(db_write_err)?;

        Ok(())
    }

    pub async fn find_finding_by_source(
        &self,
        job_id: JobId,
        source_finding_key: &str,
    ) -> Result<Option<Finding>, RepositoryError> {
        let row = sqlx::query(
            "SELECT * FROM findings WHERE source_job_id = ? AND source_finding_key = ?",
        )
        .bind(job_id.to_string())
        .bind(source_finding_key)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;

        row.as_ref().map(map_finding).transpose()
    }

    pub async fn triage_finding(&self, finding: &Finding) -> Result<(), RepositoryError> {
        sqlx::query(
            "UPDATE findings
             SET triage_state = ?, triage_note = ?, triaged_at = ?, linked_item_id = ?
             WHERE id = ?",
        )
        .bind(encode_enum(&finding.triage.state())?)
        .bind(finding.triage.triage_note())
        .bind(finding.triage.triaged_at())
        .bind(finding.triage.linked_item_id().map(|id| id.to_string()))
        .bind(finding.id.to_string())
        .execute(&self.pool)
        .await
        .map_err(db_err)?;

        Ok(())
    }

    pub async fn triage_finding_with_origin_detached(
        &self,
        finding: &Finding,
        detached_item_id: Option<ItemId>,
    ) -> Result<(), RepositoryError> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;

        sqlx::query(
            "UPDATE findings
             SET triage_state = ?, triage_note = ?, triaged_at = ?, linked_item_id = ?
             WHERE id = ?",
        )
        .bind(encode_enum(&finding.triage.state())?)
        .bind(finding.triage.triage_note())
        .bind(finding.triage.triaged_at())
        .bind(finding.triage.linked_item_id().map(|id| id.to_string()))
        .bind(finding.id.to_string())
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;

        if let Some(detached_item_id) = detached_item_id {
            sqlx::query(
                "UPDATE items
                 SET origin_kind = 'manual', origin_finding_id = NULL, updated_at = ?
                 WHERE id = ?
                   AND origin_kind = 'promoted_finding'
                   AND origin_finding_id = ?",
            )
            .bind(Utc::now())
            .bind(detached_item_id.to_string())
            .bind(finding.id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        }

        tx.commit().await.map_err(db_err)?;
        Ok(())
    }

    pub async fn link_backlog_finding(
        &self,
        finding: &Finding,
        linked_item: &Item,
        linked_revision: &ItemRevision,
        detached_item_id: Option<ItemId>,
    ) -> Result<(), RepositoryError> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;

        sqlx::query(
            "INSERT INTO items (
                id, project_id, classification, workflow_version, lifecycle_state, parking_state,
                done_reason, resolution_source, approval_state, escalation_state, escalation_reason,
                current_revision_id, origin_kind, origin_finding_id, priority, labels, operator_notes,
                created_at, updated_at, closed_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(linked_item.id.to_string())
        .bind(linked_item.project_id.to_string())
        .bind(encode_enum(&linked_item.classification)?)
        .bind(&linked_item.workflow_version)
        .bind(linked_item.lifecycle.as_db_str())
        .bind(encode_enum(&linked_item.parking_state)?)
        .bind(linked_item.lifecycle.done_reason().as_ref().map(encode_enum).transpose()?)
        .bind(linked_item.lifecycle.resolution_source().as_ref().map(encode_enum).transpose()?)
        .bind(encode_enum(&linked_item.approval_state)?)
        .bind(linked_item.escalation.as_db_str())
        .bind(linked_item.escalation.reason().as_ref().map(encode_enum).transpose()?)
        .bind(linked_item.current_revision_id.to_string())
        .bind(linked_item.origin.as_db_str())
        .bind(linked_item.origin.finding_id().map(|id| id.to_string()))
        .bind(encode_enum(&linked_item.priority)?)
        .bind(serde_json::to_string(&linked_item.labels).map_err(json_err)?)
        .bind(linked_item.operator_notes.as_deref())
        .bind(linked_item.created_at)
        .bind(linked_item.updated_at)
        .bind(linked_item.lifecycle.closed_at())
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
        .bind(linked_revision.id.to_string())
        .bind(linked_revision.item_id.to_string())
        .bind(linked_revision.revision_no as i64)
        .bind(&linked_revision.title)
        .bind(&linked_revision.description)
        .bind(&linked_revision.acceptance_criteria)
        .bind(&linked_revision.target_ref)
        .bind(encode_enum(&linked_revision.approval_policy)?)
        .bind(serde_json::to_string(&linked_revision.policy_snapshot).map_err(json_err)?)
        .bind(serde_json::to_string(&linked_revision.template_map_snapshot).map_err(json_err)?)
        .bind(linked_revision.seed.seed_commit_oid().map(CommitOid::as_str))
        .bind(linked_revision.seed.seed_target_commit_oid().as_str())
        .bind(
            linked_revision
                .supersedes_revision_id
                .map(|id| id.to_string()),
        )
        .bind(linked_revision.created_at)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;

        sqlx::query(
            "UPDATE findings
             SET triage_state = ?, linked_item_id = ?, triage_note = ?, triaged_at = ?
             WHERE id = ?",
        )
        .bind(encode_enum(&finding.triage.state())?)
        .bind(linked_item.id.to_string())
        .bind(finding.triage.triage_note())
        .bind(finding.triage.triaged_at())
        .bind(finding.id.to_string())
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;

        if let Some(detached_item_id) = detached_item_id {
            sqlx::query(
                "UPDATE items
                 SET origin_kind = 'manual', origin_finding_id = NULL, updated_at = ?
                 WHERE id = ?
                   AND origin_kind = 'promoted_finding'
                   AND origin_finding_id = ?",
            )
            .bind(Utc::now())
            .bind(detached_item_id.to_string())
            .bind(finding.id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        }

        tx.commit().await.map_err(db_err)?;
        Ok(())
    }

    pub async fn update_finding(&self, finding: &Finding) -> Result<(), RepositoryError> {
        let result = sqlx::query(
            "UPDATE findings
             SET source_step_id = ?, source_report_schema_version = ?, source_subject_kind = ?,
                 source_subject_base_commit_oid = ?, source_subject_head_commit_oid = ?,
                 code = ?, severity = ?, summary = ?, paths = ?, evidence = ?,
                 triage_state = ?, linked_item_id = ?, triage_note = ?, triaged_at = ?
             WHERE id = ?",
        )
        .bind(&finding.source_step_id)
        .bind(&finding.source_report_schema_version)
        .bind(encode_enum(&finding.source_subject_kind)?)
        .bind(finding.source_subject_base_commit_oid.as_ref().map(CommitOid::as_str))
        .bind(finding.source_subject_head_commit_oid.as_str())
        .bind(&finding.code)
        .bind(encode_enum(&finding.severity)?)
        .bind(&finding.summary)
        .bind(serde_json::to_string(&finding.paths).map_err(json_err)?)
        .bind(serde_json::to_string(&finding.evidence).map_err(json_err)?)
        .bind(encode_enum(&finding.triage.state())?)
        .bind(finding.triage.linked_item_id().map(|id| id.to_string()))
        .bind(finding.triage.triage_note())
        .bind(finding.triage.triaged_at())
        .bind(finding.id.to_string())
        .execute(&self.pool)
        .await
        .map_err(db_write_err)?;

        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound);
        }

        Ok(())
    }
}

impl FindingRepository for Database {
    async fn list_by_item(&self, item_id: ItemId) -> Result<Vec<Finding>, RepositoryError> {
        self.list_findings_by_item(item_id).await
    }
    async fn get(&self, id: FindingId) -> Result<Finding, RepositoryError> {
        self.get_finding(id).await
    }
    async fn create(&self, finding: &Finding) -> Result<(), RepositoryError> {
        self.create_finding(finding).await
    }
    async fn update(&self, finding: &Finding) -> Result<(), RepositoryError> {
        self.update_finding(finding).await
    }
    async fn find_by_source(
        &self,
        job_id: JobId,
        source_finding_key: &str,
    ) -> Result<Option<Finding>, RepositoryError> {
        self.find_finding_by_source(job_id, source_finding_key)
            .await
    }
    async fn triage(&self, finding: &Finding) -> Result<(), RepositoryError> {
        self.triage_finding(finding).await
    }
    async fn triage_with_origin_detached(
        &self,
        finding: &Finding,
        detached_item_id: Option<ItemId>,
    ) -> Result<(), RepositoryError> {
        self.triage_finding_with_origin_detached(finding, detached_item_id)
            .await
    }
    async fn link_backlog(
        &self,
        finding: &Finding,
        linked_item: &Item,
        linked_revision: &ItemRevision,
        detached_item_id: Option<ItemId>,
    ) -> Result<(), RepositoryError> {
        self.link_backlog_finding(finding, linked_item, linked_revision, detached_item_id)
            .await
    }
}

pub(super) async fn upsert_finding(
    tx: &mut Transaction<'_, Sqlite>,
    finding: &Finding,
) -> Result<(), RepositoryError> {
    sqlx::query(
        "INSERT INTO findings (
            id, project_id, source_item_id, source_item_revision_id, source_job_id, source_step_id,
            source_report_schema_version, source_finding_key, source_subject_kind,
            source_subject_base_commit_oid, source_subject_head_commit_oid, code, severity, summary,
            paths, evidence, triage_state, linked_item_id, triage_note, created_at, triaged_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(source_job_id, source_finding_key) DO UPDATE SET
            source_step_id = excluded.source_step_id,
            source_report_schema_version = excluded.source_report_schema_version,
            source_subject_kind = excluded.source_subject_kind,
            source_subject_base_commit_oid = excluded.source_subject_base_commit_oid,
            source_subject_head_commit_oid = excluded.source_subject_head_commit_oid,
            code = excluded.code,
            severity = excluded.severity,
            summary = excluded.summary,
            paths = excluded.paths,
            evidence = excluded.evidence",
    )
    .bind(finding.id.to_string())
    .bind(finding.project_id.to_string())
    .bind(finding.source_item_id.to_string())
    .bind(finding.source_item_revision_id.to_string())
    .bind(finding.source_job_id.to_string())
    .bind(&finding.source_step_id)
    .bind(&finding.source_report_schema_version)
    .bind(&finding.source_finding_key)
    .bind(encode_enum(&finding.source_subject_kind)?)
    .bind(finding.source_subject_base_commit_oid.as_ref().map(CommitOid::as_str))
    .bind(finding.source_subject_head_commit_oid.as_str())
    .bind(&finding.code)
    .bind(encode_enum(&finding.severity)?)
    .bind(&finding.summary)
    .bind(serde_json::to_string(&finding.paths).map_err(json_err)?)
    .bind(serde_json::to_string(&finding.evidence).map_err(json_err)?)
    .bind(encode_enum(&finding.triage.state())?)
    .bind(finding.triage.linked_item_id().map(|id| id.to_string()))
    .bind(finding.triage.triage_note())
    .bind(finding.created_at)
    .bind(finding.triage.triaged_at())
    .execute(&mut **tx)
    .await
    .map_err(db_err)?;

    Ok(())
}

fn map_finding(row: &SqliteRow) -> Result<Finding, RepositoryError> {
    let state: FindingTriageState = parse_enum(row.try_get("triage_state").map_err(db_err)?)?;
    let linked_item_id: Option<ItemId> = row
        .try_get::<Option<String>, _>("linked_item_id")
        .map_err(db_err)?
        .map(parse_id)
        .transpose()?;
    let triage_note: Option<String> = row.try_get("triage_note").map_err(db_err)?;
    let triaged_at: Option<chrono::DateTime<chrono::Utc>> =
        row.try_get("triaged_at").map_err(db_err)?;
    let triage = FindingTriage::try_from_parts(
        state,
        linked_item_id,
        triage_note,
        triaged_at,
        |state, field| {
            RepositoryError::Conflict(format!("{} finding missing {field}", state.as_str()))
        },
    )?;

    Ok(Finding {
        id: parse_id(row.try_get("id").map_err(db_err)?)?,
        project_id: parse_id(row.try_get("project_id").map_err(db_err)?)?,
        source_item_id: parse_id(row.try_get("source_item_id").map_err(db_err)?)?,
        source_item_revision_id: parse_id(row.try_get("source_item_revision_id").map_err(db_err)?)?,
        source_job_id: parse_id(row.try_get("source_job_id").map_err(db_err)?)?,
        source_step_id: row.try_get("source_step_id").map_err(db_err)?,
        source_report_schema_version: row
            .try_get("source_report_schema_version")
            .map_err(db_err)?,
        source_finding_key: row.try_get("source_finding_key").map_err(db_err)?,
        source_subject_kind: parse_enum(row.try_get("source_subject_kind").map_err(db_err)?)?,
        source_subject_base_commit_oid: row
            .try_get::<Option<String>, _>("source_subject_base_commit_oid")
            .map_err(db_err)?
            .map(CommitOid::new),
        source_subject_head_commit_oid: CommitOid::new(row
            .try_get::<String, _>("source_subject_head_commit_oid")
            .map_err(db_err)?),
        code: row.try_get("code").map_err(db_err)?,
        severity: parse_enum(row.try_get("severity").map_err(db_err)?)?,
        summary: row.try_get("summary").map_err(db_err)?,
        paths: parse_json(row.try_get("paths").map_err(db_err)?)?,
        evidence: parse_json(row.try_get("evidence").map_err(db_err)?)?,
        created_at: row.try_get("created_at").map_err(db_err)?,
        triage,
    })
}
