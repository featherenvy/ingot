use chrono::Utc;
use ingot_domain::activity::{Activity, ActivityEventType, ActivitySubject};
use ingot_domain::git_operation::{GitOperation, GitOperationStatus};
use ingot_domain::ids::{ActivityId, ProjectId};
use ingot_domain::ports::{ActivityRepository, GitOperationRepository};

use crate::UseCaseError;

pub(crate) async fn create_planned<GO, A>(
    git_op_repo: &GO,
    activity_repo: &A,
    operation: &GitOperation,
    project_id: ProjectId,
) -> Result<(), UseCaseError>
where
    GO: GitOperationRepository,
    A: ActivityRepository,
{
    git_op_repo
        .create(operation)
        .await
        .map_err(UseCaseError::Repository)?;
    activity_repo
        .append(&Activity {
            id: ActivityId::new(),
            project_id,
            event_type: ActivityEventType::GitOperationPlanned,
            subject: ActivitySubject::GitOperation(operation.id),
            payload: serde_json::json!({
                "operation_kind": operation.operation_kind(),
                "entity_id": operation.entity.entity_id_string(),
            }),
            created_at: Utc::now(),
        })
        .await
        .map_err(UseCaseError::Repository)
}

pub(crate) async fn mark_applied<GO>(
    git_op_repo: &GO,
    operation: &mut GitOperation,
) -> Result<(), UseCaseError>
where
    GO: GitOperationRepository,
{
    operation.status = GitOperationStatus::Applied;
    operation.completed_at = Some(Utc::now());
    git_op_repo
        .update(operation)
        .await
        .map_err(UseCaseError::Repository)
}
