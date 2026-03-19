use chrono::Utc;
use ingot_domain::activity::{Activity, ActivityEventType};
use ingot_domain::ids::{ActivityId, ItemId, ItemRevisionId, JobId, ProjectId, WorkspaceId};
use ingot_domain::item::{ApprovalState, EscalationReason, Item};
use ingot_domain::job::{Job, JobStatus, OutcomeClass};
use ingot_domain::ports::{
    ActivityRepository, FinishJobNonSuccessParams, JobRepository, RepositoryError,
    WorkspaceRepository,
};
use ingot_domain::workspace::WorkspaceStatus;
use ingot_workflow::step;

use crate::UseCaseError;
use crate::dispatch::{failure_escalation_reason, failure_status};

/// Result returned after a job termination (cancel, fail, expire).
/// Callers use this to know what infrastructure side effects to perform
/// (e.g., refresh_revision_context, workspace filesystem cleanup).
#[derive(Debug, Clone)]
pub struct JobTerminationResult {
    pub job_id: JobId,
    pub project_id: ProjectId,
    pub item_id: ItemId,
    pub revision_id: ItemRevisionId,
    pub released_workspace_id: Option<WorkspaceId>,
    pub escalation_reason: Option<EscalationReason>,
}

/// Persist a terminal non-success outcome and append the corresponding activities.
/// This does not touch workspace state, so callers remain responsible for any
/// filesystem or workspace-status cleanup that must happen first.
#[allow(clippy::too_many_arguments)]
pub async fn record_non_success_outcome<J, A>(
    job_repo: &J,
    activity_repo: &A,
    job: &Job,
    item: &Item,
    outcome_class: OutcomeClass,
    error_code: Option<String>,
    error_message: Option<String>,
    revision_stale_message: &'static str,
) -> Result<JobTerminationResult, UseCaseError>
where
    J: JobRepository,
    A: ActivityRepository,
{
    let status = failure_status(outcome_class).ok_or_else(|| {
        UseCaseError::ProtocolViolation(
            "Failure endpoints only accept transient_failure, terminal_failure, protocol_violation, or cancelled".into(),
        )
    })?;
    let escalation_reason = failure_escalation_reason(job, outcome_class);

    job_repo
        .finish_non_success(FinishJobNonSuccessParams {
            job_id: job.id,
            item_id: item.id,
            expected_item_revision_id: job.item_revision_id,
            status,
            outcome_class: Some(outcome_class),
            error_code: error_code.clone(),
            error_message,
            escalation_reason,
        })
        .await
        .map_err(|error| map_finish_non_success_error(error, revision_stale_message))?;

    append_non_success_activities(
        activity_repo,
        job.project_id,
        job.id,
        item.id,
        outcome_class,
        error_code.as_deref(),
        escalation_reason,
    )
    .await?;

    Ok(JobTerminationResult {
        job_id: job.id,
        project_id: job.project_id,
        item_id: item.id,
        revision_id: job.item_revision_id,
        released_workspace_id: None,
        escalation_reason,
    })
}

/// Append the standard job-completed activity payload used after successful job completion.
pub async fn append_job_completed_activity<A>(
    activity_repo: &A,
    project_id: ProjectId,
    job_id: JobId,
    item_id: ItemId,
    outcome_class: OutcomeClass,
) -> Result<(), UseCaseError>
where
    A: ActivityRepository,
{
    activity_repo
        .append(&Activity {
            id: ActivityId::new(),
            project_id,
            event_type: ActivityEventType::JobCompleted,
            entity_type: "job".into(),
            entity_id: job_id.to_string(),
            payload: serde_json::json!({
                "item_id": item_id,
                "outcome": outcome_class_name(outcome_class),
            }),
            created_at: Utc::now(),
        })
        .await?;
    Ok(())
}

/// Append ApprovalRequested when a clean integrated validation transitions the item
/// into pending approval.
pub async fn append_approval_requested_activity_if_needed<A>(
    activity_repo: &A,
    item: &Item,
    job: &Job,
    outcome_class: OutcomeClass,
) -> Result<bool, UseCaseError>
where
    A: ActivityRepository,
{
    if outcome_class != OutcomeClass::Clean
        || job.step_id != step::VALIDATE_INTEGRATED
        || item.approval_state != ApprovalState::Pending
    {
        return Ok(false);
    }

    activity_repo
        .append(&Activity {
            id: ActivityId::new(),
            project_id: job.project_id,
            event_type: ActivityEventType::ApprovalRequested,
            entity_type: "item".into(),
            entity_id: item.id.to_string(),
            payload: serde_json::json!({ "job_id": job.id }),
            created_at: Utc::now(),
        })
        .await?;

    Ok(true)
}

