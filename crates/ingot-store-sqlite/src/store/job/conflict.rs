use ingot_domain::ids::{ItemId, ItemRevisionId, JobId};
use ingot_domain::ports::{ConflictKind, RepositoryError};
use sqlx::{Sqlite, Transaction};

use crate::store::helpers::{db_err, item_revision_is_stale};

pub(super) async fn classify_job_conflict(
    tx: &mut Transaction<'_, Sqlite>,
    job_id: JobId,
    item_id: ItemId,
    expected_item_revision_id: ItemRevisionId,
    expected_statuses: &[&str],
    require_workspace_binding: bool,
) -> Result<RepositoryError, RepositoryError> {
    if item_revision_is_stale(tx, item_id, expected_item_revision_id).await? {
        return Ok(RepositoryError::Conflict(ConflictKind::JobRevisionStale));
    }

    let query = format!(
        "SELECT id
         FROM jobs
         WHERE id = ?
           AND status IN ({})",
        expected_statuses
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut query = sqlx::query_scalar::<_, JobId>(&query).bind(job_id);
    for status in expected_statuses {
        query = query.bind(*status);
    }

    let job_matches = query.fetch_optional(&mut **tx).await.map_err(db_err)?;
    if job_matches.is_none() {
        return Ok(RepositoryError::Conflict(ConflictKind::JobNotActive));
    }

    if require_workspace_binding {
        let workspace_id: Option<ingot_domain::ids::WorkspaceId> = sqlx::query_scalar(
            "SELECT workspace_id
             FROM jobs
             WHERE id = ?",
        )
        .bind(job_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(db_err)?
        .flatten();
        if workspace_id.is_none() {
            return Ok(RepositoryError::Conflict(ConflictKind::JobMissingWorkspace));
        }
    }

    Ok(RepositoryError::Conflict(ConflictKind::JobUpdateConflict))
}
