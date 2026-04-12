use chrono::Utc;
use ingot_domain::activity::{ActivityEventType, ActivitySubject};
use ingot_domain::convergence::CheckoutAdoptionState;
use ingot_domain::ids::ActivityId;
use ingot_domain::item::EscalationReason;
use ingot_domain::ports::{
    ConflictKind, FinalizationCheckoutAdoptionSucceededMutation, FinalizationMutation,
    FinalizationRepository, FinalizationTargetRefAdvancedMutation, RepositoryError,
};
use sqlx::{Row, Sqlite, Transaction};

use super::helpers::{db_err, db_write_err, item_revision_is_stale, json_err};
use crate::db::Database;

impl Database {
    pub async fn apply_finalization_mutation(
        &self,
        mutation: FinalizationMutation,
    ) -> Result<(), RepositoryError> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        match &mutation {
            FinalizationMutation::TargetRefAdvanced(mutation) => {
                apply_target_ref_advanced(&mut tx, mutation).await?;
            }
            FinalizationMutation::CheckoutAdoptionSucceeded(mutation) => {
                apply_checkout_adoption_succeeded(&mut tx, mutation).await?;
            }
        }
        tx.commit().await.map_err(db_err)?;
        Ok(())
    }
}

impl FinalizationRepository for Database {
    async fn apply_finalization_mutation(
        &self,
        mutation: FinalizationMutation,
    ) -> Result<(), RepositoryError> {
        Database::apply_finalization_mutation(self, mutation).await
    }
}

