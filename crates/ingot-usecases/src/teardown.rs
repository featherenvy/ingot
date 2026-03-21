use chrono::Utc;
use ingot_domain::activity::{Activity, ActivityEntityType, ActivityEventType};
use ingot_domain::convergence::ConvergenceStatus;
use ingot_domain::convergence_queue::ConvergenceQueueEntryStatus;
use ingot_domain::git_operation::{GitEntityType, GitOperationStatus, OperationKind};
use ingot_domain::ids::{
    ActivityId, ConvergenceId, ConvergenceQueueEntryId, GitOperationId, ItemId, JobId, ProjectId,
    WorkspaceId,
};
use ingot_domain::job::{JobStatus, OutcomeClass};
use ingot_domain::ports::{
    ConvergenceQueueRepository, ConvergenceRepository, FinishJobNonSuccessParams,
    GitOperationRepository, JobRepository, RevisionLaneTeardownMutation,
    RevisionLaneTeardownRepository, TeardownJobCancellation, WorkspaceRepository,
};
use ingot_domain::revision::ItemRevision;
use ingot_domain::workspace::WorkspaceStatus;

use crate::UseCaseError;
use crate::job_lifecycle::map_finish_non_success_error;

/// Result of tearing down a revision lane's active state.
/// Callers use this to decide what infrastructure side effects to perform
/// (e.g., filesystem removal, git ref cleanup).
#[derive(Default, Debug, Clone)]
pub struct RevisionLaneTeardownResult {
    pub cancelled_job_ids: Vec<JobId>,
    pub cancelled_job_workspace_ids: Vec<WorkspaceId>,
    pub cancelled_convergence_ids: Vec<ConvergenceId>,
    pub integration_workspace_ids: Vec<WorkspaceId>,
    pub cancelled_queue_entry_ids: Vec<ConvergenceQueueEntryId>,
    pub reconciled_git_operation_ids: Vec<GitOperationId>,
    pub failed_git_operation_ids: Vec<GitOperationId>,
}

impl RevisionLaneTeardownResult {
    pub fn has_cancelled_convergence(&self) -> bool {
        !self.cancelled_convergence_ids.is_empty()
    }

    pub fn has_cancelled_queue_entry(&self) -> bool {
        !self.cancelled_queue_entry_ids.is_empty()
    }

    pub fn first_cancelled_convergence_id(&self) -> Option<ConvergenceId> {
        self.cancelled_convergence_ids.first().copied()
    }

    pub fn first_cancelled_queue_entry_id(&self) -> Option<ConvergenceQueueEntryId> {
        self.cancelled_queue_entry_ids.first().copied()
    }
}

