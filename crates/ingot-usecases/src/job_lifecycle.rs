use chrono::Utc;
use ingot_domain::activity::{Activity, ActivityEventType, ActivitySubject};
use ingot_domain::ids::{ActivityId, ItemId, ItemRevisionId, JobId, ProjectId, WorkspaceId};
use ingot_domain::item::{EscalationReason, Item};
use ingot_domain::job::{Job, JobStatus, OutcomeClass};
use ingot_domain::ports::{
    ActivityRepository, FinishJobNonSuccessParams, JobRepository, RepositoryError,
    WorkspaceRepository,
};
use ingot_domain::workspace::WorkspaceStatus;

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
            subject: ActivitySubject::Job(job.id),
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
        .map_err(|error| {
            map_finish_non_success_error(
                error,
                "job failure does not match the current item revision",
            )
        })?;

    let released_workspace_id = release_workspace(
        workspace_repo,
        job.state.workspace_id(),
        target_workspace_status,
    )
    .await?;

    if escalation_reason.is_some() {
        activity_repo
            .append(&Activity {
                id: ActivityId::new(),
                project_id: job.project_id,
                event_type: ActivityEventType::ItemEscalated,
                subject: ActivitySubject::Item(item.id),
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
            project_id: job.project_id,
            event_type,
            subject: ActivitySubject::Job(job.id),
            payload: serde_json::json!({ "item_id": item.id, "error_code": error_code }),
            created_at: Utc::now(),
        })
        .await?;

    Ok(JobTerminationResult {
        job_id: job.id,
        project_id: job.project_id,
        item_id: item.id,
        revision_id: job.item_revision_id,
        released_workspace_id,
        escalation_reason,
    })
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
            subject: ActivitySubject::Job(job.id),
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

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use ingot_domain::activity::Activity;
    use ingot_domain::ids::{ItemId, ItemRevisionId, JobId, ProjectId, WorkspaceId};
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
            &FakeActivityRepository,
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
            &FakeActivityRepository,
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
            &FakeActivityRepository,
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
            &FakeActivityRepository,
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
            &FakeActivityRepository,
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
    struct FakeActivityRepository;

    impl ActivityRepository for FakeActivityRepository {
        async fn append(&self, _activity: &Activity) -> Result<(), RepositoryError> {
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
