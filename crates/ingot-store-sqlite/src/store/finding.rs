use chrono::Utc;
use ingot_domain::finding::{Finding, FindingTriage, FindingTriageState};
use ingot_domain::ids::{FindingId, ItemId, JobId};
use ingot_domain::item::Item;
use ingot_domain::ports::{ConflictKind, FindingRepository, RepositoryError};
use ingot_domain::revision::ItemRevision;
use sqlx::Row;
use sqlx::sqlite::SqliteRow;
use sqlx::{Sqlite, Transaction};

use super::helpers::{db_err, db_write_err, json_err, parse_json};
use super::item::{insert_item_query, insert_revision_query};
use crate::db::Database;

impl Database {
    pub async fn list_findings_by_item(
        &self,
        item_id: ItemId,
    ) -> Result<Vec<Finding>, RepositoryError> {
        let rows =
            sqlx::query("SELECT * FROM findings WHERE source_item_id = ? ORDER BY created_at DESC")
                .bind(item_id)
                .fetch_all(&self.pool)
                .await
                .map_err(db_err)?;

        rows.iter().map(map_finding).collect()
    }

    pub async fn get_finding(&self, finding_id: FindingId) -> Result<Finding, RepositoryError> {
        let row = sqlx::query("SELECT * FROM findings WHERE id = ?")
            .bind(finding_id)
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
                paths, evidence, investigation, triage_state, linked_item_id, triage_note, created_at, triaged_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(finding.id)
        .bind(finding.project_id)
        .bind(finding.source_item_id)
        .bind(finding.source_item_revision_id)
        .bind(finding.source_job_id)
        .bind(finding.source_step_id)
        .bind(&finding.source_report_schema_version)
        .bind(&finding.source_finding_key)
        .bind(finding.source_subject_kind)
        .bind(finding.source_subject_base_commit_oid.clone())
        .bind(finding.source_subject_head_commit_oid.clone())
        .bind(&finding.code)
        .bind(finding.severity)
        .bind(&finding.summary)
        .bind(serde_json::to_string(&finding.paths).map_err(json_err)?)
        .bind(serde_json::to_string(&finding.evidence).map_err(json_err)?)
        .bind(
            finding
                .investigation
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(json_err)?,
        )
        .bind(finding.triage.state())
        .bind(finding.triage.linked_item_id())
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
        .bind(job_id)
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
        .bind(finding.triage.state())
        .bind(finding.triage.triage_note())
        .bind(finding.triage.triaged_at())
        .bind(finding.triage.linked_item_id())
        .bind(finding.id)
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
        .bind(finding.triage.state())
        .bind(finding.triage.triage_note())
        .bind(finding.triage.triaged_at())
        .bind(finding.triage.linked_item_id())
        .bind(finding.id)
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
            .bind(detached_item_id)
            .bind(finding.id)
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

        insert_item_query(linked_item)?
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;

        insert_revision_query(linked_revision)?
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;

        sqlx::query(
            "UPDATE findings
             SET triage_state = ?, linked_item_id = ?, triage_note = ?, triaged_at = ?
             WHERE id = ?",
        )
        .bind(finding.triage.state())
        .bind(linked_item.id)
        .bind(finding.triage.triage_note())
        .bind(finding.triage.triaged_at())
        .bind(finding.id)
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
            .bind(detached_item_id)
            .bind(finding.id)
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
                 code = ?, severity = ?, summary = ?, paths = ?, evidence = ?, investigation = ?,
                 triage_state = ?, linked_item_id = ?, triage_note = ?, triaged_at = ?
             WHERE id = ?",
        )
        .bind(finding.source_step_id)
        .bind(&finding.source_report_schema_version)
        .bind(finding.source_subject_kind)
        .bind(finding.source_subject_base_commit_oid.clone())
        .bind(finding.source_subject_head_commit_oid.clone())
        .bind(&finding.code)
        .bind(finding.severity)
        .bind(&finding.summary)
        .bind(serde_json::to_string(&finding.paths).map_err(json_err)?)
        .bind(serde_json::to_string(&finding.evidence).map_err(json_err)?)
        .bind(
            finding
                .investigation
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(json_err)?,
        )
        .bind(finding.triage.state())
        .bind(finding.triage.linked_item_id())
        .bind(finding.triage.triage_note())
        .bind(finding.triage.triaged_at())
        .bind(finding.id)
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
            paths, evidence, investigation, triage_state, linked_item_id, triage_note, created_at, triaged_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
            evidence = excluded.evidence,
            investigation = excluded.investigation",
    )
    .bind(finding.id)
    .bind(finding.project_id)
    .bind(finding.source_item_id)
    .bind(finding.source_item_revision_id)
    .bind(finding.source_job_id)
    .bind(finding.source_step_id)
    .bind(&finding.source_report_schema_version)
    .bind(&finding.source_finding_key)
    .bind(finding.source_subject_kind)
    .bind(finding.source_subject_base_commit_oid.clone())
    .bind(finding.source_subject_head_commit_oid.clone())
    .bind(&finding.code)
    .bind(finding.severity)
    .bind(&finding.summary)
    .bind(serde_json::to_string(&finding.paths).map_err(json_err)?)
    .bind(serde_json::to_string(&finding.evidence).map_err(json_err)?)
    .bind(
        finding
            .investigation
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(json_err)?,
    )
    .bind(finding.triage.state())
    .bind(finding.triage.linked_item_id())
    .bind(finding.triage.triage_note())
    .bind(finding.created_at)
    .bind(finding.triage.triaged_at())
    .execute(&mut **tx)
    .await
    .map_err(db_err)?;

    Ok(())
}