/// Append ItemEscalationCleared after a successful retry clears escalation on the current revision.
pub async fn append_item_escalation_cleared_activity_if_needed<A>(
    activity_repo: &A,
    original_item: &Item,
    current_item: &Item,
    job: &Job,
) -> Result<bool, UseCaseError>
where
    A: ActivityRepository,
{
    if !original_item.escalation.is_escalated()
        || current_item.current_revision_id != job.item_revision_id
        || current_item.escalation.is_escalated()
    {
        return Ok(false);
    }

    activity_repo
        .append(&Activity {
            id: ActivityId::new(),
            project_id: job.project_id,
            event_type: ActivityEventType::ItemEscalationCleared,
            entity_type: "item".into(),
            entity_id: original_item.id.to_string(),
            payload: serde_json::json!({ "reason": "successful_retry", "job_id": job.id }),
            created_at: Utc::now(),
        })
        .await?;

    Ok(true)
}

/// Cancel an active job. Sets status to Cancelled, releases workspace (to `target_workspace_status`),
/// and appends a JobCancelled activity.
pub async fn cancel_job<J, W, A>(
    job_repo: &J,
    workspace_repo: &W,
    activity_repo: &A,
    job: &Job,
    item: &Item,
    cancel_reason: &str,
    target_workspace_status: WorkspaceStatus,
) -> Result<JobTerminationResult, UseCaseError>
where
    J: JobRepository,
    W: WorkspaceRepository,
    A: ActivityRepository,
{
    if !job.state.is_active() {
        return Err(UseCaseError::JobNotActive);
    }

    job_repo
        .finish_non_success(FinishJobNonSuccessParams {
            job_id: job.id,
            item_id: item.id,
            expected_item_revision_id: job.item_revision_id,
            status: JobStatus::Cancelled,
            outcome_class: Some(OutcomeClass::Cancelled),
            error_code: Some(cancel_reason.into()),
            error_message: None,
            escalation_reason: None,
        })
        .await
        .map_err(|error| {
            map_finish_non_success_error(
                error,
                "job cancellation does not match the current item revision",
            )
        })?;

    let released_workspace_id = release_workspace(
        workspace_repo,
        job.state.workspace_id(),
        target_workspace_status,
    )
    .await?;

    activity_repo
        .append(&Activity {
            id: ActivityId::new(),
            project_id: job.project_id,
            event_type: ActivityEventType::JobCancelled,
            entity_type: "job".into(),
            entity_id: job.id.to_string(),
            payload: serde_json::json!({ "item_id": item.id, "reason": cancel_reason }),
            created_at: Utc::now(),
        })
        .await?;

    Ok(JobTerminationResult {
        job_id: job.id,
        project_id: job.project_id,
        item_id: item.id,
        revision_id: job.item_revision_id,
        released_workspace_id,
        escalation_reason: None,
    })
}

/// Fail an active job with a given outcome class. Sets the appropriate terminal status,
/// releases workspace, appends JobFailed + optional ItemEscalated activities.
#[allow(clippy::too_many_arguments)]
pub async fn fail_job<J, W, A>(
    job_repo: &J,
    workspace_repo: &W,
    activity_repo: &A,
    job: &Job,
    item: &Item,
    outcome_class: OutcomeClass,
    error_code: Option<String>,
    error_message: Option<String>,
    target_workspace_status: WorkspaceStatus,
) -> Result<JobTerminationResult, UseCaseError>
where
    J: JobRepository,
    W: WorkspaceRepository,
    A: ActivityRepository,
{
    if !job.state.is_active() {
        return Err(UseCaseError::JobNotActive);
    }

    let mut result = record_non_success_outcome(
        job_repo,
        activity_repo,
        job,
        item,
        outcome_class,
        error_code,
        error_message,
        "job failure does not match the current item revision",
    )
    .await?;

    result.released_workspace_id = release_workspace(
        workspace_repo,
        job.state.workspace_id(),
        target_workspace_status,
    )
    .await?;

    Ok(result)
}