/// Tear down all active state for a revision lane. Pure DB mutations only.
///
/// Cancels active jobs, convergences, queue entries, and reconciles git operations.
/// Does NOT perform filesystem cleanup or refresh_revision_context — callers do that.
///
/// All writes are applied atomically via `RevisionLaneTeardownRepository`.
#[allow(clippy::too_many_arguments)]
pub async fn teardown_revision_lane<J, C, CQ, W, GO, T>(
    job_repo: &J,
    convergence_repo: &C,
    queue_repo: &CQ,
    workspace_repo: &W,
    git_op_repo: &GO,
    teardown_repo: &T,
    project_id: ProjectId,
    item_id: ItemId,
    revision: &ItemRevision,
) -> Result<RevisionLaneTeardownResult, UseCaseError>
where
    J: JobRepository,
    C: ConvergenceRepository,
    CQ: ConvergenceQueueRepository,
    W: WorkspaceRepository,
    GO: GitOperationRepository,
    T: RevisionLaneTeardownRepository,
{
    let mut result = RevisionLaneTeardownResult::default();
    let mut mutation = RevisionLaneTeardownMutation::default();

    // --- Read phase ---

    let active_jobs: Vec<_> = job_repo
        .list_by_item(item_id)
        .await?
        .into_iter()
        .filter(|job| job.item_revision_id == revision.id && job.state.is_active())
        .collect();

    let active_convergences: Vec<_> = convergence_repo
        .list_by_item(item_id)
        .await?
        .into_iter()
        .filter(|convergence| {
            convergence.item_revision_id == revision.id
                && matches!(
                    convergence.state.status(),
                    ConvergenceStatus::Queued
                        | ConvergenceStatus::Running
                        | ConvergenceStatus::Prepared
                )
        })
        .collect();

    let active_queue_entry = queue_repo.find_active_for_revision(revision.id).await?;

    // --- Compute phase ---

    // 1. Build job cancellations
    for job in &active_jobs {
        let params = FinishJobNonSuccessParams {
            job_id: job.id,
            item_id,
            expected_item_revision_id: revision.id,
            status: JobStatus::Cancelled,
            outcome_class: Some(OutcomeClass::Cancelled),
            error_code: Some("item_mutation_cancelled".into()),
            error_message: None,
            escalation_reason: None,
        };

        let workspace_update = if let Some(workspace_id) = job.state.workspace_id() {
            let mut workspace = workspace_repo.get(workspace_id).await?;
            workspace.release_to(WorkspaceStatus::Ready, Utc::now());
            result.cancelled_job_workspace_ids.push(workspace_id);
            Some(workspace)
        } else {
            None
        };

        let activity = Activity {
            id: ActivityId::new(),
            project_id,
            event_type: ActivityEventType::JobCancelled,
            entity_type: ActivityEntityType::Job,
            entity_id: job.id.to_string(),
            payload: serde_json::json!({ "item_id": item_id, "reason": "item_mutation_cancelled" }),
            created_at: Utc::now(),
        };

        mutation.job_cancellations.push(TeardownJobCancellation {
            params,
            workspace_update,
            activity,
        });
        result.cancelled_job_ids.push(job.id);
    }

    // 2. Build convergence cancellations
    for convergence in active_convergences {
        let mut cancelled = convergence;
        cancelled.transition_to_cancelled(Utc::now());
        result.cancelled_convergence_ids.push(cancelled.id);

        if let Some(workspace_id) = cancelled.state.integration_workspace_id() {
            let workspace = workspace_repo.get(workspace_id).await?;
            if workspace.state.status() != WorkspaceStatus::Abandoned {
                let mut abandoned_workspace = workspace;
                abandoned_workspace.mark_abandoned(Utc::now());
                mutation.workspace_abandonments.push(abandoned_workspace);
            }
            result.integration_workspace_ids.push(workspace_id);
        }

        mutation.convergence_updates.push(cancelled);
    }

    // 3. Build queue entry cancellation
    if let Some(mut queue_entry) = active_queue_entry {
        queue_entry.status = ConvergenceQueueEntryStatus::Cancelled;
        queue_entry.released_at = Some(Utc::now());
        queue_entry.updated_at = Utc::now();
        result.cancelled_queue_entry_ids.push(queue_entry.id);
        mutation.queue_entry_update = Some(queue_entry);
    }

    // 4. Build git operation reconciliations
    if result.has_cancelled_convergence() {
        let cancelled_ids: Vec<String> = result
            .cancelled_convergence_ids
            .iter()
            .map(|id| id.to_string())
            .collect();

        for mut operation in git_op_repo
            .find_unresolved()
            .await?
            .into_iter()
            .filter(|operation| {
                operation.project_id == project_id
                    && operation.entity_type() == GitEntityType::Convergence
                    && cancelled_ids.iter().any(|id| id == &operation.entity_id)
                    && matches!(
                        operation.operation_kind(),
                        OperationKind::PrepareConvergenceCommit | OperationKind::FinalizeTargetRef
                    )
            })
        {
            match operation.operation_kind() {
                OperationKind::PrepareConvergenceCommit => {
                    operation.status = GitOperationStatus::Reconciled;
                    operation.completed_at = Some(Utc::now());
                    mutation.git_operation_activities.push(Activity {
                        id: ActivityId::new(),
                        project_id,
                        event_type: ActivityEventType::GitOperationReconciled,
                        entity_type: ActivityEntityType::GitOperation,
                        entity_id: operation.id.to_string(),
                        payload: serde_json::json!({ "operation_kind": operation.operation_kind() }),
                        created_at: Utc::now(),
                    });
                    result.reconciled_git_operation_ids.push(operation.id);
                    mutation.git_operation_updates.push(operation);
                }
                OperationKind::FinalizeTargetRef => {
                    operation.status = GitOperationStatus::Failed;
                    operation.completed_at = Some(Utc::now());
                    result.failed_git_operation_ids.push(operation.id);
                    mutation.git_operation_updates.push(operation);
                }
                OperationKind::CreateJobCommit
                | OperationKind::CreateInvestigationRef
                | OperationKind::ResetWorkspace
                | OperationKind::RemoveWorkspaceRef
                | OperationKind::RemoveInvestigationRef => {}
            }
        }
    }

    // --- Apply phase (single atomic write) ---

    teardown_repo
        .apply_revision_lane_teardown(mutation)
        .await
        .map_err(|error| {
            map_finish_non_success_error(
                error,
                "job failure does not match the current item revision",
            )
        })?;

    Ok(result)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use chrono::Utc;
    use ingot_domain::convergence::Convergence;
    use ingot_domain::convergence_queue::ConvergenceQueueEntry;
    use ingot_domain::git_operation::GitOperation;
    use ingot_domain::ids::{
        ConvergenceId, ConvergenceQueueEntryId, ItemId, ItemRevisionId, JobId, ProjectId,
        WorkspaceId,
    };
    use ingot_domain::job::{Job, JobStatus};
    use ingot_domain::ports::{
        RepositoryError, RevisionLaneTeardownMutation, StartJobExecutionParams,
    };
    use ingot_domain::workspace::{Workspace, WorkspaceKind, WorkspaceStatus};
    use ingot_test_support::fixtures::{JobBuilder, RevisionBuilder, WorkspaceBuilder};
    use uuid::Uuid;

    use super::*;

    #[tokio::test]
    async fn teardown_maps_job_not_active_conflict() {
        let revision = test_revision();
        let workspace = test_workspace();
        let job = test_active_job(revision.id, Some(workspace.id));
        let job_repo = FakeJobRepository::with_jobs(vec![job]);
        let workspace_repo = FakeWorkspaceRepository::with_workspace(workspace);
        let teardown_repo =
            FakeTeardownRepository::with_error(RepositoryError::Conflict("job_not_active".into()));

        let result = teardown_revision_lane(
            &job_repo,
            &FakeConvergenceRepository,
            &FakeQueueRepository,
            &workspace_repo,
            &FakeGitOperationRepository,
            &teardown_repo,
            ProjectId::from_uuid(Uuid::nil()),
            revision.item_id,
            &revision,
        )
        .await;

        assert!(
            matches!(result, Err(UseCaseError::JobNotActive)),
            "expected JobNotActive, got: {result:?}"
        );
    }

    #[tokio::test]
    async fn teardown_maps_revision_stale_conflict() {
        let revision = test_revision();
        let workspace = test_workspace();
        let job = test_active_job(revision.id, Some(workspace.id));
        let job_repo = FakeJobRepository::with_jobs(vec![job]);
        let workspace_repo = FakeWorkspaceRepository::with_workspace(workspace);
        let teardown_repo = FakeTeardownRepository::with_error(RepositoryError::Conflict(
            "job_revision_stale".into(),
        ));

        let result = teardown_revision_lane(
            &job_repo,
            &FakeConvergenceRepository,
            &FakeQueueRepository,
            &workspace_repo,
            &FakeGitOperationRepository,
            &teardown_repo,
            ProjectId::from_uuid(Uuid::nil()),
            revision.item_id,
            &revision,
        )
        .await;

        assert!(matches!(
            result,
            Err(UseCaseError::ProtocolViolation(message))
                if message == "job failure does not match the current item revision"
        ));
    }

    #[tokio::test]
    async fn teardown_cancels_active_jobs_for_revision() {
        let revision = test_revision();
        let workspace = test_workspace();
        let job = test_active_job(revision.id, Some(workspace.id));
        let job_repo = FakeJobRepository::with_jobs(vec![job.clone()]);
        let workspace_repo = FakeWorkspaceRepository::with_workspace(workspace);
        let teardown_repo = FakeTeardownRepository::default();

        let result = teardown_revision_lane(
            &job_repo,
            &FakeConvergenceRepository,
            &FakeQueueRepository,
            &workspace_repo,
            &FakeGitOperationRepository,
            &teardown_repo,
            job.project_id,
            job.item_id,
            &revision,
        )
        .await
        .expect("teardown should succeed");

        assert_eq!(result.cancelled_job_ids, vec![job.id]);
        assert_eq!(
            result.cancelled_job_workspace_ids,
            vec![WorkspaceId::from_uuid(Uuid::nil())]
        );

        let captured = teardown_repo.captured_mutation();
        assert_eq!(captured.job_cancellations.len(), 1);
        let cancellation = &captured.job_cancellations[0];
        assert_eq!(cancellation.params.job_id, job.id);
        let ws_update = cancellation
            .workspace_update
            .as_ref()
            .expect("workspace update");
        assert_eq!(ws_update.state.current_job_id(), None);
        assert_eq!(ws_update.state.status(), WorkspaceStatus::Ready);
    }

    fn test_revision() -> ItemRevision {
        RevisionBuilder::new(ItemId::from_uuid(Uuid::nil()))
            .id(ItemRevisionId::from_uuid(Uuid::nil()))
            .build()
    }

    fn test_active_job(revision_id: ItemRevisionId, workspace_id: Option<WorkspaceId>) -> Job {
        let nil = Uuid::nil();
        let mut builder = JobBuilder::new(
            ProjectId::from_uuid(nil),
            ItemId::from_uuid(nil),
            revision_id,
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
        let nil = Uuid::nil();
        let job_id = JobId::from_uuid(nil);
        WorkspaceBuilder::new(ProjectId::from_uuid(nil), WorkspaceKind::Authoring)
            .id(WorkspaceId::from_uuid(nil))
            .status(WorkspaceStatus::Busy)
            .current_job_id(job_id)
            .build()
    }

    #[derive(Clone, Default)]
    struct FakeJobRepository {
        state: Arc<Mutex<FakeJobRepositoryState>>,
    }

    #[derive(Default)]
    struct FakeJobRepositoryState {
        jobs: Vec<Job>,
    }

    impl FakeJobRepository {
        fn with_jobs(jobs: Vec<Job>) -> Self {
            Self {
                state: Arc::new(Mutex::new(FakeJobRepositoryState { jobs })),
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
            let jobs = self.state.lock().expect("job state lock").jobs.clone();
            Ok(jobs)
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
            async { unreachable!("unused in test") }.await
        }
    }

    #[derive(Clone, Default)]
    struct FakeConvergenceRepository;

    impl ConvergenceRepository for FakeConvergenceRepository {
        async fn list_by_revision(
            &self,
            _revision_id: ItemRevisionId,
        ) -> Result<Vec<Convergence>, RepositoryError> {
            async { unreachable!("unused in test") }.await
        }

        async fn get(&self, _id: ConvergenceId) -> Result<Convergence, RepositoryError> {
            async { unreachable!("unused in test") }.await
        }

        async fn create(&self, _convergence: &Convergence) -> Result<(), RepositoryError> {
            async { unreachable!("unused in test") }.await
        }

        async fn update(&self, _convergence: &Convergence) -> Result<(), RepositoryError> {
            Ok(())
        }

        async fn find_active_for_revision(
            &self,
            _revision_id: ItemRevisionId,
        ) -> Result<Option<Convergence>, RepositoryError> {
            async { unreachable!("unused in test") }.await
        }

        async fn find_prepared_for_revision(
            &self,
            _revision_id: ItemRevisionId,
        ) -> Result<Option<Convergence>, RepositoryError> {
            async { unreachable!("unused in test") }.await
        }

        async fn list_by_item(
            &self,
            _item_id: ItemId,
        ) -> Result<Vec<Convergence>, RepositoryError> {
            Ok(vec![])
        }

        async fn list_active(&self) -> Result<Vec<Convergence>, RepositoryError> {
            async { unreachable!("unused in test") }.await
        }
    }

    #[derive(Clone, Default)]
    struct FakeQueueRepository;

    impl ConvergenceQueueRepository for FakeQueueRepository {
        async fn list_by_item(
            &self,
            _item_id: ItemId,
        ) -> Result<Vec<ConvergenceQueueEntry>, RepositoryError> {
            async { unreachable!("unused in test") }.await
        }

        async fn get(
            &self,
            _id: ConvergenceQueueEntryId,
        ) -> Result<ConvergenceQueueEntry, RepositoryError> {
            async { unreachable!("unused in test") }.await
        }

        async fn find_active_for_revision(
            &self,
            _revision_id: ItemRevisionId,
        ) -> Result<Option<ConvergenceQueueEntry>, RepositoryError> {
            Ok(None)
        }

        async fn find_head(
            &self,
            _project_id: ProjectId,
            _target_ref: &ingot_domain::git_ref::GitRef,
        ) -> Result<Option<ConvergenceQueueEntry>, RepositoryError> {
            async { unreachable!("unused in test") }.await
        }

        async fn find_next_queued(
            &self,
            _project_id: ProjectId,
            _target_ref: &ingot_domain::git_ref::GitRef,
        ) -> Result<Option<ConvergenceQueueEntry>, RepositoryError> {
            async { unreachable!("unused in test") }.await
        }

        async fn create(&self, _entry: &ConvergenceQueueEntry) -> Result<(), RepositoryError> {
            async { unreachable!("unused in test") }.await
        }

        async fn list_active_by_project(
            &self,
            _project_id: ProjectId,
        ) -> Result<Vec<ConvergenceQueueEntry>, RepositoryError> {
            async { unreachable!("unused in test") }.await
        }

        async fn list_active_for_lane(
            &self,
            _project_id: ProjectId,
            _target_ref: &ingot_domain::git_ref::GitRef,
        ) -> Result<Vec<ConvergenceQueueEntry>, RepositoryError> {
            async { unreachable!("unused in test") }.await
        }

        async fn update(&self, _entry: &ConvergenceQueueEntry) -> Result<(), RepositoryError> {
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
    }

    impl FakeWorkspaceRepository {
        fn with_workspace(workspace: Workspace) -> Self {
            Self {
                state: Arc::new(Mutex::new(FakeWorkspaceRepositoryState {
                    workspace: Some(workspace),
                })),
            }
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

        async fn update(&self, _workspace: &Workspace) -> Result<(), RepositoryError> {
            async { unreachable!("unused in test") }.await
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
    struct FakeGitOperationRepository;

    impl GitOperationRepository for FakeGitOperationRepository {
        async fn create(&self, _operation: &GitOperation) -> Result<(), RepositoryError> {
            async { unreachable!("unused in test") }.await
        }

        async fn update(&self, _operation: &GitOperation) -> Result<(), RepositoryError> {
            Ok(())
        }

        async fn find_unresolved(&self) -> Result<Vec<GitOperation>, RepositoryError> {
            Ok(vec![])
        }

        async fn find_unresolved_finalize_for_convergence(
            &self,
            _convergence_id: ConvergenceId,
        ) -> Result<Option<GitOperation>, RepositoryError> {
            Ok(None)
        }
    }

    #[derive(Clone, Default)]
    struct FakeTeardownRepository {
        state: Arc<Mutex<FakeTeardownRepositoryState>>,
    }

    #[derive(Default)]
    struct FakeTeardownRepositoryState {
        apply_error: Option<RepositoryError>,
        captured_mutation: Option<RevisionLaneTeardownMutation>,
    }

    impl FakeTeardownRepository {
        fn with_error(error: RepositoryError) -> Self {
            Self {
                state: Arc::new(Mutex::new(FakeTeardownRepositoryState {
                    apply_error: Some(error),
                    captured_mutation: None,
                })),
            }
        }

        fn captured_mutation(&self) -> RevisionLaneTeardownMutation {
            self.state
                .lock()
                .expect("teardown state lock")
                .captured_mutation
                .clone()
                .expect("mutation should have been captured")
        }
    }

    impl RevisionLaneTeardownRepository for FakeTeardownRepository {
        async fn apply_revision_lane_teardown(
            &self,
            mutation: RevisionLaneTeardownMutation,
        ) -> Result<(), RepositoryError> {
            let mut state = self.state.lock().expect("teardown state lock");
            state.captured_mutation = Some(mutation);
            if let Some(error) = state.apply_error.take() {
                return Err(error);
            }
            Ok(())
        }
    }
}