fn map_finding(row: &SqliteRow) -> Result<Finding, RepositoryError> {
    let state: FindingTriageState = row.try_get("triage_state").map_err(db_err)?;
    let linked_item_id: Option<ItemId> = row.try_get("linked_item_id").map_err(db_err)?;
    let triage_note: Option<String> = row.try_get("triage_note").map_err(db_err)?;
    let triaged_at: Option<chrono::DateTime<chrono::Utc>> =
        row.try_get("triaged_at").map_err(db_err)?;
    let triage = FindingTriage::try_from_parts(
        state,
        linked_item_id,
        triage_note,
        triaged_at,
        |state, field| {
            RepositoryError::Conflict(ConflictKind::Other(format!(
                "{} finding missing {field}",
                state.as_str()
            )))
        },
    )?;

    Ok(Finding {
        id: row.try_get("id").map_err(db_err)?,
        project_id: row.try_get("project_id").map_err(db_err)?,
        source_item_id: row.try_get("source_item_id").map_err(db_err)?,
        source_item_revision_id: row.try_get("source_item_revision_id").map_err(db_err)?,
        source_job_id: row.try_get("source_job_id").map_err(db_err)?,
        source_step_id: row.try_get("source_step_id").map_err(db_err)?,
        source_report_schema_version: row
            .try_get("source_report_schema_version")
            .map_err(db_err)?,
        source_finding_key: row.try_get("source_finding_key").map_err(db_err)?,
        source_subject_kind: row.try_get("source_subject_kind").map_err(db_err)?,
        source_subject_base_commit_oid: row
            .try_get("source_subject_base_commit_oid")
            .map_err(db_err)?,
        source_subject_head_commit_oid: row
            .try_get("source_subject_head_commit_oid")
            .map_err(db_err)?,
        code: row.try_get("code").map_err(db_err)?,
        severity: row.try_get("severity").map_err(db_err)?,
        summary: row.try_get("summary").map_err(db_err)?,
        paths: parse_json(row.try_get("paths").map_err(db_err)?)?,
        evidence: parse_json(row.try_get("evidence").map_err(db_err)?)?,
        investigation: row
            .try_get::<Option<String>, _>("investigation")
            .map_err(db_err)?
            .map(parse_json)
            .transpose()?,
        created_at: row.try_get("created_at").map_err(db_err)?,
        triage,
    })
}