/// Expire an active job. Sets status to Expired with TransientFailure outcome.
pub async fn expire_job<J, W, A>(
    job_repo: &J,
    workspace_repo: &W,
    activity_repo: &A,
    job: &Job,
    item: &Item,
    target_workspace_status: WorkspaceStatus,
) -> Result<JobTerminationResult, UseCaseError>
where
    J: JobRepository,
    W: WorkspaceRepository,
    A: ActivityRepository,
{
    if !job.state.is_active() {
        return Err(UseCaseError::JobNotActive);
    }

    job_repo
        .finish_non_success(FinishJobNonSuccessParams {
            job_id: job.id,
            item_id: item.id,
            expected_item_revision_id: job.item_revision_id,
            status: JobStatus::Expired,
            outcome_class: Some(OutcomeClass::TransientFailure),
            error_code: Some("job_expired".into()),
            error_message: None,
            escalation_reason: None,
        })
        .await
        .map_err(|error| {
            map_finish_non_success_error(
                error,
                "job expiration does not match the current item revision",
            )
        })?;

    let released_workspace_id = release_workspace(
        workspace_repo,
        job.state.workspace_id(),
        target_workspace_status,
    )
    .await?;

    activity_repo
        .append(&Activity {
            id: ActivityId::new(),
            project_id: job.project_id,
            event_type: ActivityEventType::JobFailed,
            entity_type: "job".into(),
            entity_id: job.id.to_string(),
            payload: serde_json::json!({ "item_id": item.id, "error_code": "job_expired" }),
            created_at: Utc::now(),
        })
        .await?;

    Ok(JobTerminationResult {
        job_id: job.id,
        project_id: job.project_id,
        item_id: item.id,
        revision_id: job.item_revision_id,
        released_workspace_id,
        escalation_reason: None,
    })
}

/// Release a workspace after job termination: clear current_job_id, set status.
async fn release_workspace<W: WorkspaceRepository>(
    workspace_repo: &W,
    workspace_id: Option<WorkspaceId>,
    target_status: WorkspaceStatus,
) -> Result<Option<WorkspaceId>, UseCaseError> {
    let Some(workspace_id) = workspace_id else {
        return Ok(None);
    };
    let mut workspace = workspace_repo.get(workspace_id).await?;
    workspace.release_to(target_status, Utc::now());
    workspace_repo.update(&workspace).await?;
    Ok(Some(workspace_id))
}

pub(crate) fn map_finish_non_success_error(
    error: RepositoryError,
    revision_stale_message: &'static str,
) -> UseCaseError {
    match error {
        RepositoryError::Conflict(message) if message == "job_not_active" => {
            UseCaseError::JobNotActive
        }
        RepositoryError::Conflict(message) if message == "job_revision_stale" => {
            UseCaseError::ProtocolViolation(revision_stale_message.into())
        }
        other => UseCaseError::Repository(other),
    }
}

async fn append_non_success_activities<A>(
    activity_repo: &A,
    project_id: ProjectId,
    job_id: JobId,
    item_id: ItemId,
    outcome_class: OutcomeClass,
    error_code: Option<&str>,
    escalation_reason: Option<EscalationReason>,
) -> Result<(), UseCaseError>
where
    A: ActivityRepository,
{
    if escalation_reason.is_some() {
        activity_repo
            .append(&Activity {
                id: ActivityId::new(),
                project_id,
                event_type: ActivityEventType::ItemEscalated,
                entity_type: "item".into(),
                entity_id: item_id.to_string(),
                payload: serde_json::json!({ "reason": escalation_reason }),
                created_at: Utc::now(),
            })
            .await?;
    }

    let event_type = if outcome_class == OutcomeClass::Cancelled {
        ActivityEventType::JobCancelled
    } else {
        ActivityEventType::JobFailed
    };
    activity_repo
        .append(&Activity {
            id: ActivityId::new(),
            project_id,
            event_type,
            entity_type: "job".into(),
            entity_id: job_id.to_string(),
            payload: serde_json::json!({ "item_id": item_id, "error_code": error_code }),
            created_at: Utc::now(),
        })
        .await?;

    Ok(())
}

