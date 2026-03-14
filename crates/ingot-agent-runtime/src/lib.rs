mod bootstrap;

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use chrono::{Duration as ChronoDuration, Utc};
use ingot_agent_adapters::claude_code::ClaudeCodeCliAdapter;
use ingot_agent_adapters::codex::CodexCliAdapter;
use ingot_agent_protocol::adapter::{AgentAdapter, AgentError};
use ingot_agent_protocol::request::AgentRequest;
use ingot_agent_protocol::response::AgentResponse;
use ingot_domain::activity::{Activity, ActivityEventType};
use ingot_domain::agent::{AdapterKind, Agent, AgentCapability, AgentStatus};
use ingot_domain::convergence::{Convergence, ConvergenceStatus};
use ingot_domain::convergence_queue::{ConvergenceQueueEntry, ConvergenceQueueEntryStatus};
use ingot_domain::finding::FindingTriageState;
use ingot_domain::git_operation::{GitEntityType, GitOperation, GitOperationStatus, OperationKind};
use ingot_domain::ids::{GitOperationId, WorkspaceId};
use ingot_domain::item::{
    ApprovalState, DoneReason, EscalationReason, EscalationState, LifecycleState, ResolutionSource,
};
use ingot_domain::job::{ExecutionPermission, Job, JobStatus, OutcomeClass, OutputArtifactKind};
use ingot_domain::ports::{JobCompletionMutation, ProjectMutationLockPort, RepositoryError};
use ingot_domain::project::Project;
use ingot_domain::revision::ItemRevision;
use ingot_domain::revision_context::RevisionContext;
use ingot_domain::workspace::{
    RetentionPolicy, Workspace, WorkspaceKind, WorkspaceStatus, WorkspaceStrategy,
};
use ingot_git::GitJobCompletionPort;
use ingot_git::commands::{
    GitCommandError, compare_and_swap_ref, delete_ref, git, head_oid, resolve_ref_oid,
};
use ingot_git::commit::{
    ConvergenceCommitTrailers, JobCommitTrailers, abort_cherry_pick, cherry_pick_no_commit,
    commit_message, create_daemon_job_commit, list_commits_oldest_first, working_tree_has_changes,
};
use ingot_git::diff::changed_paths_between;
use ingot_git::project_repo::{
    CheckoutFinalizationStatus, CheckoutSyncStatus, checkout_finalization_status,
    checkout_sync_status, ensure_mirror, project_repo_paths, sync_checkout_to_commit,
};
use ingot_store_sqlite::{Database, FinishJobNonSuccessParams, StartJobExecutionParams};
use ingot_usecases::convergence::{
    ConvergenceSystemActionPort, SystemActionItemState, SystemActionProjectState,
};
use ingot_usecases::job::{DispatchJobCommand, dispatch_job};
use ingot_usecases::reconciliation::ReconciliationPort;
use ingot_usecases::{
    CompleteJobCommand, CompleteJobService, ConvergenceService, ProjectLocks,
    ReconciliationService, rebuild_revision_context,
};
use ingot_workflow::{ClosureRelevance, Evaluator, step};
use ingot_workspace::{
    WorkspaceError, ensure_authoring_workspace_state, provision_integration_workspace,
    provision_review_workspace, remove_workspace,
};
use sha2::{Digest, Sha256};
use tokio::time::{interval, sleep};
use tracing::{Instrument, debug, error, info, info_span, warn};

#[derive(Debug, Clone)]
pub struct DispatcherConfig {
    pub state_root: PathBuf,
    pub poll_interval: Duration,
    pub heartbeat_interval: Duration,
    pub job_timeout: Duration,
}

impl DispatcherConfig {
    pub fn new(state_root: PathBuf) -> Self {
        Self {
            state_root,
            poll_interval: Duration::from_secs(1),
            heartbeat_interval: Duration::from_secs(5),
            job_timeout: Duration::from_secs(30 * 60),
        }
    }
}

pub trait AgentRunner: Send + Sync {
    fn launch<'a>(
        &'a self,
        agent: &'a Agent,
        request: &'a AgentRequest,
        working_dir: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<AgentResponse, AgentError>> + Send + 'a>>;
}

#[derive(Debug, Clone, Default)]
pub struct CliAgentRunner;

impl AgentRunner for CliAgentRunner {
    fn launch<'a>(
        &'a self,
        agent: &'a Agent,
        request: &'a AgentRequest,
        working_dir: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<AgentResponse, AgentError>> + Send + 'a>> {
        Box::pin(async move {
            match agent.adapter_kind {
                AdapterKind::Codex => {
                    CodexCliAdapter::new(agent.cli_path.clone(), agent.model.clone())
                        .launch(request, working_dir)
                        .await
                }
                AdapterKind::ClaudeCode => ClaudeCodeCliAdapter.launch(request, working_dir).await,
            }
        })
    }
}

#[derive(Clone)]
pub struct JobDispatcher {
    db: Database,
    project_locks: ProjectLocks,
    config: DispatcherConfig,
    lease_owner_id: String,
    runner: Arc<dyn AgentRunner>,
}

#[derive(Clone)]
struct RuntimeConvergencePort {
    dispatcher: JobDispatcher,
}

#[derive(Clone)]
struct RuntimeReconciliationPort {
    dispatcher: JobDispatcher,
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("repository error: {0}")]
    Repository(#[from] RepositoryError),
    #[error("git error: {0}")]
    Git(#[from] GitCommandError),
    #[error("workspace error: {0}")]
    Workspace(#[from] WorkspaceError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid runtime state: {0}")]
    InvalidState(String),
}

fn usecase_to_runtime_error(error: ingot_usecases::UseCaseError) -> RuntimeError {
    match error {
        ingot_usecases::UseCaseError::Repository(error) => RuntimeError::Repository(error),
        other => RuntimeError::InvalidState(other.to_string()),
    }
}

fn usecase_from_runtime_error(error: RuntimeError) -> ingot_usecases::UseCaseError {
    match error {
        RuntimeError::Repository(error) => ingot_usecases::UseCaseError::Repository(error),
        other => ingot_usecases::UseCaseError::Internal(other.to_string()),
    }
}

#[derive(Debug, Clone)]
struct PreparedRun {
    job: Job,
    item: ingot_domain::item::Item,
    revision: ItemRevision,
    project: Project,
    canonical_repo_path: PathBuf,
    agent: Agent,
    workspace: Workspace,
    original_head_commit_oid: String,
    prompt: String,
    workspace_lifecycle: WorkspaceLifecycle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkspaceLifecycle {
    PersistentAuthoring,
    PersistentIntegration,
    EphemeralReview,
}

impl ConvergenceSystemActionPort for RuntimeConvergencePort {
    fn load_system_action_projects(
        &self,
    ) -> impl Future<Output = Result<Vec<SystemActionProjectState>, ingot_usecases::UseCaseError>> + Send
    {
        let dispatcher = self.dispatcher.clone();
        async move {
            let mut projects = Vec::new();
            for project in dispatcher
                .db
                .list_projects()
                .await
                .map_err(ingot_usecases::UseCaseError::Repository)?
            {
                let mut items = Vec::new();
                for item in dispatcher
                    .db
                    .list_items_by_project(project.id)
                    .await
                    .map_err(ingot_usecases::UseCaseError::Repository)?
                {
                    let revision = dispatcher
                        .db
                        .get_revision(item.current_revision_id)
                        .await
                        .map_err(ingot_usecases::UseCaseError::Repository)?;
                    let jobs = dispatcher
                        .db
                        .list_jobs_by_item(item.id)
                        .await
                        .map_err(ingot_usecases::UseCaseError::Repository)?;
                    let findings = dispatcher
                        .db
                        .list_findings_by_item(item.id)
                        .await
                        .map_err(ingot_usecases::UseCaseError::Repository)?;
                    let convergences = dispatcher
                        .hydrate_convergences(
                            &project,
                            dispatcher
                                .db
                                .list_convergences_by_item(item.id)
                                .await
                                .map_err(ingot_usecases::UseCaseError::Repository)?,
                        )
                        .await
                        .map_err(|error| {
                            ingot_usecases::UseCaseError::Internal(error.to_string())
                        })?;
                    let queue_entry = dispatcher
                        .db
                        .find_active_queue_entry_for_revision(revision.id)
                        .await
                        .map_err(ingot_usecases::UseCaseError::Repository)?;
                    items.push(SystemActionItemState {
                        item_id: item.id,
                        item,
                        revision,
                        jobs,
                        findings,
                        convergences,
                        queue_entry,
                    });
                }
                projects.push(SystemActionProjectState { project, items });
            }

            Ok(projects)
        }
    }

    fn promote_queue_heads(
        &self,
        project_id: ingot_domain::ids::ProjectId,
    ) -> impl Future<Output = Result<(), ingot_usecases::UseCaseError>> + Send {
        let dispatcher = self.dispatcher.clone();
        async move {
            dispatcher
                .promote_queue_heads(project_id)
                .await
                .map_err(usecase_from_runtime_error)
        }
    }

    fn prepare_queue_head_convergence(
        &self,
        project: &Project,
        state: &SystemActionItemState,
        queue_entry: &ConvergenceQueueEntry,
    ) -> impl Future<Output = Result<(), ingot_usecases::UseCaseError>> + Send {
        let dispatcher = self.dispatcher.clone();
        let project = project.clone();
        let state = state.clone();
        let queue_entry = queue_entry.clone();
        async move {
            dispatcher
                .prepare_queue_head_convergence(
                    &project,
                    &state.item,
                    &state.revision,
                    &state.jobs,
                    &state.findings,
                    &state.convergences,
                    &queue_entry,
                )
                .await
                .map_err(usecase_from_runtime_error)
        }
    }

    fn invalidate_prepared_convergence(
        &self,
        project_id: ingot_domain::ids::ProjectId,
        item_id: ingot_domain::ids::ItemId,
    ) -> impl Future<Output = Result<(), ingot_usecases::UseCaseError>> + Send {
        let dispatcher = self.dispatcher.clone();
        async move {
            dispatcher
                .invalidate_prepared_convergence(project_id, item_id)
                .await
                .map_err(usecase_from_runtime_error)
        }
    }

    fn reconcile_checkout_sync_ready(
        &self,
        project: &Project,
        item_id: ingot_domain::ids::ItemId,
        revision: &ItemRevision,
    ) -> impl Future<Output = Result<bool, ingot_usecases::UseCaseError>> + Send {
        let dispatcher = self.dispatcher.clone();
        let project = project.clone();
        let revision = revision.clone();
        async move {
            dispatcher
                .reconcile_checkout_sync_state(&project, item_id, &revision)
                .await
                .map(|status| status == CheckoutSyncStatus::Ready)
                .map_err(usecase_from_runtime_error)
        }
    }

    fn auto_finalize_prepared_convergence(
        &self,
        project_id: ingot_domain::ids::ProjectId,
        item_id: ingot_domain::ids::ItemId,
    ) -> impl Future<Output = Result<(), ingot_usecases::UseCaseError>> + Send {
        let dispatcher = self.dispatcher.clone();
        async move {
            dispatcher
                .auto_finalize_prepared_convergence(project_id, item_id)
                .await
                .map_err(usecase_from_runtime_error)
        }
    }
}

impl ReconciliationPort for RuntimeReconciliationPort {
    fn reconcile_git_operations(
        &self,
    ) -> impl Future<Output = Result<bool, ingot_usecases::UseCaseError>> + Send {
        let dispatcher = self.dispatcher.clone();
        async move {
            dispatcher
                .reconcile_git_operations()
                .await
                .map_err(usecase_from_runtime_error)
        }
    }

    fn reconcile_active_jobs(
        &self,
    ) -> impl Future<Output = Result<bool, ingot_usecases::UseCaseError>> + Send {
        let dispatcher = self.dispatcher.clone();
        async move {
            dispatcher
                .reconcile_active_jobs()
                .await
                .map_err(usecase_from_runtime_error)
        }
    }

    fn reconcile_active_convergences(
        &self,
    ) -> impl Future<Output = Result<bool, ingot_usecases::UseCaseError>> + Send {
        let dispatcher = self.dispatcher.clone();
        async move {
            dispatcher
                .reconcile_active_convergences()
                .await
                .map_err(usecase_from_runtime_error)
        }
    }

    fn reconcile_workspace_retention(
        &self,
    ) -> impl Future<Output = Result<bool, ingot_usecases::UseCaseError>> + Send {
        let dispatcher = self.dispatcher.clone();
        async move {
            dispatcher
                .reconcile_workspace_retention()
                .await
                .map_err(usecase_from_runtime_error)
        }
    }
}

impl JobDispatcher {
    pub fn new(db: Database, project_locks: ProjectLocks, config: DispatcherConfig) -> Self {
        Self::with_runner(db, project_locks, config, Arc::new(CliAgentRunner))
    }

    pub fn with_runner(
        db: Database,
        project_locks: ProjectLocks,
        config: DispatcherConfig,
        runner: Arc<dyn AgentRunner>,
    ) -> Self {
        Self {
            db,
            project_locks,
            config,
            lease_owner_id: format!("ingotd:{}", std::process::id()),
            runner,
        }
    }

    fn project_paths(&self, project: &Project) -> ingot_git::project_repo::ProjectRepoPaths {
        project_repo_paths(
            self.config.state_root.as_path(),
            project.id,
            Path::new(&project.path),
        )
    }

    async fn refresh_project_mirror(
        &self,
        project: &Project,
    ) -> Result<ingot_git::project_repo::ProjectRepoPaths, RuntimeError> {
        let paths = self.project_paths(project);
        let has_unresolved_finalize = self
            .db
            .list_unresolved_git_operations()
            .await?
            .into_iter()
            .any(|operation| {
                operation.project_id == project.id
                    && operation.operation_kind == OperationKind::FinalizeTargetRef
            });
        if !(has_unresolved_finalize && paths.mirror_git_dir.exists()) {
            ensure_mirror(&paths).await?;
        }
        Ok(paths)
    }

    pub async fn run_forever(&self) {
        loop {
            if let Err(error) = self.tick().await {
                error!(?error, "authoring job dispatcher tick failed");
            }

            sleep(self.config.poll_interval).await;
        }
    }

    pub async fn reconcile_startup(&self) -> Result<(), RuntimeError> {
        bootstrap::ensure_default_agent(&self.db).await?;
        ReconciliationService::new(RuntimeReconciliationPort {
            dispatcher: self.clone(),
        })
        .reconcile_startup()
        .await
        .map_err(usecase_to_runtime_error)?;
        while ConvergenceService::new(RuntimeConvergencePort {
            dispatcher: self.clone(),
        })
        .tick_system_actions()
        .await
        .map_err(usecase_to_runtime_error)?
        {}
        self.auto_dispatch_projected_review_jobs().await?;
        Ok(())
    }

    pub async fn tick(&self) -> Result<bool, RuntimeError> {
        let mut made_progress = ReconciliationService::new(RuntimeReconciliationPort {
            dispatcher: self.clone(),
        })
        .tick_maintenance()
        .await
        .map_err(usecase_to_runtime_error)?;
        if ConvergenceService::new(RuntimeConvergencePort {
            dispatcher: self.clone(),
        })
        .tick_system_actions()
        .await
        .map_err(usecase_to_runtime_error)?
        {
            return Ok(true);
        }
        made_progress |= self.auto_dispatch_projected_review_jobs().await?;

        let Some(job) = self.next_runnable_job().await? else {
            return Ok(made_progress);
        };

        let Some(prepared) = self.prepare_run(job).await? else {
            return Ok(made_progress);
        };

        self.write_prompt_artifact(&prepared.job, &prepared.prompt)
            .await?;
        let request = AgentRequest {
            prompt: prepared.prompt.clone(),
            working_dir: prepared.workspace.path.clone(),
            may_mutate: prepared.job.execution_permission == ExecutionPermission::MayMutate,
            timeout_seconds: Some(self.config.job_timeout.as_secs()),
            output_schema: output_schema_for_job(&prepared.job),
        };
        let response = self.run_with_heartbeats(&prepared, request).await;

        match response {
            Ok(response) => {
                self.write_response_artifacts(&prepared.job, &response)
                    .await?;
                self.finish_run(prepared, response).await?;
            }
            Err(AgentError::Timeout) => {
                self.fail_run(
                    &prepared,
                    OutcomeClass::TransientFailure,
                    "job_timeout",
                    Some("job execution timed out".into()),
                )
                .await?;
            }
            Err(error) => {
                let current_job = self.db.get_job(prepared.job.id).await?;
                if current_job.status == JobStatus::Cancelled {
                    self.finalize_workspace_after_failure(&prepared).await?;
                    info!(job_id = %prepared.job.id, "job cancelled during runtime execution");
                    return Ok(true);
                }
                warn!(?error, job_id = %prepared.job.id, "agent launch failed");
                self.fail_run(
                    &prepared,
                    OutcomeClass::TerminalFailure,
                    "agent_launch_failed",
                    Some(error.to_string()),
                )
                .await?;
            }
        }

        made_progress = true;
        Ok(made_progress)
    }

    #[allow(dead_code)]
    async fn tick_system_action(&self) -> Result<bool, RuntimeError> {
        ConvergenceService::new(RuntimeConvergencePort {
            dispatcher: self.clone(),
        })
        .tick_system_actions()
        .await
        .map_err(usecase_to_runtime_error)
    }

    async fn promote_queue_heads(
        &self,
        project_id: ingot_domain::ids::ProjectId,
    ) -> Result<(), RuntimeError> {
        let entries = self
            .db
            .list_active_queue_entries_by_project(project_id)
            .await?;
        let mut lanes_with_heads = std::collections::HashSet::new();
        for entry in &entries {
            if entry.status == ConvergenceQueueEntryStatus::Head {
                lanes_with_heads.insert(entry.target_ref.clone());
            }
        }

        for entry in entries {
            if entry.status != ConvergenceQueueEntryStatus::Queued
                || lanes_with_heads.contains(&entry.target_ref)
            {
                continue;
            }

            let mut promoted = entry;
            promoted.status = ConvergenceQueueEntryStatus::Head;
            promoted.head_acquired_at = Some(Utc::now());
            promoted.updated_at = Utc::now();
            self.db.update_queue_entry(&promoted).await?;
            self.append_activity(
                project_id,
                ActivityEventType::ConvergenceLaneAcquired,
                "queue_entry",
                promoted.id.to_string(),
                serde_json::json!({ "item_id": promoted.item_id, "target_ref": promoted.target_ref }),
            )
            .await?;
            lanes_with_heads.insert(promoted.target_ref);
        }

        Ok(())
    }

    async fn next_runnable_job(&self) -> Result<Option<Job>, RuntimeError> {
        let jobs = self.db.list_queued_jobs(32).await?;
        let runnable_job = jobs.into_iter().find(is_supported_runtime_job);
        if let Some(job) = runnable_job.as_ref() {
            debug!(
                job_id = %job.id,
                step_id = %job.step_id,
                workspace_kind = ?job.workspace_kind,
                execution_permission = ?job.execution_permission,
                "selected queued job for runtime"
            );
        }
        Ok(runnable_job)
    }

    async fn reconcile_active_jobs(&self) -> Result<bool, RuntimeError> {
        let active_jobs = self.db.list_active_jobs().await?;
        let mut made_progress = false;
        for job in active_jobs {
            match job.status {
                JobStatus::Assigned => {
                    self.reconcile_assigned_job(job).await?;
                    made_progress = true;
                }
                JobStatus::Running => {
                    self.reconcile_running_job(job).await?;
                    made_progress = true;
                }
                _ => {}
            }
        }
        Ok(made_progress)
    }

    async fn reconcile_git_operations(&self) -> Result<bool, RuntimeError> {
        let operations = self.db.list_unresolved_git_operations().await?;
        let mut made_progress = false;
        for mut operation in operations {
            let project = self.db.get_project(operation.project_id).await?;
            let paths = self.refresh_project_mirror(&project).await?;
            let repo_path = paths.mirror_git_dir.as_path();
            if operation.operation_kind == OperationKind::FinalizeTargetRef {
                made_progress |= self
                    .reconcile_finalize_target_ref_operation(&project, &mut operation, &paths)
                    .await?;
                continue;
            }
            let reconciled = match operation.operation_kind {
                OperationKind::FinalizeTargetRef => unreachable!("handled above"),
                OperationKind::CreateJobCommit | OperationKind::PrepareConvergenceCommit => {
                    if let Some(commit_oid) = operation
                        .commit_oid
                        .as_deref()
                        .or(operation.new_oid.as_deref())
                    {
                        ingot_git::commands::commit_exists(repo_path, commit_oid).await?
                    } else {
                        false
                    }
                }
                OperationKind::RemoveWorkspaceRef => {
                    if let Some(ref_name) = operation.ref_name.as_deref() {
                        resolve_ref_oid(repo_path, ref_name).await?.is_none()
                    } else {
                        false
                    }
                }
                OperationKind::ResetWorkspace => {
                    if let (Some(workspace_id), Some(expected_oid)) =
                        (operation.workspace_id, operation.new_oid.as_deref())
                    {
                        let workspace = self.db.get_workspace(workspace_id).await?;
                        match head_oid(Path::new(&workspace.path)).await {
                            Ok(actual_head) => actual_head == expected_oid,
                            Err(_) => false,
                        }
                    } else {
                        false
                    }
                }
            };

            if reconciled {
                self.adopt_reconciled_git_operation(&operation).await?;
                self.mark_git_operation_reconciled(&mut operation).await?;
                made_progress = true;
            } else {
                operation.status = GitOperationStatus::Failed;
                operation.completed_at = Some(Utc::now());
                self.db.update_git_operation(&operation).await?;
                made_progress = true;
            }
        }
        Ok(made_progress)
    }

    async fn mark_git_operation_reconciled(
        &self,
        operation: &mut GitOperation,
    ) -> Result<(), RuntimeError> {
        operation.status = GitOperationStatus::Reconciled;
        operation.completed_at = Some(Utc::now());
        self.db.update_git_operation(operation).await?;
        self.append_activity(
            operation.project_id,
            ActivityEventType::GitOperationReconciled,
            "git_operation",
            operation.id.to_string(),
            serde_json::json!({ "operation_kind": operation.operation_kind }),
        )
        .await?;
        Ok(())
    }

    async fn adopt_reconciled_git_operation(
        &self,
        operation: &GitOperation,
    ) -> Result<(), RuntimeError> {
        match operation.operation_kind {
            OperationKind::CreateJobCommit => self.adopt_create_job_commit(operation).await,
            OperationKind::FinalizeTargetRef => self.adopt_finalized_target_ref(operation).await,
            OperationKind::PrepareConvergenceCommit => {
                self.adopt_prepared_convergence(operation).await
            }
            OperationKind::ResetWorkspace => self.adopt_reset_workspace(operation).await,
            OperationKind::RemoveWorkspaceRef => self.adopt_removed_workspace_ref(operation).await,
        }
    }

    async fn adopt_create_job_commit(&self, operation: &GitOperation) -> Result<(), RuntimeError> {
        let job_id = operation
            .entity_id
            .parse::<ingot_domain::ids::JobId>()
            .map_err(|error| RuntimeError::InvalidState(error.to_string()))?;
        let mut job = self.db.get_job(job_id).await?;
        let commit_oid = operation
            .commit_oid
            .clone()
            .or(operation.new_oid.clone())
            .ok_or_else(|| {
                RuntimeError::InvalidState("reconciled create_job_commit missing commit oid".into())
            })?;

        if !matches!(
            job.status,
            JobStatus::Queued | JobStatus::Assigned | JobStatus::Running
        ) {
            return Ok(());
        }

        job.status = JobStatus::Completed;
        job.outcome_class = Some(OutcomeClass::Clean);
        job.output_commit_oid = Some(commit_oid.clone());
        job.error_code = None;
        job.error_message = None;
        job.ended_at.get_or_insert_with(Utc::now);
        self.db.update_job(&job).await?;

        if let Some(workspace_id) = operation.workspace_id.or(job.workspace_id) {
            let mut workspace = self.db.get_workspace(workspace_id).await?;
            workspace.head_commit_oid = Some(commit_oid);
            workspace.current_job_id = None;
            if workspace.status == WorkspaceStatus::Busy {
                workspace.status = WorkspaceStatus::Ready;
            }
            workspace.updated_at = Utc::now();
            self.db.update_workspace(&workspace).await?;
        }

        self.append_activity(
            job.project_id,
            ActivityEventType::JobCompleted,
            "job",
            job.id.to_string(),
            serde_json::json!({ "item_id": job.item_id, "outcome": "clean", "reconciled": true }),
        )
        .await?;
        self.refresh_revision_context_for_ids(
            job.project_id,
            job.item_id,
            job.item_revision_id,
            Some(job.id),
        )
        .await?;
        self.auto_dispatch_projected_review(job.project_id, job.item_id)
            .await?;

        Ok(())
    }

    async fn adopt_finalized_target_ref(
        &self,
        operation: &GitOperation,
    ) -> Result<(), RuntimeError> {
        let convergence_id = operation
            .entity_id
            .parse::<ingot_domain::ids::ConvergenceId>()
            .map_err(|error| RuntimeError::InvalidState(error.to_string()))?;
        let mut convergence = self.db.get_convergence(convergence_id).await?;
        if convergence.status != ConvergenceStatus::Finalized {
            convergence.status = ConvergenceStatus::Finalized;
            convergence.final_target_commit_oid =
                operation.new_oid.clone().or(operation.commit_oid.clone());
            convergence.completed_at.get_or_insert_with(Utc::now);
            self.db.update_convergence(&convergence).await?;
        }

        let project = self.db.get_project(convergence.project_id).await?;
        if let Some(workspace_id) = convergence.integration_workspace_id {
            let workspace = self.db.get_workspace(workspace_id).await?;
            if workspace.status != WorkspaceStatus::Abandoned {
                self.finalize_integration_workspace_after_close(&project, &workspace)
                    .await?;
            }
        }

        if let Some(mut queue_entry) = self
            .db
            .find_active_queue_entry_for_revision(convergence.item_revision_id)
            .await?
        {
            queue_entry.status = ConvergenceQueueEntryStatus::Released;
            queue_entry.released_at.get_or_insert_with(Utc::now);
            queue_entry.updated_at = Utc::now();
            self.db.update_queue_entry(&queue_entry).await?;
        }

        let mut item = self.db.get_item(convergence.item_id).await?;
        if item.current_revision_id == convergence.item_revision_id {
            if item.lifecycle_state != LifecycleState::Done {
                let revision = self.db.get_revision(item.current_revision_id).await?;
                item.lifecycle_state = LifecycleState::Done;
                item.done_reason = Some(DoneReason::Completed);
                item.resolution_source = Some(match revision.approval_policy {
                    ingot_domain::revision::ApprovalPolicy::Required => {
                        ResolutionSource::ApprovalCommand
                    }
                    ingot_domain::revision::ApprovalPolicy::NotRequired => {
                        ResolutionSource::SystemCommand
                    }
                });
                item.approval_state = match revision.approval_policy {
                    ingot_domain::revision::ApprovalPolicy::Required => ApprovalState::Approved,
                    ingot_domain::revision::ApprovalPolicy::NotRequired => {
                        ApprovalState::NotRequired
                    }
                };
                item.closed_at.get_or_insert_with(Utc::now);
            }
            item.escalation_state = EscalationState::None;
            item.escalation_reason = None;
            item.updated_at = Utc::now();
            self.db.update_item(&item).await?;
        }

        Ok(())
    }

    async fn adopt_prepared_convergence(
        &self,
        operation: &GitOperation,
    ) -> Result<(), RuntimeError> {
        let convergence_id = operation
            .entity_id
            .parse::<ingot_domain::ids::ConvergenceId>()
            .map_err(|error| RuntimeError::InvalidState(error.to_string()))?;
        let mut convergence = self.db.get_convergence(convergence_id).await?;
        if matches!(
            convergence.status,
            ConvergenceStatus::Cancelled | ConvergenceStatus::Failed | ConvergenceStatus::Finalized
        ) {
            return Ok(());
        }
        if convergence.status != ConvergenceStatus::Prepared {
            convergence.status = ConvergenceStatus::Prepared;
            convergence.prepared_commit_oid =
                operation.commit_oid.clone().or(operation.new_oid.clone());
            convergence.completed_at.get_or_insert_with(Utc::now);
            self.db.update_convergence(&convergence).await?;
        }

        if let Some(workspace_id) = convergence.integration_workspace_id {
            let mut workspace = self.db.get_workspace(workspace_id).await?;
            workspace.head_commit_oid = operation.commit_oid.clone().or(operation.new_oid.clone());
            workspace.status = WorkspaceStatus::Ready;
            workspace.current_job_id = None;
            workspace.updated_at = Utc::now();
            self.db.update_workspace(&workspace).await?;
        }

        Ok(())
    }

    async fn reconcile_finalize_target_ref_operation(
        &self,
        project: &Project,
        operation: &mut GitOperation,
        paths: &ingot_git::project_repo::ProjectRepoPaths,
    ) -> Result<bool, RuntimeError> {
        let convergence_id = operation
            .entity_id
            .parse::<ingot_domain::ids::ConvergenceId>()
            .map_err(|error| RuntimeError::InvalidState(error.to_string()))?;
        let convergence = self.db.get_convergence(convergence_id).await?;
        let item = self.db.get_item(convergence.item_id).await?;
        let revision = self.db.get_revision(convergence.item_revision_id).await?;
        let target_ref = operation
            .ref_name
            .as_deref()
            .unwrap_or(convergence.target_ref.as_str());
        let prepared_commit_oid = operation
            .new_oid
            .as_deref()
            .or(operation.commit_oid.as_deref())
            .ok_or_else(|| {
                RuntimeError::InvalidState("finalize operation missing new oid".into())
            })?;
        let current_target_oid =
            resolve_ref_oid(paths.mirror_git_dir.as_path(), target_ref).await?;
        if current_target_oid.as_deref() != Some(prepared_commit_oid) {
            operation.status = GitOperationStatus::Failed;
            operation.completed_at = Some(Utc::now());
            self.db.update_git_operation(operation).await?;
            return Ok(true);
        }

        if operation.status == GitOperationStatus::Planned {
            operation.status = GitOperationStatus::Applied;
            operation.completed_at = Some(Utc::now());
            self.db.update_git_operation(operation).await?;
        }

        match checkout_finalization_status(
            Path::new(&project.path),
            &revision.target_ref,
            prepared_commit_oid,
        )
        .await?
        {
            CheckoutFinalizationStatus::Blocked { .. } => {
                self.reconcile_checkout_sync_state(project, item.id, &revision)
                    .await?;
                return Ok(false);
            }
            CheckoutFinalizationStatus::NeedsSync => {
                self.reconcile_checkout_sync_state(project, item.id, &revision)
                    .await?;
                sync_checkout_to_commit(
                    Path::new(&project.path),
                    paths.mirror_git_dir.as_path(),
                    &revision.target_ref,
                    prepared_commit_oid,
                )
                .await?;
            }
            CheckoutFinalizationStatus::Synced => {
                self.reconcile_checkout_sync_state(project, item.id, &revision)
                    .await?;
            }
        }

        self.adopt_reconciled_git_operation(operation).await?;
        self.mark_git_operation_reconciled(operation).await?;
        Ok(true)
    }

    async fn find_unresolved_finalize_operation_for_convergence(
        &self,
        convergence_id: ingot_domain::ids::ConvergenceId,
    ) -> Result<Option<GitOperation>, RuntimeError> {
        let entity_id = convergence_id.to_string();
        Ok(self
            .db
            .list_unresolved_git_operations()
            .await?
            .into_iter()
            .find(|operation| {
                operation.operation_kind == OperationKind::FinalizeTargetRef
                    && operation.entity_type == GitEntityType::Convergence
                    && operation.entity_id == entity_id
            }))
    }

    async fn adopt_reset_workspace(&self, operation: &GitOperation) -> Result<(), RuntimeError> {
        let Some(workspace_id) = operation.workspace_id else {
            return Ok(());
        };
        let mut workspace = self.db.get_workspace(workspace_id).await?;
        workspace.current_job_id = None;
        workspace.status = WorkspaceStatus::Ready;
        if let Some(new_oid) = operation.new_oid.as_ref() {
            workspace.head_commit_oid = Some(new_oid.clone());
        }
        workspace.updated_at = Utc::now();
        self.db.update_workspace(&workspace).await?;
        Ok(())
    }

    async fn adopt_removed_workspace_ref(
        &self,
        operation: &GitOperation,
    ) -> Result<(), RuntimeError> {
        let Some(workspace_id) = operation.workspace_id else {
            return Ok(());
        };
        let mut workspace = self.db.get_workspace(workspace_id).await?;
        workspace.current_job_id = None;
        workspace.status = WorkspaceStatus::Abandoned;
        if operation.ref_name.is_some() {
            workspace.workspace_ref = None;
        }
        workspace.updated_at = Utc::now();
        self.db.update_workspace(&workspace).await?;
        Ok(())
    }

    async fn reconcile_assigned_job(&self, job: Job) -> Result<(), RuntimeError> {
        let _guard = self
            .project_locks
            .acquire_project_mutation(job.project_id)
            .await;
        let mut job = self.db.get_job(job.id).await?;
        if job.status != JobStatus::Assigned {
            return Ok(());
        }

        let workspace_id = job.workspace_id;
        job.status = JobStatus::Queued;
        job.workspace_id = None;
        job.agent_id = None;
        job.process_pid = None;
        job.lease_owner_id = None;
        job.heartbeat_at = None;
        job.lease_expires_at = None;
        job.started_at = None;
        self.db.update_job(&job).await?;

        if let Some(workspace_id) = workspace_id {
            let mut workspace = self.db.get_workspace(workspace_id).await?;
            workspace.current_job_id = None;
            if workspace.status == WorkspaceStatus::Busy {
                workspace.status = WorkspaceStatus::Ready;
            }
            workspace.updated_at = Utc::now();
            self.db.update_workspace(&workspace).await?;
        }

        Ok(())
    }

    async fn reconcile_running_job(&self, job: Job) -> Result<(), RuntimeError> {
        let expired = job
            .lease_expires_at
            .map(|lease| lease <= Utc::now())
            .unwrap_or(true);
        let foreign_owner = job.lease_owner_id.as_deref() != Some(self.lease_owner_id.as_str());
        if !expired && !foreign_owner {
            return Ok(());
        }

        let _guard = self
            .project_locks
            .acquire_project_mutation(job.project_id)
            .await;
        let job = self.db.get_job(job.id).await?;
        if job.status != JobStatus::Running {
            return Ok(());
        }
        let item = self.db.get_item(job.item_id).await?;
        self.db
            .finish_job_non_success(FinishJobNonSuccessParams {
                job_id: job.id,
                item_id: item.id,
                expected_item_revision_id: job.item_revision_id,
                status: JobStatus::Expired,
                outcome_class: Some(OutcomeClass::TransientFailure),
                error_code: Some("heartbeat_expired"),
                error_message: None,
                escalation_reason: None,
            })
            .await?;

        if let Some(workspace_id) = job.workspace_id {
            let mut workspace = self.db.get_workspace(workspace_id).await?;
            workspace.current_job_id = None;
            workspace.status = WorkspaceStatus::Stale;
            workspace.updated_at = Utc::now();
            self.db.update_workspace(&workspace).await?;
        }

        self.append_activity(
            job.project_id,
            ActivityEventType::JobFailed,
            "job",
            job.id.to_string(),
            serde_json::json!({ "item_id": job.item_id, "error_code": "heartbeat_expired" }),
        )
        .await?;

        Ok(())
    }

    async fn reconcile_active_convergences(&self) -> Result<bool, RuntimeError> {
        let active_convergences = self.db.list_active_convergences().await?;
        let mut made_progress = false;
        for convergence in active_convergences {
            let _guard = self
                .project_locks
                .acquire_project_mutation(convergence.project_id)
                .await;
            let mut convergence = convergence;
            if !matches!(
                convergence.status,
                ConvergenceStatus::Queued | ConvergenceStatus::Running
            ) {
                continue;
            }
            convergence.status = ConvergenceStatus::Failed;
            convergence.conflict_summary = Some("startup_recovery_required".into());
            convergence.completed_at = Some(Utc::now());
            self.db.update_convergence(&convergence).await?;

            if let Some(workspace_id) = convergence.integration_workspace_id {
                let mut workspace = self.db.get_workspace(workspace_id).await?;
                workspace.current_job_id = None;
                workspace.status = WorkspaceStatus::Stale;
                workspace.updated_at = Utc::now();
                self.db.update_workspace(&workspace).await?;
            }

            self.append_activity(
                convergence.project_id,
                ActivityEventType::ConvergenceFailed,
                "convergence",
                convergence.id.to_string(),
                serde_json::json!({ "item_id": convergence.item_id, "reason": "startup_recovery_required" }),
            )
            .await?;
            made_progress = true;
        }
        Ok(made_progress)
    }

    async fn reconcile_workspace_retention(&self) -> Result<bool, RuntimeError> {
        let mut made_progress = false;
        for project in self.db.list_projects().await? {
            let workspaces = self.db.list_workspaces_by_project(project.id).await?;
            for workspace in workspaces {
                if workspace.status != WorkspaceStatus::Abandoned
                    || workspace.retention_policy == RetentionPolicy::RetainUntilDebug
                {
                    continue;
                }
                if !self.workspace_can_be_removed(&project, &workspace).await? {
                    continue;
                }
                self.remove_abandoned_workspace(&project, &workspace)
                    .await?;
                made_progress = true;
            }
        }
        Ok(made_progress)
    }

    async fn workspace_can_be_removed(
        &self,
        _project: &Project,
        workspace: &Workspace,
    ) -> Result<bool, RuntimeError> {
        if workspace.kind == WorkspaceKind::Review {
            return Ok(true);
        }
        let Some(revision_id) = workspace.created_for_revision_id else {
            return Ok(true);
        };
        let revision = self.db.get_revision(revision_id).await?;
        let item = self.db.get_item(revision.item_id).await?;
        if matches!(
            workspace.kind,
            WorkspaceKind::Authoring | WorkspaceKind::Integration
        ) && item.current_revision_id == revision.id
            && item.lifecycle_state == LifecycleState::Open
        {
            return Ok(false);
        }

        let findings = self.db.list_findings_by_item(item.id).await?;
        let head_commit_oid = workspace.head_commit_oid.as_deref().unwrap_or_default();
        let blocked = findings.iter().any(|finding| {
            finding.source_item_revision_id == revision.id
                && finding.triage_state.is_unresolved()
                && finding.source_subject_head_commit_oid == head_commit_oid
                && match workspace.kind {
                    WorkspaceKind::Authoring => {
                        finding.source_subject_kind
                            == ingot_domain::finding::FindingSubjectKind::Candidate
                    }
                    WorkspaceKind::Integration => {
                        finding.source_subject_kind
                            == ingot_domain::finding::FindingSubjectKind::Integrated
                    }
                    WorkspaceKind::Review => false,
                }
        });

        Ok(!blocked)
    }

    async fn remove_abandoned_workspace(
        &self,
        project: &Project,
        workspace: &Workspace,
    ) -> Result<(), RuntimeError> {
        let repo_path = self.project_paths(project).mirror_git_dir;
        let path = Path::new(&workspace.path);
        if path.exists() {
            remove_workspace(repo_path.as_path(), path).await?;
        }

        if let Some(workspace_ref) = workspace.workspace_ref.as_deref()
            && let Some(current_oid) = resolve_ref_oid(repo_path.as_path(), workspace_ref).await?
        {
            let mut operation = GitOperation {
                id: GitOperationId::new(),
                project_id: project.id,
                operation_kind: OperationKind::RemoveWorkspaceRef,
                entity_type: GitEntityType::Workspace,
                entity_id: workspace.id.to_string(),
                workspace_id: Some(workspace.id),
                ref_name: Some(workspace_ref.into()),
                expected_old_oid: Some(current_oid),
                new_oid: None,
                commit_oid: None,
                status: GitOperationStatus::Planned,
                metadata: None,
                created_at: Utc::now(),
                completed_at: None,
            };
            self.db.create_git_operation(&operation).await?;
            self.append_activity(
                project.id,
                ActivityEventType::GitOperationPlanned,
                "git_operation",
                operation.id.to_string(),
                serde_json::json!({ "operation_kind": operation.operation_kind, "entity_id": operation.entity_id }),
            )
            .await?;
            delete_ref(repo_path.as_path(), workspace_ref).await?;
            operation.status = GitOperationStatus::Applied;
            operation.completed_at = Some(Utc::now());
            self.db.update_git_operation(&operation).await?;
        }

        Ok(())
    }

    async fn run_with_heartbeats(
        &self,
        prepared: &PreparedRun,
        request: AgentRequest,
    ) -> Result<AgentResponse, AgentError> {
        let timeout_duration = self.config.job_timeout;
        let lease_expires_at = Utc::now() + ChronoDuration::minutes(30);
        self.db
            .start_job_execution(StartJobExecutionParams {
                job_id: prepared.job.id,
                item_id: prepared.item.id,
                expected_item_revision_id: prepared.job.item_revision_id,
                workspace_id: Some(prepared.workspace.id),
                agent_id: Some(prepared.agent.id),
                lease_owner_id: &self.lease_owner_id,
                process_pid: None,
                lease_expires_at,
            })
            .await
            .map_err(|error| AgentError::ProcessError(error.to_string()))?;
        info!(
            job_id = %prepared.job.id,
            agent_id = %prepared.agent.id,
            workspace_id = %prepared.workspace.id,
            lease_owner_id = %self.lease_owner_id,
            timeout_seconds = timeout_duration.as_secs(),
            "job entered running state"
        );

        let runner = self.runner.clone();
        let agent = prepared.agent.clone();
        let working_dir = PathBuf::from(&prepared.workspace.path);
        let span = info_span!(
            "job_execution",
            job_id = %prepared.job.id,
            item_id = %prepared.item.id,
            step_id = %prepared.job.step_id,
            agent_id = %prepared.agent.id,
            workspace_id = %prepared.workspace.id
        );
        let mut handle = tokio::spawn(async move {
            runner
                .launch(&agent, &request, &working_dir)
                .instrument(span)
                .await
        });
        let mut ticker = interval(self.config.heartbeat_interval);
        let timeout = tokio::time::sleep(timeout_duration);
        tokio::pin!(timeout);

        loop {
            tokio::select! {
                result = &mut handle => {
                    let result = result.map_err(|error| AgentError::ProcessError(error.to_string()))?;
                    debug!(job_id = %prepared.job.id, "job execution future resolved");
                    return result;
                }
                _ = &mut timeout => {
                    handle.abort();
                    warn!(job_id = %prepared.job.id, timeout_seconds = timeout_duration.as_secs(), "job execution timed out");
                    return Err(AgentError::Timeout);
                }
                _ = ticker.tick() => {
                    match self.db.get_job(prepared.job.id).await {
                        Ok(job) if job.status == JobStatus::Cancelled => {
                            handle.abort();
                            info!(job_id = %prepared.job.id, "cancelling running job after operator request");
                            return Err(AgentError::ProcessError("job cancelled".into()));
                        }
                        Ok(_) => {}
                        Err(error) => {
                            warn!(?error, job_id = %prepared.job.id, "failed to load job during heartbeat tick");
                        }
                    }
                    let lease_expires_at = Utc::now() + ChronoDuration::minutes(30);
                    if let Err(error) = self.db.heartbeat_job_execution(
                        prepared.job.id,
                        prepared.item.id,
                        prepared.job.item_revision_id,
                        &self.lease_owner_id,
                        lease_expires_at,
                    ).await {
                        warn!(?error, job_id = %prepared.job.id, "job heartbeat update failed");
                    } else {
                        debug!(job_id = %prepared.job.id, "job heartbeat updated");
                    }
                }
            }
        }
    }

    async fn hydrate_convergences(
        &self,
        project: &Project,
        convergences: Vec<Convergence>,
    ) -> Result<Vec<Convergence>, RuntimeError> {
        let paths = self.refresh_project_mirror(project).await?;
        let mut hydrated = Vec::with_capacity(convergences.len());
        for mut convergence in convergences {
            convergence.target_head_valid = self
                .compute_target_head_valid(paths.mirror_git_dir.as_path(), &convergence)
                .await?;
            hydrated.push(convergence);
        }
        Ok(hydrated)
    }

    async fn compute_target_head_valid(
        &self,
        repo_path: &Path,
        convergence: &Convergence,
    ) -> Result<Option<bool>, RuntimeError> {
        let resolved = resolve_ref_oid(repo_path, &convergence.target_ref).await?;
        Ok(convergence.target_head_valid_for_resolved_oid(resolved.as_deref()))
    }

    async fn prepare_run(&self, queued_job: Job) -> Result<Option<PreparedRun>, RuntimeError> {
        let _guard = self
            .project_locks
            .acquire_project_mutation(queued_job.project_id)
            .await;

        let mut job = self.db.get_job(queued_job.id).await?;
        if job.status != JobStatus::Queued || !is_supported_runtime_job(&job) {
            return Ok(None);
        }

        let item = self.db.get_item(job.item_id).await?;
        if item.current_revision_id != job.item_revision_id {
            return Ok(None);
        }

        let revision = self.db.get_revision(job.item_revision_id).await?;
        let project = self.db.get_project(job.project_id).await?;
        let paths = self.refresh_project_mirror(&project).await?;
        let Some(agent) = self.select_agent(&job).await? else {
            debug!(
                job_id = %job.id,
                step_id = %job.step_id,
                "queued job is waiting for a compatible available agent"
            );
            return Ok(None);
        };
        let now = Utc::now();
        let (workspace, workspace_lifecycle, workspace_exists) = self
            .prepare_workspace(
                &project,
                paths.mirror_git_dir.as_path(),
                &paths.worktree_root,
                &revision,
                &job,
                now,
            )
            .await?;
        let mut workspace = workspace;
        let original_head_commit_oid = workspace
            .head_commit_oid
            .clone()
            .ok_or_else(|| RuntimeError::InvalidState("workspace missing head".into()))?;

        workspace.status = WorkspaceStatus::Busy;
        workspace.current_job_id = Some(job.id);
        workspace.updated_at = now;
        if workspace_exists {
            self.db.update_workspace(&workspace).await?;
        } else {
            self.db.create_workspace(&workspace).await?;
        }

        let template = built_in_template(&job.phase_template_slug, &job.step_id);
        let prompt = self
            .assemble_prompt(&job, &item, &revision, template)
            .await?;
        job.workspace_id = Some(workspace.id);
        job.agent_id = Some(agent.id);
        job.prompt_snapshot = Some(prompt.clone());
        job.phase_template_digest = Some(template_digest(template));
        job.error_code = None;
        job.error_message = None;
        self.db.update_job(&job).await?;

        info!(
            job_id = %job.id,
            workspace_id = %workspace.id,
            agent_id = %agent.id,
            step_id = %job.step_id,
            project_id = %project.id,
            item_id = %item.id,
            "prepared job execution"
        );

        Ok(Some(PreparedRun {
            job,
            item,
            revision,
            project,
            canonical_repo_path: paths.mirror_git_dir,
            agent,
            workspace,
            original_head_commit_oid,
            prompt,
            workspace_lifecycle,
        }))
    }

    async fn select_agent(&self, job: &Job) -> Result<Option<Agent>, RuntimeError> {
        let mut agents = self
            .db
            .list_agents()
            .await?
            .into_iter()
            .filter(|agent| agent.status == AgentStatus::Available)
            .filter(|agent| agent.adapter_kind == AdapterKind::Codex)
            .filter(|agent| supports_job(agent, job))
            .collect::<Vec<_>>();
        agents.sort_by(|left, right| left.slug.cmp(&right.slug));
        Ok(agents.into_iter().next())
    }

    async fn prepare_workspace(
        &self,
        project: &Project,
        repo_path: &Path,
        workspace_root: &Path,
        revision: &ItemRevision,
        job: &Job,
        now: chrono::DateTime<Utc>,
    ) -> Result<(Workspace, WorkspaceLifecycle, bool), RuntimeError> {
        match (job.workspace_kind, job.execution_permission) {
            (WorkspaceKind::Authoring, _) => {
                let existing_workspace = self
                    .db
                    .find_authoring_workspace_for_revision(revision.id)
                    .await?;
                let workspace_exists = existing_workspace.is_some();
                let workspace = ensure_authoring_workspace_state(
                    existing_workspace,
                    project.id,
                    repo_path,
                    workspace_root,
                    revision,
                    job,
                    now,
                )
                .await?;
                Ok((
                    workspace,
                    WorkspaceLifecycle::PersistentAuthoring,
                    workspace_exists,
                ))
            }
            (WorkspaceKind::Integration, ExecutionPermission::MustNotMutate) => {
                let workspace_id = job.workspace_id.ok_or_else(|| {
                    RuntimeError::InvalidState(
                        "integration jobs require a provisioned integration workspace".into(),
                    )
                })?;
                let existing_workspace = self.db.get_workspace(workspace_id).await?;
                let workspace_exists = true;
                let expected_head_commit_oid =
                    job.input_head_commit_oid.clone().ok_or_else(|| {
                        RuntimeError::InvalidState(
                            "integration jobs require input_head_commit_oid".into(),
                        )
                    })?;
                let workspace_ref = existing_workspace.workspace_ref.clone().ok_or_else(|| {
                    RuntimeError::InvalidState("integration workspace missing workspace_ref".into())
                })?;
                let provisioned = provision_integration_workspace(
                    repo_path,
                    Path::new(&existing_workspace.path),
                    &workspace_ref,
                    &expected_head_commit_oid,
                )
                .await?;
                let mut workspace = existing_workspace;
                workspace.path = provisioned.workspace_path.display().to_string();
                workspace.head_commit_oid = Some(provisioned.head_commit_oid);
                workspace.workspace_ref = Some(provisioned.workspace_ref);
                workspace.status = WorkspaceStatus::Ready;
                workspace.updated_at = now;
                Ok((
                    workspace,
                    WorkspaceLifecycle::PersistentIntegration,
                    workspace_exists,
                ))
            }
            (WorkspaceKind::Review, ExecutionPermission::MustNotMutate) => {
                let head_commit_oid = job.input_head_commit_oid.clone().ok_or_else(|| {
                    RuntimeError::InvalidState("review jobs require input_head_commit_oid".into())
                })?;
                let workspace_id = WorkspaceId::new();
                let workspace_path = workspace_root.join(workspace_id.to_string());
                let provisioned =
                    provision_review_workspace(repo_path, &workspace_path, &head_commit_oid)
                        .await?;
                let workspace = Workspace {
                    id: workspace_id,
                    project_id: project.id,
                    kind: WorkspaceKind::Review,
                    strategy: WorkspaceStrategy::Worktree,
                    path: provisioned.workspace_path.display().to_string(),
                    created_for_revision_id: Some(revision.id),
                    parent_workspace_id: None,
                    target_ref: None,
                    workspace_ref: None,
                    base_commit_oid: job.input_base_commit_oid.clone(),
                    head_commit_oid: Some(provisioned.head_commit_oid),
                    retention_policy: RetentionPolicy::Ephemeral,
                    status: WorkspaceStatus::Ready,
                    current_job_id: None,
                    created_at: now,
                    updated_at: now,
                };
                Ok((workspace, WorkspaceLifecycle::EphemeralReview, false))
            }
            _ => Err(RuntimeError::InvalidState(format!(
                "unsupported runtime workspace kind {:?} for step {}",
                job.workspace_kind, job.step_id
            ))),
        }
    }

    async fn assemble_prompt(
        &self,
        job: &Job,
        item: &ingot_domain::item::Item,
        revision: &ItemRevision,
        template: &str,
    ) -> Result<String, RuntimeError> {
        let revision_context = self.db.get_revision_context(revision.id).await?;
        let context_block = format_revision_context(revision_context.as_ref());
        let workspace_kind = match job.workspace_kind {
            WorkspaceKind::Authoring => "authoring",
            WorkspaceKind::Review => "review",
            WorkspaceKind::Integration => "integration",
        };
        let execution = match job.execution_permission {
            ExecutionPermission::MayMutate => "may_mutate",
            ExecutionPermission::MustNotMutate => "must_not_mutate",
            ExecutionPermission::DaemonOnly => "daemon_only",
        };

        let mut prompt = format!(
            "Revision contract:\n- Item ID: {}\n- Revision: {}\n- Title: {}\n- Description: {}\n- Acceptance criteria: {}\n- Target ref: {}\n- Approval policy: {:?}\n\nWorkflow step:\n- Step: {}\n- Template: {}\n- Workspace: {}\n- Execution: {}\n",
            item.id,
            revision.revision_no,
            revision.title,
            revision.description,
            revision.acceptance_criteria,
            revision.target_ref,
            revision.approval_policy,
            job.step_id,
            job.phase_template_slug,
            workspace_kind,
            execution,
        );

        if let Some(base) = job.input_base_commit_oid.as_deref() {
            prompt.push_str(&format!("- Input base commit: {base}\n"));
        }
        if let Some(head) = job.input_head_commit_oid.as_deref() {
            prompt.push_str(&format!("- Input head commit: {head}\n"));
        }

        prompt.push_str(&format!(
            "\nTemplate prompt:\n{}\n\nRevision context:\n{}\n\n",
            template, context_block
        ));

        if matches!(
            job.step_id.as_str(),
            "repair_candidate" | "repair_after_integration"
        ) {
            let jobs = self.db.list_jobs_by_item(item.id).await?;
            let findings = self.db.list_findings_by_item(item.id).await?;
            let latest_closure_findings_job = jobs
                .iter()
                .filter(|candidate| candidate.item_revision_id == revision.id)
                .filter(|candidate| candidate.status.is_terminal())
                .filter(|candidate| candidate.outcome_class == Some(OutcomeClass::Findings))
                .filter(|candidate| is_closure_relevant_job(candidate))
                .max_by_key(|candidate| (candidate.ended_at, candidate.created_at));

            if let Some(latest_job) = latest_closure_findings_job {
                let scoped_findings = findings
                    .iter()
                    .filter(|finding| finding.source_item_revision_id == revision.id)
                    .filter(|finding| finding.source_job_id == latest_job.id)
                    .collect::<Vec<_>>();
                let fix_now_findings = scoped_findings
                    .iter()
                    .filter(|finding| finding.triage_state == FindingTriageState::FixNow)
                    .collect::<Vec<_>>();
                let accepted_findings = scoped_findings
                    .iter()
                    .filter(|finding| {
                        !matches!(
                            finding.triage_state,
                            FindingTriageState::Untriaged
                                | FindingTriageState::FixNow
                                | FindingTriageState::NeedsInvestigation
                        )
                    })
                    .collect::<Vec<_>>();

                if !fix_now_findings.is_empty() || !accepted_findings.is_empty() {
                    prompt.push_str("Finding triage for this repair:\n");
                }
                if !fix_now_findings.is_empty() {
                    prompt.push_str("- Fix now findings:\n");
                    for finding in &fix_now_findings {
                        prompt.push_str(&format!(
                            "  - [{}] {} ({:?})\n",
                            finding.code, finding.summary, finding.severity
                        ));
                    }
                }
                if !accepted_findings.is_empty() {
                    prompt.push_str("- Already triaged as non-blocking for this attempt:\n");
                    for finding in &accepted_findings {
                        prompt.push_str(&format!(
                            "  - [{}] {} => {:?}\n",
                            finding.code, finding.summary, finding.triage_state
                        ));
                    }
                }
                if !fix_now_findings.is_empty() || !accepted_findings.is_empty() {
                    prompt.push('\n');
                }
            }
        }

        match job.output_artifact_kind {
            OutputArtifactKind::Commit => {
                prompt.push_str(
                    "Protocol:\n- Edit files inside the current repository to satisfy the revision contract.\n- You may run local validation commands when useful.\n- Do not create commits, amend commits, rebase, merge, cherry-pick, or move refs.\n- Leave all changes unstaged or staged in the working tree; Ingot will create the canonical commit.\n- Return a structured object with keys `summary` and `validation`; set `validation` to null when no validation was run.\n",
                );
            }
            OutputArtifactKind::ReviewReport
            | OutputArtifactKind::ValidationReport
            | OutputArtifactKind::FindingReport => {
                prompt.push_str(
                    "Protocol:\n- Do not modify files, create commits, rebase, merge, cherry-pick, or move refs.\n- Inspect the current workspace subject and produce only the canonical structured report for this step.\n- Any non-core data must go under `extensions`.\n",
                );
                prompt.push_str(report_prompt_suffix(job));
            }
            OutputArtifactKind::None => {
                prompt.push_str("Protocol:\n- No output artifact is expected for this step.\n");
            }
        }

        Ok(prompt)
    }

    async fn finish_run(
        &self,
        prepared: PreparedRun,
        response: AgentResponse,
    ) -> Result<(), RuntimeError> {
        let current_job = self.db.get_job(prepared.job.id).await?;
        if current_job.status == JobStatus::Cancelled {
            self.finalize_workspace_after_failure(&prepared).await?;
            info!(job_id = %prepared.job.id, "job was cancelled while subprocess was running");
            return Ok(());
        }

        if response.exit_code != 0 {
            warn!(
                job_id = %prepared.job.id,
                exit_code = response.exit_code,
                stderr = non_empty_message(&response.stderr).as_deref().unwrap_or(""),
                stdout = non_empty_message(&response.stdout).as_deref().unwrap_or(""),
                "agent process exited non-zero"
            );
            return self
                .fail_run(
                    &prepared,
                    OutcomeClass::TerminalFailure,
                    "agent_exit_nonzero",
                    non_empty_message(&response.stderr)
                        .or_else(|| non_empty_message(&response.stdout)),
                )
                .await;
        }

        match prepared.job.output_artifact_kind {
            OutputArtifactKind::Commit => self.finish_commit_run(prepared, response).await,
            OutputArtifactKind::ReviewReport
            | OutputArtifactKind::ValidationReport
            | OutputArtifactKind::FindingReport => self.finish_report_run(prepared, response).await,
            OutputArtifactKind::None => {
                self.fail_run(
                    &prepared,
                    OutcomeClass::TerminalFailure,
                    "unsupported_output_artifact",
                    Some("runtime does not support artifact-free jobs yet".into()),
                )
                .await
            }
        }
    }

    async fn finish_commit_run(
        &self,
        prepared: PreparedRun,
        response: AgentResponse,
    ) -> Result<(), RuntimeError> {
        if let Err(error) = self.verify_mutating_workspace_protocol(&prepared).await {
            return self
                .fail_run(
                    &prepared,
                    OutcomeClass::ProtocolViolation,
                    "workspace_protocol_violation",
                    Some(format!("{error:?}")),
                )
                .await;
        }

        let workspace_path = Path::new(&prepared.workspace.path);
        if !working_tree_has_changes(workspace_path).await? {
            return self
                .fail_run(
                    &prepared,
                    OutcomeClass::TerminalFailure,
                    "no_valid_change_set",
                    Some("authoring job completed without producing a change set".into()),
                )
                .await;
        }

        let commit_oid = self.create_commit(&prepared, &response).await?;
        self.complete_commit_run(&prepared, &commit_oid).await
    }

    async fn finish_report_run(
        &self,
        prepared: PreparedRun,
        response: AgentResponse,
    ) -> Result<(), RuntimeError> {
        if let Err(error) = self.verify_read_only_workspace_protocol(&prepared).await {
            return self
                .fail_run(
                    &prepared,
                    OutcomeClass::ProtocolViolation,
                    "workspace_protocol_violation",
                    Some(error.to_string()),
                )
                .await;
        }

        let result_payload = response.result.clone().ok_or_else(|| {
            RuntimeError::InvalidState("report job did not return structured output".into())
        });
        let result_payload = match result_payload {
            Ok(payload) => payload,
            Err(error) => {
                return self
                    .fail_run(
                        &prepared,
                        OutcomeClass::ProtocolViolation,
                        "missing_structured_result",
                        Some(error.to_string()),
                    )
                    .await;
            }
        };

        let outcome_class =
            report_outcome_class(&result_payload).map_err(RuntimeError::InvalidState);
        let outcome_class = match outcome_class {
            Ok(outcome_class) => outcome_class,
            Err(error) => {
                return self
                    .fail_run(
                        &prepared,
                        OutcomeClass::ProtocolViolation,
                        "invalid_report_outcome",
                        Some(error.to_string()),
                    )
                    .await;
            }
        };

        let result_schema_version = result_schema_version(prepared.job.output_artifact_kind)
            .ok_or_else(|| {
                RuntimeError::InvalidState("report job missing schema version mapping".into())
            })?;

        if let Err(error) = self
            .complete_job_service()
            .execute(CompleteJobCommand {
                job_id: prepared.job.id,
                outcome_class,
                result_schema_version: Some(result_schema_version.to_string()),
                result_payload: Some(result_payload),
                output_commit_oid: None,
            })
            .await
        {
            return self
                .fail_run(
                    &prepared,
                    OutcomeClass::ProtocolViolation,
                    "report_completion_rejected",
                    Some(format!("{error:?}")),
                )
                .await;
        }
        self.append_activity(
            prepared.project.id,
            ActivityEventType::JobCompleted,
            "job",
            prepared.job.id.to_string(),
            serde_json::json!({ "item_id": prepared.item.id, "outcome": outcome_class_name(outcome_class) }),
        )
        .await?;
        if prepared.job.step_id == "validate_integrated" && outcome_class == OutcomeClass::Clean {
            let updated_item = self.db.get_item(prepared.item.id).await?;
            if updated_item.approval_state == ApprovalState::Pending {
                self.append_activity(
                    prepared.project.id,
                    ActivityEventType::ApprovalRequested,
                    "item",
                    prepared.item.id.to_string(),
                    serde_json::json!({ "job_id": prepared.job.id }),
                )
                .await?;
            }
        }

        self.finalize_workspace_after_success(&prepared, None)
            .await?;
        self.refresh_revision_context(&prepared).await?;
        self.append_escalation_cleared_activity_if_needed(&prepared)
            .await?;
        self.auto_dispatch_projected_review(prepared.project.id, prepared.item.id)
            .await?;
        info!(
            job_id = %prepared.job.id,
            step_id = %prepared.job.step_id,
            "completed report job"
        );
        Ok(())
    }

    async fn auto_finalize_prepared_convergence(
        &self,
        project_id: ingot_domain::ids::ProjectId,
        item_id: ingot_domain::ids::ItemId,
    ) -> Result<(), RuntimeError> {
        let _guard = self
            .project_locks
            .acquire_project_mutation(project_id)
            .await;
        let project = self.db.get_project(project_id).await?;
        let paths = self.refresh_project_mirror(&project).await?;
        let item = self.db.get_item(item_id).await?;
        let revision = self.db.get_revision(item.current_revision_id).await?;
        let jobs = self.db.list_jobs_by_item(item.id).await?;
        let findings = self.db.list_findings_by_item(item.id).await?;
        let convergences = self
            .hydrate_convergences(&project, self.db.list_convergences_by_item(item.id).await?)
            .await?;
        let evaluation =
            Evaluator::new().evaluate(&item, &revision, &jobs, &findings, &convergences);
        let queue_entry = self
            .db
            .find_active_queue_entry_for_revision(revision.id)
            .await?;
        if queue_entry
            .as_ref()
            .map(|entry| entry.status != ConvergenceQueueEntryStatus::Head)
            .unwrap_or(true)
        {
            return Ok(());
        }
        if item.approval_state != ApprovalState::Granted
            && !(revision.approval_policy == ingot_domain::revision::ApprovalPolicy::NotRequired
                && evaluation.next_recommended_action == "finalize_prepared_convergence")
        {
            return Ok(());
        }
        if self
            .reconcile_checkout_sync_state(&project, item.id, &revision)
            .await?
            != CheckoutSyncStatus::Ready
        {
            return Ok(());
        }

        let convergence = convergences
            .into_iter()
            .find(|convergence| {
                convergence.item_revision_id == revision.id
                    && convergence.status == ConvergenceStatus::Prepared
            })
            .ok_or_else(|| RuntimeError::InvalidState("prepared convergence missing".into()))?;
        let prepared_commit_oid = convergence
            .prepared_commit_oid
            .clone()
            .ok_or_else(|| RuntimeError::InvalidState("prepared commit missing".into()))?;
        let input_target_commit_oid = convergence
            .input_target_commit_oid
            .clone()
            .ok_or_else(|| RuntimeError::InvalidState("input target commit missing".into()))?;

        let mut operation = if let Some(operation) = self
            .find_unresolved_finalize_operation_for_convergence(convergence.id)
            .await?
        {
            operation
        } else {
            let operation = GitOperation {
                id: GitOperationId::new(),
                project_id,
                operation_kind: OperationKind::FinalizeTargetRef,
                entity_type: GitEntityType::Convergence,
                entity_id: convergence.id.to_string(),
                workspace_id: convergence.integration_workspace_id,
                ref_name: Some(convergence.target_ref.clone()),
                expected_old_oid: Some(input_target_commit_oid.clone()),
                new_oid: Some(prepared_commit_oid.clone()),
                commit_oid: Some(prepared_commit_oid.clone()),
                status: GitOperationStatus::Planned,
                metadata: None,
                created_at: Utc::now(),
                completed_at: None,
            };
            self.db.create_git_operation(&operation).await?;
            self.append_activity(
                project_id,
                ActivityEventType::GitOperationPlanned,
                "git_operation",
                operation.id.to_string(),
                serde_json::json!({ "operation_kind": operation.operation_kind, "entity_id": operation.entity_id }),
            )
            .await?;
            operation
        };

        let current_target_oid =
            resolve_ref_oid(paths.mirror_git_dir.as_path(), &convergence.target_ref).await?;
        if current_target_oid.as_deref() != Some(prepared_commit_oid.as_str()) {
            compare_and_swap_ref(
                paths.mirror_git_dir.as_path(),
                &convergence.target_ref,
                &prepared_commit_oid,
                &input_target_commit_oid,
            )
            .await?;
        }

        if operation.status == GitOperationStatus::Planned {
            operation.status = GitOperationStatus::Applied;
            operation.completed_at = Some(Utc::now());
            self.db.update_git_operation(&operation).await?;
        }

        match checkout_finalization_status(
            Path::new(&project.path),
            &revision.target_ref,
            &prepared_commit_oid,
        )
        .await?
        {
            CheckoutFinalizationStatus::Blocked { .. } => {
                self.reconcile_checkout_sync_state(&project, item.id, &revision)
                    .await?;
                return Ok(());
            }
            CheckoutFinalizationStatus::NeedsSync => {
                self.reconcile_checkout_sync_state(&project, item.id, &revision)
                    .await?;
                sync_checkout_to_commit(
                    Path::new(&project.path),
                    paths.mirror_git_dir.as_path(),
                    &revision.target_ref,
                    &prepared_commit_oid,
                )
                .await?;
            }
            CheckoutFinalizationStatus::Synced => {
                self.reconcile_checkout_sync_state(&project, item.id, &revision)
                    .await?;
            }
        }

        self.adopt_finalized_target_ref(&operation).await?;
        self.mark_git_operation_reconciled(&mut operation).await?;
        self.append_activity(
            project_id,
            ActivityEventType::ConvergenceFinalized,
            "convergence",
            convergence.id.to_string(),
            serde_json::json!({ "item_id": item.id }),
        )
        .await?;
        if revision.approval_policy == ingot_domain::revision::ApprovalPolicy::Required {
            self.append_activity(
                project_id,
                ActivityEventType::CheckoutSyncCleared,
                "item",
                item.id.to_string(),
                serde_json::json!({ "reason": "finalized" }),
            )
            .await?;
        }

        info!(item_id = %item.id, convergence_id = %convergence.id, "auto-finalized prepared convergence");
        Ok(())
    }

    async fn invalidate_prepared_convergence(
        &self,
        project_id: ingot_domain::ids::ProjectId,
        item_id: ingot_domain::ids::ItemId,
    ) -> Result<(), RuntimeError> {
        let _guard = self
            .project_locks
            .acquire_project_mutation(project_id)
            .await;
        let project = self.db.get_project(project_id).await?;
        let mut item = self.db.get_item(item_id).await?;
        let revision = self.db.get_revision(item.current_revision_id).await?;
        let jobs = self.db.list_jobs_by_item(item.id).await?;
        let findings = self.db.list_findings_by_item(item.id).await?;
        let convergences = self
            .hydrate_convergences(&project, self.db.list_convergences_by_item(item.id).await?)
            .await?;
        let evaluation =
            Evaluator::new().evaluate(&item, &revision, &jobs, &findings, &convergences);
        if evaluation.next_recommended_action != "invalidate_prepared_convergence" {
            return Ok(());
        }

        let mut convergence = convergences
            .into_iter()
            .find(|convergence| {
                convergence.item_revision_id == revision.id
                    && convergence.status == ConvergenceStatus::Prepared
            })
            .ok_or_else(|| RuntimeError::InvalidState("prepared convergence missing".into()))?;
        convergence.status = ConvergenceStatus::Failed;
        convergence.conflict_summary = Some("target_ref_moved".into());
        convergence.completed_at = Some(Utc::now());
        self.db.update_convergence(&convergence).await?;

        if let Some(workspace_id) = convergence.integration_workspace_id {
            let mut workspace = self.db.get_workspace(workspace_id).await?;
            workspace.status = WorkspaceStatus::Stale;
            workspace.current_job_id = None;
            workspace.updated_at = Utc::now();
            self.db.update_workspace(&workspace).await?;
        }

        if item.approval_state != ApprovalState::Granted {
            item.approval_state = match revision.approval_policy {
                ingot_domain::revision::ApprovalPolicy::Required => ApprovalState::NotRequested,
                ingot_domain::revision::ApprovalPolicy::NotRequired => ApprovalState::NotRequired,
            };
        }
        item.updated_at = Utc::now();
        self.db.update_item(&item).await?;
        self.append_activity(
            project_id,
            ActivityEventType::ConvergenceFailed,
            "convergence",
            convergence.id.to_string(),
            serde_json::json!({ "item_id": item.id, "reason": "target_ref_moved" }),
        )
        .await?;

        info!(item_id = %item.id, convergence_id = %convergence.id, "invalidated stale prepared convergence");
        Ok(())
    }

    async fn fail_prepare_convergence_attempt(
        &self,
        project: &Project,
        item: &ingot_domain::item::Item,
        revision: &ItemRevision,
        queue_entry: &ConvergenceQueueEntry,
        integration_workspace: &mut Workspace,
        convergence: &mut Convergence,
        operation: &mut GitOperation,
        source_commit_oids: &[String],
        prepared_commit_oids: &[String],
        summary: String,
        status: ConvergenceStatus,
    ) -> Result<(), RuntimeError> {
        integration_workspace.status = WorkspaceStatus::Error;
        integration_workspace.current_job_id = None;
        integration_workspace.updated_at = Utc::now();
        self.db.update_workspace(integration_workspace).await?;

        convergence.status = status;
        convergence.conflict_summary = Some(summary.clone());
        convergence.completed_at = Some(Utc::now());
        self.db.update_convergence(convergence).await?;

        let escalation_reason = match status {
            ConvergenceStatus::Conflicted => EscalationReason::ConvergenceConflict,
            _ => EscalationReason::StepFailed,
        };
        let mut escalated_item = self.db.get_item(item.id).await?;
        escalated_item.approval_state = match revision.approval_policy {
            ingot_domain::revision::ApprovalPolicy::Required => ApprovalState::NotRequested,
            ingot_domain::revision::ApprovalPolicy::NotRequired => ApprovalState::NotRequired,
        };
        escalated_item.escalation_state = EscalationState::OperatorRequired;
        escalated_item.escalation_reason = Some(escalation_reason);
        escalated_item.updated_at = Utc::now();
        self.db.update_item(&escalated_item).await?;

        let mut released_queue = queue_entry.clone();
        released_queue.status = ConvergenceQueueEntryStatus::Released;
        released_queue.released_at = Some(Utc::now());
        released_queue.updated_at = Utc::now();
        self.db.update_queue_entry(&released_queue).await?;

        operation.status = GitOperationStatus::Failed;
        operation.completed_at = Some(Utc::now());
        operation.metadata = Some(serde_json::json!({
            "source_commit_oids": source_commit_oids,
            "prepared_commit_oids": prepared_commit_oids,
        }));
        self.db.update_git_operation(operation).await?;

        let event_type = match status {
            ConvergenceStatus::Conflicted => ActivityEventType::ConvergenceConflicted,
            _ => ActivityEventType::ConvergenceFailed,
        };
        self.append_activity(
            project.id,
            event_type,
            "convergence",
            convergence.id.to_string(),
            serde_json::json!({ "item_id": item.id, "summary": summary }),
        )
        .await?;
        self.append_activity(
            project.id,
            ActivityEventType::ItemEscalated,
            "item",
            item.id.to_string(),
            serde_json::json!({ "reason": escalation_reason }),
        )
        .await?;

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn prepare_queue_head_convergence(
        &self,
        project: &Project,
        item: &ingot_domain::item::Item,
        revision: &ItemRevision,
        jobs: &[Job],
        findings: &[ingot_domain::finding::Finding],
        convergences: &[Convergence],
        queue_entry: &ConvergenceQueueEntry,
    ) -> Result<(), RuntimeError> {
        let _guard = self
            .project_locks
            .acquire_project_mutation(project.id)
            .await;

        let current_item = self.db.get_item(item.id).await?;
        if current_item.current_revision_id != revision.id {
            return Ok(());
        }
        let current_queue = self
            .db
            .find_active_queue_entry_for_revision(revision.id)
            .await?;
        if current_queue
            .as_ref()
            .map(|entry| {
                entry.id != queue_entry.id || entry.status != ConvergenceQueueEntryStatus::Head
            })
            .unwrap_or(true)
        {
            return Ok(());
        }

        if convergences.iter().any(|convergence| {
            convergence.item_revision_id == revision.id && convergence.status.is_active()
        }) {
            return Ok(());
        }

        let source_workspace = self
            .db
            .find_authoring_workspace_for_revision(revision.id)
            .await?
            .ok_or_else(|| RuntimeError::InvalidState("authoring workspace missing".into()))?;
        let source_head_commit_oid = current_authoring_head_for_revision(jobs, revision);
        let paths = self.refresh_project_mirror(project).await?;
        let repo_path = paths.mirror_git_dir.as_path();
        let input_target_commit_oid = resolve_ref_oid(repo_path, &revision.target_ref)
            .await?
            .ok_or_else(|| RuntimeError::InvalidState("target ref unresolved".into()))?;

        let integration_workspace_id = WorkspaceId::new();
        let integration_workspace_path = paths
            .worktree_root
            .join(integration_workspace_id.to_string());
        let integration_workspace_ref = format!("refs/ingot/workspaces/{integration_workspace_id}");
        let now = Utc::now();
        let mut integration_workspace = Workspace {
            id: integration_workspace_id,
            project_id: project.id,
            kind: WorkspaceKind::Integration,
            strategy: WorkspaceStrategy::Worktree,
            path: integration_workspace_path.display().to_string(),
            created_for_revision_id: Some(revision.id),
            parent_workspace_id: Some(source_workspace.id),
            target_ref: Some(revision.target_ref.clone()),
            workspace_ref: Some(integration_workspace_ref.clone()),
            base_commit_oid: Some(input_target_commit_oid.clone()),
            head_commit_oid: Some(input_target_commit_oid.clone()),
            retention_policy: RetentionPolicy::Persistent,
            status: WorkspaceStatus::Provisioning,
            current_job_id: None,
            created_at: now,
            updated_at: now,
        };
        self.db.create_workspace(&integration_workspace).await?;

        let provisioned = provision_integration_workspace(
            repo_path,
            &integration_workspace_path,
            &integration_workspace_ref,
            &input_target_commit_oid,
        )
        .await?;
        integration_workspace.path = provisioned.workspace_path.display().to_string();
        integration_workspace.workspace_ref = Some(provisioned.workspace_ref);
        integration_workspace.head_commit_oid = Some(provisioned.head_commit_oid);
        integration_workspace.status = WorkspaceStatus::Busy;
        integration_workspace.updated_at = Utc::now();
        self.db.update_workspace(&integration_workspace).await?;

        let mut convergence = Convergence {
            id: ingot_domain::ids::ConvergenceId::new(),
            project_id: project.id,
            item_id: item.id,
            item_revision_id: revision.id,
            source_workspace_id: source_workspace.id,
            integration_workspace_id: Some(integration_workspace.id),
            source_head_commit_oid: source_head_commit_oid.clone(),
            target_ref: revision.target_ref.clone(),
            strategy: ingot_domain::convergence::ConvergenceStrategy::RebaseThenFastForward,
            status: ConvergenceStatus::Running,
            input_target_commit_oid: Some(input_target_commit_oid.clone()),
            prepared_commit_oid: None,
            final_target_commit_oid: None,
            target_head_valid: Some(true),
            conflict_summary: None,
            created_at: now,
            completed_at: None,
        };
        self.db.create_convergence(&convergence).await?;
        self.append_activity(
            project.id,
            ActivityEventType::ConvergenceStarted,
            "convergence",
            convergence.id.to_string(),
            serde_json::json!({ "item_id": item.id, "queue_entry_id": queue_entry.id }),
        )
        .await?;

        let source_commit_oids = list_commits_oldest_first(
            repo_path,
            &revision.seed_commit_oid,
            &source_head_commit_oid,
        )
        .await?;
        let mut operation = GitOperation {
            id: GitOperationId::new(),
            project_id: project.id,
            operation_kind: OperationKind::PrepareConvergenceCommit,
            entity_type: GitEntityType::Convergence,
            entity_id: convergence.id.to_string(),
            workspace_id: Some(integration_workspace.id),
            ref_name: integration_workspace.workspace_ref.clone(),
            expected_old_oid: Some(input_target_commit_oid.clone()),
            new_oid: None,
            commit_oid: None,
            status: GitOperationStatus::Planned,
            metadata: Some(serde_json::json!({
                "source_commit_oids": source_commit_oids,
                "prepared_commit_oids": [],
            })),
            created_at: now,
            completed_at: None,
        };
        self.db.create_git_operation(&operation).await?;
        self.append_activity(
            project.id,
            ActivityEventType::GitOperationPlanned,
            "git_operation",
            operation.id.to_string(),
            serde_json::json!({ "operation_kind": operation.operation_kind, "entity_id": operation.entity_id }),
        )
        .await?;

        let integration_workspace_dir = PathBuf::from(&integration_workspace.path);
        let mut prepared_tip = input_target_commit_oid.clone();
        let mut prepared_commit_oids = Vec::with_capacity(source_commit_oids.len());

        for source_commit_oid in &source_commit_oids {
            if let Err(error) =
                cherry_pick_no_commit(&integration_workspace_dir, source_commit_oid).await
            {
                let _ = abort_cherry_pick(&integration_workspace_dir).await;
                self.fail_prepare_convergence_attempt(
                    project,
                    item,
                    revision,
                    queue_entry,
                    &mut integration_workspace,
                    &mut convergence,
                    &mut operation,
                    &source_commit_oids,
                    &prepared_commit_oids,
                    error.to_string(),
                    ConvergenceStatus::Conflicted,
                )
                .await?;
                return Ok(());
            }

            let has_replay_changes =
                match working_tree_has_changes(&integration_workspace_dir).await {
                    Ok(has_changes) => has_changes,
                    Err(error) => {
                        self.fail_prepare_convergence_attempt(
                            project,
                            item,
                            revision,
                            queue_entry,
                            &mut integration_workspace,
                            &mut convergence,
                            &mut operation,
                            &source_commit_oids,
                            &prepared_commit_oids,
                            error.to_string(),
                            ConvergenceStatus::Failed,
                        )
                        .await?;
                        return Ok(());
                    }
                };
            if !has_replay_changes {
                continue;
            }

            let original_message = match commit_message(repo_path, source_commit_oid).await {
                Ok(message) => message,
                Err(error) => {
                    self.fail_prepare_convergence_attempt(
                        project,
                        item,
                        revision,
                        queue_entry,
                        &mut integration_workspace,
                        &mut convergence,
                        &mut operation,
                        &source_commit_oids,
                        &prepared_commit_oids,
                        error.to_string(),
                        ConvergenceStatus::Failed,
                    )
                    .await?;
                    return Ok(());
                }
            };
            let next_prepared_tip = match ingot_git::commit::create_daemon_convergence_commit(
                &integration_workspace_dir,
                &original_message,
                &ConvergenceCommitTrailers {
                    operation_id: operation.id,
                    item_id: item.id,
                    revision_no: revision.revision_no,
                    convergence_id: convergence.id,
                    source_commit_oid: source_commit_oid.clone(),
                },
            )
            .await
            {
                Ok(prepared_tip) => prepared_tip,
                Err(error) => {
                    self.fail_prepare_convergence_attempt(
                        project,
                        item,
                        revision,
                        queue_entry,
                        &mut integration_workspace,
                        &mut convergence,
                        &mut operation,
                        &source_commit_oids,
                        &prepared_commit_oids,
                        error.to_string(),
                        ConvergenceStatus::Failed,
                    )
                    .await?;
                    return Ok(());
                }
            };
            if let Some(workspace_ref) = integration_workspace.workspace_ref.as_deref() {
                if let Err(error) = git(
                    repo_path,
                    &["update-ref", workspace_ref, &next_prepared_tip],
                )
                .await
                {
                    self.fail_prepare_convergence_attempt(
                        project,
                        item,
                        revision,
                        queue_entry,
                        &mut integration_workspace,
                        &mut convergence,
                        &mut operation,
                        &source_commit_oids,
                        &prepared_commit_oids,
                        error.to_string(),
                        ConvergenceStatus::Failed,
                    )
                    .await?;
                    return Ok(());
                }
            }
            prepared_tip = next_prepared_tip;
            prepared_commit_oids.push(prepared_tip.clone());
        }

        integration_workspace.head_commit_oid = Some(prepared_tip.clone());
        integration_workspace.status = WorkspaceStatus::Ready;
        integration_workspace.updated_at = Utc::now();
        self.db.update_workspace(&integration_workspace).await?;

        convergence.status = ConvergenceStatus::Prepared;
        convergence.prepared_commit_oid = Some(prepared_tip.clone());
        convergence.completed_at = Some(Utc::now());
        self.db.update_convergence(&convergence).await?;

        operation.new_oid = Some(prepared_tip.clone());
        operation.commit_oid = Some(prepared_tip.clone());
        operation.metadata = Some(serde_json::json!({
            "source_commit_oids": source_commit_oids,
            "prepared_commit_oids": prepared_commit_oids,
        }));
        self.mark_git_operation_reconciled(&mut operation).await?;

        let mut all_convergences = convergences.to_vec();
        all_convergences.push(convergence.clone());
        let validation_dispatch_jobs = if current_item.approval_state == ApprovalState::Granted
            && !convergences.iter().any(|existing| {
                existing.item_revision_id == revision.id
                    && existing.status == ConvergenceStatus::Prepared
            }) {
            jobs.iter()
                .filter(|job| {
                    !(job.item_revision_id == revision.id
                        && job.step_id == step::VALIDATE_INTEGRATED)
                })
                .cloned()
                .collect::<Vec<_>>()
        } else {
            jobs.to_vec()
        };
        let mut validation_job = dispatch_job(
            &current_item,
            revision,
            &validation_dispatch_jobs,
            findings,
            &all_convergences,
            DispatchJobCommand {
                step_id: Some("validate_integrated".into()),
            },
        )
        .map_err(|error| RuntimeError::InvalidState(error.to_string()))?;
        if current_item.approval_state == ApprovalState::Granted {
            let latest_validate_job = jobs
                .iter()
                .filter(|job| {
                    job.item_revision_id == revision.id && job.step_id == step::VALIDATE_INTEGRATED
                })
                .max_by_key(|job| {
                    (
                        (job.semantic_attempt_no, job.retry_no),
                        job.ended_at,
                        job.created_at,
                    )
                });
            if let Some(latest_validate_job) = latest_validate_job {
                validation_job.semantic_attempt_no = latest_validate_job.semantic_attempt_no + 1;
                validation_job.retry_no = 0;
                validation_job.supersedes_job_id = Some(latest_validate_job.id);
            }
        }
        validation_job.workspace_id = convergence.integration_workspace_id;
        self.db.create_job(&validation_job).await?;
        self.append_activity(
            project.id,
            ActivityEventType::ConvergencePrepared,
            "convergence",
            convergence.id.to_string(),
            serde_json::json!({ "item_id": item.id, "validation_job_id": validation_job.id }),
        )
        .await?;
        self.append_activity(
            project.id,
            ActivityEventType::JobDispatched,
            "job",
            validation_job.id.to_string(),
            serde_json::json!({ "item_id": item.id, "step_id": validation_job.step_id }),
        )
        .await?;

        Ok(())
    }

    async fn reconcile_checkout_sync_state(
        &self,
        project: &Project,
        item_id: ingot_domain::ids::ItemId,
        revision: &ItemRevision,
    ) -> Result<CheckoutSyncStatus, RuntimeError> {
        let mut item = self.db.get_item(item_id).await?;
        let status = checkout_sync_status(Path::new(&project.path), &revision.target_ref).await?;
        match &status {
            CheckoutSyncStatus::Ready => {
                if item.escalation_reason == Some(EscalationReason::CheckoutSyncBlocked) {
                    item.escalation_state = EscalationState::None;
                    item.escalation_reason = None;
                    item.updated_at = Utc::now();
                    self.db.update_item(&item).await?;
                    self.append_activity(
                        project.id,
                        ActivityEventType::CheckoutSyncCleared,
                        "item",
                        item.id.to_string(),
                        serde_json::json!({}),
                    )
                    .await?;
                    self.append_activity(
                        project.id,
                        ActivityEventType::ItemEscalationCleared,
                        "item",
                        item.id.to_string(),
                        serde_json::json!({ "reason": "checkout_sync_ready" }),
                    )
                    .await?;
                }
            }
            CheckoutSyncStatus::Blocked { message, .. } => {
                if item.escalation_reason != Some(EscalationReason::CheckoutSyncBlocked) {
                    item.escalation_state = EscalationState::OperatorRequired;
                    item.escalation_reason = Some(EscalationReason::CheckoutSyncBlocked);
                    item.updated_at = Utc::now();
                    self.db.update_item(&item).await?;
                    self.append_activity(
                        project.id,
                        ActivityEventType::CheckoutSyncBlocked,
                        "item",
                        item.id.to_string(),
                        serde_json::json!({ "message": message }),
                    )
                    .await?;
                    self.append_activity(
                        project.id,
                        ActivityEventType::ItemEscalated,
                        "item",
                        item.id.to_string(),
                        serde_json::json!({ "reason": EscalationReason::CheckoutSyncBlocked }),
                    )
                    .await?;
                }
            }
        }

        Ok(status)
    }

    async fn verify_mutating_workspace_protocol(
        &self,
        prepared: &PreparedRun,
    ) -> Result<(), RuntimeError> {
        let repo_path = prepared.canonical_repo_path.as_path();
        let workspace_ref =
            prepared.workspace.workspace_ref.as_deref().ok_or_else(|| {
                RuntimeError::InvalidState("authoring workspace missing ref".into())
            })?;
        let actual_ref = resolve_ref_oid(repo_path, workspace_ref).await?;
        let actual_head = head_oid(Path::new(&prepared.workspace.path)).await?;

        if actual_ref.as_deref() != Some(prepared.original_head_commit_oid.as_str())
            || actual_head != prepared.original_head_commit_oid
        {
            self.reset_workspace(prepared).await?;
            return Err(RuntimeError::InvalidState(
                "agent created commits or moved refs in the authoring workspace".into(),
            ));
        }

        Ok(())
    }

    async fn verify_read_only_workspace_protocol(
        &self,
        prepared: &PreparedRun,
    ) -> Result<(), RuntimeError> {
        let actual_head = head_oid(Path::new(&prepared.workspace.path)).await?;
        if actual_head != prepared.original_head_commit_oid {
            return Err(RuntimeError::InvalidState(
                "read-only job moved HEAD away from the expected commit".into(),
            ));
        }

        if working_tree_has_changes(Path::new(&prepared.workspace.path)).await? {
            return Err(RuntimeError::InvalidState(
                "read-only job dirtied the workspace".into(),
            ));
        }

        Ok(())
    }

    async fn create_commit(
        &self,
        prepared: &PreparedRun,
        response: &AgentResponse,
    ) -> Result<String, RuntimeError> {
        let _guard = self
            .project_locks
            .acquire_project_mutation(prepared.job.project_id)
            .await;

        let repo_path = prepared.canonical_repo_path.as_path();
        let workspace_ref =
            prepared.workspace.workspace_ref.clone().ok_or_else(|| {
                RuntimeError::InvalidState("authoring workspace missing ref".into())
            })?;
        let now = Utc::now();
        let mut operation = GitOperation {
            id: GitOperationId::new(),
            project_id: prepared.project.id,
            operation_kind: OperationKind::CreateJobCommit,
            entity_type: GitEntityType::Job,
            entity_id: prepared.job.id.to_string(),
            workspace_id: Some(prepared.workspace.id),
            ref_name: Some(workspace_ref.clone()),
            expected_old_oid: Some(prepared.original_head_commit_oid.clone()),
            new_oid: None,
            commit_oid: None,
            status: GitOperationStatus::Planned,
            metadata: None,
            created_at: now,
            completed_at: None,
        };
        self.db.create_git_operation(&operation).await?;
        self.append_activity(
            prepared.project.id,
            ActivityEventType::GitOperationPlanned,
            "git_operation",
            operation.id.to_string(),
            serde_json::json!({ "operation_kind": operation.operation_kind, "entity_id": operation.entity_id }),
        )
        .await?;

        let summary = response
            .result
            .as_ref()
            .and_then(|value| value.get("summary"))
            .and_then(|value| value.as_str())
            .unwrap_or("Authoring changes generated by Ingot");
        let commit_oid = create_daemon_job_commit(
            Path::new(&prepared.workspace.path),
            &commit_subject(&prepared.revision.title, &prepared.job.step_id),
            summary,
            &JobCommitTrailers {
                operation_id: operation.id,
                item_id: prepared.item.id,
                revision_no: prepared.revision.revision_no,
                job_id: prepared.job.id,
            },
        )
        .await?;
        git(repo_path, &["update-ref", &workspace_ref, &commit_oid]).await?;

        operation.new_oid = Some(commit_oid.clone());
        operation.commit_oid = Some(commit_oid.clone());
        operation.status = GitOperationStatus::Applied;
        operation.completed_at = Some(Utc::now());
        self.db.update_git_operation(&operation).await?;

        Ok(commit_oid)
    }

    async fn complete_commit_run(
        &self,
        prepared: &PreparedRun,
        commit_oid: &str,
    ) -> Result<(), RuntimeError> {
        self.finalize_workspace_after_success(prepared, Some(commit_oid))
            .await?;

        self.db
            .apply_job_completion(JobCompletionMutation {
                job_id: prepared.job.id,
                item_id: prepared.item.id,
                expected_item_revision_id: prepared.job.item_revision_id,
                outcome_class: OutcomeClass::Clean,
                clear_item_escalation: should_clear_item_escalation_on_success(
                    &prepared.item,
                    &prepared.job,
                ),
                result_schema_version: None,
                result_payload: None,
                output_commit_oid: Some(commit_oid.to_string()),
                findings: vec![],
                prepared_convergence_guard: None,
            })
            .await?;
        self.append_activity(
            prepared.project.id,
            ActivityEventType::JobCompleted,
            "job",
            prepared.job.id.to_string(),
            serde_json::json!({ "item_id": prepared.item.id, "outcome": "clean" }),
        )
        .await?;

        self.refresh_revision_context(prepared).await?;
        self.append_escalation_cleared_activity_if_needed(prepared)
            .await?;
        self.auto_dispatch_projected_review(prepared.project.id, prepared.item.id)
            .await?;

        info!(job_id = %prepared.job.id, commit_oid, "completed authoring job");

        Ok(())
    }

    async fn auto_dispatch_projected_review_jobs(&self) -> Result<bool, RuntimeError> {
        let mut dispatched_any = false;

        for project in self.db.list_projects().await? {
            let _guard = self
                .project_locks
                .acquire_project_mutation(project.id)
                .await;
            let items = self.db.list_items_by_project(project.id).await?;
            for item in items {
                if item.lifecycle_state != LifecycleState::Open {
                    continue;
                }
                dispatched_any |= self
                    .auto_dispatch_projected_review_locked(&project, item.id)
                    .await?;
            }
        }

        Ok(dispatched_any)
    }

    async fn auto_dispatch_projected_review(
        &self,
        project_id: ingot_domain::ids::ProjectId,
        item_id: ingot_domain::ids::ItemId,
    ) -> Result<bool, RuntimeError> {
        let _guard = self
            .project_locks
            .acquire_project_mutation(project_id)
            .await;

        let project = self.db.get_project(project_id).await?;
        self.auto_dispatch_projected_review_locked(&project, item_id)
            .await
    }

    async fn auto_dispatch_projected_review_locked(
        &self,
        project: &Project,
        item_id: ingot_domain::ids::ItemId,
    ) -> Result<bool, RuntimeError> {
        let item = self.db.get_item(item_id).await?;
        let revision = self.db.get_revision(item.current_revision_id).await?;
        let jobs = self.db.list_jobs_by_item(item.id).await?;
        let findings = self.db.list_findings_by_item(item.id).await?;
        let convergences = self
            .hydrate_convergences(project, self.db.list_convergences_by_item(item.id).await?)
            .await?;
        let evaluation =
            Evaluator::new().evaluate(&item, &revision, &jobs, &findings, &convergences);
        let Some(step_id) = evaluation.dispatchable_step_id.as_deref() else {
            return Ok(false);
        };

        if !step::is_closure_relevant_review_step(step_id) {
            return Ok(false);
        }

        let job = dispatch_job(
            &item,
            &revision,
            &jobs,
            &findings,
            &convergences,
            DispatchJobCommand {
                step_id: Some(step_id.to_string()),
            },
        )
        .map_err(|error| {
            RuntimeError::InvalidState(format!("failed to auto-dispatch review {step_id}: {error}"))
        })?;
        self.db.create_job(&job).await?;
        self.append_activity(
            project.id,
            ActivityEventType::JobDispatched,
            "job",
            job.id.to_string(),
            serde_json::json!({ "item_id": item.id, "step_id": job.step_id }),
        )
        .await?;
        info!(job_id = %job.id, step_id = %job.step_id, item_id = %item.id, "auto-dispatched review");

        Ok(true)
    }

    async fn fail_run(
        &self,
        prepared: &PreparedRun,
        outcome_class: OutcomeClass,
        error_code: &'static str,
        error_message: Option<String>,
    ) -> Result<(), RuntimeError> {
        self.finalize_workspace_after_failure(prepared).await?;

        let status = match outcome_class {
            OutcomeClass::Cancelled => JobStatus::Cancelled,
            OutcomeClass::TransientFailure
            | OutcomeClass::TerminalFailure
            | OutcomeClass::ProtocolViolation => JobStatus::Failed,
            OutcomeClass::Clean | OutcomeClass::Findings => JobStatus::Failed,
        };
        let escalation_reason = failure_escalation_reason(&prepared.job, outcome_class);

        self.db
            .finish_job_non_success(FinishJobNonSuccessParams {
                job_id: prepared.job.id,
                item_id: prepared.item.id,
                expected_item_revision_id: prepared.job.item_revision_id,
                status,
                outcome_class: Some(outcome_class),
                error_code: Some(error_code),
                error_message: error_message.as_deref(),
                escalation_reason,
            })
            .await?;
        let event_type = if outcome_class == OutcomeClass::Cancelled {
            ActivityEventType::JobCancelled
        } else {
            ActivityEventType::JobFailed
        };
        self.append_activity(
            prepared.project.id,
            event_type,
            "job",
            prepared.job.id.to_string(),
            serde_json::json!({ "item_id": prepared.item.id, "error_code": error_code }),
        )
        .await?;
        if let Some(escalation_reason) = escalation_reason {
            self.append_activity(
                prepared.project.id,
                ActivityEventType::ItemEscalated,
                "item",
                prepared.item.id.to_string(),
                serde_json::json!({ "reason": escalation_reason }),
            )
            .await?;
        }

        self.refresh_revision_context(prepared).await?;
        warn!(
            job_id = %prepared.job.id,
            outcome_class = ?outcome_class,
            error_code,
            error_message = error_message.as_deref().unwrap_or(""),
            "job failed"
        );

        Ok(())
    }

    async fn append_escalation_cleared_activity_if_needed(
        &self,
        prepared: &PreparedRun,
    ) -> Result<(), RuntimeError> {
        if prepared.item.escalation_state != EscalationState::OperatorRequired {
            return Ok(());
        }

        let item = self.db.get_item(prepared.item.id).await?;
        if item.current_revision_id != prepared.job.item_revision_id
            || item.escalation_state != EscalationState::None
            || item.escalation_reason.is_some()
        {
            return Ok(());
        }

        self.append_activity(
            prepared.project.id,
            ActivityEventType::ItemEscalationCleared,
            "item",
            prepared.item.id.to_string(),
            serde_json::json!({ "reason": "successful_retry", "job_id": prepared.job.id }),
        )
        .await?;

        Ok(())
    }

    async fn finalize_workspace_after_success(
        &self,
        prepared: &PreparedRun,
        head_commit_oid: Option<&str>,
    ) -> Result<(), RuntimeError> {
        let _guard = self
            .project_locks
            .acquire_project_mutation(prepared.job.project_id)
            .await;

        match prepared.workspace_lifecycle {
            WorkspaceLifecycle::PersistentAuthoring => {
                let mut workspace = self.db.get_workspace(prepared.workspace.id).await?;
                workspace.status = WorkspaceStatus::Ready;
                workspace.current_job_id = None;
                if let Some(head_commit_oid) = head_commit_oid {
                    workspace.head_commit_oid = Some(head_commit_oid.to_string());
                }
                workspace.updated_at = Utc::now();
                self.db.update_workspace(&workspace).await?;
            }
            WorkspaceLifecycle::PersistentIntegration => {
                let mut workspace = self.db.get_workspace(prepared.workspace.id).await?;
                workspace.status = WorkspaceStatus::Ready;
                workspace.current_job_id = None;
                workspace.updated_at = Utc::now();
                self.db.update_workspace(&workspace).await?;
            }
            WorkspaceLifecycle::EphemeralReview => {
                remove_workspace(
                    prepared.canonical_repo_path.as_path(),
                    Path::new(&prepared.workspace.path),
                )
                .await?;
                let mut workspace = self.db.get_workspace(prepared.workspace.id).await?;
                workspace.status = WorkspaceStatus::Abandoned;
                workspace.current_job_id = None;
                workspace.updated_at = Utc::now();
                self.db.update_workspace(&workspace).await?;
            }
        }

        Ok(())
    }

    async fn finalize_integration_workspace_after_close(
        &self,
        project: &Project,
        workspace: &Workspace,
    ) -> Result<(), RuntimeError> {
        let repo_path = self.project_paths(project).mirror_git_dir;
        remove_workspace(repo_path.as_path(), Path::new(&workspace.path)).await?;
        let mut workspace = workspace.clone();
        workspace.status = WorkspaceStatus::Abandoned;
        workspace.current_job_id = None;
        workspace.updated_at = Utc::now();
        self.db.update_workspace(&workspace).await?;
        Ok(())
    }

    async fn finalize_workspace_after_failure(
        &self,
        prepared: &PreparedRun,
    ) -> Result<(), RuntimeError> {
        let _guard = self
            .project_locks
            .acquire_project_mutation(prepared.job.project_id)
            .await;

        self.reset_workspace(prepared).await?;

        match prepared.workspace_lifecycle {
            WorkspaceLifecycle::PersistentAuthoring => {
                let mut workspace = self.db.get_workspace(prepared.workspace.id).await?;
                workspace.status = WorkspaceStatus::Ready;
                workspace.current_job_id = None;
                workspace.head_commit_oid = Some(prepared.original_head_commit_oid.clone());
                workspace.updated_at = Utc::now();
                self.db.update_workspace(&workspace).await?;
            }
            WorkspaceLifecycle::PersistentIntegration => {
                let mut workspace = self.db.get_workspace(prepared.workspace.id).await?;
                workspace.status = WorkspaceStatus::Ready;
                workspace.current_job_id = None;
                workspace.head_commit_oid = Some(prepared.original_head_commit_oid.clone());
                workspace.updated_at = Utc::now();
                self.db.update_workspace(&workspace).await?;
            }
            WorkspaceLifecycle::EphemeralReview => {
                let mut workspace = self.db.get_workspace(prepared.workspace.id).await?;
                workspace.status = WorkspaceStatus::Abandoned;
                workspace.current_job_id = None;
                workspace.updated_at = Utc::now();
                self.db.update_workspace(&workspace).await?;
            }
        }

        Ok(())
    }

    async fn reset_workspace(&self, prepared: &PreparedRun) -> Result<(), RuntimeError> {
        match prepared.workspace_lifecycle {
            WorkspaceLifecycle::PersistentAuthoring => {
                let workspace_path = Path::new(&prepared.workspace.path);
                git(
                    workspace_path,
                    &["reset", "--hard", &prepared.original_head_commit_oid],
                )
                .await?;
                git(workspace_path, &["clean", "-fd"]).await?;
                if let Some(workspace_ref) = prepared.workspace.workspace_ref.as_deref() {
                    git(
                        prepared.canonical_repo_path.as_path(),
                        &[
                            "update-ref",
                            workspace_ref,
                            &prepared.original_head_commit_oid,
                        ],
                    )
                    .await?;
                }
            }
            WorkspaceLifecycle::PersistentIntegration => {
                let workspace_path = Path::new(&prepared.workspace.path);
                git(
                    workspace_path,
                    &["reset", "--hard", &prepared.original_head_commit_oid],
                )
                .await?;
                git(workspace_path, &["clean", "-fd"]).await?;
                if let Some(workspace_ref) = prepared.workspace.workspace_ref.as_deref() {
                    git(
                        prepared.canonical_repo_path.as_path(),
                        &[
                            "update-ref",
                            workspace_ref,
                            &prepared.original_head_commit_oid,
                        ],
                    )
                    .await?;
                }
            }
            WorkspaceLifecycle::EphemeralReview => {
                remove_workspace(
                    prepared.canonical_repo_path.as_path(),
                    Path::new(&prepared.workspace.path),
                )
                .await?;
            }
        }
        Ok(())
    }

    async fn refresh_revision_context(&self, prepared: &PreparedRun) -> Result<(), RuntimeError> {
        self.refresh_revision_context_for_ids(
            prepared.project.id,
            prepared.item.id,
            prepared.revision.id,
            Some(prepared.job.id),
        )
        .await
    }

    async fn refresh_revision_context_for_ids(
        &self,
        project_id: ingot_domain::ids::ProjectId,
        item_id: ingot_domain::ids::ItemId,
        revision_id: ingot_domain::ids::ItemRevisionId,
        updated_from_job_id: Option<ingot_domain::ids::JobId>,
    ) -> Result<(), RuntimeError> {
        let project = self.db.get_project(project_id).await?;
        let item = self.db.get_item(item_id).await?;
        let revision = self.db.get_revision(revision_id).await?;
        let jobs = self.db.list_jobs_by_item(item.id).await?;
        let authoring_head_commit_oid = current_authoring_head_for_revision(&jobs, &revision);
        let changed_paths = changed_paths_between(
            self.project_paths(&project).mirror_git_dir.as_path(),
            &revision.seed_commit_oid,
            &authoring_head_commit_oid,
        )
        .await?;
        let context = rebuild_revision_context(
            &item,
            &revision,
            &jobs,
            changed_paths,
            updated_from_job_id,
            Utc::now(),
        );
        self.db.upsert_revision_context(&context).await?;
        Ok(())
    }

    fn complete_job_service(
        &self,
    ) -> CompleteJobService<Database, GitJobCompletionPort, ProjectLocks> {
        CompleteJobService::new(
            self.db.clone(),
            GitJobCompletionPort,
            self.project_locks.clone(),
        )
    }

    async fn append_activity(
        &self,
        project_id: ingot_domain::ids::ProjectId,
        event_type: ActivityEventType,
        entity_type: &str,
        entity_id: String,
        payload: serde_json::Value,
    ) -> Result<(), RuntimeError> {
        self.db
            .append_activity(&Activity {
                id: ingot_domain::ids::ActivityId::new(),
                project_id,
                event_type,
                entity_type: entity_type.into(),
                entity_id,
                payload,
                created_at: Utc::now(),
            })
            .await?;
        Ok(())
    }

    async fn write_prompt_artifact(&self, job: &Job, prompt: &str) -> Result<(), RuntimeError> {
        let dir = self.artifact_dir(job.id);
        tokio::fs::create_dir_all(&dir).await?;
        tokio::fs::write(dir.join("prompt.txt"), prompt).await?;
        Ok(())
    }

    async fn write_response_artifacts(
        &self,
        job: &Job,
        response: &AgentResponse,
    ) -> Result<(), RuntimeError> {
        let dir = self.artifact_dir(job.id);
        tokio::fs::create_dir_all(&dir).await?;
        tokio::fs::write(dir.join("stdout.log"), &response.stdout).await?;
        tokio::fs::write(dir.join("stderr.log"), &response.stderr).await?;
        let result_json = response.result.clone().unwrap_or(serde_json::Value::Null);
        tokio::fs::write(
            dir.join("result.json"),
            serde_json::to_vec_pretty(&result_json)?,
        )
        .await?;
        Ok(())
    }

    fn artifact_dir(&self, job_id: ingot_domain::ids::JobId) -> PathBuf {
        self.config.state_root.join("logs").join(job_id.to_string())
    }
}

fn is_supported_runtime_job(job: &Job) -> bool {
    matches!(
        (
            job.workspace_kind,
            job.execution_permission,
            job.output_artifact_kind,
        ),
        (
            WorkspaceKind::Authoring,
            ExecutionPermission::MayMutate,
            OutputArtifactKind::Commit
        ) | (
            WorkspaceKind::Authoring | WorkspaceKind::Review | WorkspaceKind::Integration,
            ExecutionPermission::MustNotMutate,
            OutputArtifactKind::ReviewReport
                | OutputArtifactKind::ValidationReport
                | OutputArtifactKind::FindingReport,
        )
    )
}

fn supports_job(agent: &Agent, job: &Job) -> bool {
    if !agent
        .capabilities
        .contains(&AgentCapability::StructuredOutput)
    {
        return false;
    }

    match job.execution_permission {
        ExecutionPermission::MayMutate => {
            agent.capabilities.contains(&AgentCapability::MutatingJobs)
        }
        ExecutionPermission::MustNotMutate => {
            agent.capabilities.contains(&AgentCapability::ReadOnlyJobs)
        }
        ExecutionPermission::DaemonOnly => false,
    }
}

fn built_in_template(template_slug: &str, step_id: &str) -> &'static str {
    match template_slug {
        "author-initial" => {
            "Implement the requested change directly in the repository. Keep the edit set focused on the acceptance criteria and preserve surrounding style."
        }
        "repair-candidate" | "repair-integrated" => {
            "Repair the current candidate based on the latest validation or review feedback while preserving the accepted parts of the prior work."
        }
        "review-incremental" => {
            "Review only the requested incremental diff and report concrete findings against the exact review subject."
        }
        "review-candidate" => {
            "Review the full candidate diff from the seed commit to the current head and report concrete findings when necessary."
        }
        "validate-candidate" | "validate-integrated" => {
            "Run objective validation against the current workspace subject and report failed checks or findings only when they are real."
        }
        "investigate-item" => {
            "Investigate the current subject and produce a finding report only when there is a concrete issue worth tracking."
        }
        _ => match step_id {
            "author_initial" => {
                "Implement the requested change directly in the repository. Keep the edit set focused on the acceptance criteria and preserve surrounding style."
            }
            "review_incremental_initial"
            | "review_incremental_repair"
            | "review_incremental_after_integration_repair" => {
                "Review only the requested incremental diff and report concrete findings against the exact review subject."
            }
            "review_candidate_initial"
            | "review_candidate_repair"
            | "review_after_integration_repair" => {
                "Review the full candidate diff from the seed commit to the current head and report concrete findings when necessary."
            }
            "validate_candidate_initial"
            | "validate_candidate_repair"
            | "validate_after_integration_repair"
            | "validate_integrated" => {
                "Run objective validation against the current workspace subject and report failed checks or findings only when they are real."
            }
            "investigate_item" => {
                "Investigate the current subject and produce a finding report only when there is a concrete issue worth tracking."
            }
            _ => {
                "Update the repository for the current authoring step and keep the change set narrowly scoped to the revision contract."
            }
        },
    }
}

fn report_prompt_suffix(job: &Job) -> &'static str {
    match job.output_artifact_kind {
        OutputArtifactKind::ValidationReport => {
            "Return JSON matching `validation_report:v1` with keys `outcome`, `summary`, `checks`, `findings`, and `extensions`. Set `extensions` to null when unused. Use `outcome=clean` only when there are no failed checks and no findings."
        }
        OutputArtifactKind::ReviewReport => {
            "Return JSON matching `review_report:v1` with keys `outcome`, `summary`, `review_subject`, `overall_risk`, `findings`, and `extensions`. Set `extensions` to null when unused. The `review_subject.base_commit_oid` and `review_subject.head_commit_oid` must exactly match the provided input commits."
        }
        OutputArtifactKind::FindingReport => {
            "Return JSON matching `finding_report:v1` with keys `outcome`, `summary`, `findings`, and `extensions`. Set `extensions` to null when unused."
        }
        _ => "",
    }
}

fn output_schema_for_job(job: &Job) -> Option<serde_json::Value> {
    match job.output_artifact_kind {
        OutputArtifactKind::Commit => Some(commit_summary_schema()),
        OutputArtifactKind::ValidationReport => Some(validation_report_schema()),
        OutputArtifactKind::ReviewReport => Some(review_report_schema()),
        OutputArtifactKind::FindingReport => Some(finding_report_schema()),
        OutputArtifactKind::None => None,
    }
}

fn result_schema_version(output_artifact_kind: OutputArtifactKind) -> Option<&'static str> {
    match output_artifact_kind {
        OutputArtifactKind::ValidationReport => Some("validation_report:v1"),
        OutputArtifactKind::ReviewReport => Some("review_report:v1"),
        OutputArtifactKind::FindingReport => Some("finding_report:v1"),
        _ => None,
    }
}

fn report_outcome_class(result_payload: &serde_json::Value) -> Result<OutcomeClass, String> {
    match result_payload
        .get("outcome")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
    {
        "clean" => Ok(OutcomeClass::Clean),
        "findings" => Ok(OutcomeClass::Findings),
        other => Err(format!("unsupported report outcome `{other}`")),
    }
}

fn commit_summary_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "summary": { "type": "string" },
            "validation": {
                "type": ["string", "null"]
            }
        },
        "required": ["summary", "validation"],
        "additionalProperties": false
    })
}

fn finding_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "finding_key": { "type": "string" },
            "code": { "type": "string" },
            "severity": { "type": "string", "enum": ["low", "medium", "high", "critical"] },
            "summary": { "type": "string" },
            "paths": {
                "type": "array",
                "items": { "type": "string" }
            },
            "evidence": {
                "type": "array",
                "items": { "type": "string" }
            }
        },
        "required": ["finding_key", "code", "severity", "summary", "paths", "evidence"],
        "additionalProperties": false
    })
}

fn nullable_closed_extensions_schema() -> serde_json::Value {
    serde_json::json!({
        "anyOf": [
            {
                "type": "object",
                "additionalProperties": false
            },
            {
                "type": "null"
            }
        ]
    })
}

fn validation_report_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "outcome": { "type": "string", "enum": ["clean", "findings"] },
            "summary": { "type": "string" },
            "checks": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "status": { "type": "string", "enum": ["pass", "fail", "skip"] },
                        "summary": { "type": "string" }
                    },
                    "required": ["name", "status", "summary"],
                    "additionalProperties": false
                }
            },
            "findings": {
                "type": "array",
                "items": finding_schema()
            },
            "extensions": nullable_closed_extensions_schema()
        },
        "required": ["outcome", "summary", "checks", "findings", "extensions"],
        "additionalProperties": false
    })
}

fn review_report_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "outcome": { "type": "string", "enum": ["clean", "findings"] },
            "summary": { "type": "string" },
            "review_subject": {
                "type": "object",
                "properties": {
                    "base_commit_oid": { "type": "string" },
                    "head_commit_oid": { "type": "string" }
                },
                "required": ["base_commit_oid", "head_commit_oid"],
                "additionalProperties": false
            },
            "overall_risk": { "type": "string", "enum": ["low", "medium", "high"] },
            "findings": {
                "type": "array",
                "items": finding_schema()
            },
            "extensions": nullable_closed_extensions_schema()
        },
        "required": ["outcome", "summary", "review_subject", "overall_risk", "findings", "extensions"],
        "additionalProperties": false
    })
}

fn finding_report_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "outcome": { "type": "string", "enum": ["clean", "findings"] },
            "summary": { "type": "string" },
            "findings": {
                "type": "array",
                "items": finding_schema()
            },
            "extensions": nullable_closed_extensions_schema()
        },
        "required": ["outcome", "summary", "findings", "extensions"],
        "additionalProperties": false
    })
}

fn format_revision_context(revision_context: Option<&RevisionContext>) -> String {
    revision_context
        .map(|context| {
            serde_json::to_string_pretty(&context.payload).unwrap_or_else(|_| "{}".into())
        })
        .unwrap_or_else(|| "none".into())
}

fn commit_subject(title: &str, step_id: &str) -> String {
    let title = title.trim();
    if title.is_empty() {
        format!("Ingot {step_id}")
    } else {
        format!("Ingot: {title}")
    }
}

fn non_empty_message(message: &str) -> Option<String> {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn current_authoring_head_for_revision(jobs: &[Job], revision: &ItemRevision) -> String {
    jobs.iter()
        .filter(|job| job.item_revision_id == revision.id)
        .filter(|job| job.status == JobStatus::Completed)
        .filter(|job| job.output_artifact_kind == OutputArtifactKind::Commit)
        .filter_map(|job| {
            job.output_commit_oid
                .as_ref()
                .map(|commit_oid| ((job.ended_at, job.created_at), commit_oid.clone()))
        })
        .max_by_key(|(sort_key, _)| *sort_key)
        .map(|(_, commit_oid)| commit_oid)
        .unwrap_or_else(|| revision.seed_commit_oid.clone())
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

fn template_digest(template: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(template.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn failure_escalation_reason(job: &Job, outcome_class: OutcomeClass) -> Option<EscalationReason> {
    if !is_closure_relevant_job(job) {
        return None;
    }

    match outcome_class {
        OutcomeClass::TerminalFailure => Some(EscalationReason::StepFailed),
        OutcomeClass::ProtocolViolation => Some(EscalationReason::ProtocolViolation),
        OutcomeClass::Clean
        | OutcomeClass::Findings
        | OutcomeClass::TransientFailure
        | OutcomeClass::Cancelled => None,
    }
}

fn should_clear_item_escalation_on_success(item: &ingot_domain::item::Item, job: &Job) -> bool {
    item.escalation_state == EscalationState::OperatorRequired
        && job.retry_no > 0
        && is_closure_relevant_job(job)
}

fn is_closure_relevant_job(job: &Job) -> bool {
    matches!(
        step::find_step(&job.step_id).map(|step| step.closure_relevance),
        Some(ClosureRelevance::ClosureRelevant)
    )
}

#[cfg(test)]
mod tests {
    #![allow(unused_variables)]

    use super::*;
    use chrono::Utc;
    use ingot_domain::agent::{AdapterKind, Agent, AgentCapability, AgentStatus};
    use ingot_domain::convergence::ConvergenceStrategy;
    use ingot_domain::finding::{Finding, FindingSeverity, FindingSubjectKind};
    use ingot_domain::item::{
        ApprovalState, Classification, EscalationState, LifecycleState, OriginKind, ParkingState,
        Priority,
    };
    use ingot_domain::job::{ContextPolicy, PhaseKind};
    use ingot_domain::revision::{ApprovalPolicy, ItemRevision};
    use ingot_usecases::job::{DispatchJobCommand, dispatch_job};
    use ingot_workflow::Evaluator;
    use uuid::Uuid;

    struct FakeRunner;

    impl AgentRunner for FakeRunner {
        fn launch<'a>(
            &'a self,
            _agent: &'a Agent,
            _request: &'a AgentRequest,
            working_dir: &'a Path,
        ) -> Pin<Box<dyn Future<Output = Result<AgentResponse, AgentError>> + Send + 'a>> {
            Box::pin(async move {
                tokio::fs::write(working_dir.join("generated.txt"), "hello")
                    .await
                    .unwrap();
                Ok(AgentResponse {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                    result: Some(serde_json::json!({ "message": "implemented change" })),
                })
            })
        }
    }

    async fn ensure_test_mirror(
        state_root: &Path,
        project: &Project,
    ) -> ingot_git::project_repo::ProjectRepoPaths {
        let paths = ingot_git::project_repo::project_repo_paths(
            state_root,
            project.id,
            Path::new(&project.path),
        );
        ensure_mirror(&paths).await.expect("ensure mirror");
        paths
    }

    async fn create_mirror_only_commit(
        mirror_git_dir: &Path,
        base_commit: &str,
        workspace_ref: &str,
        message: &str,
    ) -> (PathBuf, String) {
        let worktree_path =
            std::env::temp_dir().join(format!("ingot-runtime-mirror-only-{}", Uuid::now_v7()));
        git_sync(
            mirror_git_dir,
            &[
                "worktree",
                "add",
                "--detach",
                worktree_path.to_str().expect("worktree path"),
                base_commit,
            ],
        );
        git_sync(&worktree_path, &["config", "user.name", "Ingot Test"]);
        git_sync(
            &worktree_path,
            &["config", "user.email", "ingot@example.com"],
        );
        std::fs::write(worktree_path.join("tracked.txt"), message).expect("write tracked file");
        git_sync(&worktree_path, &["add", "tracked.txt"]);
        git_sync(&worktree_path, &["commit", "-m", message]);
        let commit_oid = head_oid(&worktree_path).await.expect("mirror-only head");
        git_sync(mirror_git_dir, &["update-ref", workspace_ref, &commit_oid]);
        (worktree_path, commit_oid)
    }

    #[test]
    fn commit_and_report_schemas_require_every_declared_property() {
        assert_schema_requires_all_properties(&commit_summary_schema());
        assert_schema_requires_all_properties(&validation_report_schema());
        assert_schema_requires_all_properties(&review_report_schema());
        assert_schema_requires_all_properties(&finding_report_schema());
    }

    #[test]
    fn nullable_fields_remain_present_in_required_schema_contracts() {
        let commit_schema = commit_summary_schema();
        assert_eq!(
            schema_property_type(&commit_schema, "validation"),
            Some(serde_json::json!(["string", "null"]))
        );

        let validation_schema = validation_report_schema();
        assert_eq!(
            schema_property(&validation_schema, "extensions"),
            Some(nullable_closed_extensions_schema())
        );
    }

    fn assert_schema_requires_all_properties(schema: &serde_json::Value) {
        let properties = schema["properties"]
            .as_object()
            .expect("schema properties object");
        let required = schema["required"]
            .as_array()
            .expect("schema required array")
            .iter()
            .filter_map(|value| value.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        let property_names = properties
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();

        assert_eq!(required, property_names);
    }

    fn schema_property_type(
        schema: &serde_json::Value,
        property: &str,
    ) -> Option<serde_json::Value> {
        schema_property(schema, property).and_then(|value| value.get("type").cloned())
    }

    fn schema_property(schema: &serde_json::Value, property: &str) -> Option<serde_json::Value> {
        schema
            .get("properties")
            .and_then(|value| value.get(property))
            .cloned()
    }

    #[tokio::test]
    async fn tick_executes_a_queued_authoring_job_and_creates_a_commit() {
        let repo = temp_git_repo();
        let db_path = std::env::temp_dir().join(format!("ingot-runtime-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let state_root =
            std::env::temp_dir().join(format!("ingot-runtime-state-{}", Uuid::now_v7()));
        let dispatcher = JobDispatcher::with_runner(
            db.clone(),
            ProjectLocks::default(),
            DispatcherConfig::new(state_root.clone()),
            Arc::new(FakeRunner),
        );

        let created_at = Utc::now();
        let project = Project {
            id: ingot_domain::ids::ProjectId::new(),
            name: "repo".into(),
            path: repo.display().to_string(),
            default_branch: "main".into(),
            color: "#000".into(),
            created_at,
            updated_at: created_at,
        };
        db.create_project(&project).await.expect("create project");

        let agent = Agent {
            id: ingot_domain::ids::AgentId::new(),
            slug: "codex".into(),
            name: "Codex".into(),
            adapter_kind: AdapterKind::Codex,
            provider: "openai".into(),
            model: "gpt-5-codex".into(),
            cli_path: "codex".into(),
            capabilities: vec![
                AgentCapability::MutatingJobs,
                AgentCapability::StructuredOutput,
            ],
            health_check: Some("ok".into()),
            status: AgentStatus::Available,
        };
        db.create_agent(&agent).await.expect("create agent");

        let item_id = ingot_domain::ids::ItemId::new();
        let revision_id = ingot_domain::ids::ItemRevisionId::new();
        let seed_commit = head_oid(&repo).await.expect("seed head");
        let item = ingot_domain::item::Item {
            id: item_id,
            project_id: project.id,
            classification: Classification::Change,
            workflow_version: "delivery:v1".into(),
            lifecycle_state: LifecycleState::Open,
            parking_state: ParkingState::Active,
            done_reason: None,
            resolution_source: None,
            approval_state: ApprovalState::NotRequested,
            escalation_state: EscalationState::None,
            escalation_reason: None,
            current_revision_id: revision_id,
            origin_kind: OriginKind::Manual,
            origin_finding_id: None,
            priority: Priority::Major,
            labels: vec![],
            operator_notes: None,
            created_at,
            updated_at: created_at,
            closed_at: None,
        };
        let seed_commit = head_oid(&repo).await.expect("seed head");
        let revision = ItemRevision {
            id: revision_id,
            item_id,
            revision_no: 1,
            title: "Add generated file".into(),
            description: "Create one file".into(),
            acceptance_criteria: "generated file exists".into(),
            target_ref: "refs/heads/main".into(),
            approval_policy: ApprovalPolicy::Required,
            policy_snapshot: serde_json::json!({}),
            template_map_snapshot: serde_json::json!({ "author_initial": "author-initial" }),
            seed_commit_oid: head_oid(&repo).await.expect("seed head"),
            seed_target_commit_oid: Some(head_oid(&repo).await.expect("seed head")),
            supersedes_revision_id: None,
            created_at,
        };
        db.create_item_with_revision(&item, &revision)
            .await
            .expect("create item");

        let job = Job {
            id: ingot_domain::ids::JobId::new(),
            project_id: project.id,
            item_id,
            item_revision_id: revision_id,
            step_id: "author_initial".into(),
            semantic_attempt_no: 1,
            retry_no: 0,
            supersedes_job_id: None,
            status: JobStatus::Queued,
            outcome_class: None,
            phase_kind: PhaseKind::Author,
            workspace_id: None,
            workspace_kind: WorkspaceKind::Authoring,
            execution_permission: ExecutionPermission::MayMutate,
            context_policy: ContextPolicy::Fresh,
            phase_template_slug: "author-initial".into(),
            phase_template_digest: None,
            prompt_snapshot: None,
            input_base_commit_oid: None,
            input_head_commit_oid: Some(head_oid(&repo).await.expect("seed head")),
            output_artifact_kind: OutputArtifactKind::Commit,
            output_commit_oid: None,
            result_schema_version: None,
            result_payload: None,
            agent_id: None,
            process_pid: None,
            lease_owner_id: None,
            heartbeat_at: None,
            lease_expires_at: None,
            error_code: None,
            error_message: None,
            created_at,
            started_at: None,
            ended_at: None,
        };
        db.create_job(&job).await.expect("create job");

        assert!(dispatcher.tick().await.expect("tick should run"));

        let updated_job = db.get_job(job.id).await.expect("updated job");
        assert_eq!(updated_job.status, JobStatus::Completed);
        assert_eq!(updated_job.outcome_class, Some(OutcomeClass::Clean));
        assert!(updated_job.output_commit_oid.is_some());

        let workspace = db
            .find_authoring_workspace_for_revision(revision.id)
            .await
            .expect("workspace query")
            .expect("workspace exists");
        assert_eq!(workspace.status, WorkspaceStatus::Ready);
        assert_eq!(workspace.current_job_id, None);
        assert_eq!(workspace.head_commit_oid, updated_job.output_commit_oid);

        let prompt_path = state_root
            .join("logs")
            .join(job.id.to_string())
            .join("prompt.txt");
        assert!(prompt_path.exists(), "prompt artifact should exist");
    }

    #[tokio::test]
    async fn tick_executes_a_review_job_and_persists_structured_report() {
        let repo = temp_git_repo();
        let db_path =
            std::env::temp_dir().join(format!("ingot-runtime-review-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let state_root =
            std::env::temp_dir().join(format!("ingot-runtime-review-state-{}", Uuid::now_v7()));

        let created_at = Utc::now();
        let project = Project {
            id: ingot_domain::ids::ProjectId::new(),
            name: "repo".into(),
            path: repo.display().to_string(),
            default_branch: "main".into(),
            color: "#000".into(),
            created_at,
            updated_at: created_at,
        };
        db.create_project(&project).await.expect("create project");

        let agent = Agent {
            id: ingot_domain::ids::AgentId::new(),
            slug: "codex-review".into(),
            name: "Codex".into(),
            adapter_kind: AdapterKind::Codex,
            provider: "openai".into(),
            model: "gpt-5-codex".into(),
            cli_path: "codex".into(),
            capabilities: vec![
                AgentCapability::ReadOnlyJobs,
                AgentCapability::StructuredOutput,
            ],
            health_check: Some("ok".into()),
            status: AgentStatus::Available,
        };
        db.create_agent(&agent).await.expect("create agent");

        let item_id = ingot_domain::ids::ItemId::new();
        let revision_id = ingot_domain::ids::ItemRevisionId::new();
        let seed_commit = head_oid(&repo).await.expect("seed head");
        let item = ingot_domain::item::Item {
            id: item_id,
            project_id: project.id,
            classification: Classification::Change,
            workflow_version: "delivery:v1".into(),
            lifecycle_state: LifecycleState::Open,
            parking_state: ParkingState::Active,
            done_reason: None,
            resolution_source: None,
            approval_state: ApprovalState::NotRequested,
            escalation_state: EscalationState::None,
            escalation_reason: None,
            current_revision_id: revision_id,
            origin_kind: OriginKind::Manual,
            origin_finding_id: None,
            priority: Priority::Major,
            labels: vec![],
            operator_notes: None,
            created_at,
            updated_at: created_at,
            closed_at: None,
        };
        let base_commit = head_oid(&repo).await.expect("base head");
        std::fs::write(repo.join("tracked.txt"), "next").expect("update tracked file");
        git_sync(&repo, &["add", "tracked.txt"]);
        git_sync(&repo, &["commit", "-m", "next"]);
        let head_commit = head_oid(&repo).await.expect("head oid");

        let revision = ItemRevision {
            id: revision_id,
            item_id,
            revision_no: 1,
            title: "Review candidate".into(),
            description: "Review the current candidate".into(),
            acceptance_criteria: "No issues".into(),
            target_ref: "refs/heads/main".into(),
            approval_policy: ApprovalPolicy::Required,
            policy_snapshot: serde_json::json!({}),
            template_map_snapshot: serde_json::json!({ "review_candidate_initial": "review-candidate" }),
            seed_commit_oid: base_commit.clone(),
            seed_target_commit_oid: Some(base_commit.clone()),
            supersedes_revision_id: None,
            created_at,
        };
        db.create_item_with_revision(&item, &revision)
            .await
            .expect("create item");

        let job = Job {
            id: ingot_domain::ids::JobId::new(),
            project_id: project.id,
            item_id,
            item_revision_id: revision_id,
            step_id: "review_candidate_initial".into(),
            semantic_attempt_no: 1,
            retry_no: 0,
            supersedes_job_id: None,
            status: JobStatus::Queued,
            outcome_class: None,
            phase_kind: PhaseKind::Review,
            workspace_id: None,
            workspace_kind: WorkspaceKind::Review,
            execution_permission: ExecutionPermission::MustNotMutate,
            context_policy: ContextPolicy::Fresh,
            phase_template_slug: "review-candidate".into(),
            phase_template_digest: None,
            prompt_snapshot: None,
            input_base_commit_oid: Some(base_commit.clone()),
            input_head_commit_oid: Some(head_commit.clone()),
            output_artifact_kind: OutputArtifactKind::ReviewReport,
            output_commit_oid: None,
            result_schema_version: None,
            result_payload: None,
            agent_id: None,
            process_pid: None,
            lease_owner_id: None,
            heartbeat_at: None,
            lease_expires_at: None,
            error_code: None,
            error_message: None,
            created_at,
            started_at: None,
            ended_at: None,
        };
        db.create_job(&job).await.expect("create job");

        let dispatcher = JobDispatcher::with_runner(
            db.clone(),
            ProjectLocks::default(),
            DispatcherConfig::new(state_root),
            Arc::new(StaticReviewRunner {
                base_commit_oid: base_commit.clone(),
                head_commit_oid: head_commit.clone(),
            }),
        );

        assert!(dispatcher.tick().await.expect("tick should run"));

        let updated_job = db.get_job(job.id).await.expect("updated job");
        assert_eq!(updated_job.status, JobStatus::Completed);
        assert_eq!(updated_job.outcome_class, Some(OutcomeClass::Clean));
        assert_eq!(
            updated_job.result_schema_version.as_deref(),
            Some("review_report:v1")
        );
        assert_eq!(
            updated_job
                .result_payload
                .as_ref()
                .and_then(|payload| payload.get("outcome"))
                .and_then(|value| value.as_str()),
            Some("clean")
        );

        let workspaces = db
            .list_workspaces_by_item(item.id)
            .await
            .expect("list workspaces");
        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0].kind, WorkspaceKind::Review);
        assert_eq!(workspaces[0].status, WorkspaceStatus::Abandoned);
        assert!(!Path::new(&workspaces[0].path).exists());
    }

    #[tokio::test]
    async fn tick_auto_finalizes_prepared_convergence_for_not_required_approval() {
        let repo = temp_git_repo();
        let base_commit = head_oid(&repo).await.expect("base head");
        std::fs::write(repo.join("tracked.txt"), "prepared").expect("write file");
        git_sync(&repo, &["add", "tracked.txt"]);
        git_sync(&repo, &["commit", "-m", "prepared"]);
        let prepared_commit = head_oid(&repo).await.expect("prepared head");
        git_sync(&repo, &["reset", "--hard", &base_commit]);
        let integration_workspace_path =
            std::env::temp_dir().join(format!("ingot-runtime-integration-{}", Uuid::now_v7()));

        let db_path =
            std::env::temp_dir().join(format!("ingot-runtime-finalize-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let state_root =
            std::env::temp_dir().join(format!("ingot-runtime-finalize-state-{}", Uuid::now_v7()));
        let dispatcher = JobDispatcher::with_runner(
            db.clone(),
            ProjectLocks::default(),
            DispatcherConfig::new(state_root.clone()),
            Arc::new(FakeRunner),
        );

        let created_at = Utc::now();
        let project = Project {
            id: ingot_domain::ids::ProjectId::new(),
            name: "repo".into(),
            path: repo.display().to_string(),
            default_branch: "main".into(),
            color: "#000".into(),
            created_at,
            updated_at: created_at,
        };
        let seed_commit = head_oid(&repo).await.expect("seed head");
        db.create_project(&project).await.expect("create project");
        let paths = ensure_test_mirror(state_root.as_path(), &project).await;
        git_sync(
            &paths.mirror_git_dir,
            &[
                "update-ref",
                "refs/ingot/workspaces/wrk_integration_test",
                &prepared_commit,
            ],
        );
        git_sync(&repo, &["reset", "--hard", &base_commit]);
        git_sync(
            &paths.mirror_git_dir,
            &[
                "worktree",
                "add",
                "--detach",
                integration_workspace_path.to_str().expect("workspace path"),
                "refs/ingot/workspaces/wrk_integration_test",
            ],
        );

        let item_id = ingot_domain::ids::ItemId::new();
        let revision_id = ingot_domain::ids::ItemRevisionId::new();
        let seed_commit = head_oid(&repo).await.expect("seed head");
        let item = ingot_domain::item::Item {
            id: item_id,
            project_id: project.id,
            classification: Classification::Change,
            workflow_version: "delivery:v1".into(),
            lifecycle_state: LifecycleState::Open,
            parking_state: ParkingState::Active,
            done_reason: None,
            resolution_source: None,
            approval_state: ApprovalState::NotRequired,
            escalation_state: EscalationState::None,
            escalation_reason: None,
            current_revision_id: revision_id,
            origin_kind: OriginKind::Manual,
            origin_finding_id: None,
            priority: Priority::Major,
            labels: vec![],
            operator_notes: None,
            created_at,
            updated_at: created_at,
            closed_at: None,
        };
        let seed_commit = head_oid(&repo).await.expect("seed head");
        let revision = ItemRevision {
            id: revision_id,
            item_id,
            revision_no: 1,
            title: "Finalize".into(),
            description: "Finalize integrated".into(),
            acceptance_criteria: "done".into(),
            target_ref: "refs/heads/main".into(),
            approval_policy: ApprovalPolicy::NotRequired,
            policy_snapshot: serde_json::json!({}),
            template_map_snapshot: serde_json::json!({}),
            seed_commit_oid: base_commit.clone(),
            seed_target_commit_oid: Some(base_commit.clone()),
            supersedes_revision_id: None,
            created_at,
        };
        db.create_item_with_revision(&item, &revision)
            .await
            .expect("create item");

        let integration_workspace = Workspace {
            id: WorkspaceId::new(),
            project_id: project.id,
            kind: WorkspaceKind::Integration,
            strategy: WorkspaceStrategy::Worktree,
            path: integration_workspace_path.display().to_string(),
            created_for_revision_id: Some(revision.id),
            parent_workspace_id: None,
            target_ref: Some("refs/heads/main".into()),
            workspace_ref: Some("refs/ingot/workspaces/wrk_integration_test".into()),
            base_commit_oid: Some(base_commit.clone()),
            head_commit_oid: Some(prepared_commit.clone()),
            retention_policy: RetentionPolicy::Persistent,
            status: WorkspaceStatus::Ready,
            current_job_id: None,
            created_at,
            updated_at: created_at,
        };
        db.create_workspace(&integration_workspace)
            .await
            .expect("create workspace");
        let source_workspace = Workspace {
            id: WorkspaceId::new(),
            project_id: project.id,
            kind: WorkspaceKind::Authoring,
            strategy: WorkspaceStrategy::Worktree,
            path: std::env::temp_dir()
                .join(format!("ingot-runtime-source-{}", Uuid::now_v7()))
                .display()
                .to_string(),
            created_for_revision_id: Some(revision.id),
            parent_workspace_id: None,
            target_ref: Some("refs/heads/main".into()),
            workspace_ref: Some("refs/ingot/workspaces/wrk_source_test".into()),
            base_commit_oid: Some(base_commit.clone()),
            head_commit_oid: Some(prepared_commit.clone()),
            retention_policy: RetentionPolicy::Persistent,
            status: WorkspaceStatus::Ready,
            current_job_id: None,
            created_at,
            updated_at: created_at,
        };
        db.create_workspace(&source_workspace)
            .await
            .expect("create source workspace");

        let validate_job = Job {
            id: ingot_domain::ids::JobId::new(),
            project_id: project.id,
            item_id,
            item_revision_id: revision_id,
            step_id: "validate_integrated".into(),
            semantic_attempt_no: 1,
            retry_no: 0,
            supersedes_job_id: None,
            status: JobStatus::Completed,
            outcome_class: Some(OutcomeClass::Clean),
            phase_kind: PhaseKind::Validate,
            workspace_id: Some(integration_workspace.id),
            workspace_kind: WorkspaceKind::Integration,
            execution_permission: ExecutionPermission::MustNotMutate,
            context_policy: ContextPolicy::ResumeContext,
            phase_template_slug: "validate-integrated".into(),
            phase_template_digest: None,
            prompt_snapshot: None,
            input_base_commit_oid: Some(base_commit.clone()),
            input_head_commit_oid: Some(prepared_commit.clone()),
            output_artifact_kind: OutputArtifactKind::ValidationReport,
            output_commit_oid: None,
            result_schema_version: Some("validation_report:v1".into()),
            result_payload: Some(serde_json::json!({
                "outcome": "clean",
                "summary": "integrated clean",
                "checks": [],
                "findings": []
            })),
            agent_id: None,
            process_pid: None,
            lease_owner_id: None,
            heartbeat_at: None,
            lease_expires_at: None,
            error_code: None,
            error_message: None,
            created_at,
            started_at: Some(created_at),
            ended_at: Some(created_at),
        };
        db.create_job(&validate_job).await.expect("create job");

        let convergence = Convergence {
            id: ingot_domain::ids::ConvergenceId::new(),
            project_id: project.id,
            item_id,
            item_revision_id: revision_id,
            source_workspace_id: source_workspace.id,
            integration_workspace_id: Some(integration_workspace.id),
            source_head_commit_oid: prepared_commit.clone(),
            target_ref: "refs/heads/main".into(),
            strategy: ingot_domain::convergence::ConvergenceStrategy::RebaseThenFastForward,
            status: ConvergenceStatus::Prepared,
            input_target_commit_oid: Some(base_commit.clone()),
            prepared_commit_oid: Some(prepared_commit.clone()),
            final_target_commit_oid: None,
            target_head_valid: Some(true),
            conflict_summary: None,
            created_at,
            completed_at: None,
        };
        db.create_convergence(&convergence)
            .await
            .expect("create convergence");
        db.create_queue_entry(&ConvergenceQueueEntry {
            id: ingot_domain::ids::ConvergenceQueueEntryId::new(),
            project_id: project.id,
            item_id: item.id,
            item_revision_id: revision.id,
            target_ref: "refs/heads/main".into(),
            status: ConvergenceQueueEntryStatus::Head,
            head_acquired_at: Some(created_at),
            created_at,
            updated_at: created_at,
            released_at: None,
        })
        .await
        .expect("insert queue entry");

        assert!(dispatcher.tick().await.expect("tick should finalize"));

        let updated_item = db.get_item(item.id).await.expect("updated item");
        assert_eq!(updated_item.lifecycle_state, LifecycleState::Done);
        assert_eq!(
            updated_item.resolution_source,
            Some(ResolutionSource::SystemCommand)
        );
        let updated_convergence = db
            .list_convergences_by_item(item.id)
            .await
            .expect("list convergences")
            .into_iter()
            .next()
            .expect("convergence");
        assert_eq!(updated_convergence.status, ConvergenceStatus::Finalized);
        assert_eq!(
            git_output(&repo, &["rev-parse", "refs/heads/main"]),
            prepared_commit
        );
        assert!(!integration_workspace_path.exists());
        let unresolved = db
            .list_unresolved_git_operations()
            .await
            .expect("list unresolved");
        assert!(
            unresolved.is_empty(),
            "auto-finalize should resolve git ops"
        );

        std::fs::write(repo.join("tracked.txt"), "post-finalize refresh")
            .expect("write post-finalize change");
        git_sync(&repo, &["add", "tracked.txt"]);
        git_sync(&repo, &["commit", "-m", "post-finalize refresh"]);
        let refreshed_head = head_oid(&repo).await.expect("refreshed head");
        let refreshed_paths = dispatcher
            .refresh_project_mirror(&project)
            .await
            .expect("refresh mirror");
        assert_eq!(
            resolve_ref_oid(refreshed_paths.mirror_git_dir.as_path(), "refs/heads/main")
                .await
                .expect("resolve mirror head"),
            Some(refreshed_head)
        );
    }

    #[tokio::test]
    async fn tick_auto_finalizes_granted_prepared_convergence_even_when_commit_exists_only_in_mirror()
     {
        let repo = temp_git_repo();
        let base_commit = head_oid(&repo).await.expect("base head");
        let db_path = std::env::temp_dir().join(format!(
            "ingot-runtime-finalize-mirror-only-{}.db",
            Uuid::now_v7()
        ));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let state_root = std::env::temp_dir().join(format!(
            "ingot-runtime-finalize-mirror-only-state-{}",
            Uuid::now_v7()
        ));
        let dispatcher = JobDispatcher::with_runner(
            db.clone(),
            ProjectLocks::default(),
            DispatcherConfig::new(state_root.clone()),
            Arc::new(FakeRunner),
        );

        let created_at = Utc::now();
        let project = Project {
            id: ingot_domain::ids::ProjectId::new(),
            name: "repo".into(),
            path: repo.display().to_string(),
            default_branch: "main".into(),
            color: "#000".into(),
            created_at,
            updated_at: created_at,
        };
        db.create_project(&project).await.expect("create project");
        let paths = ensure_test_mirror(state_root.as_path(), &project).await;
        let workspace_ref = "refs/ingot/workspaces/mirror-only-finalize";
        let (integration_workspace_path, prepared_commit) = create_mirror_only_commit(
            paths.mirror_git_dir.as_path(),
            &base_commit,
            workspace_ref,
            "mirror-only prepared",
        )
        .await;

        let checkout_has_commit = std::process::Command::new("git")
            .args(["cat-file", "-e", &format!("{prepared_commit}^{{commit}}")])
            .current_dir(&repo)
            .status()
            .expect("check checkout object");
        assert!(
            !checkout_has_commit.success(),
            "test setup requires the prepared commit to be absent from the registered checkout"
        );

        let item_id = ingot_domain::ids::ItemId::new();
        let revision_id = ingot_domain::ids::ItemRevisionId::new();
        let item = ingot_domain::item::Item {
            id: item_id,
            project_id: project.id,
            classification: Classification::Change,
            workflow_version: "delivery:v1".into(),
            lifecycle_state: LifecycleState::Open,
            parking_state: ParkingState::Active,
            done_reason: None,
            resolution_source: None,
            approval_state: ApprovalState::Granted,
            escalation_state: EscalationState::None,
            escalation_reason: None,
            current_revision_id: revision_id,
            origin_kind: OriginKind::Manual,
            origin_finding_id: None,
            priority: Priority::Major,
            labels: vec![],
            operator_notes: None,
            created_at,
            updated_at: created_at,
            closed_at: None,
        };
        let revision = ItemRevision {
            id: revision_id,
            item_id,
            revision_no: 1,
            title: "Finalize mirror-only prepared commit".into(),
            description: "finalize granted prepared convergence".into(),
            acceptance_criteria: "checkout syncs and item closes".into(),
            target_ref: "refs/heads/main".into(),
            approval_policy: ApprovalPolicy::Required,
            policy_snapshot: serde_json::json!({}),
            template_map_snapshot: serde_json::json!({}),
            seed_commit_oid: base_commit.clone(),
            seed_target_commit_oid: Some(base_commit.clone()),
            supersedes_revision_id: None,
            created_at,
        };
        db.create_item_with_revision(&item, &revision)
            .await
            .expect("create item");

        let integration_workspace = Workspace {
            id: WorkspaceId::new(),
            project_id: project.id,
            kind: WorkspaceKind::Integration,
            strategy: WorkspaceStrategy::Worktree,
            path: integration_workspace_path.display().to_string(),
            created_for_revision_id: Some(revision.id),
            parent_workspace_id: None,
            target_ref: Some("refs/heads/main".into()),
            workspace_ref: Some(workspace_ref.into()),
            base_commit_oid: Some(base_commit.clone()),
            head_commit_oid: Some(prepared_commit.clone()),
            retention_policy: RetentionPolicy::Persistent,
            status: WorkspaceStatus::Ready,
            current_job_id: None,
            created_at,
            updated_at: created_at,
        };
        db.create_workspace(&integration_workspace)
            .await
            .expect("create integration workspace");
        let source_workspace = Workspace {
            id: WorkspaceId::new(),
            project_id: project.id,
            kind: WorkspaceKind::Authoring,
            strategy: WorkspaceStrategy::Worktree,
            path: std::env::temp_dir()
                .join(format!(
                    "ingot-runtime-mirror-only-source-{}",
                    Uuid::now_v7()
                ))
                .display()
                .to_string(),
            created_for_revision_id: Some(revision.id),
            parent_workspace_id: None,
            target_ref: Some("refs/heads/main".into()),
            workspace_ref: Some("refs/ingot/workspaces/mirror-only-source".into()),
            base_commit_oid: Some(base_commit.clone()),
            head_commit_oid: Some(prepared_commit.clone()),
            retention_policy: RetentionPolicy::Persistent,
            status: WorkspaceStatus::Ready,
            current_job_id: None,
            created_at,
            updated_at: created_at,
        };
        db.create_workspace(&source_workspace)
            .await
            .expect("create source workspace");

        let validate_job = Job {
            id: ingot_domain::ids::JobId::new(),
            project_id: project.id,
            item_id,
            item_revision_id: revision_id,
            step_id: step::VALIDATE_INTEGRATED.into(),
            semantic_attempt_no: 1,
            retry_no: 0,
            supersedes_job_id: None,
            status: JobStatus::Completed,
            outcome_class: Some(OutcomeClass::Clean),
            phase_kind: PhaseKind::Validate,
            workspace_id: Some(integration_workspace.id),
            workspace_kind: WorkspaceKind::Integration,
            execution_permission: ExecutionPermission::MustNotMutate,
            context_policy: ContextPolicy::ResumeContext,
            phase_template_slug: "validate-integrated".into(),
            phase_template_digest: None,
            prompt_snapshot: None,
            input_base_commit_oid: Some(base_commit.clone()),
            input_head_commit_oid: Some(prepared_commit.clone()),
            output_artifact_kind: OutputArtifactKind::ValidationReport,
            output_commit_oid: None,
            result_schema_version: Some("validation_report:v1".into()),
            result_payload: Some(serde_json::json!({
                "outcome": "clean",
                "summary": "integrated clean",
                "checks": [],
                "findings": []
            })),
            agent_id: None,
            process_pid: None,
            lease_owner_id: None,
            heartbeat_at: None,
            lease_expires_at: None,
            error_code: None,
            error_message: None,
            created_at,
            started_at: Some(created_at),
            ended_at: Some(created_at),
        };
        db.create_job(&validate_job)
            .await
            .expect("create validation");

        let convergence = Convergence {
            id: ingot_domain::ids::ConvergenceId::new(),
            project_id: project.id,
            item_id,
            item_revision_id: revision_id,
            source_workspace_id: source_workspace.id,
            integration_workspace_id: Some(integration_workspace.id),
            source_head_commit_oid: prepared_commit.clone(),
            target_ref: "refs/heads/main".into(),
            strategy: ConvergenceStrategy::RebaseThenFastForward,
            status: ConvergenceStatus::Prepared,
            input_target_commit_oid: Some(base_commit.clone()),
            prepared_commit_oid: Some(prepared_commit.clone()),
            final_target_commit_oid: None,
            target_head_valid: Some(true),
            conflict_summary: None,
            created_at,
            completed_at: None,
        };
        db.create_convergence(&convergence)
            .await
            .expect("create convergence");
        db.create_queue_entry(&ConvergenceQueueEntry {
            id: ingot_domain::ids::ConvergenceQueueEntryId::new(),
            project_id: project.id,
            item_id,
            item_revision_id: revision.id,
            target_ref: "refs/heads/main".into(),
            status: ConvergenceQueueEntryStatus::Head,
            head_acquired_at: Some(created_at),
            created_at,
            updated_at: created_at,
            released_at: None,
        })
        .await
        .expect("insert queue entry");

        assert!(dispatcher.tick().await.expect("tick should finalize"));

        assert_eq!(
            head_oid(&repo).await.expect("checkout head"),
            prepared_commit
        );
        let updated_convergence = db
            .get_convergence(convergence.id)
            .await
            .expect("convergence");
        assert_eq!(updated_convergence.status, ConvergenceStatus::Finalized);
        let updated_item = db.get_item(item.id).await.expect("item");
        assert_eq!(updated_item.lifecycle_state, LifecycleState::Done);
        assert_eq!(updated_item.approval_state, ApprovalState::Approved);
        let queue_entries = db
            .list_queue_entries_by_item(item.id)
            .await
            .expect("queue entries");
        assert_eq!(
            queue_entries[0].status,
            ConvergenceQueueEntryStatus::Released
        );
        let unresolved = db
            .list_unresolved_git_operations()
            .await
            .expect("list unresolved");
        assert!(unresolved.is_empty(), "finalize op should reconcile");
    }

    #[tokio::test]
    async fn tick_invalidates_stale_prepared_convergence() {
        let repo = temp_git_repo();
        let base_commit = head_oid(&repo).await.expect("base head");
        std::fs::write(repo.join("tracked.txt"), "prepared").expect("write file");
        git_sync(&repo, &["add", "tracked.txt"]);
        git_sync(&repo, &["commit", "-m", "prepared"]);
        let prepared_commit = head_oid(&repo).await.expect("prepared head");
        git_sync(&repo, &["reset", "--hard", &base_commit]);
        std::fs::write(repo.join("tracked.txt"), "moved target").expect("write moved target");
        git_sync(&repo, &["add", "tracked.txt"]);
        git_sync(&repo, &["commit", "-m", "moved target"]);

        let db_path =
            std::env::temp_dir().join(format!("ingot-runtime-invalidate-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let dispatcher = JobDispatcher::with_runner(
            db.clone(),
            ProjectLocks::default(),
            DispatcherConfig::new(
                std::env::temp_dir()
                    .join(format!("ingot-runtime-invalidate-state-{}", Uuid::now_v7())),
            ),
            Arc::new(FakeRunner),
        );

        let created_at = Utc::now();
        let project = Project {
            id: ingot_domain::ids::ProjectId::new(),
            name: "repo".into(),
            path: repo.display().to_string(),
            default_branch: "main".into(),
            color: "#000".into(),
            created_at,
            updated_at: created_at,
        };
        db.create_project(&project).await.expect("create project");

        let item_id = ingot_domain::ids::ItemId::new();
        let revision_id = ingot_domain::ids::ItemRevisionId::new();
        let seed_commit = head_oid(&repo).await.expect("seed head");
        let item = ingot_domain::item::Item {
            id: item_id,
            project_id: project.id,
            classification: Classification::Change,
            workflow_version: "delivery:v1".into(),
            lifecycle_state: LifecycleState::Open,
            parking_state: ParkingState::Active,
            done_reason: None,
            resolution_source: None,
            approval_state: ApprovalState::Pending,
            escalation_state: EscalationState::None,
            escalation_reason: None,
            current_revision_id: revision_id,
            origin_kind: OriginKind::Manual,
            origin_finding_id: None,
            priority: Priority::Major,
            labels: vec![],
            operator_notes: None,
            created_at,
            updated_at: created_at,
            closed_at: None,
        };
        let revision = ItemRevision {
            id: revision_id,
            item_id,
            revision_no: 1,
            title: "Invalidate".into(),
            description: "invalidate prepared".into(),
            acceptance_criteria: "reset approval".into(),
            target_ref: "refs/heads/main".into(),
            approval_policy: ApprovalPolicy::Required,
            policy_snapshot: serde_json::json!({}),
            template_map_snapshot: serde_json::json!({}),
            seed_commit_oid: base_commit.clone(),
            seed_target_commit_oid: Some(base_commit.clone()),
            supersedes_revision_id: None,
            created_at,
        };
        db.create_item_with_revision(&item, &revision)
            .await
            .expect("create item");

        let integration_workspace = Workspace {
            id: WorkspaceId::new(),
            project_id: project.id,
            kind: WorkspaceKind::Integration,
            strategy: WorkspaceStrategy::Worktree,
            path: std::env::temp_dir()
                .join(format!("ingot-runtime-stale-{}", Uuid::now_v7()))
                .display()
                .to_string(),
            created_for_revision_id: Some(revision.id),
            parent_workspace_id: None,
            target_ref: Some("refs/heads/main".into()),
            workspace_ref: Some("refs/ingot/workspaces/stale".into()),
            base_commit_oid: Some(base_commit.clone()),
            head_commit_oid: Some(prepared_commit.clone()),
            retention_policy: RetentionPolicy::Persistent,
            status: WorkspaceStatus::Ready,
            current_job_id: None,
            created_at,
            updated_at: created_at,
        };
        db.create_workspace(&integration_workspace)
            .await
            .expect("create workspace");
        let source_workspace = Workspace {
            id: WorkspaceId::new(),
            project_id: project.id,
            kind: WorkspaceKind::Authoring,
            strategy: WorkspaceStrategy::Worktree,
            path: std::env::temp_dir()
                .join(format!("ingot-runtime-source-{}", Uuid::now_v7()))
                .display()
                .to_string(),
            created_for_revision_id: Some(revision.id),
            parent_workspace_id: None,
            target_ref: Some("refs/heads/main".into()),
            workspace_ref: Some("refs/ingot/workspaces/stale-source".into()),
            base_commit_oid: Some(base_commit.clone()),
            head_commit_oid: Some(prepared_commit.clone()),
            retention_policy: RetentionPolicy::Persistent,
            status: WorkspaceStatus::Ready,
            current_job_id: None,
            created_at,
            updated_at: created_at,
        };
        db.create_workspace(&source_workspace)
            .await
            .expect("create source workspace");

        let validate_job = Job {
            id: ingot_domain::ids::JobId::new(),
            project_id: project.id,
            item_id,
            item_revision_id: revision_id,
            step_id: "validate_integrated".into(),
            semantic_attempt_no: 1,
            retry_no: 0,
            supersedes_job_id: None,
            status: JobStatus::Completed,
            outcome_class: Some(OutcomeClass::Clean),
            phase_kind: PhaseKind::Validate,
            workspace_id: Some(integration_workspace.id),
            workspace_kind: WorkspaceKind::Integration,
            execution_permission: ExecutionPermission::MustNotMutate,
            context_policy: ContextPolicy::ResumeContext,
            phase_template_slug: "validate-integrated".into(),
            phase_template_digest: None,
            prompt_snapshot: None,
            input_base_commit_oid: Some(base_commit.clone()),
            input_head_commit_oid: Some(prepared_commit.clone()),
            output_artifact_kind: OutputArtifactKind::ValidationReport,
            output_commit_oid: None,
            result_schema_version: Some("validation_report:v1".into()),
            result_payload: Some(serde_json::json!({
                "outcome": "clean",
                "summary": "integrated clean",
                "checks": [],
                "findings": []
            })),
            agent_id: None,
            process_pid: None,
            lease_owner_id: None,
            heartbeat_at: None,
            lease_expires_at: None,
            error_code: None,
            error_message: None,
            created_at,
            started_at: Some(created_at),
            ended_at: Some(created_at),
        };
        db.create_job(&validate_job).await.expect("create job");

        let convergence = Convergence {
            id: ingot_domain::ids::ConvergenceId::new(),
            project_id: project.id,
            item_id,
            item_revision_id: revision_id,
            source_workspace_id: source_workspace.id,
            integration_workspace_id: Some(integration_workspace.id),
            source_head_commit_oid: prepared_commit.clone(),
            target_ref: "refs/heads/main".into(),
            strategy: ingot_domain::convergence::ConvergenceStrategy::RebaseThenFastForward,
            status: ConvergenceStatus::Prepared,
            input_target_commit_oid: Some(base_commit.clone()),
            prepared_commit_oid: Some(prepared_commit.clone()),
            final_target_commit_oid: None,
            target_head_valid: Some(false),
            conflict_summary: None,
            created_at,
            completed_at: None,
        };
        db.create_convergence(&convergence)
            .await
            .expect("create convergence");

        assert!(dispatcher.tick().await.expect("tick should invalidate"));

        let updated_item = db.get_item(item.id).await.expect("updated item");
        assert_eq!(updated_item.approval_state, ApprovalState::NotRequested);
        let updated_convergence = db
            .list_convergences_by_item(item.id)
            .await
            .expect("list convergences")
            .into_iter()
            .next()
            .expect("convergence");
        assert_eq!(updated_convergence.status, ConvergenceStatus::Failed);
        assert_eq!(
            updated_convergence.conflict_summary.as_deref(),
            Some("target_ref_moved")
        );
        let updated_workspace = db
            .get_workspace(integration_workspace.id)
            .await
            .expect("workspace");
        assert_eq!(updated_workspace.status, WorkspaceStatus::Stale);
    }

    #[tokio::test]
    async fn tick_reconciles_applied_finalize_operation_instead_of_invalidating_prepared_convergence()
     {
        let repo = temp_git_repo();
        let base_commit = head_oid(&repo).await.expect("base head");
        std::fs::write(repo.join("tracked.txt"), "prepared").expect("write prepared");
        git_sync(&repo, &["add", "tracked.txt"]);
        git_sync(&repo, &["commit", "-m", "prepared"]);
        let prepared_commit = head_oid(&repo).await.expect("prepared head");

        let db_path = std::env::temp_dir().join(format!(
            "ingot-runtime-finalize-reconcile-{}.db",
            Uuid::now_v7()
        ));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let state_root = std::env::temp_dir().join(format!(
            "ingot-runtime-finalize-reconcile-state-{}",
            Uuid::now_v7()
        ));
        let dispatcher = JobDispatcher::with_runner(
            db.clone(),
            ProjectLocks::default(),
            DispatcherConfig::new(state_root.clone()),
            Arc::new(FakeRunner),
        );

        let created_at = Utc::now();
        let project = Project {
            id: ingot_domain::ids::ProjectId::new(),
            name: "repo".into(),
            path: repo.display().to_string(),
            default_branch: "main".into(),
            color: "#000".into(),
            created_at,
            updated_at: created_at,
        };
        db.create_project(&project).await.expect("create project");
        let paths = ensure_test_mirror(state_root.as_path(), &project).await;
        assert_eq!(
            resolve_ref_oid(paths.mirror_git_dir.as_path(), "refs/heads/main")
                .await
                .expect("mirror head"),
            Some(prepared_commit.clone())
        );

        let item_id = ingot_domain::ids::ItemId::new();
        let revision_id = ingot_domain::ids::ItemRevisionId::new();
        let item = ingot_domain::item::Item {
            id: item_id,
            project_id: project.id,
            classification: Classification::Change,
            workflow_version: "delivery:v1".into(),
            lifecycle_state: LifecycleState::Open,
            parking_state: ParkingState::Active,
            done_reason: None,
            resolution_source: None,
            approval_state: ApprovalState::Granted,
            escalation_state: EscalationState::None,
            escalation_reason: None,
            current_revision_id: revision_id,
            origin_kind: OriginKind::Manual,
            origin_finding_id: None,
            priority: Priority::Major,
            labels: vec![],
            operator_notes: None,
            created_at,
            updated_at: created_at,
            closed_at: None,
        };
        let revision = ItemRevision {
            id: revision_id,
            item_id,
            revision_no: 1,
            title: "Reconcile applied finalize".into(),
            description: "finalize should reconcile".into(),
            acceptance_criteria: "prepared convergence becomes finalized".into(),
            target_ref: "refs/heads/main".into(),
            approval_policy: ApprovalPolicy::Required,
            policy_snapshot: serde_json::json!({}),
            template_map_snapshot: serde_json::json!({}),
            seed_commit_oid: base_commit.clone(),
            seed_target_commit_oid: Some(base_commit.clone()),
            supersedes_revision_id: None,
            created_at,
        };
        db.create_item_with_revision(&item, &revision)
            .await
            .expect("create item");

        let source_workspace = Workspace {
            id: WorkspaceId::new(),
            project_id: project.id,
            kind: WorkspaceKind::Authoring,
            strategy: WorkspaceStrategy::Worktree,
            path: std::env::temp_dir()
                .join(format!(
                    "ingot-runtime-finalize-reconcile-source-{}",
                    Uuid::now_v7()
                ))
                .display()
                .to_string(),
            created_for_revision_id: Some(revision.id),
            parent_workspace_id: None,
            target_ref: Some("refs/heads/main".into()),
            workspace_ref: Some("refs/ingot/workspaces/finalize-reconcile-source".into()),
            base_commit_oid: Some(base_commit.clone()),
            head_commit_oid: Some(prepared_commit.clone()),
            retention_policy: RetentionPolicy::Persistent,
            status: WorkspaceStatus::Ready,
            current_job_id: None,
            created_at,
            updated_at: created_at,
        };
        db.create_workspace(&source_workspace)
            .await
            .expect("create source workspace");

        let validate_job = Job {
            id: ingot_domain::ids::JobId::new(),
            project_id: project.id,
            item_id,
            item_revision_id: revision_id,
            step_id: step::VALIDATE_INTEGRATED.into(),
            semantic_attempt_no: 1,
            retry_no: 0,
            supersedes_job_id: None,
            status: JobStatus::Completed,
            outcome_class: Some(OutcomeClass::Clean),
            phase_kind: PhaseKind::Validate,
            workspace_id: None,
            workspace_kind: WorkspaceKind::Integration,
            execution_permission: ExecutionPermission::MustNotMutate,
            context_policy: ContextPolicy::ResumeContext,
            phase_template_slug: "validate-integrated".into(),
            phase_template_digest: None,
            prompt_snapshot: None,
            input_base_commit_oid: Some(base_commit.clone()),
            input_head_commit_oid: Some(prepared_commit.clone()),
            output_artifact_kind: OutputArtifactKind::ValidationReport,
            output_commit_oid: None,
            result_schema_version: Some("validation_report:v1".into()),
            result_payload: Some(serde_json::json!({
                "outcome": "clean",
                "summary": "integrated clean",
                "checks": [],
                "findings": []
            })),
            agent_id: None,
            process_pid: None,
            lease_owner_id: None,
            heartbeat_at: None,
            lease_expires_at: None,
            error_code: None,
            error_message: None,
            created_at,
            started_at: Some(created_at),
            ended_at: Some(created_at),
        };
        db.create_job(&validate_job)
            .await
            .expect("create validation");

        let convergence = Convergence {
            id: ingot_domain::ids::ConvergenceId::new(),
            project_id: project.id,
            item_id,
            item_revision_id: revision_id,
            source_workspace_id: source_workspace.id,
            integration_workspace_id: None,
            source_head_commit_oid: prepared_commit.clone(),
            target_ref: "refs/heads/main".into(),
            strategy: ConvergenceStrategy::RebaseThenFastForward,
            status: ConvergenceStatus::Prepared,
            input_target_commit_oid: Some(base_commit.clone()),
            prepared_commit_oid: Some(prepared_commit.clone()),
            final_target_commit_oid: None,
            target_head_valid: Some(true),
            conflict_summary: None,
            created_at,
            completed_at: None,
        };
        db.create_convergence(&convergence)
            .await
            .expect("create convergence");
        db.create_queue_entry(&ConvergenceQueueEntry {
            id: ingot_domain::ids::ConvergenceQueueEntryId::new(),
            project_id: project.id,
            item_id,
            item_revision_id: revision.id,
            target_ref: "refs/heads/main".into(),
            status: ConvergenceQueueEntryStatus::Head,
            head_acquired_at: Some(created_at),
            created_at,
            updated_at: created_at,
            released_at: None,
        })
        .await
        .expect("insert queue entry");
        db.create_git_operation(&GitOperation {
            id: GitOperationId::new(),
            project_id: project.id,
            operation_kind: OperationKind::FinalizeTargetRef,
            entity_type: GitEntityType::Convergence,
            entity_id: convergence.id.to_string(),
            workspace_id: None,
            ref_name: Some("refs/heads/main".into()),
            expected_old_oid: Some(base_commit.clone()),
            new_oid: Some(prepared_commit.clone()),
            commit_oid: Some(prepared_commit.clone()),
            status: GitOperationStatus::Applied,
            metadata: None,
            created_at,
            completed_at: Some(created_at),
        })
        .await
        .expect("create finalize operation");

        assert!(
            dispatcher
                .tick()
                .await
                .expect("tick should reconcile finalize")
        );

        let updated_convergence = db
            .get_convergence(convergence.id)
            .await
            .expect("convergence");
        assert_eq!(updated_convergence.status, ConvergenceStatus::Finalized);
        let updated_item = db.get_item(item.id).await.expect("item");
        assert_eq!(updated_item.lifecycle_state, LifecycleState::Done);
        let queue_entries = db
            .list_queue_entries_by_item(item.id)
            .await
            .expect("queue entries");
        assert_eq!(
            queue_entries[0].status,
            ConvergenceQueueEntryStatus::Released
        );
        let unresolved = db
            .list_unresolved_git_operations()
            .await
            .expect("list unresolved");
        assert!(
            unresolved.is_empty(),
            "applied finalize op should reconcile"
        );
    }

    #[tokio::test]
    async fn tick_reprepares_granted_lane_head_without_prepared_convergence() {
        let repo = temp_git_repo();
        let seed_commit = head_oid(&repo).await.expect("seed head");
        let db_path = std::env::temp_dir().join(format!(
            "ingot-runtime-granted-reprepare-{}.db",
            Uuid::now_v7()
        ));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let dispatcher = JobDispatcher::with_runner(
            db.clone(),
            ProjectLocks::default(),
            DispatcherConfig::new(std::env::temp_dir().join(format!(
                "ingot-runtime-granted-reprepare-state-{}",
                Uuid::now_v7()
            ))),
            Arc::new(FakeRunner),
        );

        let created_at = Utc::now();
        let project = Project {
            id: ingot_domain::ids::ProjectId::new(),
            name: "repo".into(),
            path: repo.display().to_string(),
            default_branch: "main".into(),
            color: "#000".into(),
            created_at,
            updated_at: created_at,
        };
        db.create_project(&project).await.expect("create project");

        let item_id = ingot_domain::ids::ItemId::new();
        let revision_id = ingot_domain::ids::ItemRevisionId::new();
        let item = ingot_domain::item::Item {
            id: item_id,
            project_id: project.id,
            classification: Classification::Change,
            workflow_version: "delivery:v1".into(),
            lifecycle_state: LifecycleState::Open,
            parking_state: ParkingState::Active,
            done_reason: None,
            resolution_source: None,
            approval_state: ApprovalState::Granted,
            escalation_state: EscalationState::None,
            escalation_reason: None,
            current_revision_id: revision_id,
            origin_kind: OriginKind::Manual,
            origin_finding_id: None,
            priority: Priority::Major,
            labels: vec![],
            operator_notes: None,
            created_at,
            updated_at: created_at,
            closed_at: None,
        };
        let revision = ItemRevision {
            id: revision_id,
            item_id,
            revision_no: 1,
            title: "Reprepare".into(),
            description: "reprepare granted head".into(),
            acceptance_criteria: "prepare again".into(),
            target_ref: "refs/heads/main".into(),
            approval_policy: ApprovalPolicy::Required,
            policy_snapshot: serde_json::json!({}),
            template_map_snapshot: serde_json::json!({}),
            seed_commit_oid: seed_commit.clone(),
            seed_target_commit_oid: Some(seed_commit.clone()),
            supersedes_revision_id: None,
            created_at,
        };
        db.create_item_with_revision(&item, &revision)
            .await
            .expect("create item");

        let authoring_workspace = Workspace {
            id: WorkspaceId::new(),
            project_id: project.id,
            kind: WorkspaceKind::Authoring,
            strategy: WorkspaceStrategy::Worktree,
            path: std::env::temp_dir()
                .join(format!(
                    "ingot-runtime-granted-authoring-{}",
                    Uuid::now_v7()
                ))
                .display()
                .to_string(),
            created_for_revision_id: Some(revision.id),
            parent_workspace_id: None,
            target_ref: Some("refs/heads/main".into()),
            workspace_ref: Some("refs/ingot/workspaces/granted-head".into()),
            base_commit_oid: Some(seed_commit.clone()),
            head_commit_oid: Some(seed_commit.clone()),
            retention_policy: RetentionPolicy::Persistent,
            status: WorkspaceStatus::Ready,
            current_job_id: None,
            created_at,
            updated_at: created_at,
        };
        db.create_workspace(&authoring_workspace)
            .await
            .expect("create workspace");

        let candidate_validate_job = Job {
            id: ingot_domain::ids::JobId::new(),
            project_id: project.id,
            item_id,
            item_revision_id: revision_id,
            step_id: step::VALIDATE_CANDIDATE_INITIAL.into(),
            semantic_attempt_no: 1,
            retry_no: 0,
            supersedes_job_id: None,
            status: JobStatus::Completed,
            outcome_class: Some(OutcomeClass::Clean),
            phase_kind: PhaseKind::Validate,
            workspace_id: None,
            workspace_kind: WorkspaceKind::Authoring,
            execution_permission: ExecutionPermission::MustNotMutate,
            context_policy: ContextPolicy::ResumeContext,
            phase_template_slug: "validate-candidate".into(),
            phase_template_digest: None,
            prompt_snapshot: None,
            input_base_commit_oid: Some(seed_commit.clone()),
            input_head_commit_oid: Some(seed_commit.clone()),
            output_artifact_kind: OutputArtifactKind::ValidationReport,
            output_commit_oid: None,
            result_schema_version: Some("validation_report:v1".into()),
            result_payload: Some(serde_json::json!({
                "outcome": "clean",
                "summary": "integrated clean",
                "checks": [],
                "findings": []
            })),
            agent_id: None,
            process_pid: None,
            lease_owner_id: None,
            heartbeat_at: None,
            lease_expires_at: None,
            error_code: None,
            error_message: None,
            created_at,
            started_at: Some(created_at),
            ended_at: Some(created_at),
        };
        db.create_job(&candidate_validate_job)
            .await
            .expect("create candidate validation");
        let stale_validate_job = Job {
            id: ingot_domain::ids::JobId::new(),
            project_id: project.id,
            item_id,
            item_revision_id: revision_id,
            step_id: step::VALIDATE_INTEGRATED.into(),
            semantic_attempt_no: 1,
            retry_no: 0,
            supersedes_job_id: None,
            status: JobStatus::Completed,
            outcome_class: Some(OutcomeClass::Clean),
            phase_kind: PhaseKind::Validate,
            workspace_id: None,
            workspace_kind: WorkspaceKind::Integration,
            execution_permission: ExecutionPermission::MustNotMutate,
            context_policy: ContextPolicy::ResumeContext,
            phase_template_slug: "validate-integrated".into(),
            phase_template_digest: None,
            prompt_snapshot: None,
            input_base_commit_oid: Some(seed_commit.clone()),
            input_head_commit_oid: Some(seed_commit.clone()),
            output_artifact_kind: OutputArtifactKind::ValidationReport,
            output_commit_oid: None,
            result_schema_version: Some("validation_report:v1".into()),
            result_payload: Some(serde_json::json!({
                "outcome": "clean",
                "summary": "integrated clean",
                "checks": [],
                "findings": []
            })),
            agent_id: None,
            process_pid: None,
            lease_owner_id: None,
            heartbeat_at: None,
            lease_expires_at: None,
            error_code: None,
            error_message: None,
            created_at,
            started_at: Some(created_at),
            ended_at: Some(created_at + ChronoDuration::seconds(1)),
        };
        db.create_job(&stale_validate_job)
            .await
            .expect("create stale validation");
        db.create_queue_entry(&ConvergenceQueueEntry {
            id: ingot_domain::ids::ConvergenceQueueEntryId::new(),
            project_id: project.id,
            item_id,
            item_revision_id: revision.id,
            target_ref: "refs/heads/main".into(),
            status: ConvergenceQueueEntryStatus::Head,
            head_acquired_at: Some(created_at),
            created_at,
            updated_at: created_at,
            released_at: None,
        })
        .await
        .expect("insert queue entry");

        assert!(dispatcher.tick().await.expect("tick should reprepare"));

        let convergences = db
            .list_convergences_by_item(item.id)
            .await
            .expect("list convergences");
        assert!(
            convergences
                .iter()
                .any(|convergence| convergence.status == ConvergenceStatus::Prepared)
        );
        let jobs = db.list_jobs_by_item(item.id).await.expect("jobs");
        assert!(
            jobs.iter().any(|job| {
                job.step_id == step::VALIDATE_INTEGRATED
                    && job.status == JobStatus::Queued
                    && job.item_revision_id == revision.id
            }),
            "reprepare should dispatch a fresh integrated validation job"
        );
        let unresolved = db
            .list_unresolved_git_operations()
            .await
            .expect("list unresolved");
        assert!(
            unresolved.is_empty(),
            "successful reprepare should reconcile its git operation"
        );
    }

    #[tokio::test]
    async fn tick_reprepare_of_already_integrated_patch_does_not_leave_running_busy_planned_state()
    {
        let repo = temp_git_repo();
        let seed_commit = head_oid(&repo).await.expect("seed head");
        std::fs::write(repo.join("tracked.txt"), "already integrated").expect("write change");
        git_sync(&repo, &["add", "tracked.txt"]);
        git_sync(&repo, &["commit", "-m", "already integrated"]);
        let source_commit = head_oid(&repo).await.expect("source commit");

        let db_path = std::env::temp_dir().join(format!(
            "ingot-runtime-empty-reprepare-{}.db",
            Uuid::now_v7()
        ));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let dispatcher = JobDispatcher::with_runner(
            db.clone(),
            ProjectLocks::default(),
            DispatcherConfig::new(std::env::temp_dir().join(format!(
                "ingot-runtime-empty-reprepare-state-{}",
                Uuid::now_v7()
            ))),
            Arc::new(FakeRunner),
        );

        let created_at = Utc::now();
        let project = Project {
            id: ingot_domain::ids::ProjectId::new(),
            name: "repo".into(),
            path: repo.display().to_string(),
            default_branch: "main".into(),
            color: "#000".into(),
            created_at,
            updated_at: created_at,
        };
        db.create_project(&project).await.expect("create project");

        let item_id = ingot_domain::ids::ItemId::new();
        let revision_id = ingot_domain::ids::ItemRevisionId::new();
        let item = ingot_domain::item::Item {
            id: item_id,
            project_id: project.id,
            classification: Classification::Change,
            workflow_version: "delivery:v1".into(),
            lifecycle_state: LifecycleState::Open,
            parking_state: ParkingState::Active,
            done_reason: None,
            resolution_source: None,
            approval_state: ApprovalState::Granted,
            escalation_state: EscalationState::None,
            escalation_reason: None,
            current_revision_id: revision_id,
            origin_kind: OriginKind::Manual,
            origin_finding_id: None,
            priority: Priority::Major,
            labels: vec![],
            operator_notes: None,
            created_at,
            updated_at: created_at,
            closed_at: None,
        };
        let revision = ItemRevision {
            id: revision_id,
            item_id,
            revision_no: 1,
            title: "Reprepare already integrated patch".into(),
            description: "reprepare granted head with empty replay".into(),
            acceptance_criteria: "workflow advances without getting stuck".into(),
            target_ref: "refs/heads/main".into(),
            approval_policy: ApprovalPolicy::Required,
            policy_snapshot: serde_json::json!({}),
            template_map_snapshot: serde_json::json!({}),
            seed_commit_oid: seed_commit.clone(),
            seed_target_commit_oid: Some(seed_commit.clone()),
            supersedes_revision_id: None,
            created_at,
        };
        db.create_item_with_revision(&item, &revision)
            .await
            .expect("create item");

        let authoring_workspace = Workspace {
            id: WorkspaceId::new(),
            project_id: project.id,
            kind: WorkspaceKind::Authoring,
            strategy: WorkspaceStrategy::Worktree,
            path: std::env::temp_dir()
                .join(format!(
                    "ingot-runtime-empty-reprepare-authoring-{}",
                    Uuid::now_v7()
                ))
                .display()
                .to_string(),
            created_for_revision_id: Some(revision.id),
            parent_workspace_id: None,
            target_ref: Some("refs/heads/main".into()),
            workspace_ref: Some("refs/ingot/workspaces/empty-reprepare-head".into()),
            base_commit_oid: Some(seed_commit.clone()),
            head_commit_oid: Some(source_commit.clone()),
            retention_policy: RetentionPolicy::Persistent,
            status: WorkspaceStatus::Ready,
            current_job_id: None,
            created_at,
            updated_at: created_at,
        };
        db.create_workspace(&authoring_workspace)
            .await
            .expect("create workspace");

        let author_job = Job {
            id: ingot_domain::ids::JobId::new(),
            project_id: project.id,
            item_id,
            item_revision_id: revision_id,
            step_id: step::AUTHOR_INITIAL.into(),
            semantic_attempt_no: 1,
            retry_no: 0,
            supersedes_job_id: None,
            status: JobStatus::Completed,
            outcome_class: Some(OutcomeClass::Clean),
            phase_kind: PhaseKind::Author,
            workspace_id: Some(authoring_workspace.id),
            workspace_kind: WorkspaceKind::Authoring,
            execution_permission: ExecutionPermission::MayMutate,
            context_policy: ContextPolicy::Fresh,
            phase_template_slug: "author-initial".into(),
            phase_template_digest: None,
            prompt_snapshot: None,
            input_base_commit_oid: Some(seed_commit.clone()),
            input_head_commit_oid: Some(seed_commit.clone()),
            output_artifact_kind: OutputArtifactKind::Commit,
            output_commit_oid: Some(source_commit.clone()),
            result_schema_version: Some("commit_summary:v1".into()),
            result_payload: Some(serde_json::json!({
                "summary": "already integrated",
                "validation": null
            })),
            agent_id: None,
            process_pid: None,
            lease_owner_id: None,
            heartbeat_at: None,
            lease_expires_at: None,
            error_code: None,
            error_message: None,
            created_at,
            started_at: Some(created_at),
            ended_at: Some(created_at),
        };
        db.create_job(&author_job).await.expect("create author job");

        let candidate_validate_job = Job {
            id: ingot_domain::ids::JobId::new(),
            project_id: project.id,
            item_id,
            item_revision_id: revision_id,
            step_id: step::VALIDATE_CANDIDATE_INITIAL.into(),
            semantic_attempt_no: 1,
            retry_no: 0,
            supersedes_job_id: None,
            status: JobStatus::Completed,
            outcome_class: Some(OutcomeClass::Clean),
            phase_kind: PhaseKind::Validate,
            workspace_id: Some(authoring_workspace.id),
            workspace_kind: WorkspaceKind::Authoring,
            execution_permission: ExecutionPermission::MustNotMutate,
            context_policy: ContextPolicy::ResumeContext,
            phase_template_slug: "validate-candidate".into(),
            phase_template_digest: None,
            prompt_snapshot: None,
            input_base_commit_oid: Some(seed_commit.clone()),
            input_head_commit_oid: Some(source_commit.clone()),
            output_artifact_kind: OutputArtifactKind::ValidationReport,
            output_commit_oid: None,
            result_schema_version: Some("validation_report:v1".into()),
            result_payload: Some(serde_json::json!({
                "outcome": "clean",
                "summary": "candidate clean",
                "checks": [],
                "findings": []
            })),
            agent_id: None,
            process_pid: None,
            lease_owner_id: None,
            heartbeat_at: None,
            lease_expires_at: None,
            error_code: None,
            error_message: None,
            created_at,
            started_at: Some(created_at),
            ended_at: Some(created_at),
        };
        db.create_job(&candidate_validate_job)
            .await
            .expect("create candidate validation");
        let stale_validate_job = Job {
            id: ingot_domain::ids::JobId::new(),
            project_id: project.id,
            item_id,
            item_revision_id: revision_id,
            step_id: step::VALIDATE_INTEGRATED.into(),
            semantic_attempt_no: 1,
            retry_no: 0,
            supersedes_job_id: None,
            status: JobStatus::Completed,
            outcome_class: Some(OutcomeClass::Clean),
            phase_kind: PhaseKind::Validate,
            workspace_id: None,
            workspace_kind: WorkspaceKind::Integration,
            execution_permission: ExecutionPermission::MustNotMutate,
            context_policy: ContextPolicy::ResumeContext,
            phase_template_slug: "validate-integrated".into(),
            phase_template_digest: None,
            prompt_snapshot: None,
            input_base_commit_oid: Some(seed_commit.clone()),
            input_head_commit_oid: Some(source_commit.clone()),
            output_artifact_kind: OutputArtifactKind::ValidationReport,
            output_commit_oid: None,
            result_schema_version: Some("validation_report:v1".into()),
            result_payload: Some(serde_json::json!({
                "outcome": "clean",
                "summary": "integrated clean",
                "checks": [],
                "findings": []
            })),
            agent_id: None,
            process_pid: None,
            lease_owner_id: None,
            heartbeat_at: None,
            lease_expires_at: None,
            error_code: None,
            error_message: None,
            created_at,
            started_at: Some(created_at),
            ended_at: Some(created_at + ChronoDuration::seconds(1)),
        };
        db.create_job(&stale_validate_job)
            .await
            .expect("create stale validation");
        db.create_queue_entry(&ConvergenceQueueEntry {
            id: ingot_domain::ids::ConvergenceQueueEntryId::new(),
            project_id: project.id,
            item_id,
            item_revision_id: revision.id,
            target_ref: "refs/heads/main".into(),
            status: ConvergenceQueueEntryStatus::Head,
            head_acquired_at: Some(created_at),
            created_at,
            updated_at: created_at,
            released_at: None,
        })
        .await
        .expect("insert queue entry");

        assert!(
            dispatcher
                .tick()
                .await
                .expect("tick should reprepare cleanly")
        );

        let convergences = db
            .list_convergences_by_item(item.id)
            .await
            .expect("list convergences");
        assert!(
            convergences
                .iter()
                .any(|convergence| convergence.status == ConvergenceStatus::Prepared),
            "reprepare should leave a prepared convergence rather than a running one"
        );
        assert!(
            !convergences
                .iter()
                .any(|convergence| convergence.status == ConvergenceStatus::Running),
            "empty replay must not strand a running convergence"
        );
        let workspaces = db
            .list_workspaces_by_item(item.id)
            .await
            .expect("list workspaces");
        assert!(
            !workspaces.iter().any(|workspace| {
                workspace.kind == WorkspaceKind::Integration
                    && workspace.status == WorkspaceStatus::Busy
            }),
            "empty replay must not strand a busy integration workspace"
        );
        let unresolved = db
            .list_unresolved_git_operations()
            .await
            .expect("list unresolved");
        assert!(
            unresolved.is_empty(),
            "empty replay should not leave a planned convergence git operation behind"
        );
        let jobs = db.list_jobs_by_item(item.id).await.expect("jobs");
        assert!(
            jobs.iter().any(|job| {
                job.step_id == step::VALIDATE_INTEGRATED
                    && job.status == JobStatus::Queued
                    && job.item_revision_id == revision.id
            }),
            "reprepare should queue a fresh integrated validation job"
        );
    }

    #[tokio::test]
    async fn fail_prepare_convergence_attempt_marks_non_conflict_failures_as_step_failed() {
        let repo = temp_git_repo();
        let seed_commit = head_oid(&repo).await.expect("seed head");
        let db_path = std::env::temp_dir().join(format!(
            "ingot-runtime-prepare-failure-classification-{}.db",
            Uuid::now_v7()
        ));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let dispatcher = JobDispatcher::with_runner(
            db.clone(),
            ProjectLocks::default(),
            DispatcherConfig::new(std::env::temp_dir().join(format!(
                "ingot-runtime-prepare-failure-classification-state-{}",
                Uuid::now_v7()
            ))),
            Arc::new(FakeRunner),
        );

        let created_at = Utc::now();
        let project = Project {
            id: ingot_domain::ids::ProjectId::new(),
            name: "repo".into(),
            path: repo.display().to_string(),
            default_branch: "main".into(),
            color: "#000".into(),
            created_at,
            updated_at: created_at,
        };
        db.create_project(&project).await.expect("create project");

        let item_id = ingot_domain::ids::ItemId::new();
        let revision_id = ingot_domain::ids::ItemRevisionId::new();
        let item = ingot_domain::item::Item {
            id: item_id,
            project_id: project.id,
            classification: Classification::Change,
            workflow_version: "delivery:v1".into(),
            lifecycle_state: LifecycleState::Open,
            parking_state: ParkingState::Active,
            done_reason: None,
            resolution_source: None,
            approval_state: ApprovalState::Granted,
            escalation_state: EscalationState::None,
            escalation_reason: None,
            current_revision_id: revision_id,
            origin_kind: OriginKind::Manual,
            origin_finding_id: None,
            priority: Priority::Major,
            labels: vec![],
            operator_notes: None,
            created_at,
            updated_at: created_at,
            closed_at: None,
        };
        let revision = ItemRevision {
            id: revision_id,
            item_id,
            revision_no: 1,
            title: "Prepare failure classification".into(),
            description: "non-conflict prepare failures should escalate as step failures".into(),
            acceptance_criteria: "step failure classification".into(),
            target_ref: "refs/heads/main".into(),
            approval_policy: ApprovalPolicy::Required,
            policy_snapshot: serde_json::json!({}),
            template_map_snapshot: serde_json::json!({}),
            seed_commit_oid: seed_commit.clone(),
            seed_target_commit_oid: Some(seed_commit.clone()),
            supersedes_revision_id: None,
            created_at,
        };
        db.create_item_with_revision(&item, &revision)
            .await
            .expect("create item");

        let mut integration_workspace = Workspace {
            id: WorkspaceId::new(),
            project_id: project.id,
            kind: WorkspaceKind::Integration,
            strategy: WorkspaceStrategy::Worktree,
            path: std::env::temp_dir()
                .join(format!(
                    "ingot-runtime-prepare-failure-workspace-{}",
                    Uuid::now_v7()
                ))
                .display()
                .to_string(),
            created_for_revision_id: Some(revision.id),
            parent_workspace_id: None,
            target_ref: Some("refs/heads/main".into()),
            workspace_ref: Some("refs/ingot/workspaces/prepare-failure".into()),
            base_commit_oid: Some(seed_commit.clone()),
            head_commit_oid: Some(seed_commit.clone()),
            retention_policy: RetentionPolicy::Persistent,
            status: WorkspaceStatus::Busy,
            current_job_id: None,
            created_at,
            updated_at: created_at,
        };
        db.create_workspace(&integration_workspace)
            .await
            .expect("create integration workspace");
        let source_workspace = Workspace {
            id: WorkspaceId::new(),
            project_id: project.id,
            kind: WorkspaceKind::Authoring,
            strategy: WorkspaceStrategy::Worktree,
            path: std::env::temp_dir()
                .join(format!(
                    "ingot-runtime-prepare-failure-source-{}",
                    Uuid::now_v7()
                ))
                .display()
                .to_string(),
            created_for_revision_id: Some(revision.id),
            parent_workspace_id: None,
            target_ref: Some("refs/heads/main".into()),
            workspace_ref: Some("refs/ingot/workspaces/prepare-failure-source".into()),
            base_commit_oid: Some(seed_commit.clone()),
            head_commit_oid: Some(seed_commit.clone()),
            retention_policy: RetentionPolicy::Persistent,
            status: WorkspaceStatus::Ready,
            current_job_id: None,
            created_at,
            updated_at: created_at,
        };
        db.create_workspace(&source_workspace)
            .await
            .expect("create source workspace");

        let mut convergence = Convergence {
            id: ingot_domain::ids::ConvergenceId::new(),
            project_id: project.id,
            item_id,
            item_revision_id: revision_id,
            source_workspace_id: source_workspace.id,
            integration_workspace_id: Some(integration_workspace.id),
            source_head_commit_oid: seed_commit.clone(),
            target_ref: "refs/heads/main".into(),
            strategy: ConvergenceStrategy::RebaseThenFastForward,
            status: ConvergenceStatus::Running,
            input_target_commit_oid: Some(seed_commit.clone()),
            prepared_commit_oid: None,
            final_target_commit_oid: None,
            target_head_valid: Some(true),
            conflict_summary: None,
            created_at,
            completed_at: None,
        };
        db.create_convergence(&convergence)
            .await
            .expect("create convergence");
        let queue_entry = ConvergenceQueueEntry {
            id: ingot_domain::ids::ConvergenceQueueEntryId::new(),
            project_id: project.id,
            item_id,
            item_revision_id: revision.id,
            target_ref: "refs/heads/main".into(),
            status: ConvergenceQueueEntryStatus::Head,
            head_acquired_at: Some(created_at),
            created_at,
            updated_at: created_at,
            released_at: None,
        };
        db.create_queue_entry(&queue_entry)
            .await
            .expect("create queue entry");
        let mut operation = GitOperation {
            id: GitOperationId::new(),
            project_id: project.id,
            operation_kind: OperationKind::PrepareConvergenceCommit,
            entity_type: GitEntityType::Convergence,
            entity_id: convergence.id.to_string(),
            workspace_id: Some(integration_workspace.id),
            ref_name: integration_workspace.workspace_ref.clone(),
            expected_old_oid: Some(seed_commit.clone()),
            new_oid: None,
            commit_oid: None,
            status: GitOperationStatus::Planned,
            metadata: Some(serde_json::json!({
                "source_commit_oids": [seed_commit.clone()],
                "prepared_commit_oids": [],
            })),
            created_at,
            completed_at: None,
        };
        db.create_git_operation(&operation)
            .await
            .expect("create git operation");

        dispatcher
            .fail_prepare_convergence_attempt(
                &project,
                &item,
                &revision,
                &queue_entry,
                &mut integration_workspace,
                &mut convergence,
                &mut operation,
                std::slice::from_ref(&seed_commit),
                &[],
                "non-conflict failure".into(),
                ConvergenceStatus::Failed,
            )
            .await
            .expect("fail prepare attempt");

        let updated_item = db.get_item(item.id).await.expect("item");
        assert_eq!(
            updated_item.escalation_state,
            EscalationState::OperatorRequired
        );
        assert_eq!(
            updated_item.escalation_reason,
            Some(EscalationReason::StepFailed)
        );
        let activity = db
            .list_activity_by_project(project.id, 20, 0)
            .await
            .expect("activity");
        assert!(
            activity.iter().any(|row| {
                row.event_type == ActivityEventType::ItemEscalated
                    && row.payload.get("reason").and_then(|value| value.as_str())
                        == Some("step_failed")
            }),
            "item escalation activity should carry the step_failed reason"
        );
    }

    #[tokio::test]
    async fn tick_times_out_long_running_job_and_marks_it_failed() {
        struct SlowRunner;

        impl AgentRunner for SlowRunner {
            fn launch<'a>(
                &'a self,
                _agent: &'a Agent,
                _request: &'a AgentRequest,
                _working_dir: &'a Path,
            ) -> Pin<Box<dyn Future<Output = Result<AgentResponse, AgentError>> + Send + 'a>>
            {
                Box::pin(async move {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    Ok(AgentResponse {
                        exit_code: 0,
                        stdout: String::new(),
                        stderr: String::new(),
                        result: Some(serde_json::json!({ "summary": "done" })),
                    })
                })
            }
        }

        let repo = temp_git_repo();
        let db_path =
            std::env::temp_dir().join(format!("ingot-runtime-timeout-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let mut config = DispatcherConfig::new(
            std::env::temp_dir().join(format!("ingot-runtime-timeout-state-{}", Uuid::now_v7())),
        );
        config.job_timeout = Duration::from_millis(50);
        config.heartbeat_interval = Duration::from_millis(10);
        let dispatcher = JobDispatcher::with_runner(
            db.clone(),
            ProjectLocks::default(),
            config,
            Arc::new(SlowRunner),
        );

        let created_at = Utc::now();
        let project = Project {
            id: ingot_domain::ids::ProjectId::new(),
            name: "repo".into(),
            path: repo.display().to_string(),
            default_branch: "main".into(),
            color: "#000".into(),
            created_at,
            updated_at: created_at,
        };
        db.create_project(&project).await.expect("create project");

        let agent = Agent {
            id: ingot_domain::ids::AgentId::new(),
            slug: "codex".into(),
            name: "Codex".into(),
            adapter_kind: AdapterKind::Codex,
            provider: "openai".into(),
            model: "gpt-5-codex".into(),
            cli_path: "codex".into(),
            capabilities: vec![
                AgentCapability::MutatingJobs,
                AgentCapability::StructuredOutput,
            ],
            health_check: Some("ok".into()),
            status: AgentStatus::Available,
        };
        db.create_agent(&agent).await.expect("create agent");

        let item_id = ingot_domain::ids::ItemId::new();
        let revision_id = ingot_domain::ids::ItemRevisionId::new();
        let seed_commit = head_oid(&repo).await.expect("seed head");
        let item = ingot_domain::item::Item {
            id: item_id,
            project_id: project.id,
            classification: Classification::Change,
            workflow_version: "delivery:v1".into(),
            lifecycle_state: LifecycleState::Open,
            parking_state: ParkingState::Active,
            done_reason: None,
            resolution_source: None,
            approval_state: ApprovalState::NotRequested,
            escalation_state: EscalationState::None,
            escalation_reason: None,
            current_revision_id: revision_id,
            origin_kind: OriginKind::Manual,
            origin_finding_id: None,
            priority: Priority::Major,
            labels: vec![],
            operator_notes: None,
            created_at,
            updated_at: created_at,
            closed_at: None,
        };
        let seed_commit = head_oid(&repo).await.expect("seed head");
        let revision = ItemRevision {
            id: revision_id,
            item_id,
            revision_no: 1,
            title: "Timeout".into(),
            description: "Timeout test".into(),
            acceptance_criteria: "times out".into(),
            target_ref: "refs/heads/main".into(),
            approval_policy: ApprovalPolicy::Required,
            policy_snapshot: serde_json::json!({}),
            template_map_snapshot: serde_json::json!({ "author_initial": "author-initial" }),
            seed_commit_oid: head_oid(&repo).await.expect("seed head"),
            seed_target_commit_oid: Some(head_oid(&repo).await.expect("seed head")),
            supersedes_revision_id: None,
            created_at,
        };
        db.create_item_with_revision(&item, &revision)
            .await
            .expect("create item");

        let job = Job {
            id: ingot_domain::ids::JobId::new(),
            project_id: project.id,
            item_id,
            item_revision_id: revision_id,
            step_id: "author_initial".into(),
            semantic_attempt_no: 1,
            retry_no: 0,
            supersedes_job_id: None,
            status: JobStatus::Queued,
            outcome_class: None,
            phase_kind: PhaseKind::Author,
            workspace_id: None,
            workspace_kind: WorkspaceKind::Authoring,
            execution_permission: ExecutionPermission::MayMutate,
            context_policy: ContextPolicy::Fresh,
            phase_template_slug: "author-initial".into(),
            phase_template_digest: None,
            prompt_snapshot: None,
            input_base_commit_oid: None,
            input_head_commit_oid: Some(head_oid(&repo).await.expect("seed head")),
            output_artifact_kind: OutputArtifactKind::Commit,
            output_commit_oid: None,
            result_schema_version: None,
            result_payload: None,
            agent_id: None,
            process_pid: None,
            lease_owner_id: None,
            heartbeat_at: None,
            lease_expires_at: None,
            error_code: None,
            error_message: None,
            created_at,
            started_at: None,
            ended_at: None,
        };
        db.create_job(&job).await.expect("create job");

        assert!(dispatcher.tick().await.expect("tick should run"));

        let updated_job = db.get_job(job.id).await.expect("updated job");
        assert_eq!(updated_job.status, JobStatus::Failed);
        assert_eq!(
            updated_job.outcome_class,
            Some(OutcomeClass::TransientFailure)
        );
        assert_eq!(updated_job.error_code.as_deref(), Some("job_timeout"));
    }

    #[tokio::test]
    async fn runtime_terminal_failure_escalates_closure_relevant_item() {
        struct FailingRunner;

        impl AgentRunner for FailingRunner {
            fn launch<'a>(
                &'a self,
                _agent: &'a Agent,
                _request: &'a AgentRequest,
                _working_dir: &'a Path,
            ) -> Pin<Box<dyn Future<Output = Result<AgentResponse, AgentError>> + Send + 'a>>
            {
                Box::pin(async move { Err(AgentError::ProcessError("boom".into())) })
            }
        }

        let repo = temp_git_repo();
        let db_path =
            std::env::temp_dir().join(format!("ingot-runtime-fail-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let dispatcher = JobDispatcher::with_runner(
            db.clone(),
            ProjectLocks::default(),
            DispatcherConfig::new(
                std::env::temp_dir().join(format!("ingot-runtime-fail-state-{}", Uuid::now_v7())),
            ),
            Arc::new(FailingRunner),
        );

        let created_at = Utc::now();
        let project = Project {
            id: ingot_domain::ids::ProjectId::new(),
            name: "repo".into(),
            path: repo.display().to_string(),
            default_branch: "main".into(),
            color: "#000".into(),
            created_at,
            updated_at: created_at,
        };
        db.create_project(&project).await.expect("create project");

        let agent = Agent {
            id: ingot_domain::ids::AgentId::new(),
            slug: "codex".into(),
            name: "Codex".into(),
            adapter_kind: AdapterKind::Codex,
            provider: "openai".into(),
            model: "gpt-5-codex".into(),
            cli_path: "codex".into(),
            capabilities: vec![
                AgentCapability::MutatingJobs,
                AgentCapability::StructuredOutput,
            ],
            health_check: Some("ok".into()),
            status: AgentStatus::Available,
        };
        db.create_agent(&agent).await.expect("create agent");

        let item_id = ingot_domain::ids::ItemId::new();
        let revision_id = ingot_domain::ids::ItemRevisionId::new();
        let seed_commit = head_oid(&repo).await.expect("seed head");
        let item = ingot_domain::item::Item {
            id: item_id,
            project_id: project.id,
            classification: Classification::Change,
            workflow_version: "delivery:v1".into(),
            lifecycle_state: LifecycleState::Open,
            parking_state: ParkingState::Active,
            done_reason: None,
            resolution_source: None,
            approval_state: ApprovalState::NotRequested,
            escalation_state: EscalationState::None,
            escalation_reason: None,
            current_revision_id: revision_id,
            origin_kind: OriginKind::Manual,
            origin_finding_id: None,
            priority: Priority::Major,
            labels: vec![],
            operator_notes: None,
            created_at,
            updated_at: created_at,
            closed_at: None,
        };
        let seed_commit = head_oid(&repo).await.expect("seed head");
        let revision = ItemRevision {
            id: revision_id,
            item_id,
            revision_no: 1,
            title: "Fail".into(),
            description: "Fail".into(),
            acceptance_criteria: "Fail".into(),
            target_ref: "refs/heads/main".into(),
            approval_policy: ApprovalPolicy::Required,
            policy_snapshot: serde_json::json!({}),
            template_map_snapshot: serde_json::json!({ "author_initial": "author-initial" }),
            seed_commit_oid: seed_commit.clone(),
            seed_target_commit_oid: Some(seed_commit.clone()),
            supersedes_revision_id: None,
            created_at,
        };
        db.create_item_with_revision(&item, &revision)
            .await
            .expect("create item");

        let job = Job {
            id: ingot_domain::ids::JobId::new(),
            project_id: project.id,
            item_id,
            item_revision_id: revision_id,
            step_id: "author_initial".into(),
            semantic_attempt_no: 1,
            retry_no: 0,
            supersedes_job_id: None,
            status: JobStatus::Queued,
            outcome_class: None,
            phase_kind: PhaseKind::Author,
            workspace_id: None,
            workspace_kind: WorkspaceKind::Authoring,
            execution_permission: ExecutionPermission::MayMutate,
            context_policy: ContextPolicy::Fresh,
            phase_template_slug: "author-initial".into(),
            phase_template_digest: None,
            prompt_snapshot: None,
            input_base_commit_oid: None,
            input_head_commit_oid: Some(head_oid(&repo).await.expect("seed head")),
            output_artifact_kind: OutputArtifactKind::Commit,
            output_commit_oid: None,
            result_schema_version: None,
            result_payload: None,
            agent_id: None,
            process_pid: None,
            lease_owner_id: None,
            heartbeat_at: None,
            lease_expires_at: None,
            error_code: None,
            error_message: None,
            created_at,
            started_at: None,
            ended_at: None,
        };
        db.create_job(&job).await.expect("create job");

        assert!(dispatcher.tick().await.expect("tick should run"));

        let updated_item = db.get_item(item_id).await.expect("item");
        assert_eq!(
            updated_item.escalation_state,
            EscalationState::OperatorRequired
        );
        assert_eq!(
            updated_item.escalation_reason,
            Some(EscalationReason::StepFailed)
        );
    }

    #[tokio::test]
    async fn successful_authoring_retry_clears_escalation_and_reopens_review_dispatch() {
        let repo = temp_git_repo();
        let db_path =
            std::env::temp_dir().join(format!("ingot-runtime-retry-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let state_root =
            std::env::temp_dir().join(format!("ingot-runtime-retry-state-{}", Uuid::now_v7()));
        let dispatcher = JobDispatcher::with_runner(
            db.clone(),
            ProjectLocks::default(),
            DispatcherConfig::new(state_root),
            Arc::new(FakeRunner),
        );

        let created_at = Utc::now();
        let project = Project {
            id: ingot_domain::ids::ProjectId::new(),
            name: "repo".into(),
            path: repo.display().to_string(),
            default_branch: "main".into(),
            color: "#000".into(),
            created_at,
            updated_at: created_at,
        };
        db.create_project(&project).await.expect("create project");

        let agent = Agent {
            id: ingot_domain::ids::AgentId::new(),
            slug: "codex".into(),
            name: "Codex".into(),
            adapter_kind: AdapterKind::Codex,
            provider: "openai".into(),
            model: "gpt-5-codex".into(),
            cli_path: "codex".into(),
            capabilities: vec![
                AgentCapability::MutatingJobs,
                AgentCapability::StructuredOutput,
            ],
            health_check: Some("ok".into()),
            status: AgentStatus::Available,
        };
        db.create_agent(&agent).await.expect("create agent");

        let item_id = ingot_domain::ids::ItemId::new();
        let revision_id = ingot_domain::ids::ItemRevisionId::new();
        let item = ingot_domain::item::Item {
            id: item_id,
            project_id: project.id,
            classification: Classification::Change,
            workflow_version: "delivery:v1".into(),
            lifecycle_state: LifecycleState::Open,
            parking_state: ParkingState::Active,
            done_reason: None,
            resolution_source: None,
            approval_state: ApprovalState::NotRequested,
            escalation_state: EscalationState::OperatorRequired,
            escalation_reason: Some(EscalationReason::StepFailed),
            current_revision_id: revision_id,
            origin_kind: OriginKind::Manual,
            origin_finding_id: None,
            priority: Priority::Major,
            labels: vec![],
            operator_notes: None,
            created_at,
            updated_at: created_at,
            closed_at: None,
        };
        let revision = ItemRevision {
            id: revision_id,
            item_id,
            revision_no: 1,
            title: "Retry authoring".into(),
            description: "Retry authoring".into(),
            acceptance_criteria: "generated file exists".into(),
            target_ref: "refs/heads/main".into(),
            approval_policy: ApprovalPolicy::Required,
            policy_snapshot: serde_json::json!({}),
            template_map_snapshot: serde_json::json!({ "author_initial": "author-initial" }),
            seed_commit_oid: head_oid(&repo).await.expect("seed head"),
            seed_target_commit_oid: Some(head_oid(&repo).await.expect("seed head")),
            supersedes_revision_id: None,
            created_at,
        };
        db.create_item_with_revision(&item, &revision)
            .await
            .expect("create item");

        let failed_job_id = ingot_domain::ids::JobId::new();
        db.create_job(&Job {
            id: failed_job_id,
            project_id: project.id,
            item_id,
            item_revision_id: revision_id,
            step_id: "author_initial".into(),
            semantic_attempt_no: 1,
            retry_no: 0,
            supersedes_job_id: None,
            status: JobStatus::Failed,
            outcome_class: Some(OutcomeClass::TerminalFailure),
            phase_kind: PhaseKind::Author,
            workspace_id: None,
            workspace_kind: WorkspaceKind::Authoring,
            execution_permission: ExecutionPermission::MayMutate,
            context_policy: ContextPolicy::Fresh,
            phase_template_slug: "author-initial".into(),
            phase_template_digest: None,
            prompt_snapshot: None,
            input_base_commit_oid: None,
            input_head_commit_oid: Some(head_oid(&repo).await.expect("seed head")),
            output_artifact_kind: OutputArtifactKind::Commit,
            output_commit_oid: None,
            result_schema_version: None,
            result_payload: None,
            agent_id: None,
            process_pid: None,
            lease_owner_id: None,
            heartbeat_at: None,
            lease_expires_at: None,
            error_code: Some("step_failed".into()),
            error_message: Some("first attempt failed".into()),
            created_at,
            started_at: Some(created_at),
            ended_at: Some(created_at),
        })
        .await
        .expect("create failed job");

        db.create_job(&Job {
            id: ingot_domain::ids::JobId::new(),
            project_id: project.id,
            item_id,
            item_revision_id: revision_id,
            step_id: "author_initial".into(),
            semantic_attempt_no: 1,
            retry_no: 1,
            supersedes_job_id: Some(failed_job_id),
            status: JobStatus::Queued,
            outcome_class: None,
            phase_kind: PhaseKind::Author,
            workspace_id: None,
            workspace_kind: WorkspaceKind::Authoring,
            execution_permission: ExecutionPermission::MayMutate,
            context_policy: ContextPolicy::Fresh,
            phase_template_slug: "author-initial".into(),
            phase_template_digest: None,
            prompt_snapshot: None,
            input_base_commit_oid: None,
            input_head_commit_oid: Some(head_oid(&repo).await.expect("seed head")),
            output_artifact_kind: OutputArtifactKind::Commit,
            output_commit_oid: None,
            result_schema_version: None,
            result_payload: None,
            agent_id: None,
            process_pid: None,
            lease_owner_id: None,
            heartbeat_at: None,
            lease_expires_at: None,
            error_code: None,
            error_message: None,
            created_at,
            started_at: None,
            ended_at: None,
        })
        .await
        .expect("create retry job");

        assert!(dispatcher.tick().await.expect("tick should run"));

        let updated_item = db.get_item(item_id).await.expect("item");
        assert_eq!(updated_item.escalation_state, EscalationState::None);
        assert_eq!(updated_item.escalation_reason, None);

        let jobs = db.list_jobs_by_item(item_id).await.expect("jobs");
        let evaluation = Evaluator::new().evaluate(&updated_item, &revision, &jobs, &[], &[]);
        assert_eq!(evaluation.dispatchable_step_id, None);
        let review_job = jobs
            .iter()
            .find(|job| job.step_id == step::REVIEW_INCREMENTAL_INITIAL)
            .expect("auto-dispatched review job");
        assert_eq!(review_job.status, JobStatus::Queued);

        let activity = db
            .list_activity_by_project(project.id, 20, 0)
            .await
            .expect("activity");
        assert!(activity.iter().any(|entry| {
            entry.event_type == ActivityEventType::ItemEscalationCleared
                && entry.entity_id == item_id.to_string()
        }));
    }

    #[tokio::test]
    async fn reconcile_startup_expires_stale_running_jobs_and_marks_workspace_stale() {
        let repo = temp_git_repo();
        let db_path =
            std::env::temp_dir().join(format!("ingot-runtime-reconcile-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let dispatcher = JobDispatcher::with_runner(
            db.clone(),
            ProjectLocks::default(),
            DispatcherConfig::new(
                std::env::temp_dir()
                    .join(format!("ingot-runtime-reconcile-state-{}", Uuid::now_v7())),
            ),
            Arc::new(FakeRunner),
        );

        let created_at = Utc::now();
        let project = Project {
            id: ingot_domain::ids::ProjectId::new(),
            name: "repo".into(),
            path: repo.display().to_string(),
            default_branch: "main".into(),
            color: "#000".into(),
            created_at,
            updated_at: created_at,
        };
        db.create_project(&project).await.expect("create project");

        let item_id = ingot_domain::ids::ItemId::new();
        let revision_id = ingot_domain::ids::ItemRevisionId::new();
        let item = ingot_domain::item::Item {
            id: item_id,
            project_id: project.id,
            classification: Classification::Change,
            workflow_version: "delivery:v1".into(),
            lifecycle_state: LifecycleState::Open,
            parking_state: ParkingState::Active,
            done_reason: None,
            resolution_source: None,
            approval_state: ApprovalState::NotRequested,
            escalation_state: EscalationState::None,
            escalation_reason: None,
            current_revision_id: revision_id,
            origin_kind: OriginKind::Manual,
            origin_finding_id: None,
            priority: Priority::Major,
            labels: vec![],
            operator_notes: None,
            created_at,
            updated_at: created_at,
            closed_at: None,
        };
        let seed_commit = head_oid(&repo).await.expect("seed head");
        let revision = ItemRevision {
            id: revision_id,
            item_id,
            revision_no: 1,
            title: "Recover".into(),
            description: "Recover startup".into(),
            acceptance_criteria: "recover".into(),
            target_ref: "refs/heads/main".into(),
            approval_policy: ApprovalPolicy::Required,
            policy_snapshot: serde_json::json!({}),
            template_map_snapshot: serde_json::json!({}),
            seed_commit_oid: seed_commit.clone(),
            seed_target_commit_oid: Some(seed_commit.clone()),
            supersedes_revision_id: None,
            created_at,
        };
        db.create_item_with_revision(&item, &revision)
            .await
            .expect("create item");

        let workspace = Workspace {
            id: WorkspaceId::new(),
            project_id: project.id,
            kind: WorkspaceKind::Authoring,
            strategy: WorkspaceStrategy::Worktree,
            path: std::env::temp_dir()
                .join(format!("ingot-runtime-stale-workspace-{}", Uuid::now_v7()))
                .display()
                .to_string(),
            created_for_revision_id: Some(revision.id),
            parent_workspace_id: None,
            target_ref: Some("refs/heads/main".into()),
            workspace_ref: Some("refs/ingot/workspaces/reconcile".into()),
            base_commit_oid: Some(seed_commit.clone()),
            head_commit_oid: Some(seed_commit.clone()),
            retention_policy: RetentionPolicy::Persistent,
            status: WorkspaceStatus::Busy,
            current_job_id: None,
            created_at,
            updated_at: created_at,
        };
        db.create_workspace(&workspace)
            .await
            .expect("create workspace");

        let stale_job = Job {
            id: ingot_domain::ids::JobId::new(),
            project_id: project.id,
            item_id,
            item_revision_id: revision_id,
            step_id: "author_initial".into(),
            semantic_attempt_no: 1,
            retry_no: 0,
            supersedes_job_id: None,
            status: JobStatus::Running,
            outcome_class: None,
            phase_kind: PhaseKind::Author,
            workspace_id: Some(workspace.id),
            workspace_kind: WorkspaceKind::Authoring,
            execution_permission: ExecutionPermission::MayMutate,
            context_policy: ContextPolicy::Fresh,
            phase_template_slug: "author-initial".into(),
            phase_template_digest: None,
            prompt_snapshot: None,
            input_base_commit_oid: None,
            input_head_commit_oid: Some(seed_commit.clone()),
            output_artifact_kind: OutputArtifactKind::Commit,
            output_commit_oid: None,
            result_schema_version: None,
            result_payload: None,
            agent_id: None,
            process_pid: None,
            lease_owner_id: Some("old-daemon".into()),
            heartbeat_at: Some(created_at),
            lease_expires_at: Some(created_at - ChronoDuration::minutes(1)),
            error_code: None,
            error_message: None,
            created_at,
            started_at: Some(created_at),
            ended_at: None,
        };
        db.create_job(&stale_job).await.expect("create stale job");

        dispatcher
            .reconcile_startup()
            .await
            .expect("reconcile startup");

        let updated_job = db.get_job(stale_job.id).await.expect("updated job");
        assert_eq!(updated_job.status, JobStatus::Expired);
        assert_eq!(
            updated_job.outcome_class,
            Some(OutcomeClass::TransientFailure)
        );
        assert_eq!(updated_job.error_code.as_deref(), Some("heartbeat_expired"));

        let updated_workspace = db.get_workspace(workspace.id).await.expect("workspace");
        assert_eq!(updated_workspace.status, WorkspaceStatus::Stale);
        assert_eq!(updated_workspace.current_job_id, None);
    }

    #[tokio::test]
    async fn reconcile_active_jobs_reports_progress_when_it_expires_a_running_job() {
        let repo = temp_git_repo();
        let db_path = std::env::temp_dir()
            .join(format!("ingot-runtime-reconcile-progress-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let dispatcher = JobDispatcher::with_runner(
            db.clone(),
            ProjectLocks::default(),
            DispatcherConfig::new(
                std::env::temp_dir()
                    .join(format!("ingot-runtime-reconcile-progress-state-{}", Uuid::now_v7())),
            ),
            Arc::new(FakeRunner),
        );

        let created_at = Utc::now();
        let project = Project {
            id: ingot_domain::ids::ProjectId::new(),
            name: "repo".into(),
            path: repo.display().to_string(),
            default_branch: "main".into(),
            color: "#000".into(),
            created_at,
            updated_at: created_at,
        };
        db.create_project(&project).await.expect("create project");

        let item_id = ingot_domain::ids::ItemId::new();
        let revision_id = ingot_domain::ids::ItemRevisionId::new();
        let item = ingot_domain::item::Item {
            id: item_id,
            project_id: project.id,
            classification: Classification::Change,
            workflow_version: "delivery:v1".into(),
            lifecycle_state: LifecycleState::Open,
            parking_state: ParkingState::Active,
            done_reason: None,
            resolution_source: None,
            approval_state: ApprovalState::NotRequested,
            escalation_state: EscalationState::None,
            escalation_reason: None,
            current_revision_id: revision_id,
            origin_kind: OriginKind::Manual,
            origin_finding_id: None,
            priority: Priority::Major,
            labels: vec![],
            operator_notes: None,
            created_at,
            updated_at: created_at,
            closed_at: None,
        };
        let seed_commit = head_oid(&repo).await.expect("seed head");
        let revision = ItemRevision {
            id: revision_id,
            item_id,
            revision_no: 1,
            title: "Recover".into(),
            description: "Recover job".into(),
            acceptance_criteria: "recover".into(),
            target_ref: "refs/heads/main".into(),
            approval_policy: ApprovalPolicy::Required,
            policy_snapshot: serde_json::json!({}),
            template_map_snapshot: serde_json::json!({}),
            seed_commit_oid: seed_commit.clone(),
            seed_target_commit_oid: Some(seed_commit.clone()),
            supersedes_revision_id: None,
            created_at,
        };
        db.create_item_with_revision(&item, &revision)
            .await
            .expect("create item");

        let workspace = Workspace {
            id: WorkspaceId::new(),
            project_id: project.id,
            kind: WorkspaceKind::Authoring,
            strategy: WorkspaceStrategy::Worktree,
            path: std::env::temp_dir()
                .join(format!("ingot-runtime-progress-workspace-{}", Uuid::now_v7()))
                .display()
                .to_string(),
            created_for_revision_id: Some(revision.id),
            parent_workspace_id: None,
            target_ref: Some("refs/heads/main".into()),
            workspace_ref: Some("refs/ingot/workspaces/progress".into()),
            base_commit_oid: Some(seed_commit.clone()),
            head_commit_oid: Some(seed_commit.clone()),
            retention_policy: RetentionPolicy::Persistent,
            status: WorkspaceStatus::Busy,
            current_job_id: None,
            created_at,
            updated_at: created_at,
        };
        db.create_workspace(&workspace)
            .await
            .expect("create workspace");

        let stale_job = Job {
            id: ingot_domain::ids::JobId::new(),
            project_id: project.id,
            item_id,
            item_revision_id: revision_id,
            step_id: "author_initial".into(),
            semantic_attempt_no: 1,
            retry_no: 0,
            supersedes_job_id: None,
            status: JobStatus::Running,
            outcome_class: None,
            phase_kind: PhaseKind::Author,
            workspace_id: Some(workspace.id),
            workspace_kind: WorkspaceKind::Authoring,
            execution_permission: ExecutionPermission::MayMutate,
            context_policy: ContextPolicy::Fresh,
            phase_template_slug: "author-initial".into(),
            phase_template_digest: None,
            prompt_snapshot: None,
            input_base_commit_oid: None,
            input_head_commit_oid: Some(seed_commit.clone()),
            output_artifact_kind: OutputArtifactKind::Commit,
            output_commit_oid: None,
            result_schema_version: None,
            result_payload: None,
            agent_id: None,
            process_pid: None,
            lease_owner_id: Some("old-daemon".into()),
            heartbeat_at: Some(created_at),
            lease_expires_at: Some(created_at - ChronoDuration::minutes(1)),
            error_code: None,
            error_message: None,
            created_at,
            started_at: Some(created_at),
            ended_at: None,
        };
        db.create_job(&stale_job).await.expect("create stale job");

        let made_progress = dispatcher
            .reconcile_active_jobs()
            .await
            .expect("reconcile active jobs");

        assert!(made_progress);
        let updated_job = db.get_job(stale_job.id).await.expect("updated job");
        assert_eq!(updated_job.status, JobStatus::Expired);
    }

    #[tokio::test]
    async fn reconcile_startup_fails_inflight_convergences_and_marks_workspace_stale() {
        let repo = temp_git_repo();
        let db_path = std::env::temp_dir().join(format!(
            "ingot-runtime-conv-reconcile-{}.db",
            Uuid::now_v7()
        ));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let dispatcher = JobDispatcher::with_runner(
            db.clone(),
            ProjectLocks::default(),
            DispatcherConfig::new(std::env::temp_dir().join(format!(
                "ingot-runtime-conv-reconcile-state-{}",
                Uuid::now_v7()
            ))),
            Arc::new(FakeRunner),
        );

        let created_at = Utc::now();
        let project = Project {
            id: ingot_domain::ids::ProjectId::new(),
            name: "repo".into(),
            path: repo.display().to_string(),
            default_branch: "main".into(),
            color: "#000".into(),
            created_at,
            updated_at: created_at,
        };
        db.create_project(&project).await.expect("create project");

        let item_id = ingot_domain::ids::ItemId::new();
        let revision_id = ingot_domain::ids::ItemRevisionId::new();
        let item = ingot_domain::item::Item {
            id: item_id,
            project_id: project.id,
            classification: Classification::Change,
            workflow_version: "delivery:v1".into(),
            lifecycle_state: LifecycleState::Open,
            parking_state: ParkingState::Active,
            done_reason: None,
            resolution_source: None,
            approval_state: ApprovalState::NotRequested,
            escalation_state: EscalationState::None,
            escalation_reason: None,
            current_revision_id: revision_id,
            origin_kind: OriginKind::Manual,
            origin_finding_id: None,
            priority: Priority::Major,
            labels: vec![],
            operator_notes: None,
            created_at,
            updated_at: created_at,
            closed_at: None,
        };
        let seed_commit = head_oid(&repo).await.expect("seed head");
        let revision = ItemRevision {
            id: revision_id,
            item_id,
            revision_no: 1,
            title: "Recover convergence".into(),
            description: "Recover convergence".into(),
            acceptance_criteria: "recover".into(),
            target_ref: "refs/heads/main".into(),
            approval_policy: ApprovalPolicy::Required,
            policy_snapshot: serde_json::json!({}),
            template_map_snapshot: serde_json::json!({}),
            seed_commit_oid: seed_commit.clone(),
            seed_target_commit_oid: Some(seed_commit.clone()),
            supersedes_revision_id: None,
            created_at,
        };
        db.create_item_with_revision(&item, &revision)
            .await
            .expect("create item");

        let workspace = Workspace {
            id: WorkspaceId::new(),
            project_id: project.id,
            kind: WorkspaceKind::Integration,
            strategy: WorkspaceStrategy::Worktree,
            path: std::env::temp_dir()
                .join(format!("ingot-runtime-conv-workspace-{}", Uuid::now_v7()))
                .display()
                .to_string(),
            created_for_revision_id: Some(revision.id),
            parent_workspace_id: None,
            target_ref: Some("refs/heads/main".into()),
            workspace_ref: Some("refs/ingot/workspaces/reconcile-conv".into()),
            base_commit_oid: Some(seed_commit.clone()),
            head_commit_oid: Some(seed_commit.clone()),
            retention_policy: RetentionPolicy::Persistent,
            status: WorkspaceStatus::Busy,
            current_job_id: None,
            created_at,
            updated_at: created_at,
        };
        db.create_workspace(&workspace)
            .await
            .expect("create workspace");

        let convergence = Convergence {
            id: ingot_domain::ids::ConvergenceId::new(),
            project_id: project.id,
            item_id,
            item_revision_id: revision_id,
            source_workspace_id: workspace.id,
            integration_workspace_id: Some(workspace.id),
            source_head_commit_oid: seed_commit.clone(),
            target_ref: "refs/heads/main".into(),
            strategy: ConvergenceStrategy::RebaseThenFastForward,
            status: ConvergenceStatus::Running,
            input_target_commit_oid: Some(seed_commit.clone()),
            prepared_commit_oid: None,
            final_target_commit_oid: None,
            target_head_valid: None,
            conflict_summary: None,
            created_at,
            completed_at: None,
        };
        db.create_convergence(&convergence)
            .await
            .expect("create convergence");

        dispatcher
            .reconcile_startup()
            .await
            .expect("reconcile startup");

        let updated_convergence = db
            .list_convergences_by_item(item.id)
            .await
            .expect("list convergences")
            .into_iter()
            .next()
            .expect("convergence");
        assert_eq!(updated_convergence.status, ConvergenceStatus::Failed);
        assert_eq!(
            updated_convergence.conflict_summary.as_deref(),
            Some("startup_recovery_required")
        );

        let updated_workspace = db.get_workspace(workspace.id).await.expect("workspace");
        assert_eq!(updated_workspace.status, WorkspaceStatus::Stale);
    }

    #[tokio::test]
    async fn reconcile_startup_marks_finalized_target_ref_git_operation_reconciled() {
        let repo = temp_git_repo();
        let base_commit = head_oid(&repo).await.expect("base head");
        std::fs::write(repo.join("tracked.txt"), "next").expect("write file");
        git_sync(&repo, &["add", "tracked.txt"]);
        git_sync(&repo, &["commit", "-m", "next"]);
        let new_head = head_oid(&repo).await.expect("new head");

        let db_path = std::env::temp_dir().join(format!("ingot-runtime-gop-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let dispatcher = JobDispatcher::with_runner(
            db.clone(),
            ProjectLocks::default(),
            DispatcherConfig::new(
                std::env::temp_dir().join(format!("ingot-runtime-gop-state-{}", Uuid::now_v7())),
            ),
            Arc::new(FakeRunner),
        );

        let created_at = Utc::now();
        let project = Project {
            id: ingot_domain::ids::ProjectId::new(),
            name: "repo".into(),
            path: repo.display().to_string(),
            default_branch: "main".into(),
            color: "#000".into(),
            created_at,
            updated_at: created_at,
        };
        db.create_project(&project).await.expect("create project");

        let item_id = ingot_domain::ids::ItemId::new();
        let revision_id = ingot_domain::ids::ItemRevisionId::new();
        let item = ingot_domain::item::Item {
            id: item_id,
            project_id: project.id,
            classification: Classification::Change,
            workflow_version: "delivery:v1".into(),
            lifecycle_state: LifecycleState::Open,
            parking_state: ParkingState::Active,
            done_reason: None,
            resolution_source: None,
            approval_state: ApprovalState::NotRequested,
            escalation_state: EscalationState::None,
            escalation_reason: None,
            current_revision_id: revision_id,
            origin_kind: OriginKind::Manual,
            origin_finding_id: None,
            priority: Priority::Major,
            labels: vec![],
            operator_notes: None,
            created_at,
            updated_at: created_at,
            closed_at: None,
        };
        let revision = ItemRevision {
            id: revision_id,
            item_id,
            revision_no: 1,
            title: "Finalize adopt".into(),
            description: "adopt finalize".into(),
            acceptance_criteria: "finalized".into(),
            target_ref: "refs/heads/main".into(),
            approval_policy: ApprovalPolicy::Required,
            policy_snapshot: serde_json::json!({}),
            template_map_snapshot: serde_json::json!({}),
            seed_commit_oid: base_commit.clone(),
            seed_target_commit_oid: Some(base_commit.clone()),
            supersedes_revision_id: None,
            created_at,
        };
        db.create_item_with_revision(&item, &revision)
            .await
            .expect("create item");

        let workspace = Workspace {
            id: WorkspaceId::new(),
            project_id: project.id,
            kind: WorkspaceKind::Integration,
            strategy: WorkspaceStrategy::Worktree,
            path: repo.display().to_string(),
            created_for_revision_id: Some(revision.id),
            parent_workspace_id: None,
            target_ref: Some("refs/heads/main".into()),
            workspace_ref: Some("refs/ingot/workspaces/finalize-adopt".into()),
            base_commit_oid: Some(base_commit.clone()),
            head_commit_oid: Some(new_head.clone()),
            retention_policy: RetentionPolicy::Persistent,
            status: WorkspaceStatus::Ready,
            current_job_id: None,
            created_at,
            updated_at: created_at,
        };
        db.create_workspace(&workspace)
            .await
            .expect("create workspace");

        let convergence = Convergence {
            id: ingot_domain::ids::ConvergenceId::new(),
            project_id: project.id,
            item_id,
            item_revision_id: revision_id,
            source_workspace_id: workspace.id,
            integration_workspace_id: None,
            source_head_commit_oid: new_head.clone(),
            target_ref: "refs/heads/main".into(),
            strategy: ConvergenceStrategy::RebaseThenFastForward,
            status: ConvergenceStatus::Prepared,
            input_target_commit_oid: Some(base_commit.clone()),
            prepared_commit_oid: Some(new_head.clone()),
            final_target_commit_oid: None,
            target_head_valid: Some(true),
            conflict_summary: None,
            created_at,
            completed_at: None,
        };
        db.create_convergence(&convergence)
            .await
            .expect("create convergence");

        let operation = GitOperation {
            id: GitOperationId::new(),
            project_id: project.id,
            operation_kind: OperationKind::FinalizeTargetRef,
            entity_type: GitEntityType::Convergence,
            entity_id: convergence.id.to_string(),
            workspace_id: None,
            ref_name: Some("refs/heads/main".into()),
            expected_old_oid: Some(base_commit.clone()),
            new_oid: Some(new_head.clone()),
            commit_oid: Some(new_head),
            status: GitOperationStatus::Applied,
            metadata: None,
            created_at,
            completed_at: None,
        };
        db.create_git_operation(&operation)
            .await
            .expect("create git operation");

        dispatcher
            .reconcile_startup()
            .await
            .expect("reconcile startup");

        let operations = db
            .list_unresolved_git_operations()
            .await
            .expect("list unresolved");
        assert!(operations.is_empty(), "operation should be reconciled");

        let updated_convergence = db
            .get_convergence(convergence.id)
            .await
            .expect("convergence");
        assert_eq!(updated_convergence.status, ConvergenceStatus::Finalized);
        assert_eq!(
            updated_convergence.final_target_commit_oid.as_deref(),
            Some(
                operation
                    .commit_oid
                    .as_deref()
                    .expect("operation commit oid")
            )
        );

        let updated_item = db.get_item(item.id).await.expect("item");
        assert_eq!(updated_item.lifecycle_state, LifecycleState::Done);
        assert_eq!(updated_item.done_reason, Some(DoneReason::Completed));
        assert_eq!(updated_item.approval_state, ApprovalState::Approved);
        assert_eq!(
            updated_item.resolution_source,
            Some(ResolutionSource::ApprovalCommand)
        );

        let activity = db
            .list_activity_by_project(project.id, 10, 0)
            .await
            .expect("list activity");
        assert!(
            activity
                .iter()
                .any(|row| row.event_type == ActivityEventType::GitOperationReconciled)
        );
    }

    #[tokio::test]
    async fn reconcile_startup_syncs_checkout_before_adopting_finalize() {
        let repo = temp_git_repo();
        let base_commit = head_oid(&repo).await.expect("base head");
        std::fs::write(repo.join("tracked.txt"), "prepared").expect("write prepared");
        git_sync(&repo, &["add", "tracked.txt"]);
        git_sync(&repo, &["commit", "-m", "prepared"]);
        let prepared_commit = head_oid(&repo).await.expect("prepared head");
        git_sync(
            &repo,
            &[
                "update-ref",
                "refs/ingot/tests/finalize-prepared",
                &prepared_commit,
            ],
        );
        git_sync(&repo, &["reset", "--hard", &base_commit]);

        let db_path =
            std::env::temp_dir().join(format!("ingot-runtime-finalize-sync-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let dispatcher = JobDispatcher::with_runner(
            db.clone(),
            ProjectLocks::default(),
            DispatcherConfig::new(std::env::temp_dir().join(format!(
                "ingot-runtime-finalize-sync-state-{}",
                Uuid::now_v7()
            ))),
            Arc::new(FakeRunner),
        );

        let created_at = Utc::now();
        let project = Project {
            id: ingot_domain::ids::ProjectId::new(),
            name: "repo".into(),
            path: repo.display().to_string(),
            default_branch: "main".into(),
            color: "#000".into(),
            created_at,
            updated_at: created_at,
        };
        db.create_project(&project).await.expect("create project");

        let item_id = ingot_domain::ids::ItemId::new();
        let revision_id = ingot_domain::ids::ItemRevisionId::new();
        let item = ingot_domain::item::Item {
            id: item_id,
            project_id: project.id,
            classification: Classification::Change,
            workflow_version: "delivery:v1".into(),
            lifecycle_state: LifecycleState::Open,
            parking_state: ParkingState::Active,
            done_reason: None,
            resolution_source: None,
            approval_state: ApprovalState::Granted,
            escalation_state: EscalationState::None,
            escalation_reason: None,
            current_revision_id: revision_id,
            origin_kind: OriginKind::Manual,
            origin_finding_id: None,
            priority: Priority::Major,
            labels: vec![],
            operator_notes: None,
            created_at,
            updated_at: created_at,
            closed_at: None,
        };
        let revision = ItemRevision {
            id: revision_id,
            item_id,
            revision_no: 1,
            title: "Finalize resume".into(),
            description: "resume".into(),
            acceptance_criteria: "sync checkout".into(),
            target_ref: "refs/heads/main".into(),
            approval_policy: ApprovalPolicy::Required,
            policy_snapshot: serde_json::json!({}),
            template_map_snapshot: serde_json::json!({}),
            seed_commit_oid: base_commit.clone(),
            seed_target_commit_oid: Some(base_commit.clone()),
            supersedes_revision_id: None,
            created_at,
        };
        db.create_item_with_revision(&item, &revision)
            .await
            .expect("create item");
        let source_workspace = Workspace {
            id: WorkspaceId::new(),
            project_id: project.id,
            kind: WorkspaceKind::Authoring,
            strategy: WorkspaceStrategy::Worktree,
            path: std::env::temp_dir()
                .join(format!("ingot-runtime-finalize-source-{}", Uuid::now_v7()))
                .display()
                .to_string(),
            created_for_revision_id: Some(revision.id),
            parent_workspace_id: None,
            target_ref: Some("refs/heads/main".into()),
            workspace_ref: Some("refs/ingot/workspaces/finalize-source".into()),
            base_commit_oid: Some(base_commit.clone()),
            head_commit_oid: Some(prepared_commit.clone()),
            retention_policy: RetentionPolicy::Persistent,
            status: WorkspaceStatus::Ready,
            current_job_id: None,
            created_at,
            updated_at: created_at,
        };
        db.create_workspace(&source_workspace)
            .await
            .expect("create source workspace");

        let convergence = Convergence {
            id: ingot_domain::ids::ConvergenceId::new(),
            project_id: project.id,
            item_id,
            item_revision_id: revision_id,
            source_workspace_id: source_workspace.id,
            integration_workspace_id: None,
            source_head_commit_oid: prepared_commit.clone(),
            target_ref: "refs/heads/main".into(),
            strategy: ConvergenceStrategy::RebaseThenFastForward,
            status: ConvergenceStatus::Prepared,
            input_target_commit_oid: Some(base_commit.clone()),
            prepared_commit_oid: Some(prepared_commit.clone()),
            final_target_commit_oid: None,
            target_head_valid: Some(true),
            conflict_summary: None,
            created_at,
            completed_at: None,
        };
        db.create_convergence(&convergence)
            .await
            .expect("create convergence");
        db.create_queue_entry(&ConvergenceQueueEntry {
            id: ingot_domain::ids::ConvergenceQueueEntryId::new(),
            project_id: project.id,
            item_id,
            item_revision_id: revision_id,
            target_ref: "refs/heads/main".into(),
            status: ConvergenceQueueEntryStatus::Head,
            head_acquired_at: Some(created_at),
            created_at,
            updated_at: created_at,
            released_at: None,
        })
        .await
        .expect("insert queue entry");

        let paths = dispatcher
            .refresh_project_mirror(&project)
            .await
            .expect("refresh mirror");
        compare_and_swap_ref(
            paths.mirror_git_dir.as_path(),
            "refs/heads/main",
            &prepared_commit,
            &base_commit,
        )
        .await
        .expect("move mirror ref");

        let operation = GitOperation {
            id: GitOperationId::new(),
            project_id: project.id,
            operation_kind: OperationKind::FinalizeTargetRef,
            entity_type: GitEntityType::Convergence,
            entity_id: convergence.id.to_string(),
            workspace_id: None,
            ref_name: Some("refs/heads/main".into()),
            expected_old_oid: Some(base_commit.clone()),
            new_oid: Some(prepared_commit.clone()),
            commit_oid: Some(prepared_commit.clone()),
            status: GitOperationStatus::Applied,
            metadata: None,
            created_at,
            completed_at: Some(created_at),
        };
        db.create_git_operation(&operation)
            .await
            .expect("create git operation");

        assert_eq!(head_oid(&repo).await.expect("checkout head"), base_commit);

        dispatcher
            .reconcile_startup()
            .await
            .expect("reconcile startup");

        assert_eq!(head_oid(&repo).await.expect("synced head"), prepared_commit);
        let unresolved = db
            .list_unresolved_git_operations()
            .await
            .expect("list unresolved");
        assert!(unresolved.is_empty(), "finalize op should reconcile");
        let updated_item = db.get_item(item.id).await.expect("item");
        assert_eq!(updated_item.lifecycle_state, LifecycleState::Done);
        let queue_entries = db
            .list_queue_entries_by_item(item.id)
            .await
            .expect("list queue entries");
        assert_eq!(
            queue_entries[0].status,
            ConvergenceQueueEntryStatus::Released
        );
    }

    #[tokio::test]
    async fn reconcile_startup_leaves_finalize_open_when_checkout_sync_is_blocked() {
        let repo = temp_git_repo();
        let base_commit = head_oid(&repo).await.expect("base head");
        std::fs::write(repo.join("tracked.txt"), "prepared").expect("write prepared");
        git_sync(&repo, &["add", "tracked.txt"]);
        git_sync(&repo, &["commit", "-m", "prepared"]);
        let prepared_commit = head_oid(&repo).await.expect("prepared head");
        git_sync(
            &repo,
            &[
                "update-ref",
                "refs/ingot/tests/finalize-prepared",
                &prepared_commit,
            ],
        );
        git_sync(&repo, &["reset", "--hard", &base_commit]);
        git_sync(&repo, &["checkout", "-b", "feature"]);

        let db_path = std::env::temp_dir().join(format!(
            "ingot-runtime-finalize-blocked-{}.db",
            Uuid::now_v7()
        ));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let dispatcher = JobDispatcher::with_runner(
            db.clone(),
            ProjectLocks::default(),
            DispatcherConfig::new(std::env::temp_dir().join(format!(
                "ingot-runtime-finalize-blocked-state-{}",
                Uuid::now_v7()
            ))),
            Arc::new(FakeRunner),
        );

        let created_at = Utc::now();
        let project = Project {
            id: ingot_domain::ids::ProjectId::new(),
            name: "repo".into(),
            path: repo.display().to_string(),
            default_branch: "main".into(),
            color: "#000".into(),
            created_at,
            updated_at: created_at,
        };
        db.create_project(&project).await.expect("create project");

        let item_id = ingot_domain::ids::ItemId::new();
        let revision_id = ingot_domain::ids::ItemRevisionId::new();
        let item = ingot_domain::item::Item {
            id: item_id,
            project_id: project.id,
            classification: Classification::Change,
            workflow_version: "delivery:v1".into(),
            lifecycle_state: LifecycleState::Open,
            parking_state: ParkingState::Active,
            done_reason: None,
            resolution_source: None,
            approval_state: ApprovalState::Granted,
            escalation_state: EscalationState::None,
            escalation_reason: None,
            current_revision_id: revision_id,
            origin_kind: OriginKind::Manual,
            origin_finding_id: None,
            priority: Priority::Major,
            labels: vec![],
            operator_notes: None,
            created_at,
            updated_at: created_at,
            closed_at: None,
        };
        let revision = ItemRevision {
            id: revision_id,
            item_id,
            revision_no: 1,
            title: "Finalize blocked".into(),
            description: "blocked".into(),
            acceptance_criteria: "wait for operator".into(),
            target_ref: "refs/heads/main".into(),
            approval_policy: ApprovalPolicy::Required,
            policy_snapshot: serde_json::json!({}),
            template_map_snapshot: serde_json::json!({}),
            seed_commit_oid: base_commit.clone(),
            seed_target_commit_oid: Some(base_commit.clone()),
            supersedes_revision_id: None,
            created_at,
        };
        db.create_item_with_revision(&item, &revision)
            .await
            .expect("create item");
        let source_workspace = Workspace {
            id: WorkspaceId::new(),
            project_id: project.id,
            kind: WorkspaceKind::Authoring,
            strategy: WorkspaceStrategy::Worktree,
            path: std::env::temp_dir()
                .join(format!(
                    "ingot-runtime-finalize-blocked-source-{}",
                    Uuid::now_v7()
                ))
                .display()
                .to_string(),
            created_for_revision_id: Some(revision.id),
            parent_workspace_id: None,
            target_ref: Some("refs/heads/main".into()),
            workspace_ref: Some("refs/ingot/workspaces/finalize-blocked-source".into()),
            base_commit_oid: Some(base_commit.clone()),
            head_commit_oid: Some(prepared_commit.clone()),
            retention_policy: RetentionPolicy::Persistent,
            status: WorkspaceStatus::Ready,
            current_job_id: None,
            created_at,
            updated_at: created_at,
        };
        db.create_workspace(&source_workspace)
            .await
            .expect("create source workspace");

        let convergence = Convergence {
            id: ingot_domain::ids::ConvergenceId::new(),
            project_id: project.id,
            item_id,
            item_revision_id: revision_id,
            source_workspace_id: source_workspace.id,
            integration_workspace_id: None,
            source_head_commit_oid: prepared_commit.clone(),
            target_ref: "refs/heads/main".into(),
            strategy: ConvergenceStrategy::RebaseThenFastForward,
            status: ConvergenceStatus::Prepared,
            input_target_commit_oid: Some(base_commit.clone()),
            prepared_commit_oid: Some(prepared_commit.clone()),
            final_target_commit_oid: None,
            target_head_valid: Some(true),
            conflict_summary: None,
            created_at,
            completed_at: None,
        };
        db.create_convergence(&convergence)
            .await
            .expect("create convergence");
        db.create_queue_entry(&ConvergenceQueueEntry {
            id: ingot_domain::ids::ConvergenceQueueEntryId::new(),
            project_id: project.id,
            item_id,
            item_revision_id: revision_id,
            target_ref: "refs/heads/main".into(),
            status: ConvergenceQueueEntryStatus::Head,
            head_acquired_at: Some(created_at),
            created_at,
            updated_at: created_at,
            released_at: None,
        })
        .await
        .expect("insert queue entry");

        let paths = dispatcher
            .refresh_project_mirror(&project)
            .await
            .expect("refresh mirror");
        compare_and_swap_ref(
            paths.mirror_git_dir.as_path(),
            "refs/heads/main",
            &prepared_commit,
            &base_commit,
        )
        .await
        .expect("move mirror ref");

        let operation = GitOperation {
            id: GitOperationId::new(),
            project_id: project.id,
            operation_kind: OperationKind::FinalizeTargetRef,
            entity_type: GitEntityType::Convergence,
            entity_id: convergence.id.to_string(),
            workspace_id: None,
            ref_name: Some("refs/heads/main".into()),
            expected_old_oid: Some(base_commit),
            new_oid: Some(prepared_commit),
            commit_oid: Some(
                convergence
                    .prepared_commit_oid
                    .clone()
                    .expect("prepared oid"),
            ),
            status: GitOperationStatus::Applied,
            metadata: None,
            created_at,
            completed_at: Some(created_at),
        };
        db.create_git_operation(&operation)
            .await
            .expect("create git operation");

        dispatcher
            .reconcile_startup()
            .await
            .expect("reconcile startup");

        let unresolved = db
            .list_unresolved_git_operations()
            .await
            .expect("list unresolved");
        assert_eq!(
            unresolved.len(),
            1,
            "blocked finalize should stay unresolved"
        );
        let updated_item = db.get_item(item.id).await.expect("item");
        assert_eq!(updated_item.lifecycle_state, LifecycleState::Open);
        assert_eq!(
            updated_item.escalation_reason,
            Some(EscalationReason::CheckoutSyncBlocked)
        );
        let updated_convergence = db
            .get_convergence(convergence.id)
            .await
            .expect("convergence");
        assert_eq!(updated_convergence.status, ConvergenceStatus::Prepared);
        let queue_entries = db
            .list_queue_entries_by_item(item.id)
            .await
            .expect("list queue entries");
        assert_eq!(queue_entries[0].status, ConvergenceQueueEntryStatus::Head);
    }

    #[tokio::test]
    async fn reconcile_startup_adopts_prepared_convergence_from_git_operation() {
        let repo = temp_git_repo();
        let base_commit = head_oid(&repo).await.expect("base head");
        std::fs::write(repo.join("tracked.txt"), "prepared").expect("write file");
        git_sync(&repo, &["add", "tracked.txt"]);
        git_sync(&repo, &["commit", "-m", "prepared"]);
        let prepared_head = head_oid(&repo).await.expect("prepared head");
        git_sync(
            &repo,
            &[
                "update-ref",
                "refs/ingot/workspaces/prepare-adopt",
                &prepared_head,
            ],
        );
        git_sync(&repo, &["reset", "--hard", &base_commit]);

        let db_path =
            std::env::temp_dir().join(format!("ingot-runtime-prepare-adopt-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let dispatcher = JobDispatcher::with_runner(
            db.clone(),
            ProjectLocks::default(),
            DispatcherConfig::new(std::env::temp_dir().join(format!(
                "ingot-runtime-prepare-adopt-state-{}",
                Uuid::now_v7()
            ))),
            Arc::new(FakeRunner),
        );

        let created_at = Utc::now();
        let project = Project {
            id: ingot_domain::ids::ProjectId::new(),
            name: "repo".into(),
            path: repo.display().to_string(),
            default_branch: "main".into(),
            color: "#000".into(),
            created_at,
            updated_at: created_at,
        };
        db.create_project(&project).await.expect("create project");

        let item_id = ingot_domain::ids::ItemId::new();
        let revision_id = ingot_domain::ids::ItemRevisionId::new();
        let item = ingot_domain::item::Item {
            id: item_id,
            project_id: project.id,
            classification: Classification::Change,
            workflow_version: "delivery:v1".into(),
            lifecycle_state: LifecycleState::Open,
            parking_state: ParkingState::Active,
            done_reason: None,
            resolution_source: None,
            approval_state: ApprovalState::NotRequested,
            escalation_state: EscalationState::None,
            escalation_reason: None,
            current_revision_id: revision_id,
            origin_kind: OriginKind::Manual,
            origin_finding_id: None,
            priority: Priority::Major,
            labels: vec![],
            operator_notes: None,
            created_at,
            updated_at: created_at,
            closed_at: None,
        };
        let revision = ItemRevision {
            id: revision_id,
            item_id,
            revision_no: 1,
            title: "Prepare adopt".into(),
            description: "adopt prepare".into(),
            acceptance_criteria: "prepared".into(),
            target_ref: "refs/heads/main".into(),
            approval_policy: ApprovalPolicy::Required,
            policy_snapshot: serde_json::json!({}),
            template_map_snapshot: serde_json::json!({}),
            seed_commit_oid: base_commit.clone(),
            seed_target_commit_oid: Some(base_commit.clone()),
            supersedes_revision_id: None,
            created_at,
        };
        db.create_item_with_revision(&item, &revision)
            .await
            .expect("create item");

        let workspace = Workspace {
            id: WorkspaceId::new(),
            project_id: project.id,
            kind: WorkspaceKind::Integration,
            strategy: WorkspaceStrategy::Worktree,
            path: repo.display().to_string(),
            created_for_revision_id: Some(revision.id),
            parent_workspace_id: None,
            target_ref: Some("refs/heads/main".into()),
            workspace_ref: Some("refs/ingot/workspaces/prepare-adopt".into()),
            base_commit_oid: Some(base_commit.clone()),
            head_commit_oid: Some(base_commit.clone()),
            retention_policy: RetentionPolicy::Persistent,
            status: WorkspaceStatus::Busy,
            current_job_id: None,
            created_at,
            updated_at: created_at,
        };
        db.create_workspace(&workspace)
            .await
            .expect("create workspace");

        let convergence = Convergence {
            id: ingot_domain::ids::ConvergenceId::new(),
            project_id: project.id,
            item_id,
            item_revision_id: revision_id,
            source_workspace_id: workspace.id,
            integration_workspace_id: Some(workspace.id),
            source_head_commit_oid: prepared_head.clone(),
            target_ref: "refs/heads/main".into(),
            strategy: ConvergenceStrategy::RebaseThenFastForward,
            status: ConvergenceStatus::Running,
            input_target_commit_oid: Some(base_commit),
            prepared_commit_oid: None,
            final_target_commit_oid: None,
            target_head_valid: None,
            conflict_summary: None,
            created_at,
            completed_at: None,
        };
        db.create_convergence(&convergence)
            .await
            .expect("create convergence");

        let operation = GitOperation {
            id: GitOperationId::new(),
            project_id: project.id,
            operation_kind: OperationKind::PrepareConvergenceCommit,
            entity_type: GitEntityType::Convergence,
            entity_id: convergence.id.to_string(),
            workspace_id: Some(workspace.id),
            ref_name: workspace.workspace_ref.clone(),
            expected_old_oid: workspace.base_commit_oid.clone(),
            new_oid: Some(prepared_head.clone()),
            commit_oid: Some(prepared_head.clone()),
            status: GitOperationStatus::Applied,
            metadata: Some(serde_json::json!({
                "source_commit_oids": [prepared_head],
                "prepared_commit_oids": [prepared_head]
            })),
            created_at,
            completed_at: None,
        };
        db.create_git_operation(&operation)
            .await
            .expect("create operation");

        dispatcher
            .reconcile_startup()
            .await
            .expect("reconcile startup");

        let updated_convergence = db
            .get_convergence(convergence.id)
            .await
            .expect("convergence");
        assert_eq!(updated_convergence.status, ConvergenceStatus::Prepared);
        assert!(updated_convergence.prepared_commit_oid.is_some());
        let updated_workspace = db.get_workspace(workspace.id).await.expect("workspace");
        assert_eq!(updated_workspace.status, WorkspaceStatus::Ready);
        assert_eq!(
            updated_workspace.head_commit_oid,
            updated_convergence.prepared_commit_oid
        );
        let unresolved = db
            .list_unresolved_git_operations()
            .await
            .expect("list unresolved");
        assert!(unresolved.is_empty(), "prepare op should reconcile");
    }

    #[tokio::test]
    async fn reconcile_startup_does_not_resurrect_cancelled_convergence_from_prepare_git_operation()
    {
        let repo = temp_git_repo();
        let base_commit = head_oid(&repo).await.expect("base head");
        std::fs::write(repo.join("tracked.txt"), "prepared").expect("write file");
        git_sync(&repo, &["add", "tracked.txt"]);
        git_sync(&repo, &["commit", "-m", "prepared"]);
        let prepared_head = head_oid(&repo).await.expect("prepared head");
        git_sync(
            &repo,
            &[
                "update-ref",
                "refs/ingot/workspaces/prepare-cancelled",
                &prepared_head,
            ],
        );
        git_sync(&repo, &["reset", "--hard", &base_commit]);

        let db_path = std::env::temp_dir().join(format!(
            "ingot-runtime-prepare-cancelled-{}.db",
            Uuid::now_v7()
        ));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let dispatcher = JobDispatcher::with_runner(
            db.clone(),
            ProjectLocks::default(),
            DispatcherConfig::new(std::env::temp_dir().join(format!(
                "ingot-runtime-prepare-cancelled-state-{}",
                Uuid::now_v7()
            ))),
            Arc::new(FakeRunner),
        );

        let created_at = Utc::now();
        let project = Project {
            id: ingot_domain::ids::ProjectId::new(),
            name: "repo".into(),
            path: repo.display().to_string(),
            default_branch: "main".into(),
            color: "#000".into(),
            created_at,
            updated_at: created_at,
        };
        db.create_project(&project).await.expect("create project");

        let item_id = ingot_domain::ids::ItemId::new();
        let revision_id = ingot_domain::ids::ItemRevisionId::new();
        let item = ingot_domain::item::Item {
            id: item_id,
            project_id: project.id,
            classification: Classification::Change,
            workflow_version: "delivery:v1".into(),
            lifecycle_state: LifecycleState::Open,
            parking_state: ParkingState::Active,
            done_reason: None,
            resolution_source: None,
            approval_state: ApprovalState::NotRequested,
            escalation_state: EscalationState::None,
            escalation_reason: None,
            current_revision_id: revision_id,
            origin_kind: OriginKind::Manual,
            origin_finding_id: None,
            priority: Priority::Major,
            labels: vec![],
            operator_notes: None,
            created_at,
            updated_at: created_at,
            closed_at: None,
        };
        let revision = ItemRevision {
            id: revision_id,
            item_id,
            revision_no: 1,
            title: "Prepare cancelled".into(),
            description: "cancelled prepare".into(),
            acceptance_criteria: "stay cancelled".into(),
            target_ref: "refs/heads/main".into(),
            approval_policy: ApprovalPolicy::Required,
            policy_snapshot: serde_json::json!({}),
            template_map_snapshot: serde_json::json!({}),
            seed_commit_oid: base_commit.clone(),
            seed_target_commit_oid: Some(base_commit.clone()),
            supersedes_revision_id: None,
            created_at,
        };
        db.create_item_with_revision(&item, &revision)
            .await
            .expect("create item");

        let workspace = Workspace {
            id: WorkspaceId::new(),
            project_id: project.id,
            kind: WorkspaceKind::Integration,
            strategy: WorkspaceStrategy::Worktree,
            path: repo.display().to_string(),
            created_for_revision_id: Some(revision.id),
            parent_workspace_id: None,
            target_ref: Some("refs/heads/main".into()),
            workspace_ref: Some("refs/ingot/workspaces/prepare-cancelled".into()),
            base_commit_oid: Some(base_commit.clone()),
            head_commit_oid: Some(prepared_head.clone()),
            retention_policy: RetentionPolicy::Persistent,
            status: WorkspaceStatus::Abandoned,
            current_job_id: None,
            created_at,
            updated_at: created_at,
        };
        db.create_workspace(&workspace)
            .await
            .expect("create workspace");

        let convergence = Convergence {
            id: ingot_domain::ids::ConvergenceId::new(),
            project_id: project.id,
            item_id,
            item_revision_id: revision_id,
            source_workspace_id: workspace.id,
            integration_workspace_id: Some(workspace.id),
            source_head_commit_oid: prepared_head.clone(),
            target_ref: "refs/heads/main".into(),
            strategy: ConvergenceStrategy::RebaseThenFastForward,
            status: ConvergenceStatus::Cancelled,
            input_target_commit_oid: Some(base_commit),
            prepared_commit_oid: Some(prepared_head.clone()),
            final_target_commit_oid: None,
            target_head_valid: None,
            conflict_summary: None,
            created_at,
            completed_at: Some(created_at),
        };
        db.create_convergence(&convergence)
            .await
            .expect("create convergence");

        let operation = GitOperation {
            id: GitOperationId::new(),
            project_id: project.id,
            operation_kind: OperationKind::PrepareConvergenceCommit,
            entity_type: GitEntityType::Convergence,
            entity_id: convergence.id.to_string(),
            workspace_id: Some(workspace.id),
            ref_name: workspace.workspace_ref.clone(),
            expected_old_oid: workspace.base_commit_oid.clone(),
            new_oid: Some(prepared_head.clone()),
            commit_oid: Some(prepared_head),
            status: GitOperationStatus::Applied,
            metadata: Some(serde_json::json!({
                "source_commit_oids": [],
                "prepared_commit_oids": []
            })),
            created_at,
            completed_at: None,
        };
        db.create_git_operation(&operation)
            .await
            .expect("create operation");

        dispatcher
            .reconcile_startup()
            .await
            .expect("reconcile startup");

        let updated_convergence = db
            .get_convergence(convergence.id)
            .await
            .expect("convergence");
        assert_eq!(updated_convergence.status, ConvergenceStatus::Cancelled);
        let updated_workspace = db.get_workspace(workspace.id).await.expect("workspace");
        assert_eq!(updated_workspace.status, WorkspaceStatus::Abandoned);
        let unresolved = db
            .list_unresolved_git_operations()
            .await
            .expect("list unresolved");
        assert!(unresolved.is_empty(), "cancelled prepare op should resolve");
    }

    #[tokio::test]
    async fn reconcile_startup_adopts_create_job_commit_into_completed_job() {
        let repo = temp_git_repo();
        let base_commit = head_oid(&repo).await.expect("base head");
        std::fs::write(repo.join("tracked.txt"), "authored").expect("write file");
        git_sync(&repo, &["add", "tracked.txt"]);
        git_sync(&repo, &["commit", "-m", "authored"]);
        let authored_commit = head_oid(&repo).await.expect("authored head");
        git_sync(
            &repo,
            &[
                "update-ref",
                "refs/ingot/workspaces/adopt-job",
                &authored_commit,
            ],
        );
        git_sync(&repo, &["reset", "--hard", &base_commit]);

        let db_path =
            std::env::temp_dir().join(format!("ingot-runtime-adopt-job-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let dispatcher = JobDispatcher::with_runner(
            db.clone(),
            ProjectLocks::default(),
            DispatcherConfig::new(
                std::env::temp_dir()
                    .join(format!("ingot-runtime-adopt-job-state-{}", Uuid::now_v7())),
            ),
            Arc::new(FakeRunner),
        );

        let created_at = Utc::now();
        let project = Project {
            id: ingot_domain::ids::ProjectId::new(),
            name: "repo".into(),
            path: repo.display().to_string(),
            default_branch: "main".into(),
            color: "#000".into(),
            created_at,
            updated_at: created_at,
        };
        db.create_project(&project).await.expect("create project");

        let item_id = ingot_domain::ids::ItemId::new();
        let revision_id = ingot_domain::ids::ItemRevisionId::new();
        let item = ingot_domain::item::Item {
            id: item_id,
            project_id: project.id,
            classification: Classification::Change,
            workflow_version: "delivery:v1".into(),
            lifecycle_state: LifecycleState::Open,
            parking_state: ParkingState::Active,
            done_reason: None,
            resolution_source: None,
            approval_state: ApprovalState::NotRequested,
            escalation_state: EscalationState::None,
            escalation_reason: None,
            current_revision_id: revision_id,
            origin_kind: OriginKind::Manual,
            origin_finding_id: None,
            priority: Priority::Major,
            labels: vec![],
            operator_notes: None,
            created_at,
            updated_at: created_at,
            closed_at: None,
        };
        let revision = ItemRevision {
            id: revision_id,
            item_id,
            revision_no: 1,
            title: "Adopt job commit".into(),
            description: "adopt".into(),
            acceptance_criteria: "adopt".into(),
            target_ref: "refs/heads/main".into(),
            approval_policy: ApprovalPolicy::Required,
            policy_snapshot: serde_json::json!({}),
            template_map_snapshot: serde_json::json!({}),
            seed_commit_oid: base_commit.clone(),
            seed_target_commit_oid: Some(base_commit.clone()),
            supersedes_revision_id: None,
            created_at,
        };
        db.create_item_with_revision(&item, &revision)
            .await
            .expect("create item");

        let workspace = Workspace {
            id: WorkspaceId::new(),
            project_id: project.id,
            kind: WorkspaceKind::Authoring,
            strategy: WorkspaceStrategy::Worktree,
            path: repo.display().to_string(),
            created_for_revision_id: Some(revision.id),
            parent_workspace_id: None,
            target_ref: Some("refs/heads/main".into()),
            workspace_ref: Some("refs/ingot/workspaces/adopt-job".into()),
            base_commit_oid: Some(base_commit.clone()),
            head_commit_oid: Some(base_commit.clone()),
            retention_policy: RetentionPolicy::Persistent,
            status: WorkspaceStatus::Busy,
            current_job_id: None,
            created_at,
            updated_at: created_at,
        };
        db.create_workspace(&workspace)
            .await
            .expect("create workspace");

        let job = Job {
            id: ingot_domain::ids::JobId::new(),
            project_id: project.id,
            item_id,
            item_revision_id: revision_id,
            step_id: "author_initial".into(),
            semantic_attempt_no: 1,
            retry_no: 0,
            supersedes_job_id: None,
            status: JobStatus::Running,
            outcome_class: None,
            phase_kind: PhaseKind::Author,
            workspace_id: Some(workspace.id),
            workspace_kind: WorkspaceKind::Authoring,
            execution_permission: ExecutionPermission::MayMutate,
            context_policy: ContextPolicy::Fresh,
            phase_template_slug: "author-initial".into(),
            phase_template_digest: None,
            prompt_snapshot: None,
            input_base_commit_oid: None,
            input_head_commit_oid: Some(base_commit.clone()),
            output_artifact_kind: OutputArtifactKind::Commit,
            output_commit_oid: None,
            result_schema_version: None,
            result_payload: None,
            agent_id: None,
            process_pid: None,
            lease_owner_id: Some("old-daemon".into()),
            heartbeat_at: Some(created_at),
            lease_expires_at: Some(created_at + ChronoDuration::minutes(5)),
            error_code: None,
            error_message: None,
            created_at,
            started_at: Some(created_at),
            ended_at: None,
        };
        db.create_job(&job).await.expect("create job");

        let operation = GitOperation {
            id: GitOperationId::new(),
            project_id: project.id,
            operation_kind: OperationKind::CreateJobCommit,
            entity_type: GitEntityType::Job,
            entity_id: job.id.to_string(),
            workspace_id: Some(workspace.id),
            ref_name: workspace.workspace_ref.clone(),
            expected_old_oid: Some(base_commit.clone()),
            new_oid: Some(authored_commit.clone()),
            commit_oid: Some(authored_commit.clone()),
            status: GitOperationStatus::Applied,
            metadata: None,
            created_at,
            completed_at: None,
        };
        db.create_git_operation(&operation)
            .await
            .expect("create operation");

        dispatcher
            .reconcile_startup()
            .await
            .expect("reconcile startup");

        let updated_job = db.get_job(job.id).await.expect("updated job");
        assert_eq!(updated_job.status, JobStatus::Completed);
        assert_eq!(updated_job.outcome_class, Some(OutcomeClass::Clean));
        assert_eq!(
            updated_job.output_commit_oid.as_deref(),
            Some(authored_commit.as_str())
        );

        let updated_workspace = db.get_workspace(workspace.id).await.expect("workspace");
        assert_eq!(updated_workspace.status, WorkspaceStatus::Ready);
        assert_eq!(
            updated_workspace.head_commit_oid.as_deref(),
            Some(authored_commit.as_str())
        );

        let jobs = db.list_jobs_by_item(item.id).await.expect("jobs");
        let review_job = jobs
            .iter()
            .find(|job| job.step_id == step::REVIEW_INCREMENTAL_INITIAL)
            .expect("auto-dispatched review job after startup adoption");
        assert_eq!(review_job.status, JobStatus::Queued);
        assert_eq!(
            review_job.input_base_commit_oid.as_deref(),
            Some(base_commit.as_str())
        );
        assert_eq!(
            review_job.input_head_commit_oid.as_deref(),
            Some(authored_commit.as_str())
        );
    }

    #[tokio::test]
    async fn reconcile_startup_adopts_reset_workspace_operation() {
        let repo = temp_git_repo();
        let head = head_oid(&repo).await.expect("head");
        let db_path =
            std::env::temp_dir().join(format!("ingot-runtime-reset-adopt-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let dispatcher = JobDispatcher::with_runner(
            db.clone(),
            ProjectLocks::default(),
            DispatcherConfig::new(std::env::temp_dir().join(format!(
                "ingot-runtime-reset-adopt-state-{}",
                Uuid::now_v7()
            ))),
            Arc::new(FakeRunner),
        );
        let created_at = Utc::now();
        let project = Project {
            id: ingot_domain::ids::ProjectId::new(),
            name: "repo".into(),
            path: repo.display().to_string(),
            default_branch: "main".into(),
            color: "#000".into(),
            created_at,
            updated_at: created_at,
        };
        db.create_project(&project).await.expect("create project");
        let workspace = Workspace {
            id: WorkspaceId::new(),
            project_id: project.id,
            kind: WorkspaceKind::Authoring,
            strategy: WorkspaceStrategy::Worktree,
            path: repo.display().to_string(),
            created_for_revision_id: None,
            parent_workspace_id: None,
            target_ref: Some("refs/heads/main".into()),
            workspace_ref: Some("refs/ingot/workspaces/reset-adopt".into()),
            base_commit_oid: Some(head.clone()),
            head_commit_oid: Some(head.clone()),
            retention_policy: RetentionPolicy::Persistent,
            status: WorkspaceStatus::Busy,
            current_job_id: Some(ingot_domain::ids::JobId::new()),
            created_at,
            updated_at: created_at,
        };
        db.create_workspace(&workspace)
            .await
            .expect("create workspace");
        let operation = GitOperation {
            id: GitOperationId::new(),
            project_id: project.id,
            operation_kind: OperationKind::ResetWorkspace,
            entity_type: GitEntityType::Workspace,
            entity_id: workspace.id.to_string(),
            workspace_id: Some(workspace.id),
            ref_name: workspace.workspace_ref.clone(),
            expected_old_oid: workspace.head_commit_oid.clone(),
            new_oid: Some(head),
            commit_oid: None,
            status: GitOperationStatus::Applied,
            metadata: None,
            created_at,
            completed_at: None,
        };
        db.create_git_operation(&operation)
            .await
            .expect("create operation");

        dispatcher
            .reconcile_startup()
            .await
            .expect("reconcile startup");

        let updated_workspace = db.get_workspace(workspace.id).await.expect("workspace");
        assert_eq!(updated_workspace.status, WorkspaceStatus::Ready);
        assert_eq!(updated_workspace.current_job_id, None);
    }

    #[tokio::test]
    async fn reconcile_startup_adopts_remove_workspace_ref_operation() {
        let repo = temp_git_repo();
        let head = head_oid(&repo).await.expect("head");
        let workspace_path =
            std::env::temp_dir().join(format!("ingot-runtime-remove-adopt-{}", Uuid::now_v7()));

        let db_path =
            std::env::temp_dir().join(format!("ingot-runtime-remove-adopt-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let state_root = std::env::temp_dir().join(format!(
            "ingot-runtime-remove-adopt-state-{}",
            Uuid::now_v7()
        ));
        let dispatcher = JobDispatcher::with_runner(
            db.clone(),
            ProjectLocks::default(),
            DispatcherConfig::new(state_root.clone()),
            Arc::new(FakeRunner),
        );
        let created_at = Utc::now();
        let project = Project {
            id: ingot_domain::ids::ProjectId::new(),
            name: "repo".into(),
            path: repo.display().to_string(),
            default_branch: "main".into(),
            color: "#000".into(),
            created_at,
            updated_at: created_at,
        };
        let seed_commit = head_oid(&repo).await.expect("seed head");
        db.create_project(&project).await.expect("create project");
        let paths = ensure_test_mirror(state_root.as_path(), &project).await;
        git_sync(
            &paths.mirror_git_dir,
            &["update-ref", "refs/ingot/workspaces/remove-adopt", &head],
        );
        git_sync(
            &paths.mirror_git_dir,
            &[
                "worktree",
                "add",
                "--detach",
                workspace_path.to_str().expect("workspace path"),
                "refs/ingot/workspaces/remove-adopt",
            ],
        );
        delete_ref(&paths.mirror_git_dir, "refs/ingot/workspaces/remove-adopt")
            .await
            .expect("delete ref");
        let workspace = Workspace {
            id: WorkspaceId::new(),
            project_id: project.id,
            kind: WorkspaceKind::Review,
            strategy: WorkspaceStrategy::Worktree,
            path: workspace_path.display().to_string(),
            created_for_revision_id: None,
            parent_workspace_id: None,
            target_ref: None,
            workspace_ref: Some("refs/ingot/workspaces/remove-adopt".into()),
            base_commit_oid: Some(head.clone()),
            head_commit_oid: Some(head),
            retention_policy: RetentionPolicy::Ephemeral,
            status: WorkspaceStatus::Removing,
            current_job_id: Some(ingot_domain::ids::JobId::new()),
            created_at,
            updated_at: created_at,
        };
        db.create_workspace(&workspace)
            .await
            .expect("create workspace");
        let operation = GitOperation {
            id: GitOperationId::new(),
            project_id: project.id,
            operation_kind: OperationKind::RemoveWorkspaceRef,
            entity_type: GitEntityType::Workspace,
            entity_id: workspace.id.to_string(),
            workspace_id: Some(workspace.id),
            ref_name: workspace.workspace_ref.clone(),
            expected_old_oid: workspace.head_commit_oid.clone(),
            new_oid: None,
            commit_oid: None,
            status: GitOperationStatus::Applied,
            metadata: None,
            created_at,
            completed_at: None,
        };
        db.create_git_operation(&operation)
            .await
            .expect("create operation");

        dispatcher
            .reconcile_startup()
            .await
            .expect("reconcile startup");

        let updated_workspace = db.get_workspace(workspace.id).await.expect("workspace");
        assert_eq!(updated_workspace.status, WorkspaceStatus::Abandoned);
        assert_eq!(updated_workspace.current_job_id, None);
        assert_eq!(updated_workspace.workspace_ref, None);
    }

    #[tokio::test]
    async fn reconcile_startup_removes_abandoned_review_workspace_when_safe() {
        let repo = temp_git_repo();
        let workspace_path =
            std::env::temp_dir().join(format!("ingot-runtime-review-cleanup-{}", Uuid::now_v7()));

        let db_path =
            std::env::temp_dir().join(format!("ingot-runtime-cleanup-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let state_root =
            std::env::temp_dir().join(format!("ingot-runtime-cleanup-state-{}", Uuid::now_v7()));
        let dispatcher = JobDispatcher::with_runner(
            db.clone(),
            ProjectLocks::default(),
            DispatcherConfig::new(state_root.clone()),
            Arc::new(FakeRunner),
        );

        let created_at = Utc::now();
        let project = Project {
            id: ingot_domain::ids::ProjectId::new(),
            name: "repo".into(),
            path: repo.display().to_string(),
            default_branch: "main".into(),
            color: "#000".into(),
            created_at,
            updated_at: created_at,
        };
        let seed_commit = head_oid(&repo).await.expect("seed head");
        db.create_project(&project).await.expect("create project");
        let paths = ensure_test_mirror(state_root.as_path(), &project).await;
        git_sync(
            &paths.mirror_git_dir,
            &[
                "worktree",
                "add",
                "--detach",
                workspace_path.to_str().expect("workspace path"),
                "HEAD",
            ],
        );

        let item_id = ingot_domain::ids::ItemId::new();
        let revision_id = ingot_domain::ids::ItemRevisionId::new();
        let item = ingot_domain::item::Item {
            id: item_id,
            project_id: project.id,
            classification: Classification::Change,
            workflow_version: "delivery:v1".into(),
            lifecycle_state: LifecycleState::Done,
            parking_state: ParkingState::Active,
            done_reason: Some(DoneReason::Completed),
            resolution_source: Some(ResolutionSource::ManualCommand),
            approval_state: ApprovalState::NotRequested,
            escalation_state: EscalationState::None,
            escalation_reason: None,
            current_revision_id: revision_id,
            origin_kind: OriginKind::Manual,
            origin_finding_id: None,
            priority: Priority::Major,
            labels: vec![],
            operator_notes: None,
            created_at,
            updated_at: created_at,
            closed_at: Some(created_at),
        };
        let seed_commit = head_oid(&repo).await.expect("seed head");
        let revision = ItemRevision {
            id: revision_id,
            item_id,
            revision_no: 1,
            title: "Cleanup".into(),
            description: "cleanup".into(),
            acceptance_criteria: "cleanup".into(),
            target_ref: "refs/heads/main".into(),
            approval_policy: ApprovalPolicy::Required,
            policy_snapshot: serde_json::json!({}),
            template_map_snapshot: serde_json::json!({}),
            seed_commit_oid: seed_commit.clone(),
            seed_target_commit_oid: Some(seed_commit.clone()),
            supersedes_revision_id: None,
            created_at,
        };
        db.create_item_with_revision(&item, &revision)
            .await
            .expect("create item");

        let workspace = Workspace {
            id: WorkspaceId::new(),
            project_id: project.id,
            kind: WorkspaceKind::Review,
            strategy: WorkspaceStrategy::Worktree,
            path: workspace_path.display().to_string(),
            created_for_revision_id: Some(revision.id),
            parent_workspace_id: None,
            target_ref: None,
            workspace_ref: None,
            base_commit_oid: Some(seed_commit.clone()),
            head_commit_oid: Some(seed_commit),
            retention_policy: RetentionPolicy::Ephemeral,
            status: WorkspaceStatus::Abandoned,
            current_job_id: None,
            created_at,
            updated_at: created_at,
        };
        db.create_workspace(&workspace)
            .await
            .expect("create workspace");

        dispatcher
            .reconcile_startup()
            .await
            .expect("reconcile startup");

        assert!(
            !workspace_path.exists(),
            "abandoned review workspace should be removed"
        );
    }

    #[tokio::test]
    async fn reconcile_startup_removes_abandoned_authoring_workspace_when_item_is_done_and_safe() {
        let repo = temp_git_repo();
        let workspace_path =
            std::env::temp_dir().join(format!("ingot-runtime-author-cleanup-{}", Uuid::now_v7()));

        let db_path = std::env::temp_dir().join(format!(
            "ingot-runtime-author-cleanup-{}.db",
            Uuid::now_v7()
        ));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let state_root = std::env::temp_dir().join(format!(
            "ingot-runtime-author-cleanup-state-{}",
            Uuid::now_v7()
        ));
        let dispatcher = JobDispatcher::with_runner(
            db.clone(),
            ProjectLocks::default(),
            DispatcherConfig::new(state_root.clone()),
            Arc::new(FakeRunner),
        );

        let created_at = Utc::now();
        let project = Project {
            id: ingot_domain::ids::ProjectId::new(),
            name: "repo".into(),
            path: repo.display().to_string(),
            default_branch: "main".into(),
            color: "#000".into(),
            created_at,
            updated_at: created_at,
        };
        let seed_commit = head_oid(&repo).await.expect("seed head");
        db.create_project(&project).await.expect("create project");
        let paths = ensure_test_mirror(state_root.as_path(), &project).await;
        git_sync(
            &paths.mirror_git_dir,
            &[
                "update-ref",
                "refs/ingot/workspaces/author-cleanup",
                &seed_commit,
            ],
        );
        git_sync(
            &paths.mirror_git_dir,
            &[
                "worktree",
                "add",
                "--detach",
                workspace_path.to_str().expect("workspace path"),
                "refs/ingot/workspaces/author-cleanup",
            ],
        );

        let item_id = ingot_domain::ids::ItemId::new();
        let revision_id = ingot_domain::ids::ItemRevisionId::new();
        let item = ingot_domain::item::Item {
            id: item_id,
            project_id: project.id,
            classification: Classification::Change,
            workflow_version: "delivery:v1".into(),
            lifecycle_state: LifecycleState::Done,
            parking_state: ParkingState::Active,
            done_reason: Some(DoneReason::Completed),
            resolution_source: Some(ResolutionSource::ManualCommand),
            approval_state: ApprovalState::NotRequested,
            escalation_state: EscalationState::None,
            escalation_reason: None,
            current_revision_id: revision_id,
            origin_kind: OriginKind::Manual,
            origin_finding_id: None,
            priority: Priority::Major,
            labels: vec![],
            operator_notes: None,
            created_at,
            updated_at: created_at,
            closed_at: Some(created_at),
        };
        let revision = ItemRevision {
            id: revision_id,
            item_id,
            revision_no: 1,
            title: "cleanup".into(),
            description: "cleanup".into(),
            acceptance_criteria: "cleanup".into(),
            target_ref: "refs/heads/main".into(),
            approval_policy: ApprovalPolicy::Required,
            policy_snapshot: serde_json::json!({}),
            template_map_snapshot: serde_json::json!({}),
            seed_commit_oid: seed_commit.clone(),
            seed_target_commit_oid: Some(seed_commit.clone()),
            supersedes_revision_id: None,
            created_at,
        };
        db.create_item_with_revision(&item, &revision)
            .await
            .expect("create item");

        let workspace = Workspace {
            id: WorkspaceId::new(),
            project_id: project.id,
            kind: WorkspaceKind::Authoring,
            strategy: WorkspaceStrategy::Worktree,
            path: workspace_path.display().to_string(),
            created_for_revision_id: Some(revision.id),
            parent_workspace_id: None,
            target_ref: Some("refs/heads/main".into()),
            workspace_ref: Some("refs/ingot/workspaces/author-cleanup".into()),
            base_commit_oid: Some(seed_commit.clone()),
            head_commit_oid: Some(seed_commit),
            retention_policy: RetentionPolicy::Persistent,
            status: WorkspaceStatus::Abandoned,
            current_job_id: None,
            created_at,
            updated_at: created_at,
        };
        db.create_workspace(&workspace)
            .await
            .expect("create workspace");

        dispatcher
            .reconcile_startup()
            .await
            .expect("reconcile startup");

        assert!(
            !workspace_path.exists(),
            "abandoned authoring workspace should be removed when safe"
        );
    }

    #[tokio::test]
    async fn reconcile_startup_retains_abandoned_authoring_workspace_with_untriaged_candidate_finding()
     {
        let repo = temp_git_repo();
        let seed_commit = head_oid(&repo).await.expect("seed head");
        let workspace_path =
            std::env::temp_dir().join(format!("ingot-runtime-author-retain-{}", Uuid::now_v7()));
        git_sync(
            &repo,
            &[
                "worktree",
                "add",
                "--detach",
                workspace_path.to_str().expect("workspace path"),
                "HEAD",
            ],
        );

        let db_path =
            std::env::temp_dir().join(format!("ingot-runtime-retain-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let dispatcher = JobDispatcher::with_runner(
            db.clone(),
            ProjectLocks::default(),
            DispatcherConfig::new(
                std::env::temp_dir().join(format!("ingot-runtime-retain-state-{}", Uuid::now_v7())),
            ),
            Arc::new(FakeRunner),
        );

        let created_at = Utc::now();
        let project = Project {
            id: ingot_domain::ids::ProjectId::new(),
            name: "repo".into(),
            path: repo.display().to_string(),
            default_branch: "main".into(),
            color: "#000".into(),
            created_at,
            updated_at: created_at,
        };
        db.create_project(&project).await.expect("create project");

        let item_id = ingot_domain::ids::ItemId::new();
        let revision_id = ingot_domain::ids::ItemRevisionId::new();
        let item = ingot_domain::item::Item {
            id: item_id,
            project_id: project.id,
            classification: Classification::Change,
            workflow_version: "delivery:v1".into(),
            lifecycle_state: LifecycleState::Done,
            parking_state: ParkingState::Active,
            done_reason: Some(DoneReason::Dismissed),
            resolution_source: Some(ResolutionSource::ManualCommand),
            approval_state: ApprovalState::NotRequested,
            escalation_state: EscalationState::None,
            escalation_reason: None,
            current_revision_id: revision_id,
            origin_kind: OriginKind::Manual,
            origin_finding_id: None,
            priority: Priority::Major,
            labels: vec![],
            operator_notes: None,
            created_at,
            updated_at: created_at,
            closed_at: Some(created_at),
        };
        let revision = ItemRevision {
            id: revision_id,
            item_id,
            revision_no: 1,
            title: "Retain".into(),
            description: "retain".into(),
            acceptance_criteria: "retain".into(),
            target_ref: "refs/heads/main".into(),
            approval_policy: ApprovalPolicy::Required,
            policy_snapshot: serde_json::json!({}),
            template_map_snapshot: serde_json::json!({}),
            seed_commit_oid: seed_commit.clone(),
            seed_target_commit_oid: Some(seed_commit.clone()),
            supersedes_revision_id: None,
            created_at,
        };
        db.create_item_with_revision(&item, &revision)
            .await
            .expect("create item");

        let workspace = Workspace {
            id: WorkspaceId::new(),
            project_id: project.id,
            kind: WorkspaceKind::Authoring,
            strategy: WorkspaceStrategy::Worktree,
            path: workspace_path.display().to_string(),
            created_for_revision_id: Some(revision.id),
            parent_workspace_id: None,
            target_ref: Some("refs/heads/main".into()),
            workspace_ref: Some("refs/ingot/workspaces/retain".into()),
            base_commit_oid: Some(seed_commit.clone()),
            head_commit_oid: Some(seed_commit.clone()),
            retention_policy: RetentionPolicy::Ephemeral,
            status: WorkspaceStatus::Abandoned,
            current_job_id: None,
            created_at,
            updated_at: created_at,
        };
        db.create_workspace(&workspace)
            .await
            .expect("create workspace");

        let source_job = Job {
            id: ingot_domain::ids::JobId::new(),
            project_id: project.id,
            item_id,
            item_revision_id: revision_id,
            step_id: "review_candidate_initial".into(),
            semantic_attempt_no: 1,
            retry_no: 0,
            supersedes_job_id: None,
            status: JobStatus::Completed,
            outcome_class: Some(OutcomeClass::Findings),
            phase_kind: PhaseKind::Review,
            workspace_id: Some(workspace.id),
            workspace_kind: WorkspaceKind::Review,
            execution_permission: ExecutionPermission::MustNotMutate,
            context_policy: ContextPolicy::Fresh,
            phase_template_slug: "review-candidate".into(),
            phase_template_digest: None,
            prompt_snapshot: None,
            input_base_commit_oid: Some(seed_commit.clone()),
            input_head_commit_oid: Some(seed_commit.clone()),
            output_artifact_kind: OutputArtifactKind::ReviewReport,
            output_commit_oid: None,
            result_schema_version: Some("review_report:v1".into()),
            result_payload: Some(serde_json::json!({
                "outcome": "findings",
                "summary": "finding",
                "review_subject": {
                    "base_commit_oid": seed_commit.clone(),
                    "head_commit_oid": seed_commit.clone()
                },
                "overall_risk": "medium",
                "findings": []
            })),
            agent_id: None,
            process_pid: None,
            lease_owner_id: None,
            heartbeat_at: None,
            lease_expires_at: None,
            error_code: None,
            error_message: None,
            created_at,
            started_at: Some(created_at),
            ended_at: Some(created_at),
        };
        db.create_job(&source_job).await.expect("create source job");

        let finding = ingot_domain::finding::Finding {
            id: ingot_domain::ids::FindingId::new(),
            project_id: project.id,
            source_item_id: item.id,
            source_item_revision_id: revision.id,
            source_job_id: source_job.id,
            source_step_id: "review_candidate_initial".into(),
            source_report_schema_version: "review_report:v1".into(),
            source_finding_key: "fnd".into(),
            source_subject_kind: ingot_domain::finding::FindingSubjectKind::Candidate,
            source_subject_base_commit_oid: Some(seed_commit.clone()),
            source_subject_head_commit_oid: seed_commit.clone(),
            code: "CODE".into(),
            severity: ingot_domain::finding::FindingSeverity::Medium,
            summary: "retain me".into(),
            paths: vec!["tracked.txt".into()],
            evidence: serde_json::json!(["evidence"]),
            triage_state: FindingTriageState::Untriaged,
            linked_item_id: None,
            triage_note: None,
            created_at,
            triaged_at: None,
        };
        db.create_finding(&finding).await.expect("create finding");

        dispatcher
            .reconcile_startup()
            .await
            .expect("reconcile startup");

        assert!(workspace_path.exists(), "workspace should be retained");
    }

    #[tokio::test]
    async fn reconcile_startup_retains_abandoned_integration_workspace_with_untriaged_integrated_finding()
     {
        let repo = temp_git_repo();
        let seed_commit = head_oid(&repo).await.expect("seed head");
        let workspace_path = std::env::temp_dir().join(format!(
            "ingot-runtime-integration-retain-{}",
            Uuid::now_v7()
        ));
        git_sync(
            &repo,
            &[
                "worktree",
                "add",
                "--detach",
                workspace_path.to_str().expect("workspace path"),
                "HEAD",
            ],
        );

        let db_path = std::env::temp_dir().join(format!(
            "ingot-runtime-integration-retain-{}.db",
            Uuid::now_v7()
        ));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let dispatcher = JobDispatcher::with_runner(
            db.clone(),
            ProjectLocks::default(),
            DispatcherConfig::new(std::env::temp_dir().join(format!(
                "ingot-runtime-integration-retain-state-{}",
                Uuid::now_v7()
            ))),
            Arc::new(FakeRunner),
        );

        let created_at = Utc::now();
        let project = Project {
            id: ingot_domain::ids::ProjectId::new(),
            name: "repo".into(),
            path: repo.display().to_string(),
            default_branch: "main".into(),
            color: "#000".into(),
            created_at,
            updated_at: created_at,
        };
        db.create_project(&project).await.expect("create project");

        let item_id = ingot_domain::ids::ItemId::new();
        let revision_id = ingot_domain::ids::ItemRevisionId::new();
        let item = ingot_domain::item::Item {
            id: item_id,
            project_id: project.id,
            classification: Classification::Change,
            workflow_version: "delivery:v1".into(),
            lifecycle_state: LifecycleState::Done,
            parking_state: ParkingState::Active,
            done_reason: Some(DoneReason::Completed),
            resolution_source: Some(ResolutionSource::ManualCommand),
            approval_state: ApprovalState::NotRequested,
            escalation_state: EscalationState::None,
            escalation_reason: None,
            current_revision_id: revision_id,
            origin_kind: OriginKind::Manual,
            origin_finding_id: None,
            priority: Priority::Major,
            labels: vec![],
            operator_notes: None,
            created_at,
            updated_at: created_at,
            closed_at: Some(created_at),
        };
        let revision = ItemRevision {
            id: revision_id,
            item_id,
            revision_no: 1,
            title: "retain integration".into(),
            description: "retain".into(),
            acceptance_criteria: "retain".into(),
            target_ref: "refs/heads/main".into(),
            approval_policy: ApprovalPolicy::Required,
            policy_snapshot: serde_json::json!({}),
            template_map_snapshot: serde_json::json!({}),
            seed_commit_oid: seed_commit.clone(),
            seed_target_commit_oid: Some(seed_commit.clone()),
            supersedes_revision_id: None,
            created_at,
        };
        db.create_item_with_revision(&item, &revision)
            .await
            .expect("create item");

        let source_job = Job {
            id: ingot_domain::ids::JobId::new(),
            project_id: project.id,
            item_id,
            item_revision_id: revision_id,
            step_id: "validate_integrated".into(),
            semantic_attempt_no: 1,
            retry_no: 0,
            supersedes_job_id: None,
            status: JobStatus::Completed,
            outcome_class: Some(OutcomeClass::Findings),
            phase_kind: PhaseKind::Validate,
            workspace_id: None,
            workspace_kind: WorkspaceKind::Integration,
            execution_permission: ExecutionPermission::MustNotMutate,
            context_policy: ContextPolicy::ResumeContext,
            phase_template_slug: "validate-integrated".into(),
            phase_template_digest: None,
            prompt_snapshot: None,
            input_base_commit_oid: Some(seed_commit.clone()),
            input_head_commit_oid: Some(seed_commit.clone()),
            output_artifact_kind: OutputArtifactKind::ValidationReport,
            output_commit_oid: None,
            result_schema_version: Some("validation_report:v1".into()),
            result_payload: Some(serde_json::json!({
                "outcome": "findings",
                "summary": "finding",
                "checks": [],
                "findings": []
            })),
            agent_id: None,
            process_pid: None,
            lease_owner_id: None,
            heartbeat_at: None,
            lease_expires_at: None,
            error_code: None,
            error_message: None,
            created_at,
            started_at: Some(created_at),
            ended_at: Some(created_at),
        };
        db.create_job(&source_job).await.expect("create source job");

        let workspace = Workspace {
            id: WorkspaceId::new(),
            project_id: project.id,
            kind: WorkspaceKind::Integration,
            strategy: WorkspaceStrategy::Worktree,
            path: workspace_path.display().to_string(),
            created_for_revision_id: Some(revision.id),
            parent_workspace_id: None,
            target_ref: Some("refs/heads/main".into()),
            workspace_ref: Some("refs/ingot/workspaces/integration-retain".into()),
            base_commit_oid: Some(seed_commit.clone()),
            head_commit_oid: Some(seed_commit.clone()),
            retention_policy: RetentionPolicy::Persistent,
            status: WorkspaceStatus::Abandoned,
            current_job_id: None,
            created_at,
            updated_at: created_at,
        };
        db.create_workspace(&workspace)
            .await
            .expect("create workspace");

        let finding = ingot_domain::finding::Finding {
            id: ingot_domain::ids::FindingId::new(),
            project_id: project.id,
            source_item_id: item.id,
            source_item_revision_id: revision.id,
            source_job_id: source_job.id,
            source_step_id: "validate_integrated".into(),
            source_report_schema_version: "validation_report:v1".into(),
            source_finding_key: "fnd".into(),
            source_subject_kind: ingot_domain::finding::FindingSubjectKind::Integrated,
            source_subject_base_commit_oid: Some(seed_commit.clone()),
            source_subject_head_commit_oid: seed_commit.clone(),
            code: "CODE".into(),
            severity: ingot_domain::finding::FindingSeverity::High,
            summary: "retain integration".into(),
            paths: vec!["tracked.txt".into()],
            evidence: serde_json::json!(["evidence"]),
            triage_state: FindingTriageState::Untriaged,
            linked_item_id: None,
            triage_note: None,
            created_at,
            triaged_at: None,
        };
        db.create_finding(&finding).await.expect("create finding");

        dispatcher
            .reconcile_startup()
            .await
            .expect("reconcile startup");

        assert!(
            workspace_path.exists(),
            "integration workspace should be retained"
        );
    }

    #[tokio::test]
    async fn reconcile_startup_handles_mixed_inflight_states_conservatively() {
        let repo = temp_git_repo();
        let seed_commit = head_oid(&repo).await.expect("seed head");

        let db_path = std::env::temp_dir().join(format!(
            "ingot-runtime-mixed-reconcile-{}.db",
            Uuid::now_v7()
        ));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let dispatcher = JobDispatcher::with_runner(
            db.clone(),
            ProjectLocks::default(),
            DispatcherConfig::new(std::env::temp_dir().join(format!(
                "ingot-runtime-mixed-reconcile-state-{}",
                Uuid::now_v7()
            ))),
            Arc::new(FakeRunner),
        );

        let created_at = Utc::now();
        let project = Project {
            id: ingot_domain::ids::ProjectId::new(),
            name: "repo".into(),
            path: repo.display().to_string(),
            default_branch: "main".into(),
            color: "#000".into(),
            created_at,
            updated_at: created_at,
        };
        db.create_project(&project).await.expect("create project");

        let rev_a = make_runtime_revision(
            ingot_domain::ids::ItemId::new(),
            1,
            &seed_commit,
            created_at,
        );
        let item_a = make_runtime_item(project.id, rev_a.id, rev_a.item_id, created_at);
        db.create_item_with_revision(&item_a, &rev_a)
            .await
            .expect("create item a");
        let workspace_a = make_runtime_workspace(
            project.id,
            Some(rev_a.id),
            WorkspaceKind::Authoring,
            WorkspaceStatus::Busy,
            &seed_commit,
            created_at,
        );
        db.create_workspace(&workspace_a)
            .await
            .expect("workspace a");
        let mut assigned_job = make_runtime_job(
            project.id,
            item_a.id,
            rev_a.id,
            "author_initial",
            WorkspaceKind::Authoring,
            OutputArtifactKind::Commit,
            created_at,
        );
        assigned_job.status = JobStatus::Assigned;
        assigned_job.workspace_id = Some(workspace_a.id);
        db.create_job(&assigned_job).await.expect("assigned job");

        let rev_b = make_runtime_revision(
            ingot_domain::ids::ItemId::new(),
            1,
            &seed_commit,
            created_at,
        );
        let item_b = make_runtime_item(project.id, rev_b.id, rev_b.item_id, created_at);
        db.create_item_with_revision(&item_b, &rev_b)
            .await
            .expect("create item b");
        let workspace_b = make_runtime_workspace(
            project.id,
            Some(rev_b.id),
            WorkspaceKind::Authoring,
            WorkspaceStatus::Busy,
            &seed_commit,
            created_at,
        );
        db.create_workspace(&workspace_b)
            .await
            .expect("workspace b");
        let mut running_job = make_runtime_job(
            project.id,
            item_b.id,
            rev_b.id,
            "author_initial",
            WorkspaceKind::Authoring,
            OutputArtifactKind::Commit,
            created_at,
        );
        running_job.status = JobStatus::Running;
        running_job.workspace_id = Some(workspace_b.id);
        running_job.lease_owner_id = Some("old-daemon".into());
        running_job.lease_expires_at = Some(created_at - ChronoDuration::minutes(1));
        running_job.started_at = Some(created_at);
        db.create_job(&running_job).await.expect("running job");

        dispatcher
            .reconcile_startup()
            .await
            .expect("reconcile startup");

        let updated_assigned = db.get_job(assigned_job.id).await.expect("assigned");
        assert_eq!(updated_assigned.status, JobStatus::Queued);
        assert_eq!(updated_assigned.workspace_id, None);

        let updated_running = db.get_job(running_job.id).await.expect("running");
        assert_eq!(updated_running.status, JobStatus::Expired);
        assert_eq!(
            updated_running.outcome_class,
            Some(OutcomeClass::TransientFailure)
        );

        let updated_workspace_a = db.get_workspace(workspace_a.id).await.expect("workspace a");
        assert_eq!(updated_workspace_a.status, WorkspaceStatus::Ready);
        let updated_workspace_b = db.get_workspace(workspace_b.id).await.expect("workspace b");
        assert_eq!(updated_workspace_b.status, WorkspaceStatus::Stale);
    }

    fn make_runtime_item(
        project_id: ingot_domain::ids::ProjectId,
        revision_id: ingot_domain::ids::ItemRevisionId,
        item_id: ingot_domain::ids::ItemId,
        created_at: chrono::DateTime<Utc>,
    ) -> ingot_domain::item::Item {
        ingot_domain::item::Item {
            id: item_id,
            project_id,
            classification: Classification::Change,
            workflow_version: "delivery:v1".into(),
            lifecycle_state: LifecycleState::Open,
            parking_state: ParkingState::Active,
            done_reason: None,
            resolution_source: None,
            approval_state: ApprovalState::NotRequested,
            escalation_state: EscalationState::None,
            escalation_reason: None,
            current_revision_id: revision_id,
            origin_kind: OriginKind::Manual,
            origin_finding_id: None,
            priority: Priority::Major,
            labels: vec![],
            operator_notes: None,
            created_at,
            updated_at: created_at,
            closed_at: None,
        }
    }

    fn make_runtime_revision(
        item_id: ingot_domain::ids::ItemId,
        revision_no: u32,
        seed_commit_oid: &str,
        created_at: chrono::DateTime<Utc>,
    ) -> ItemRevision {
        ItemRevision {
            id: ingot_domain::ids::ItemRevisionId::new(),
            item_id,
            revision_no,
            title: "Runtime".into(),
            description: "runtime".into(),
            acceptance_criteria: "runtime".into(),
            target_ref: "refs/heads/main".into(),
            approval_policy: ApprovalPolicy::Required,
            policy_snapshot: serde_json::json!({}),
            template_map_snapshot: serde_json::json!({}),
            seed_commit_oid: seed_commit_oid.into(),
            seed_target_commit_oid: Some(seed_commit_oid.into()),
            supersedes_revision_id: None,
            created_at,
        }
    }

    fn make_runtime_workspace(
        project_id: ingot_domain::ids::ProjectId,
        revision_id: Option<ingot_domain::ids::ItemRevisionId>,
        kind: WorkspaceKind,
        status: WorkspaceStatus,
        head_commit_oid: &str,
        created_at: chrono::DateTime<Utc>,
    ) -> Workspace {
        Workspace {
            id: WorkspaceId::new(),
            project_id,
            kind,
            strategy: WorkspaceStrategy::Worktree,
            path: std::env::temp_dir()
                .join(format!("ingot-runtime-mixed-workspace-{}", Uuid::now_v7()))
                .display()
                .to_string(),
            created_for_revision_id: revision_id,
            parent_workspace_id: None,
            target_ref: Some("refs/heads/main".into()),
            workspace_ref: Some(format!("refs/ingot/workspaces/{}", Uuid::now_v7().simple())),
            base_commit_oid: Some(head_commit_oid.into()),
            head_commit_oid: Some(head_commit_oid.into()),
            retention_policy: RetentionPolicy::Persistent,
            status,
            current_job_id: None,
            created_at,
            updated_at: created_at,
        }
    }

    fn make_runtime_job(
        project_id: ingot_domain::ids::ProjectId,
        item_id: ingot_domain::ids::ItemId,
        revision_id: ingot_domain::ids::ItemRevisionId,
        step_id: &str,
        workspace_kind: WorkspaceKind,
        output_artifact_kind: OutputArtifactKind,
        created_at: chrono::DateTime<Utc>,
    ) -> Job {
        Job {
            id: ingot_domain::ids::JobId::new(),
            project_id,
            item_id,
            item_revision_id: revision_id,
            step_id: step_id.into(),
            semantic_attempt_no: 1,
            retry_no: 0,
            supersedes_job_id: None,
            status: JobStatus::Queued,
            outcome_class: None,
            phase_kind: PhaseKind::Author,
            workspace_id: None,
            workspace_kind,
            execution_permission: ExecutionPermission::MayMutate,
            context_policy: ContextPolicy::Fresh,
            phase_template_slug: "template".into(),
            phase_template_digest: None,
            prompt_snapshot: None,
            input_base_commit_oid: None,
            input_head_commit_oid: Some("seed".into()),
            output_artifact_kind,
            output_commit_oid: None,
            result_schema_version: None,
            result_payload: None,
            agent_id: None,
            process_pid: None,
            lease_owner_id: None,
            heartbeat_at: None,
            lease_expires_at: None,
            error_code: None,
            error_message: None,
            created_at,
            started_at: None,
            ended_at: None,
        }
    }

    struct StaticReviewRunner {
        base_commit_oid: String,
        head_commit_oid: String,
    }

    impl AgentRunner for StaticReviewRunner {
        fn launch<'a>(
            &'a self,
            _agent: &'a Agent,
            _request: &'a AgentRequest,
            _working_dir: &'a Path,
        ) -> Pin<Box<dyn Future<Output = Result<AgentResponse, AgentError>> + Send + 'a>> {
            Box::pin(async move {
                Ok(AgentResponse {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                    result: Some(serde_json::json!({
                        "outcome": "clean",
                        "summary": "No issues found",
                        "review_subject": {
                            "base_commit_oid": self.base_commit_oid,
                            "head_commit_oid": self.head_commit_oid
                        },
                        "overall_risk": "low",
                        "findings": []
                    })),
                })
            })
        }
    }

    fn temp_git_repo() -> PathBuf {
        let path = std::env::temp_dir().join(format!("ingot-runtime-repo-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&path).expect("create temp repo dir");
        git_sync(&path, &["init"]);
        git_sync(&path, &["branch", "-M", "main"]);
        git_sync(&path, &["config", "user.name", "Ingot Test"]);
        git_sync(&path, &["config", "user.email", "ingot@example.com"]);
        std::fs::write(path.join("tracked.txt"), "initial").expect("write tracked file");
        git_sync(&path, &["add", "tracked.txt"]);
        git_sync(&path, &["commit", "-m", "initial"]);
        path
    }

    fn git_sync(path: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .expect("run git");
        assert!(status.success(), "git {:?} failed", args);
    }

    fn git_output(path: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("run git output");
        assert!(output.status.success(), "git {:?} failed", args);
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    struct ScriptedLoopRunner;

    impl AgentRunner for ScriptedLoopRunner {
        fn launch<'a>(
            &'a self,
            _agent: &'a Agent,
            request: &'a AgentRequest,
            working_dir: &'a Path,
        ) -> Pin<Box<dyn Future<Output = Result<AgentResponse, AgentError>> + Send + 'a>> {
            Box::pin(async move {
                let step = prompt_value(&request.prompt, "Step");
                match step.as_deref() {
                    Some("author_initial") => {
                        tokio::fs::write(working_dir.join("feature.txt"), "initial change")
                            .await
                            .expect("write feature");
                        Ok(AgentResponse {
                            exit_code: 0,
                            stdout: String::new(),
                            stderr: String::new(),
                            result: Some(serde_json::json!({ "summary": "initial authored" })),
                        })
                    }
                    Some("repair_candidate") => {
                        tokio::fs::write(working_dir.join("feature.txt"), "repaired change")
                            .await
                            .expect("repair feature");
                        Ok(AgentResponse {
                            exit_code: 0,
                            stdout: String::new(),
                            stderr: String::new(),
                            result: Some(serde_json::json!({ "summary": "candidate repaired" })),
                        })
                    }
                    Some("review_incremental_initial") => Ok(AgentResponse {
                        exit_code: 0,
                        stdout: String::new(),
                        stderr: String::new(),
                        result: Some(serde_json::json!({
                            "outcome": "findings",
                            "summary": "initial review found an issue",
                            "review_subject": {
                                "base_commit_oid": prompt_value(&request.prompt, "Input base commit").unwrap_or_default(),
                                "head_commit_oid": prompt_value(&request.prompt, "Input head commit").unwrap_or_default()
                            },
                            "overall_risk": "medium",
                            "findings": [{
                                "finding_key": "fix-me",
                                "code": "BUG",
                                "severity": "medium",
                                "summary": "needs repair",
                                "paths": ["feature.txt"],
                                "evidence": ["fix me"]
                            }]
                        })),
                    }),
                    Some("review_incremental_repair") | Some("review_candidate_repair") => {
                        Ok(AgentResponse {
                            exit_code: 0,
                            stdout: String::new(),
                            stderr: String::new(),
                            result: Some(serde_json::json!({
                                "outcome": "clean",
                                "summary": "review clean",
                                "review_subject": {
                                    "base_commit_oid": prompt_value(&request.prompt, "Input base commit").unwrap_or_default(),
                                    "head_commit_oid": prompt_value(&request.prompt, "Input head commit").unwrap_or_default()
                                },
                                "overall_risk": "low",
                                "findings": []
                            })),
                        })
                    }
                    Some("validate_candidate_repair") => Ok(AgentResponse {
                        exit_code: 0,
                        stdout: String::new(),
                        stderr: String::new(),
                        result: Some(serde_json::json!({
                            "outcome": "clean",
                            "summary": "validation clean",
                            "checks": [],
                            "findings": []
                        })),
                    }),
                    other => Err(AgentError::ProtocolViolation(format!(
                        "unexpected step in scripted loop runner: {other:?}"
                    ))),
                }
            })
        }
    }

    struct CleanInitialReviewRunner;

    impl AgentRunner for CleanInitialReviewRunner {
        fn launch<'a>(
            &'a self,
            _agent: &'a Agent,
            request: &'a AgentRequest,
            _working_dir: &'a Path,
        ) -> Pin<Box<dyn Future<Output = Result<AgentResponse, AgentError>> + Send + 'a>> {
            Box::pin(async move {
                match prompt_value(&request.prompt, "Step").as_deref() {
                    Some("review_incremental_initial") => Ok(AgentResponse {
                        exit_code: 0,
                        stdout: String::new(),
                        stderr: String::new(),
                        result: Some(serde_json::json!({
                            "outcome": "clean",
                            "summary": "incremental review clean",
                            "review_subject": {
                                "base_commit_oid": prompt_value(&request.prompt, "Input base commit").unwrap_or_default(),
                                "head_commit_oid": prompt_value(&request.prompt, "Input head commit").unwrap_or_default()
                            },
                            "overall_risk": "low",
                            "findings": []
                        })),
                    }),
                    other => Err(AgentError::ProtocolViolation(format!(
                        "unexpected step in clean initial review runner: {other:?}"
                    ))),
                }
            })
        }
    }

    #[tokio::test]
    async fn authoring_success_auto_dispatches_incremental_review() {
        let repo = temp_git_repo();
        let db_path =
            std::env::temp_dir().join(format!("ingot-runtime-auto-review-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let dispatcher = JobDispatcher::with_runner(
            db.clone(),
            ProjectLocks::default(),
            DispatcherConfig::new(std::env::temp_dir().join(format!(
                "ingot-runtime-auto-review-state-{}",
                Uuid::now_v7()
            ))),
            Arc::new(FakeRunner),
        );

        let created_at = Utc::now();
        let project = Project {
            id: ingot_domain::ids::ProjectId::new(),
            name: "repo".into(),
            path: repo.display().to_string(),
            default_branch: "main".into(),
            color: "#000".into(),
            created_at,
            updated_at: created_at,
        };
        db.create_project(&project).await.expect("create project");

        let agent = Agent {
            id: ingot_domain::ids::AgentId::new(),
            slug: "codex".into(),
            name: "Codex".into(),
            adapter_kind: AdapterKind::Codex,
            provider: "openai".into(),
            model: "gpt-5-codex".into(),
            cli_path: "codex".into(),
            capabilities: vec![
                AgentCapability::MutatingJobs,
                AgentCapability::ReadOnlyJobs,
                AgentCapability::StructuredOutput,
            ],
            health_check: Some("ok".into()),
            status: AgentStatus::Available,
        };
        db.create_agent(&agent).await.expect("create agent");

        let item_id = ingot_domain::ids::ItemId::new();
        let revision_id = ingot_domain::ids::ItemRevisionId::new();
        let item = ingot_domain::item::Item {
            id: item_id,
            project_id: project.id,
            classification: Classification::Change,
            workflow_version: "delivery:v1".into(),
            lifecycle_state: LifecycleState::Open,
            parking_state: ParkingState::Active,
            done_reason: None,
            resolution_source: None,
            approval_state: ApprovalState::NotRequested,
            escalation_state: EscalationState::None,
            escalation_reason: None,
            current_revision_id: revision_id,
            origin_kind: OriginKind::Manual,
            origin_finding_id: None,
            priority: Priority::Major,
            labels: vec![],
            operator_notes: None,
            created_at,
            updated_at: created_at,
            closed_at: None,
        };
        let seed_commit = head_oid(&repo).await.expect("seed head");
        let revision = ItemRevision {
            id: revision_id,
            item_id,
            revision_no: 1,
            title: "Auto review".into(),
            description: "queue incremental review".into(),
            acceptance_criteria: "author then review".into(),
            target_ref: "refs/heads/main".into(),
            approval_policy: ApprovalPolicy::Required,
            policy_snapshot: serde_json::json!({}),
            template_map_snapshot: serde_json::json!({}),
            seed_commit_oid: seed_commit.clone(),
            seed_target_commit_oid: Some(seed_commit.clone()),
            supersedes_revision_id: None,
            created_at,
        };
        db.create_item_with_revision(&item, &revision)
            .await
            .expect("create item");

        let author_job = dispatch_job(
            &item,
            &revision,
            &[],
            &[],
            &[],
            DispatchJobCommand { step_id: None },
        )
        .expect("dispatch author initial");
        db.create_job(&author_job).await.expect("create author job");

        dispatcher.tick().await.expect("author tick");

        let jobs = db.list_jobs_by_item(item.id).await.expect("jobs");
        assert_eq!(jobs.len(), 2, "author success should auto-queue review");

        let completed_author = jobs
            .iter()
            .find(|job| job.step_id == step::AUTHOR_INITIAL)
            .expect("completed author job");
        assert_eq!(completed_author.status, JobStatus::Completed);
        assert_eq!(completed_author.outcome_class, Some(OutcomeClass::Clean));

        let review_job = jobs
            .iter()
            .find(|job| job.step_id == step::REVIEW_INCREMENTAL_INITIAL)
            .expect("auto-dispatched incremental review job");
        assert_eq!(review_job.status, JobStatus::Queued);
        assert_eq!(
            review_job.input_base_commit_oid.as_deref(),
            Some(seed_commit.as_str())
        );
        assert_eq!(
            review_job.input_head_commit_oid.as_deref(),
            completed_author.output_commit_oid.as_deref()
        );
    }

    #[tokio::test]
    async fn clean_incremental_review_auto_dispatches_candidate_review() {
        let repo = temp_git_repo();
        let seed_commit = head_oid(&repo).await.expect("seed head");
        std::fs::write(repo.join("feature.txt"), "candidate change").expect("write feature");
        git_sync(&repo, &["add", "feature.txt"]);
        git_sync(&repo, &["commit", "-m", "candidate change"]);
        let candidate_head = head_oid(&repo).await.expect("candidate head");

        let db_path = std::env::temp_dir().join(format!(
            "ingot-runtime-auto-candidate-review-{}.db",
            Uuid::now_v7()
        ));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let dispatcher = JobDispatcher::with_runner(
            db.clone(),
            ProjectLocks::default(),
            DispatcherConfig::new(std::env::temp_dir().join(format!(
                "ingot-runtime-auto-candidate-review-state-{}",
                Uuid::now_v7()
            ))),
            Arc::new(CleanInitialReviewRunner),
        );

        let created_at = Utc::now();
        let project = Project {
            id: ingot_domain::ids::ProjectId::new(),
            name: "repo".into(),
            path: repo.display().to_string(),
            default_branch: "main".into(),
            color: "#000".into(),
            created_at,
            updated_at: created_at,
        };
        db.create_project(&project).await.expect("create project");

        let agent = Agent {
            id: ingot_domain::ids::AgentId::new(),
            slug: "codex".into(),
            name: "Codex".into(),
            adapter_kind: AdapterKind::Codex,
            provider: "openai".into(),
            model: "gpt-5-codex".into(),
            cli_path: "codex".into(),
            capabilities: vec![
                AgentCapability::ReadOnlyJobs,
                AgentCapability::StructuredOutput,
            ],
            health_check: Some("ok".into()),
            status: AgentStatus::Available,
        };
        db.create_agent(&agent).await.expect("create agent");

        let item_id = ingot_domain::ids::ItemId::new();
        let revision_id = ingot_domain::ids::ItemRevisionId::new();
        let item = ingot_domain::item::Item {
            id: item_id,
            project_id: project.id,
            classification: Classification::Change,
            workflow_version: "delivery:v1".into(),
            lifecycle_state: LifecycleState::Open,
            parking_state: ParkingState::Active,
            done_reason: None,
            resolution_source: None,
            approval_state: ApprovalState::NotRequested,
            escalation_state: EscalationState::None,
            escalation_reason: None,
            current_revision_id: revision_id,
            origin_kind: OriginKind::Manual,
            origin_finding_id: None,
            priority: Priority::Major,
            labels: vec![],
            operator_notes: None,
            created_at,
            updated_at: created_at,
            closed_at: None,
        };
        let revision = ItemRevision {
            id: revision_id,
            item_id,
            revision_no: 1,
            title: "Auto candidate review".into(),
            description: "queue candidate review".into(),
            acceptance_criteria: "candidate review queued".into(),
            target_ref: "refs/heads/main".into(),
            approval_policy: ApprovalPolicy::Required,
            policy_snapshot: serde_json::json!({}),
            template_map_snapshot: serde_json::json!({}),
            seed_commit_oid: seed_commit.clone(),
            seed_target_commit_oid: Some(seed_commit.clone()),
            supersedes_revision_id: None,
            created_at,
        };
        db.create_item_with_revision(&item, &revision)
            .await
            .expect("create item");

        db.create_job(&Job {
            id: ingot_domain::ids::JobId::new(),
            project_id: project.id,
            item_id,
            item_revision_id: revision_id,
            step_id: step::AUTHOR_INITIAL.into(),
            semantic_attempt_no: 1,
            retry_no: 0,
            supersedes_job_id: None,
            status: JobStatus::Completed,
            outcome_class: Some(OutcomeClass::Clean),
            phase_kind: PhaseKind::Author,
            workspace_id: None,
            workspace_kind: WorkspaceKind::Authoring,
            execution_permission: ExecutionPermission::MayMutate,
            context_policy: ContextPolicy::Fresh,
            phase_template_slug: "author-initial".into(),
            phase_template_digest: None,
            prompt_snapshot: None,
            input_base_commit_oid: None,
            input_head_commit_oid: Some(seed_commit.clone()),
            output_artifact_kind: OutputArtifactKind::Commit,
            output_commit_oid: Some(candidate_head.clone()),
            result_schema_version: None,
            result_payload: None,
            agent_id: None,
            process_pid: None,
            lease_owner_id: None,
            heartbeat_at: None,
            lease_expires_at: None,
            error_code: None,
            error_message: None,
            created_at,
            started_at: Some(created_at),
            ended_at: Some(created_at),
        })
        .await
        .expect("create author job");

        db.create_job(&Job {
            id: ingot_domain::ids::JobId::new(),
            project_id: project.id,
            item_id,
            item_revision_id: revision_id,
            step_id: step::REVIEW_INCREMENTAL_INITIAL.into(),
            semantic_attempt_no: 1,
            retry_no: 0,
            supersedes_job_id: None,
            status: JobStatus::Queued,
            outcome_class: None,
            phase_kind: PhaseKind::Review,
            workspace_id: None,
            workspace_kind: WorkspaceKind::Review,
            execution_permission: ExecutionPermission::MustNotMutate,
            context_policy: ContextPolicy::Fresh,
            phase_template_slug: "review-incremental".into(),
            phase_template_digest: None,
            prompt_snapshot: None,
            input_base_commit_oid: Some(seed_commit.clone()),
            input_head_commit_oid: Some(candidate_head.clone()),
            output_artifact_kind: OutputArtifactKind::ReviewReport,
            output_commit_oid: None,
            result_schema_version: None,
            result_payload: None,
            agent_id: None,
            process_pid: None,
            lease_owner_id: None,
            heartbeat_at: None,
            lease_expires_at: None,
            error_code: None,
            error_message: None,
            created_at,
            started_at: None,
            ended_at: None,
        })
        .await
        .expect("create review job");

        assert!(dispatcher.tick().await.expect("review tick"));

        let jobs = db.list_jobs_by_item(item.id).await.expect("jobs");
        let completed_review = jobs
            .iter()
            .find(|job| job.step_id == step::REVIEW_INCREMENTAL_INITIAL)
            .expect("completed incremental review");
        assert_eq!(completed_review.status, JobStatus::Completed);
        assert_eq!(completed_review.outcome_class, Some(OutcomeClass::Clean));

        let candidate_review = jobs
            .iter()
            .find(|job| job.step_id == step::REVIEW_CANDIDATE_INITIAL)
            .expect("auto-dispatched candidate review");
        assert_eq!(candidate_review.status, JobStatus::Queued);
        assert_eq!(
            candidate_review.input_base_commit_oid.as_deref(),
            Some(seed_commit.as_str())
        );
        assert_eq!(
            candidate_review.input_head_commit_oid.as_deref(),
            Some(candidate_head.as_str())
        );
    }

    #[tokio::test]
    async fn idle_item_auto_dispatches_candidate_review_after_nonblocking_incremental_triage() {
        let repo = temp_git_repo();
        let db_path = std::env::temp_dir().join(format!(
            "ingot-runtime-triage-auto-review-{}.db",
            Uuid::now_v7()
        ));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let dispatcher = JobDispatcher::with_runner(
            db.clone(),
            ProjectLocks::default(),
            DispatcherConfig::new(std::env::temp_dir().join(format!(
                "ingot-runtime-triage-auto-review-state-{}",
                Uuid::now_v7()
            ))),
            Arc::new(FakeRunner),
        );

        let created_at = Utc::now();
        let project = Project {
            id: ingot_domain::ids::ProjectId::new(),
            name: "repo".into(),
            path: repo.display().to_string(),
            default_branch: "main".into(),
            color: "#000".into(),
            created_at,
            updated_at: created_at,
        };
        db.create_project(&project).await.expect("create project");

        let item_id = ingot_domain::ids::ItemId::new();
        let revision_id = ingot_domain::ids::ItemRevisionId::new();
        let seed_commit = head_oid(&repo).await.expect("seed head");
        std::fs::write(repo.join("feature.txt"), "authored change").expect("write feature");
        git_sync(&repo, &["add", "feature.txt"]);
        git_sync(&repo, &["commit", "-m", "author change"]);
        let authored_commit = head_oid(&repo).await.expect("authored head");

        let item = ingot_domain::item::Item {
            id: item_id,
            project_id: project.id,
            classification: Classification::Change,
            workflow_version: "delivery:v1".into(),
            lifecycle_state: LifecycleState::Open,
            parking_state: ParkingState::Active,
            done_reason: None,
            resolution_source: None,
            approval_state: ApprovalState::NotRequested,
            escalation_state: EscalationState::None,
            escalation_reason: None,
            current_revision_id: revision_id,
            origin_kind: OriginKind::Manual,
            origin_finding_id: None,
            priority: Priority::Major,
            labels: vec![],
            operator_notes: None,
            created_at,
            updated_at: created_at,
            closed_at: None,
        };
        let revision = ItemRevision {
            id: revision_id,
            item_id,
            revision_no: 1,
            title: "Auto candidate review".into(),
            description: "auto dispatch candidate review after triage".into(),
            acceptance_criteria: "candidate review queued".into(),
            target_ref: "refs/heads/main".into(),
            approval_policy: ApprovalPolicy::Required,
            policy_snapshot: serde_json::json!({}),
            template_map_snapshot: serde_json::json!({}),
            seed_commit_oid: seed_commit.clone(),
            seed_target_commit_oid: Some(seed_commit.clone()),
            supersedes_revision_id: None,
            created_at,
        };
        db.create_item_with_revision(&item, &revision)
            .await
            .expect("create item");

        let author_job = Job {
            id: ingot_domain::ids::JobId::new(),
            project_id: project.id,
            item_id,
            item_revision_id: revision_id,
            step_id: step::AUTHOR_INITIAL.into(),
            semantic_attempt_no: 1,
            retry_no: 0,
            supersedes_job_id: None,
            status: JobStatus::Completed,
            outcome_class: Some(OutcomeClass::Clean),
            phase_kind: PhaseKind::Author,
            workspace_id: None,
            workspace_kind: WorkspaceKind::Authoring,
            execution_permission: ExecutionPermission::MayMutate,
            context_policy: ContextPolicy::Fresh,
            phase_template_slug: "author-initial".into(),
            phase_template_digest: None,
            prompt_snapshot: None,
            input_base_commit_oid: None,
            input_head_commit_oid: Some(seed_commit.clone()),
            output_artifact_kind: OutputArtifactKind::Commit,
            output_commit_oid: Some(authored_commit.clone()),
            result_schema_version: None,
            result_payload: None,
            agent_id: None,
            process_pid: None,
            lease_owner_id: None,
            heartbeat_at: None,
            lease_expires_at: None,
            error_code: None,
            error_message: None,
            created_at,
            started_at: Some(created_at),
            ended_at: Some(created_at),
        };
        db.create_job(&author_job).await.expect("create author job");

        let review_job = Job {
            id: ingot_domain::ids::JobId::new(),
            project_id: project.id,
            item_id,
            item_revision_id: revision_id,
            step_id: step::REVIEW_INCREMENTAL_INITIAL.into(),
            semantic_attempt_no: 1,
            retry_no: 0,
            supersedes_job_id: None,
            status: JobStatus::Completed,
            outcome_class: Some(OutcomeClass::Findings),
            phase_kind: PhaseKind::Review,
            workspace_id: None,
            workspace_kind: WorkspaceKind::Review,
            execution_permission: ExecutionPermission::MustNotMutate,
            context_policy: ContextPolicy::Fresh,
            phase_template_slug: "review-incremental".into(),
            phase_template_digest: None,
            prompt_snapshot: None,
            input_base_commit_oid: Some(seed_commit.clone()),
            input_head_commit_oid: Some(authored_commit.clone()),
            output_artifact_kind: OutputArtifactKind::ReviewReport,
            output_commit_oid: None,
            result_schema_version: Some("review_report:v1".into()),
            result_payload: Some(serde_json::json!({
                "outcome": "findings",
                "summary": "non-blocking note",
                "review_subject": {
                    "base_commit_oid": seed_commit,
                    "head_commit_oid": authored_commit
                },
                "overall_risk": "low",
                "findings": [{
                    "finding_key": "note",
                    "code": "NOTE001",
                    "severity": "low",
                    "summary": "acceptable note",
                    "paths": ["feature.txt"],
                    "evidence": ["acceptable"]
                }],
                "extensions": null
            })),
            agent_id: None,
            process_pid: None,
            lease_owner_id: None,
            heartbeat_at: None,
            lease_expires_at: None,
            error_code: None,
            error_message: None,
            created_at,
            started_at: Some(created_at),
            ended_at: Some(created_at),
        };
        db.create_job(&review_job).await.expect("create review job");

        db.create_finding(&Finding {
            id: ingot_domain::ids::FindingId::new(),
            project_id: project.id,
            source_item_id: item_id,
            source_item_revision_id: revision_id,
            source_job_id: review_job.id,
            source_step_id: step::REVIEW_INCREMENTAL_INITIAL.into(),
            source_report_schema_version: "review_report:v1".into(),
            source_finding_key: "note".into(),
            source_subject_kind: FindingSubjectKind::Candidate,
            source_subject_base_commit_oid: review_job.input_base_commit_oid.clone(),
            source_subject_head_commit_oid: review_job
                .input_head_commit_oid
                .clone()
                .expect("review head"),
            code: "NOTE001".into(),
            severity: FindingSeverity::Low,
            summary: "acceptable note".into(),
            paths: vec!["feature.txt".into()],
            evidence: serde_json::json!(["acceptable"]),
            triage_state: FindingTriageState::WontFix,
            linked_item_id: None,
            triage_note: Some("accepted for now".into()),
            created_at,
            triaged_at: Some(created_at),
        })
        .await
        .expect("create finding");

        assert!(dispatcher.tick().await.expect("tick should auto-dispatch"));

        let jobs = db.list_jobs_by_item(item.id).await.expect("jobs");
        let candidate_review = jobs
            .iter()
            .find(|job| job.step_id == step::REVIEW_CANDIDATE_INITIAL)
            .expect("auto-dispatched candidate review");
        assert_eq!(candidate_review.status, JobStatus::Queued);
        assert_eq!(
            candidate_review.input_base_commit_oid.as_deref(),
            Some(revision.seed_commit_oid.as_str())
        );
        assert_eq!(
            candidate_review.input_head_commit_oid.as_deref(),
            author_job.output_commit_oid.as_deref()
        );
    }

    #[tokio::test]
    async fn candidate_repair_loop_advances_to_prepare_convergence() {
        let repo = temp_git_repo();
        let db_path = std::env::temp_dir().join(format!(
            "ingot-runtime-candidate-loop-{}.db",
            Uuid::now_v7()
        ));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let dispatcher = JobDispatcher::with_runner(
            db.clone(),
            ProjectLocks::default(),
            DispatcherConfig::new(std::env::temp_dir().join(format!(
                "ingot-runtime-candidate-loop-state-{}",
                Uuid::now_v7()
            ))),
            Arc::new(ScriptedLoopRunner),
        );

        let created_at = Utc::now();
        let project = Project {
            id: ingot_domain::ids::ProjectId::new(),
            name: "repo".into(),
            path: repo.display().to_string(),
            default_branch: "main".into(),
            color: "#000".into(),
            created_at,
            updated_at: created_at,
        };
        db.create_project(&project).await.expect("create project");

        let agent = Agent {
            id: ingot_domain::ids::AgentId::new(),
            slug: "codex".into(),
            name: "Codex".into(),
            adapter_kind: AdapterKind::Codex,
            provider: "openai".into(),
            model: "gpt-5-codex".into(),
            cli_path: "codex".into(),
            capabilities: vec![
                AgentCapability::MutatingJobs,
                AgentCapability::ReadOnlyJobs,
                AgentCapability::StructuredOutput,
            ],
            health_check: Some("ok".into()),
            status: AgentStatus::Available,
        };
        db.create_agent(&agent).await.expect("create agent");

        let item_id = ingot_domain::ids::ItemId::new();
        let revision_id = ingot_domain::ids::ItemRevisionId::new();
        let item = ingot_domain::item::Item {
            id: item_id,
            project_id: project.id,
            classification: Classification::Change,
            workflow_version: "delivery:v1".into(),
            lifecycle_state: LifecycleState::Open,
            parking_state: ParkingState::Active,
            done_reason: None,
            resolution_source: None,
            approval_state: ApprovalState::NotRequested,
            escalation_state: EscalationState::None,
            escalation_reason: None,
            current_revision_id: revision_id,
            origin_kind: OriginKind::Manual,
            origin_finding_id: None,
            priority: Priority::Major,
            labels: vec![],
            operator_notes: None,
            created_at,
            updated_at: created_at,
            closed_at: None,
        };
        let seed_commit = head_oid(&repo).await.expect("seed head");
        let revision = ItemRevision {
            id: revision_id,
            item_id,
            revision_no: 1,
            title: "Candidate loop".into(),
            description: "run candidate loop".into(),
            acceptance_criteria: "prepare convergence".into(),
            target_ref: "refs/heads/main".into(),
            approval_policy: ApprovalPolicy::Required,
            policy_snapshot: serde_json::json!({}),
            template_map_snapshot: serde_json::json!({}),
            seed_commit_oid: seed_commit.clone(),
            seed_target_commit_oid: Some(seed_commit.clone()),
            supersedes_revision_id: None,
            created_at,
        };
        db.create_item_with_revision(&item, &revision)
            .await
            .expect("create item");

        let author_job = dispatch_job(
            &item,
            &revision,
            &[],
            &[],
            &[],
            DispatchJobCommand { step_id: None },
        )
        .expect("dispatch author initial");
        db.create_job(&author_job).await.expect("create author job");
        dispatcher.tick().await.expect("author tick");

        let mut jobs = db.list_jobs_by_item(item.id).await.expect("jobs");
        let review_initial = jobs
            .iter()
            .find(|job| job.step_id == step::REVIEW_INCREMENTAL_INITIAL)
            .cloned()
            .expect("auto-dispatched review initial");
        assert_eq!(review_initial.status, JobStatus::Queued);
        dispatcher.tick().await.expect("review initial tick");

        jobs = db.list_jobs_by_item(item.id).await.expect("jobs");
        let repair_job = dispatch_job(
            &item,
            &revision,
            &jobs,
            &[],
            &[],
            DispatchJobCommand { step_id: None },
        )
        .expect("dispatch repair candidate");
        db.create_job(&repair_job).await.expect("create repair");
        dispatcher.tick().await.expect("repair tick");

        jobs = db.list_jobs_by_item(item.id).await.expect("jobs");
        let review_incremental_repair = jobs
            .iter()
            .find(|job| job.step_id == step::REVIEW_INCREMENTAL_REPAIR)
            .cloned()
            .expect("auto-dispatched review incremental repair");
        assert_eq!(review_incremental_repair.status, JobStatus::Queued);
        dispatcher
            .tick()
            .await
            .expect("review incremental repair tick");

        jobs = db.list_jobs_by_item(item.id).await.expect("jobs");
        let review_candidate_repair = jobs
            .iter()
            .find(|job| job.step_id == step::REVIEW_CANDIDATE_REPAIR)
            .cloned()
            .expect("auto-dispatched review candidate repair");
        assert_eq!(review_candidate_repair.status, JobStatus::Queued);
        dispatcher
            .tick()
            .await
            .expect("review candidate repair tick");

        jobs = db.list_jobs_by_item(item.id).await.expect("jobs");
        let validate_candidate_repair = dispatch_job(
            &item,
            &revision,
            &jobs,
            &[],
            &[],
            DispatchJobCommand { step_id: None },
        )
        .expect("dispatch validate candidate repair");
        db.create_job(&validate_candidate_repair)
            .await
            .expect("create validate candidate repair");
        dispatcher
            .tick()
            .await
            .expect("validate candidate repair tick");

        let jobs = db.list_jobs_by_item(item.id).await.expect("jobs");
        let evaluation = Evaluator::new().evaluate(&item, &revision, &jobs, &[], &[]);
        assert_eq!(evaluation.next_recommended_action, "prepare_convergence");
    }

    fn prompt_value(prompt: &str, label: &str) -> Option<String> {
        prompt.lines().find_map(|line| {
            let prefix = format!("- {label}: ");
            line.strip_prefix(&prefix).map(ToOwned::to_owned)
        })
    }
}
