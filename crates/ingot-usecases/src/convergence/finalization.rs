use chrono::Utc;
use ingot_domain::activity::{Activity, ActivityEventType, ActivitySubject};
use ingot_domain::convergence::{Convergence, FinalizedCheckoutAdoption};
use ingot_domain::convergence_queue::{ConvergenceQueueEntry, ConvergenceQueueEntryStatus};
use ingot_domain::finding::Finding;
use ingot_domain::git_operation::{
    GitOperation, GitOperationEntityRef, GitOperationStatus, OperationPayload,
};
use ingot_domain::ids::ActivityId;
use ingot_domain::job::Job;
use ingot_domain::ports::{ActivityRepository, GitOperationRepository, RepositoryError};
use ingot_domain::project::Project;
use ingot_domain::revision::{ApprovalPolicy, ItemRevision};
use ingot_workflow::{Evaluator, NamedRecommendedAction, RecommendedAction};

use crate::UseCaseError;

use super::types::{
    CheckoutFinalizationReadiness, FinalizationTarget, FinalizePreparedTrigger,
    FinalizeTargetRefResult, PreparedConvergenceFinalizePort,
};

#[must_use]
pub fn should_prepare_convergence(
    item: &ingot_domain::item::Item,
    revision: &ItemRevision,
    jobs: &[Job],
    findings: &[Finding],
    convergences: &[Convergence],
) -> bool {
    Evaluator::new()
        .evaluate(item, revision, jobs, findings, convergences)
        .next_recommended_action
        == RecommendedAction::named(NamedRecommendedAction::PrepareConvergence)
}

#[must_use]
pub fn should_invalidate_prepared_convergence(
    item: &ingot_domain::item::Item,
    revision: &ItemRevision,
    jobs: &[Job],
    findings: &[Finding],
    convergences: &[Convergence],
) -> bool {
    Evaluator::new()
        .evaluate(item, revision, jobs, findings, convergences)
        .next_recommended_action
        == RecommendedAction::named(NamedRecommendedAction::InvalidatePreparedConvergence)
}

#[must_use]
pub fn should_auto_finalize_prepared_convergence(
    item: &ingot_domain::item::Item,
    revision: &ItemRevision,
    jobs: &[Job],
    findings: &[Finding],
    convergences: &[Convergence],
    queue_entry: Option<&ConvergenceQueueEntry>,
) -> bool {
    revision.approval_policy == ApprovalPolicy::NotRequired
        && matches!(
            queue_entry,
            Some(queue_entry) if queue_entry.status == ConvergenceQueueEntryStatus::Head
        )
        && Evaluator::new()
            .evaluate(item, revision, jobs, findings, convergences)
            .next_recommended_action
            == RecommendedAction::named(NamedRecommendedAction::FinalizePreparedConvergence)
}

pub async fn find_or_create_finalize_operation<DB>(
    db: &DB,
    operation: &GitOperation,
) -> Result<GitOperation, UseCaseError>
where
    DB: GitOperationRepository + ActivityRepository,
{
    let convergence_id = match &operation.entity {
        GitOperationEntityRef::Convergence(id) => *id,
        other => {
            return Err(UseCaseError::Internal(format!(
                "expected convergence entity, got {:?}",
                other.entity_type()
            )));
        }
    };

    if let Some(existing) = db
        .find_unresolved_finalize_for_convergence(convergence_id)
        .await
        .map_err(UseCaseError::Repository)?
    {
        return Ok(existing);
    }

    match db.create(operation).await {
        Ok(()) => {
            db.append(&Activity {
                id: ActivityId::new(),
                project_id: operation.project_id,
                event_type: ActivityEventType::GitOperationPlanned,
                subject: ActivitySubject::GitOperation(operation.id),
                payload: serde_json::json!({
                    "operation_kind": operation.operation_kind(),
                    "entity_id": operation.entity.entity_id_string(),
                }),
                created_at: Utc::now(),
            })
            .await
            .map_err(UseCaseError::Repository)?;
            Ok(operation.clone())
        }
        Err(RepositoryError::Conflict(_)) => db
            .find_unresolved_finalize_for_convergence(convergence_id)
            .await
            .map_err(UseCaseError::Repository)?
            .ok_or_else(|| {
                UseCaseError::Internal(
                    "finalize git operation conflict without existing row".into(),
                )
            }),
        Err(other) => Err(UseCaseError::Repository(other)),
    }
}