fn outcome_class_name(outcome_class: OutcomeClass) -> &'static str {
    match outcome_class {
        OutcomeClass::Clean => "clean",
        OutcomeClass::Findings => "findings",
        OutcomeClass::TransientFailure => "transient_failure",
        OutcomeClass::TerminalFailure => "terminal_failure",
        OutcomeClass::ProtocolViolation => "protocol_violation",
        OutcomeClass::Cancelled => "cancelled",
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use ingot_domain::activity::{Activity, ActivityEventType};
    use ingot_domain::ids::{ItemId, ItemRevisionId, JobId, ProjectId, WorkspaceId};
    use ingot_domain::item::{ApprovalState, Escalation};
    use ingot_domain::job::{JobStatus, OutcomeClass};
    use ingot_domain::ports::{RepositoryError, StartJobExecutionParams};
    use ingot_domain::workspace::{Workspace, WorkspaceKind, WorkspaceStatus};
    use ingot_test_support::fixtures::{JobBuilder, WorkspaceBuilder, nil_item};
    use uuid::Uuid;

    use super::*;

    #[tokio::test]
    async fn cancel_job_maps_job_not_active_conflict() {
        let job = test_job(None);
        let item = nil_item();
        let job_repo = FakeJobRepository::with_finish_error(RepositoryError::Conflict(
            "job_not_active".into(),
        ));

        let result = cancel_job(
            &job_repo,
            &FakeWorkspaceRepository::default(),
            &FakeActivityRepository::default(),
            &job,
            &item,
            "operator_cancelled",
            WorkspaceStatus::Ready,
        )
        .await;

        assert!(matches!(result, Err(UseCaseError::JobNotActive)));
    }

    #[tokio::test]
    async fn cancel_job_maps_revision_stale_conflict() {
        let job = test_job(None);
        let item = nil_item();
        let job_repo = FakeJobRepository::with_finish_error(RepositoryError::Conflict(
            "job_revision_stale".into(),
        ));

        let result = cancel_job(
            &job_repo,
            &FakeWorkspaceRepository::default(),
            &FakeActivityRepository::default(),
            &job,
            &item,
            "operator_cancelled",
            WorkspaceStatus::Ready,
        )
        .await;

        assert!(matches!(
            result,
            Err(UseCaseError::ProtocolViolation(message))
                if message == "job cancellation does not match the current item revision"
        ));
    }

    #[tokio::test]
    async fn fail_job_maps_revision_stale_conflict() {
        let job = test_job(None);
        let item = nil_item();
        let job_repo = FakeJobRepository::with_finish_error(RepositoryError::Conflict(
            "job_revision_stale".into(),
        ));

        let result = fail_job(
            &job_repo,
            &FakeWorkspaceRepository::default(),
            &FakeActivityRepository::default(),
            &job,
            &item,
            OutcomeClass::TransientFailure,
            Some("agent_crashed".into()),
            Some("agent crashed".into()),
            WorkspaceStatus::Ready,
        )
        .await;

        assert!(matches!(
            result,
            Err(UseCaseError::ProtocolViolation(message))
                if message == "job failure does not match the current item revision"
        ));
    }

    #[tokio::test]
    async fn expire_job_maps_revision_stale_conflict() {
        let job = test_job(None);
        let item = nil_item();
        let job_repo = FakeJobRepository::with_finish_error(RepositoryError::Conflict(
            "job_revision_stale".into(),
        ));

        let result = expire_job(
            &job_repo,
            &FakeWorkspaceRepository::default(),
            &FakeActivityRepository::default(),
            &job,
            &item,
            WorkspaceStatus::Ready,
        )
        .await;

        assert!(matches!(
            result,
            Err(UseCaseError::ProtocolViolation(message))
                if message == "job expiration does not match the current item revision"
        ));
    }

    #[tokio::test]
    async fn cancel_job_releases_workspace_after_success() {
        let workspace = test_workspace();
        let job = test_job(Some(workspace.id));
        let item = nil_item();
        let workspace_repo = FakeWorkspaceRepository::with_workspace(workspace);

        let result = cancel_job(
            &FakeJobRepository::default(),
            &workspace_repo,
            &FakeActivityRepository::default(),
            &job,
            &item,
            "operator_cancelled",
            WorkspaceStatus::Ready,
        )
        .await
        .expect("cancellation should succeed");

        let updated_workspace = workspace_repo.last_updated().expect("updated workspace");
        assert_eq!(result.released_workspace_id, Some(updated_workspace.id));
        assert_eq!(updated_workspace.state.current_job_id(), None);
        assert_eq!(updated_workspace.state.status(), WorkspaceStatus::Ready);
    }

    #[tokio::test]
    async fn record_non_success_outcome_appends_failure_and_escalation_activities() {
        let job = test_job(None);
        let item = nil_item();
        let activity_repo = FakeActivityRepository::default();

        let result = record_non_success_outcome(
            &FakeJobRepository::default(),
            &activity_repo,
            &job,
            &item,
            OutcomeClass::TerminalFailure,
            Some("agent_launch_failed".into()),
            Some("boom".into()),
            "job failure does not match the current item revision",
        )
        .await
        .expect("failure should be recorded");

        assert_eq!(result.escalation_reason, Some(EscalationReason::StepFailed));

        let activities = activity_repo.appended();
        assert!(activities.iter().any(|activity| {
            activity.event_type == ActivityEventType::JobFailed
                && activity.entity_id == job.id.to_string()
        }));
        assert!(activities.iter().any(|activity| {
            activity.event_type == ActivityEventType::ItemEscalated
                && activity.entity_id == item.id.to_string()
        }));
    }

    #[tokio::test]
    async fn append_approval_requested_activity_if_needed_requires_pending_clean_validation() {
        let mut item = nil_item();
        item.approval_state = ApprovalState::Pending;
        let job = JobBuilder::new(
            ProjectId::from_uuid(Uuid::nil()),
            item.id,
            item.current_revision_id,
            step::VALIDATE_INTEGRATED,
        )
        .id(JobId::from_uuid(Uuid::nil()))
        .build();
        let activity_repo = FakeActivityRepository::default();

        let appended = append_approval_requested_activity_if_needed(
            &activity_repo,
            &item,
            &job,
            OutcomeClass::Clean,
        )
        .await
        .expect("approval activity should succeed");

        assert!(appended);
        let activities = activity_repo.appended();
        assert!(activities.iter().any(|activity| {
            activity.event_type == ActivityEventType::ApprovalRequested
                && activity.entity_id == item.id.to_string()
        }));
    }

    #[tokio::test]
    async fn append_item_escalation_cleared_activity_if_needed_appends_when_current_item_cleared() {
        let mut original_item = nil_item();
        original_item.escalation = Escalation::OperatorRequired {
            reason: EscalationReason::StepFailed,
        };
        let mut current_item = original_item.clone();
        current_item.escalation = Escalation::None;
        let job = test_job(None);
        let activity_repo = FakeActivityRepository::default();

        let appended = append_item_escalation_cleared_activity_if_needed(
            &activity_repo,
            &original_item,
            &current_item,
            &job,
        )
        .await
        .expect("escalation clear activity should succeed");

        assert!(appended);
        let activities = activity_repo.appended();
        assert!(activities.iter().any(|activity| {
            activity.event_type == ActivityEventType::ItemEscalationCleared
                && activity.entity_id == original_item.id.to_string()
        }));
    }

    fn test_job(workspace_id: Option<WorkspaceId>) -> Job {
        let nil = Uuid::nil();
        let mut builder = JobBuilder::new(
            ProjectId::from_uuid(nil),
            ItemId::from_uuid(nil),
            ItemRevisionId::from_uuid(nil),
            "author_initial",
        )
        .id(JobId::from_uuid(nil))
        .status(JobStatus::Running);
        if let Some(workspace_id) = workspace_id {
            builder = builder.workspace_id(workspace_id);
        }
        builder.build()
    }

    fn test_workspace() -> Workspace {
        WorkspaceBuilder::new(ProjectId::from_uuid(Uuid::nil()), WorkspaceKind::Authoring)
            .id(WorkspaceId::from_uuid(Uuid::nil()))
            .status(WorkspaceStatus::Busy)
            .current_job_id(JobId::from_uuid(Uuid::nil()))
            .build()
    }

    #[derive(Clone, Default)]
    struct FakeJobRepository {
        finish_error: Arc<Mutex<Option<RepositoryError>>>,
    }

    impl FakeJobRepository {
        fn with_finish_error(error: RepositoryError) -> Self {
            Self {
                finish_error: Arc::new(Mutex::new(Some(error))),
            }
        }
    }

    impl JobRepository for FakeJobRepository {
        async fn list_by_project(
            &self,
            _project_id: ProjectId,
        ) -> Result<Vec<Job>, RepositoryError> {
            async { unreachable!("unused in test") }.await
        }

        async fn list_by_revision(
            &self,
            _revision_id: ItemRevisionId,
        ) -> Result<Vec<Job>, RepositoryError> {
            async { unreachable!("unused in test") }.await
        }

        async fn get(&self, _id: JobId) -> Result<Job, RepositoryError> {
            async { unreachable!("unused in test") }.await
        }

        async fn create(&self, _job: &Job) -> Result<(), RepositoryError> {
            async { unreachable!("unused in test") }.await
        }

        async fn update(&self, _job: &Job) -> Result<(), RepositoryError> {
            async { unreachable!("unused in test") }.await
        }

        async fn find_active_for_revision(
            &self,
            _revision_id: ItemRevisionId,
        ) -> Result<Option<Job>, RepositoryError> {
            async { unreachable!("unused in test") }.await
        }

        async fn list_by_item(&self, _item_id: ItemId) -> Result<Vec<Job>, RepositoryError> {
            async { unreachable!("unused in test") }.await
        }

        async fn list_queued(&self, _limit: u32) -> Result<Vec<Job>, RepositoryError> {
            async { unreachable!("unused in test") }.await
        }

        async fn list_active(&self) -> Result<Vec<Job>, RepositoryError> {
            async { unreachable!("unused in test") }.await
        }

        async fn start_execution(
            &self,
            _params: StartJobExecutionParams,
        ) -> Result<(), RepositoryError> {
            async { unreachable!("unused in test") }.await
        }

        async fn heartbeat_execution(
            &self,
            _job_id: JobId,
            _item_id: ItemId,
            _revision_id: ItemRevisionId,
            _lease_owner_id: &str,
            _lease_expires_at: chrono::DateTime<Utc>,
        ) -> Result<(), RepositoryError> {
            async { unreachable!("unused in test") }.await
        }

        async fn finish_non_success(
            &self,
            _params: FinishJobNonSuccessParams,
        ) -> Result<(), RepositoryError> {
            let finish_error = self.finish_error.clone();
            if let Some(error) = finish_error.lock().expect("finish error lock").take() {
                return Err(error);
            }
            Ok(())
        }
    }

    #[derive(Clone, Default)]
    struct FakeWorkspaceRepository {
        state: Arc<Mutex<FakeWorkspaceRepositoryState>>,
    }

    #[derive(Default)]
    struct FakeWorkspaceRepositoryState {
        workspace: Option<Workspace>,
        updated: Option<Workspace>,
    }

    impl FakeWorkspaceRepository {
        fn with_workspace(workspace: Workspace) -> Self {
            Self {
                state: Arc::new(Mutex::new(FakeWorkspaceRepositoryState {
                    workspace: Some(workspace),
                    updated: None,
                })),
            }
        }

        fn last_updated(&self) -> Option<Workspace> {
            self.state
                .lock()
                .expect("workspace state lock")
                .updated
                .clone()
        }
    }

    impl WorkspaceRepository for FakeWorkspaceRepository {
        async fn list_by_project(
            &self,
            _project_id: ProjectId,
        ) -> Result<Vec<Workspace>, RepositoryError> {
            async { unreachable!("unused in test") }.await
        }

        async fn get(&self, _id: WorkspaceId) -> Result<Workspace, RepositoryError> {
            let workspace = self
                .state
                .lock()
                .expect("workspace state lock")
                .workspace
                .clone();
            workspace.ok_or(RepositoryError::NotFound)
        }

        async fn create(&self, _workspace: &Workspace) -> Result<(), RepositoryError> {
            async { unreachable!("unused in test") }.await
        }

        async fn update(&self, workspace: &Workspace) -> Result<(), RepositoryError> {
            let state = self.state.clone();
            let workspace = workspace.clone();
            let mut state = state.lock().expect("workspace state lock");
            state.updated = Some(workspace.clone());
            state.workspace = Some(workspace);
            Ok(())
        }

        async fn find_authoring_for_revision(
            &self,
            _revision_id: ItemRevisionId,
        ) -> Result<Option<Workspace>, RepositoryError> {
            async { unreachable!("unused in test") }.await
        }

        async fn list_by_item(&self, _item_id: ItemId) -> Result<Vec<Workspace>, RepositoryError> {
            async { unreachable!("unused in test") }.await
        }
    }

    #[derive(Clone, Default)]
    struct FakeActivityRepository {
        activities: Arc<Mutex<Vec<Activity>>>,
    }

    impl FakeActivityRepository {
        fn appended(&self) -> Vec<Activity> {
            self.activities.lock().expect("activity lock").clone()
        }
    }

    impl ActivityRepository for FakeActivityRepository {
        async fn append(&self, activity: &Activity) -> Result<(), RepositoryError> {
            self.activities
                .lock()
                .expect("activity lock")
                .push(activity.clone());
            Ok(())
        }

        async fn list_by_project(
            &self,
            _project_id: ProjectId,
            _limit: u32,
            _offset: u32,
        ) -> Result<Vec<Activity>, RepositoryError> {
            async { unreachable!("unused in test") }.await
        }
    }
}