async fn apply_target_ref_advanced(
    tx: &mut Transaction<'_, Sqlite>,
    mutation: &FinalizationTargetRefAdvancedMutation,
) -> Result<(), RepositoryError> {
    if item_revision_is_stale(tx, mutation.item_id, mutation.expected_item_revision_id).await? {
        return Err(RepositoryError::Conflict(ConflictKind::Other(
            "finalization_item_revision_stale".into(),
        )));
    }

    let now = Utc::now();
    let convergence_row = sqlx::query(
        "SELECT status, integration_workspace_id
         FROM convergences
         WHERE id = ?
           AND item_id = ?
           AND item_revision_id = ?",
    )
    .bind(mutation.convergence_id)
    .bind(mutation.item_id)
    .bind(mutation.expected_item_revision_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db_err)?
    .ok_or(RepositoryError::NotFound)?;

    let previous_status: ingot_domain::convergence::ConvergenceStatus =
        convergence_row.try_get("status").map_err(db_err)?;
    let integration_workspace_id: Option<ingot_domain::ids::WorkspaceId> = convergence_row
        .try_get("integration_workspace_id")
        .map_err(db_err)?;

    let item_row = sqlx::query(
        "SELECT lifecycle_state, escalation_reason
         FROM items
         WHERE id = ?
           AND current_revision_id = ?",
    )
    .bind(mutation.item_id)
    .bind(mutation.expected_item_revision_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db_err)?
    .ok_or(RepositoryError::NotFound)?;

    let lifecycle_state: String = item_row.try_get("lifecycle_state").map_err(db_err)?;
    let escalation_reason: Option<EscalationReason> =
        item_row.try_get("escalation_reason").map_err(db_err)?;
    let item_is_done = lifecycle_state == "done";
    let checkout_sync_escalated = escalation_reason == Some(EscalationReason::CheckoutSyncBlocked);

    let convergence_result = sqlx::query(
        "UPDATE convergences
         SET status = 'finalized',
             final_target_commit_oid = ?,
             checkout_adoption_state = ?,
             checkout_adoption_message = ?,
             checkout_adoption_updated_at = ?,
             checkout_adoption_synced_at = ?,
             completed_at = CASE WHEN status = 'finalized' THEN completed_at ELSE ? END
         WHERE id = ?",
    )
    .bind(mutation.final_target_commit_oid.clone())
    .bind(mutation.checkout_adoption.state)
    .bind(mutation.checkout_adoption.blocker_message.as_deref())
    .bind(mutation.checkout_adoption.updated_at)
    .bind(mutation.checkout_adoption.synced_at)
    .bind(now)
    .bind(mutation.convergence_id)
    .execute(&mut **tx)
    .await
    .map_err(db_write_err)?;

    if convergence_result.rows_affected() != 1 {
        return Err(RepositoryError::NotFound);
    }

    let git_op_result = sqlx::query(
        "UPDATE git_operations
         SET status = CASE
                 WHEN status = 'planned' THEN 'applied'
                 WHEN status = 'applied' THEN 'applied'
                 ELSE status
             END,
             completed_at = CASE
                 WHEN status = 'planned' THEN ?
                 ELSE completed_at
             END
         WHERE id = ?
           AND entity_type = 'convergence'
           AND entity_id = ?
           AND operation_kind = 'finalize_target_ref'",
    )
    .bind(now)
    .bind(mutation.git_operation_id)
    .bind(mutation.convergence_id)
    .execute(&mut **tx)
    .await
    .map_err(db_write_err)?;

    if git_op_result.rows_affected() != 1 {
        return Err(RepositoryError::NotFound);
    }

    sqlx::query(
        "UPDATE convergence_queue_entries
         SET status = 'released',
             released_at = COALESCE(released_at, ?),
             updated_at = ?
         WHERE item_revision_id = ?
           AND status IN ('queued', 'head')",
    )
    .bind(now)
    .bind(now)
    .bind(mutation.expected_item_revision_id)
    .execute(&mut **tx)
    .await
    .map_err(db_write_err)?;

    if let Some(workspace_id) = integration_workspace_id {
        sqlx::query(
            "UPDATE workspaces
             SET status = 'abandoned',
                 current_job_id = NULL,
                 updated_at = ?
             WHERE id = ?
               AND status != 'abandoned'",
        )
        .bind(now)
        .bind(workspace_id)
        .execute(&mut **tx)
        .await
        .map_err(db_write_err)?;
    }

    match mutation.checkout_adoption.state {
        CheckoutAdoptionState::Blocked if !item_is_done => {
            if !checkout_sync_escalated {
                sqlx::query(
                    "UPDATE items
                     SET escalation_state = 'operator_required',
                         escalation_reason = 'checkout_sync_blocked',
                         updated_at = ?
                     WHERE id = ?
                       AND current_revision_id = ?",
                )
                .bind(now)
                .bind(mutation.item_id)
                .bind(mutation.expected_item_revision_id)
                .execute(&mut **tx)
                .await
                .map_err(db_write_err)?;

                insert_activity(
                    tx,
                    mutation.project_id,
                    ActivityEventType::CheckoutSyncBlocked,
                    ActivitySubject::Item(mutation.item_id),
                    serde_json::json!({
                        "message": mutation.checkout_adoption.blocker_message.clone().unwrap_or_default(),
                    }),
                )
                .await?;
                insert_activity(
                    tx,
                    mutation.project_id,
                    ActivityEventType::ItemEscalated,
                    ActivitySubject::Item(mutation.item_id),
                    serde_json::json!({ "reason": EscalationReason::CheckoutSyncBlocked }),
                )
                .await?;
            }
        }
        _ if checkout_sync_escalated && !item_is_done => {
            sqlx::query(
                "UPDATE items
                 SET escalation_state = 'none',
                     escalation_reason = NULL,
                     updated_at = ?
                 WHERE id = ?
                   AND current_revision_id = ?",
            )
            .bind(now)
            .bind(mutation.item_id)
            .bind(mutation.expected_item_revision_id)
            .execute(&mut **tx)
            .await
            .map_err(db_write_err)?;

            insert_activity(
                tx,
                mutation.project_id,
                ActivityEventType::CheckoutSyncCleared,
                ActivitySubject::Item(mutation.item_id),
                serde_json::json!({}),
            )
            .await?;
            insert_activity(
                tx,
                mutation.project_id,
                ActivityEventType::ItemEscalationCleared,
                ActivitySubject::Item(mutation.item_id),
                serde_json::json!({ "reason": "checkout_sync_ready" }),
            )
            .await?;
        }
        _ => {}
    }

    if previous_status != ingot_domain::convergence::ConvergenceStatus::Finalized {
        insert_activity(
            tx,
            mutation.project_id,
            ActivityEventType::ConvergenceFinalized,
            ActivitySubject::Convergence(mutation.convergence_id),
            serde_json::json!({ "item_id": mutation.item_id }),
        )
        .await?;
    }

    Ok(())
}