pub async fn finalize_prepared_convergence<P>(
    port: &P,
    trigger: FinalizePreparedTrigger,
    project: &Project,
    item: &ingot_domain::item::Item,
    revision: &ItemRevision,
    convergence: &Convergence,
    queue_entry: &ConvergenceQueueEntry,
) -> Result<(), UseCaseError>
where
    P: PreparedConvergenceFinalizePort,
{
    let prepared_commit_oid = convergence
        .state
        .prepared_commit_oid()
        .map(ToOwned::to_owned)
        .ok_or(UseCaseError::PreparedConvergenceMissing)?;
    let input_target_commit_oid = convergence
        .state
        .input_target_commit_oid()
        .map(ToOwned::to_owned)
        .ok_or(UseCaseError::PreparedConvergenceMissing)?;

    let planned_operation = GitOperation {
        id: ingot_domain::ids::GitOperationId::new(),
        project_id: project.id,
        entity: GitOperationEntityRef::Convergence(convergence.id),
        payload: OperationPayload::FinalizeTargetRef {
            workspace_id: convergence.state.integration_workspace_id(),
            ref_name: convergence.target_ref.clone(),
            expected_old_oid: input_target_commit_oid,
            new_oid: prepared_commit_oid.clone(),
            commit_oid: Some(prepared_commit_oid.clone()),
        },
        status: GitOperationStatus::Planned,
        created_at: Utc::now(),
        completed_at: None,
    };
    let mut operation = port
        .find_or_create_finalize_operation(&planned_operation)
        .await?;

    if port.finalize_target_ref(project, convergence).await? == FinalizeTargetRefResult::Stale {
        operation.status = GitOperationStatus::Failed;
        operation.completed_at = Some(Utc::now());
        port.update_git_operation(&operation).await?;
        return Err(UseCaseError::PreparedConvergenceStale);
    }

    if operation.status == GitOperationStatus::Planned {
        operation.status = GitOperationStatus::Applied;
        operation.completed_at = Some(Utc::now());
        port.update_git_operation(&operation).await?;
    }

    let target = FinalizationTarget {
        convergence,
        queue_entry,
    };
    let readiness = port
        .checkout_finalization_readiness(project, item, revision, &prepared_commit_oid)
        .await;
    let initial_checkout_adoption = match &readiness {
        Ok(CheckoutFinalizationReadiness::Blocked { message }) => {
            FinalizedCheckoutAdoption::blocked(message.clone(), Utc::now())
        }
        Ok(CheckoutFinalizationReadiness::NeedsSync) => {
            FinalizedCheckoutAdoption::pending(Utc::now())
        }
        Ok(CheckoutFinalizationReadiness::Synced) => FinalizedCheckoutAdoption::synced(Utc::now()),
        Err(_) => FinalizedCheckoutAdoption::pending(Utc::now()),
    };

    port.persist_target_ref_advance(
        trigger,
        project,
        item,
        revision,
        target,
        &operation,
        &initial_checkout_adoption,
    )
    .await?;

    let readiness = readiness?;
    let checkout_adopted = match readiness {
        CheckoutFinalizationReadiness::Blocked { .. } => false,
        CheckoutFinalizationReadiness::NeedsSync => {
            if port
                .sync_checkout_to_prepared_commit(project, revision, &prepared_commit_oid)
                .await
                .is_ok()
            {
                true
            } else {
                if let Ok(CheckoutFinalizationReadiness::Blocked { message }) = port
                    .checkout_finalization_readiness(project, item, revision, &prepared_commit_oid)
                    .await
                {
                    let blocked = FinalizedCheckoutAdoption::blocked(message, Utc::now());
                    port.persist_target_ref_advance(
                        trigger, project, item, revision, target, &operation, &blocked,
                    )
                    .await?;
                }
                false
            }
        }
        CheckoutFinalizationReadiness::Synced => true,
    };

    if checkout_adopted {
        operation.status = GitOperationStatus::Reconciled;
        operation.completed_at = Some(Utc::now());
        port.update_git_operation(&operation).await?;
        port.persist_checkout_adoption_success(
            trigger, project, item, revision, target, &operation,
        )
        .await?;
    }

    Ok(())
}
