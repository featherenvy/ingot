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
    ApprovalState, DoneReason, Escalation, EscalationReason, Lifecycle, ResolutionSource,
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
use ingot_workflow::{Evaluator, RecommendedAction, step};
use ingot_workspace::{
    WorkspaceError, ensure_authoring_workspace_state, provision_integration_workspace,
    provision_review_workspace, remove_workspace,
};
use sha2::{Digest, Sha256};
use tokio::time::{interval, sleep};
use tracing::{Instrument, debug, error, info, info_span, warn};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FinalizeCompletionOutcome {
    Blocked,
    Failed,
    Completed,
}

struct FinalizeOperationContext<'a> {
    project: &'a Project,
    item_id: ingot_domain::ids::ItemId,
    revision: &'a ItemRevision,
    mirror_target_ref: &'a str,
    prepared_commit_oid: &'a str,
    paths: &'a ingot_git::project_repo::ProjectRepoPaths,
}

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
                    let convergences = match dispatcher
                        .hydrate_convergences(
                            &project,
                            dispatcher
                                .db
                                .list_convergences_by_item(item.id)
                                .await
                                .map_err(ingot_usecases::UseCaseError::Repository)?,
                        )
                        .await
                    {
                        Ok(convergences) => convergences,
                        Err(error) => {
                            warn!(
                                ?error,
                                project_id = %project.id,
                                item_id = %item.id,
                                "skipping system-action item because convergence hydration failed"
                            );
                            continue;
                        }
                    };
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

    pub async fn refresh_project_mirror(
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
        let _ = self.recover_projected_review_jobs().await?;
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
            self.recover_projected_review_jobs().await?;
            return Ok(true);
        }

        if let Some(job) = self.next_runnable_job().await? {
            if let Some(prepared) = self.prepare_run(job).await? {
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
                            info!(
                                job_id = %prepared.job.id,
                                "job cancelled during runtime execution"
                            );
                            let _ = self.recover_projected_review_jobs().await?;
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
            }
        }

        made_progress |= self.recover_projected_review_jobs().await?;
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
        ingot_usecases::convergence::promote_queue_heads(&self.db, &self.db, project_id)
            .await
            .map_err(|e| RuntimeError::InvalidState(e.to_string()))?;
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

    pub async fn reconcile_active_jobs(&self) -> Result<bool, RuntimeError> {
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
                OperationKind::CreateInvestigationRef => {
                    if let (Some(ref_name), Some(expected_oid)) =
                        (operation.ref_name.as_deref(), operation.new_oid.as_deref())
                    {
                        resolve_ref_oid(repo_path, ref_name).await?.as_deref() == Some(expected_oid)
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
                OperationKind::RemoveInvestigationRef => {
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

    async fn complete_finalize_target_ref_operation(
        &self,
        context: FinalizeOperationContext<'_>,
        operation: &mut GitOperation,
    ) -> Result<FinalizeCompletionOutcome, RuntimeError> {
        let current_target_oid = resolve_ref_oid(
            context.paths.mirror_git_dir.as_path(),
            context.mirror_target_ref,
        )
        .await?;
        if current_target_oid.as_deref() != Some(context.prepared_commit_oid) {
            operation.status = GitOperationStatus::Failed;
            operation.completed_at = Some(Utc::now());
            self.db.update_git_operation(operation).await?;
            return Ok(FinalizeCompletionOutcome::Failed);
        }

        if operation.status == GitOperationStatus::Planned {
            operation.status = GitOperationStatus::Applied;
            operation.completed_at = Some(Utc::now());
            self.db.update_git_operation(operation).await?;
        }

        match checkout_finalization_status(
            Path::new(&context.project.path),
            &context.revision.target_ref,
            context.prepared_commit_oid,
        )
        .await?
        {
            CheckoutFinalizationStatus::Blocked { .. } => {
                self.reconcile_checkout_sync_state(
                    context.project,
                    context.item_id,
                    context.revision,
                )
                .await?;
                Ok(FinalizeCompletionOutcome::Blocked)
            }
            CheckoutFinalizationStatus::NeedsSync => {
                self.reconcile_checkout_sync_state(
                    context.project,
                    context.item_id,
                    context.revision,
                )
                .await?;
                sync_checkout_to_commit(
                    Path::new(&context.project.path),
                    context.paths.mirror_git_dir.as_path(),
                    &context.revision.target_ref,
                    context.prepared_commit_oid,
                )
                .await?;
                self.adopt_finalized_target_ref(operation).await?;
                self.mark_git_operation_reconciled(operation).await?;
                Ok(FinalizeCompletionOutcome::Completed)
            }
            CheckoutFinalizationStatus::Synced => {
                self.reconcile_checkout_sync_state(
                    context.project,
                    context.item_id,
                    context.revision,
                )
                .await?;
                self.adopt_finalized_target_ref(operation).await?;
                self.mark_git_operation_reconciled(operation).await?;
                Ok(FinalizeCompletionOutcome::Completed)
            }
        }
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
            OperationKind::CreateInvestigationRef => Ok(()),
            OperationKind::ResetWorkspace => self.adopt_reset_workspace(operation).await,
            OperationKind::RemoveWorkspaceRef => self.adopt_removed_workspace_ref(operation).await,
            OperationKind::RemoveInvestigationRef => Ok(()),
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
            if !item.lifecycle.is_done() {
                let revision = self.db.get_revision(item.current_revision_id).await?;
                item.lifecycle = Lifecycle::Done {
                    reason: DoneReason::Completed,
                    source: match revision.approval_policy {
                        ingot_domain::revision::ApprovalPolicy::Required => {
                            ResolutionSource::ApprovalCommand
                        }
                        ingot_domain::revision::ApprovalPolicy::NotRequired => {
                            ResolutionSource::SystemCommand
                        }
                    },
                    closed_at: Utc::now(),
                };
                item.approval_state = match revision.approval_policy {
                    ingot_domain::revision::ApprovalPolicy::Required => ApprovalState::Approved,
                    ingot_domain::revision::ApprovalPolicy::NotRequired => {
                        ApprovalState::NotRequired
                    }
                };
            }
            item.escalation = Escalation::None;
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
        let target_ref = target_ref.to_string();
        let prepared_commit_oid = operation
            .new_oid
            .as_deref()
            .or(operation.commit_oid.as_deref())
            .ok_or_else(|| RuntimeError::InvalidState("finalize operation missing new oid".into()))?
            .to_string();
        Ok(!matches!(
            self.complete_finalize_target_ref_operation(
                FinalizeOperationContext {
                    project,
                    item_id: item.id,
                    revision: &revision,
                    mirror_target_ref: &target_ref,
                    prepared_commit_oid: &prepared_commit_oid,
                    paths,
                },
                operation,
            )
            .await?,
            FinalizeCompletionOutcome::Blocked
        ))
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
                error_code: Some("heartbeat_expired".into()),
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
            && item.lifecycle.is_open()
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
                lease_owner_id: self.lease_owner_id.clone(),
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
        if convergences.is_empty() {
            return Ok(convergences);
        }

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
                let expected_head_commit_oid = job
                    .job_input
                    .head_commit_oid()
                    .map(ToOwned::to_owned)
                    .ok_or_else(|| {
                        RuntimeError::InvalidState("integration jobs require job_input head".into())
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
                let head_commit_oid = job
                    .job_input
                    .head_commit_oid()
                    .map(ToOwned::to_owned)
                    .ok_or_else(|| {
                        RuntimeError::InvalidState("review jobs require job_input head".into())
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
                    base_commit_oid: job.job_input.base_commit_oid().map(ToOwned::to_owned),
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

        if let Some(base) = job.job_input.base_commit_oid() {
            prompt.push_str(&format!("- Input base commit: {base}\n"));
        }
        if let Some(head) = job.job_input.head_commit_oid() {
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
                && evaluation.next_recommended_action
                    == RecommendedAction::FinalizePreparedConvergence)
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

        let outcome = self
            .complete_finalize_target_ref_operation(
                FinalizeOperationContext {
                    project: &project,
                    item_id: item.id,
                    revision: &revision,
                    mirror_target_ref: &convergence.target_ref,
                    prepared_commit_oid: &prepared_commit_oid,
                    paths: &paths,
                },
                &mut operation,
            )
            .await?;
        if outcome != FinalizeCompletionOutcome::Completed {
            return Ok(());
        }
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
        if evaluation.next_recommended_action != RecommendedAction::InvalidatePreparedConvergence {
            return Ok(());
        }

        let invalidated = ingot_usecases::convergence::invalidate_prepared_convergence(
            &self.db,
            &self.db,
            &self.db,
            &self.db,
            &mut item,
            &revision,
            &convergences,
        )
        .await
        .map_err(|e| RuntimeError::InvalidState(e.to_string()))?;

        if invalidated {
            info!(item_id = %item.id, "invalidated stale prepared convergence");
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn fail_prepare_convergence_attempt(
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
        escalated_item.escalation = Escalation::OperatorRequired {
            reason: escalation_reason,
        };
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
        let source_head_commit_oid = self
            .current_authoring_head_for_revision_with_workspace(revision, jobs)
            .await?
            .ok_or_else(|| RuntimeError::InvalidState("authoring head commit missing".into()))?;
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

        let source_base_commit_oid = self
            .effective_authoring_base_commit_oid(revision)
            .await?
            .ok_or_else(|| RuntimeError::InvalidState("authoring base commit missing".into()))?;
        let source_commit_oids =
            list_commits_oldest_first(repo_path, &source_base_commit_oid, &source_head_commit_oid)
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
                if matches!(
                    item.escalation,
                    Escalation::OperatorRequired {
                        reason: EscalationReason::CheckoutSyncBlocked
                    }
                ) {
                    item.escalation = Escalation::None;
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
                if !matches!(
                    item.escalation,
                    Escalation::OperatorRequired {
                        reason: EscalationReason::CheckoutSyncBlocked
                    }
                ) {
                    item.escalation = Escalation::OperatorRequired {
                        reason: EscalationReason::CheckoutSyncBlocked,
                    };
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

    async fn recover_projected_review_jobs(&self) -> Result<bool, RuntimeError> {
        let mut dispatched_any = false;

        for project in self.db.list_projects().await? {
            let _guard = self
                .project_locks
                .acquire_project_mutation(project.id)
                .await;
            let items = match self.db.list_items_by_project(project.id).await {
                Ok(items) => items,
                Err(error) => {
                    warn!(
                        ?error,
                        project_id = %project.id,
                        "projected review recovery skipped project"
                    );
                    continue;
                }
            };
            for item in items {
                if !item.lifecycle.is_open() {
                    continue;
                }
                match self
                    .auto_dispatch_projected_review_locked(&project, item.id)
                    .await
                {
                    Ok(dispatched) => {
                        dispatched_any |= dispatched;
                    }
                    Err(error) => {
                        warn!(
                            ?error,
                            project_id = %project.id,
                            item_id = %item.id,
                            "projected review recovery skipped item"
                        );
                    }
                }
            }
        }

        if dispatched_any {
            info!("projected review recovery queued review work");
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

    pub async fn auto_dispatch_projected_review_locked(
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

        let result = ingot_usecases::dispatch::auto_dispatch_review(
            &self.db,
            &self.db,
            &self.db,
            project,
            &item,
            &revision,
            &jobs,
            &findings,
            &convergences,
        )
        .await
        .map_err(|error| {
            RuntimeError::InvalidState(format!("failed to auto-dispatch review: {error}"))
        })?;

        if let Some(job) = result {
            info!(job_id = %job.id, step_id = %job.step_id, item_id = %item.id, "auto-dispatched review");
            Ok(true)
        } else {
            Ok(false)
        }
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

        let error_message_log = error_message.as_deref().unwrap_or("").to_string();
        self.db
            .finish_job_non_success(FinishJobNonSuccessParams {
                job_id: prepared.job.id,
                item_id: prepared.item.id,
                expected_item_revision_id: prepared.job.item_revision_id,
                status,
                outcome_class: Some(outcome_class),
                error_code: Some(error_code.into()),
                error_message,
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
            error_message = %error_message_log,
            "job failed"
        );

        Ok(())
    }

    async fn append_escalation_cleared_activity_if_needed(
        &self,
        prepared: &PreparedRun,
    ) -> Result<(), RuntimeError> {
        if !prepared.item.escalation.is_escalated() {
            return Ok(());
        }

        let item = self.db.get_item(prepared.item.id).await?;
        if item.current_revision_id != prepared.job.item_revision_id
            || item.escalation.is_escalated()
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
        let authoring_head_commit_oid = self
            .current_authoring_head_for_revision_with_workspace(&revision, &jobs)
            .await?;
        let authoring_base_commit_oid = self.effective_authoring_base_commit_oid(&revision).await?;
        let changed_paths = if let (Some(base_commit_oid), Some(head_commit_oid)) = (
            authoring_base_commit_oid.as_deref(),
            authoring_head_commit_oid.as_deref(),
        ) {
            changed_paths_between(
                self.project_paths(&project).mirror_git_dir.as_path(),
                base_commit_oid,
                head_commit_oid,
            )
            .await?
        } else {
            Vec::new()
        };
        let context = rebuild_revision_context(
            &item,
            &revision,
            &jobs,
            authoring_head_commit_oid,
            changed_paths,
            updated_from_job_id,
            Utc::now(),
        );
        self.db.upsert_revision_context(&context).await?;
        Ok(())
    }

    async fn current_authoring_head_for_revision_with_workspace(
        &self,
        revision: &ItemRevision,
        jobs: &[Job],
    ) -> Result<Option<String>, RuntimeError> {
        let workspace = self
            .db
            .find_authoring_workspace_for_revision(revision.id)
            .await?;
        Ok(
            ingot_usecases::dispatch::current_authoring_head_for_revision_with_workspace(
                revision,
                jobs,
                workspace.as_ref(),
            ),
        )
    }

    async fn effective_authoring_base_commit_oid(
        &self,
        revision: &ItemRevision,
    ) -> Result<Option<String>, RuntimeError> {
        let workspace = self
            .db
            .find_authoring_workspace_for_revision(revision.id)
            .await?;
        Ok(
            ingot_usecases::dispatch::effective_authoring_base_commit_oid(
                revision,
                workspace.as_ref(),
            ),
        )
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
    ingot_usecases::dispatch::failure_escalation_reason(job, outcome_class)
}

fn should_clear_item_escalation_on_success(item: &ingot_domain::item::Item, job: &Job) -> bool {
    ingot_usecases::dispatch::should_clear_item_escalation_on_success(item, job)
}

fn is_closure_relevant_job(job: &Job) -> bool {
    ingot_usecases::dispatch::is_closure_relevant_job(job)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