async fn apply_checkout_adoption_succeeded(
    tx: &mut Transaction<'_, Sqlite>,
    mutation: &FinalizationCheckoutAdoptionSucceededMutation,
) -> Result<(), RepositoryError> {
    if item_revision_is_stale(tx, mutation.item_id, mutation.expected_item_revision_id).await? {
        return Err(RepositoryError::Conflict(ConflictKind::Other(
            "finalization_item_revision_stale".into(),
        )));
    }

    let convergence_row = sqlx::query(
        "SELECT status
         FROM convergences
         WHERE id = ?
           AND item_id = ?
           AND item_revision_id = ?",
    )
    .bind(mutation.convergence_id)
    .bind(mutation.item_id)
    .bind(mutation.expected_item_revision_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db_err)?
    .ok_or(RepositoryError::NotFound)?;

    let convergence_status: ingot_domain::convergence::ConvergenceStatus =
        convergence_row.try_get("status").map_err(db_err)?;
    if convergence_status != ingot_domain::convergence::ConvergenceStatus::Finalized {
        return Err(RepositoryError::Conflict(ConflictKind::Other(
            "finalization_requires_finalized_convergence".into(),
        )));
    }

    let item_row = sqlx::query(
        "SELECT lifecycle_state, escalation_reason
         FROM items
         WHERE id = ?
           AND current_revision_id = ?",
    )
    .bind(mutation.item_id)
    .bind(mutation.expected_item_revision_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db_err)?
    .ok_or(RepositoryError::NotFound)?;

    let _item_was_done: String = item_row.try_get("lifecycle_state").map_err(db_err)?;
    let escalation_reason: Option<EscalationReason> =
        item_row.try_get("escalation_reason").map_err(db_err)?;
    let checkout_sync_escalated = escalation_reason == Some(EscalationReason::CheckoutSyncBlocked);

    let git_op_row = sqlx::query(
        "SELECT status
         FROM git_operations
         WHERE id = ?
           AND entity_type = 'convergence'
           AND entity_id = ?
           AND operation_kind = 'finalize_target_ref'",
    )
    .bind(mutation.git_operation_id)
    .bind(mutation.convergence_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db_err)?
    .ok_or(RepositoryError::NotFound)?;

    let previous_git_op_status: ingot_domain::git_operation::GitOperationStatus =
        git_op_row.try_get("status").map_err(db_err)?;

    sqlx::query(
        "UPDATE convergences
         SET checkout_adoption_state = 'synced',
             checkout_adoption_message = NULL,
             checkout_adoption_updated_at = ?,
             checkout_adoption_synced_at = ?
         WHERE id = ?",
    )
    .bind(mutation.synced_at)
    .bind(mutation.synced_at)
    .bind(mutation.convergence_id)
    .execute(&mut **tx)
    .await
    .map_err(db_write_err)?;

    sqlx::query(
        "UPDATE git_operations
         SET status = 'reconciled',
             completed_at = ?
         WHERE id = ?",
    )
    .bind(mutation.synced_at)
    .bind(mutation.git_operation_id)
    .execute(&mut **tx)
    .await
    .map_err(db_write_err)?;

    sqlx::query(
        "UPDATE items
         SET lifecycle_state = 'done',
             done_reason = 'completed',
             resolution_source = ?,
             approval_state = ?,
             escalation_state = 'none',
             escalation_reason = NULL,
             updated_at = ?,
             closed_at = ?
         WHERE id = ?
           AND current_revision_id = ?",
    )
    .bind(mutation.resolution_source)
    .bind(mutation.approval_state)
    .bind(mutation.synced_at)
    .bind(mutation.synced_at)
    .bind(mutation.item_id)
    .bind(mutation.expected_item_revision_id)
    .execute(&mut **tx)
    .await
    .map_err(db_write_err)?;

    if checkout_sync_escalated {
        insert_activity(
            tx,
            mutation.project_id,
            ActivityEventType::CheckoutSyncCleared,
            ActivitySubject::Item(mutation.item_id),
            serde_json::json!({}),
        )
        .await?;
        insert_activity(
            tx,
            mutation.project_id,
            ActivityEventType::ItemEscalationCleared,
            ActivitySubject::Item(mutation.item_id),
            serde_json::json!({ "reason": "checkout_sync_ready" }),
        )
        .await?;
    }

    if previous_git_op_status != ingot_domain::git_operation::GitOperationStatus::Reconciled {
        insert_activity(
            tx,
            mutation.project_id,
            ActivityEventType::GitOperationReconciled,
            ActivitySubject::GitOperation(mutation.git_operation_id),
            serde_json::json!({ "operation_kind": "finalize_target_ref" }),
        )
        .await?;
    }

    Ok(())
}

async fn insert_activity(
    tx: &mut Transaction<'_, Sqlite>,
    project_id: ingot_domain::ids::ProjectId,
    event_type: ActivityEventType,
    subject: ActivitySubject,
    payload: serde_json::Value,
) -> Result<(), RepositoryError> {
    sqlx::query(
        "INSERT INTO activity (
            id, project_id, event_type, entity_type, entity_id, payload, created_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(ActivityId::new())
    .bind(project_id)
    .bind(event_type)
    .bind(subject.entity_type())
    .bind(subject.entity_id_string())
    .bind(serde_json::to_string(&payload).map_err(json_err)?)
    .bind(Utc::now())
    .execute(&mut **tx)
    .await
    .map_err(db_write_err)?;
    Ok(())
}
