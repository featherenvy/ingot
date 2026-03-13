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
use ingot_git::commit::{JobCommitTrailers, create_daemon_job_commit, working_tree_has_changes};
use ingot_git::diff::changed_paths_between;
use ingot_store_sqlite::{Database, FinishJobNonSuccessParams, StartJobExecutionParams};
use ingot_usecases::{
    CompleteJobCommand, CompleteJobService, ProjectLocks, rebuild_revision_context,
};
use ingot_workflow::{ClosureRelevance, Evaluator, step};
use ingot_workspace::{
    WorkspaceError, ensure_authoring_workspace_state, provision_integration_workspace,
    provision_review_workspace, remove_workspace, workspace_root_path,
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

#[derive(Debug, Clone)]
struct PreparedRun {
    job: Job,
    item: ingot_domain::item::Item,
    revision: ItemRevision,
    project: Project,
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

    pub async fn run_forever(&self) {
        loop {
            if let Err(error) = self.tick().await {
                error!(?error, "authoring job dispatcher tick failed");
            }

            sleep(self.config.poll_interval).await;
        }
    }

    pub async fn reconcile_startup(&self) -> Result<(), RuntimeError> {
        self.reconcile_git_operations().await?;
        self.reconcile_active_jobs().await?;
        self.reconcile_active_convergences().await?;
        self.reconcile_workspace_retention().await?;
        while self.tick_system_action().await? {}
        Ok(())
    }

    pub async fn tick(&self) -> Result<bool, RuntimeError> {
        if self.tick_system_action().await? {
            return Ok(true);
        }

        let Some(job) = self.next_runnable_job().await? else {
            return Ok(false);
        };

        let Some(prepared) = self.prepare_run(job).await? else {
            return Ok(false);
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

        Ok(true)
    }

    async fn tick_system_action(&self) -> Result<bool, RuntimeError> {
        for project in self.db.list_projects().await? {
            let items = self.db.list_items_by_project(project.id).await?;
            for item in items {
                let revision = self.db.get_revision(item.current_revision_id).await?;
                let jobs = self.db.list_jobs_by_item(item.id).await?;
                let convergences = self
                    .hydrate_convergences(
                        &project,
                        self.db.list_convergences_by_item(item.id).await?,
                    )
                    .await?;
                let evaluation = Evaluator::new().evaluate(&item, &revision, &jobs, &convergences);

                match evaluation.next_recommended_action.as_str() {
                    "finalize_prepared_convergence" => {
                        self.auto_finalize_prepared_convergence(project.id, item.id)
                            .await?;
                        return Ok(true);
                    }
                    "invalidate_prepared_convergence" => {
                        self.invalidate_prepared_convergence(project.id, item.id)
                            .await?;
                        return Ok(true);
                    }
                    _ => {}
                }
            }
        }

        Ok(false)
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

    async fn reconcile_active_jobs(&self) -> Result<(), RuntimeError> {
        let active_jobs = self.db.list_active_jobs().await?;
        for job in active_jobs {
            match job.status {
                JobStatus::Assigned => self.reconcile_assigned_job(job).await?,
                JobStatus::Running => self.reconcile_running_job(job).await?,
                _ => {}
            }
        }
        Ok(())
    }

    async fn reconcile_git_operations(&self) -> Result<(), RuntimeError> {
        let operations = self.db.list_unresolved_git_operations().await?;
        for mut operation in operations {
            let project = self.db.get_project(operation.project_id).await?;
            let repo_path = Path::new(&project.path);
            let reconciled = match operation.operation_kind {
                OperationKind::FinalizeTargetRef => {
                    if let (Some(ref_name), Some(new_oid)) =
                        (operation.ref_name.as_deref(), operation.new_oid.as_deref())
                    {
                        resolve_ref_oid(repo_path, ref_name).await?.as_deref() == Some(new_oid)
                    } else {
                        false
                    }
                }
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

            operation.status = if reconciled {
                GitOperationStatus::Reconciled
            } else {
                GitOperationStatus::Failed
            };
            operation.completed_at = Some(Utc::now());
            self.db.update_git_operation(&operation).await?;
            if reconciled {
                self.adopt_reconciled_git_operation(&operation).await?;
                self.append_activity(
                    operation.project_id,
                    ActivityEventType::GitOperationReconciled,
                    "git_operation",
                    operation.id.to_string(),
                    serde_json::json!({ "operation_kind": operation.operation_kind }),
                )
                .await?;
            }
        }
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

        let mut item = self.db.get_item(convergence.item_id).await?;
        if item.current_revision_id == convergence.item_revision_id
            && item.lifecycle_state != LifecycleState::Done
        {
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
                ingot_domain::revision::ApprovalPolicy::NotRequired => ApprovalState::NotRequired,
            };
            item.closed_at.get_or_insert_with(Utc::now);
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

    async fn reconcile_active_convergences(&self) -> Result<(), RuntimeError> {
        let active_convergences = self.db.list_active_convergences().await?;
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
        }
        Ok(())
    }

    async fn reconcile_workspace_retention(&self) -> Result<(), RuntimeError> {
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
            }
        }
        Ok(())
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
                && finding.triage_state == FindingTriageState::Untriaged
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
        let path = Path::new(&workspace.path);
        if path.exists() {
            remove_workspace(Path::new(&project.path), path).await?;
        }

        if let Some(workspace_ref) = workspace.workspace_ref.as_deref()
            && let Some(current_oid) =
                resolve_ref_oid(Path::new(&project.path), workspace_ref).await?
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
            delete_ref(Path::new(&project.path), workspace_ref).await?;
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
        let mut hydrated = Vec::with_capacity(convergences.len());
        for mut convergence in convergences {
            convergence.target_head_valid = self
                .compute_target_head_valid(Path::new(&project.path), &convergence)
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
        let Some(expected_target_oid) = convergence.input_target_commit_oid.as_deref() else {
            return Ok(None);
        };
        let resolved = resolve_ref_oid(repo_path, &convergence.target_ref).await?;
        Ok(Some(resolved.as_deref() == Some(expected_target_oid)))
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
            .prepare_workspace(&project, &revision, &job, now)
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
                    Path::new(&project.path),
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
                    Path::new(&project.path),
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
                let workspace_path =
                    workspace_root_path(Path::new(&project.path)).join(workspace_id.to_string());
                let provisioned = provision_review_workspace(
                    Path::new(&project.path),
                    &workspace_path,
                    &head_commit_oid,
                )
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
        let mut item = self.db.get_item(item_id).await?;
        let revision = self.db.get_revision(item.current_revision_id).await?;
        let jobs = self.db.list_jobs_by_item(item.id).await?;
        let convergences = self
            .hydrate_convergences(&project, self.db.list_convergences_by_item(item.id).await?)
            .await?;
        let evaluation = Evaluator::new().evaluate(&item, &revision, &jobs, &convergences);
        if evaluation.next_recommended_action != "finalize_prepared_convergence" {
            return Ok(());
        }

        let mut convergence = convergences
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

        let mut operation = GitOperation {
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
        compare_and_swap_ref(
            Path::new(&project.path),
            &convergence.target_ref,
            &prepared_commit_oid,
            &input_target_commit_oid,
        )
        .await?;

        operation.status = GitOperationStatus::Applied;
        operation.completed_at = Some(Utc::now());
        self.db.update_git_operation(&operation).await?;

        convergence.status = ConvergenceStatus::Finalized;
        convergence.final_target_commit_oid = Some(prepared_commit_oid.clone());
        convergence.completed_at = Some(Utc::now());
        self.db.update_convergence(&convergence).await?;

        if let Some(workspace_id) = convergence.integration_workspace_id {
            let workspace = self.db.get_workspace(workspace_id).await?;
            self.finalize_integration_workspace_after_close(&project, &workspace)
                .await?;
        }

        item.lifecycle_state = LifecycleState::Done;
        item.done_reason = Some(DoneReason::Completed);
        item.resolution_source = Some(ResolutionSource::SystemCommand);
        item.closed_at = Some(Utc::now());
        item.updated_at = Utc::now();
        self.db.update_item(&item).await?;
        self.append_activity(
            project_id,
            ActivityEventType::ConvergenceFinalized,
            "convergence",
            convergence.id.to_string(),
            serde_json::json!({ "item_id": item.id }),
        )
        .await?;

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
        let convergences = self
            .hydrate_convergences(&project, self.db.list_convergences_by_item(item.id).await?)
            .await?;
        let evaluation = Evaluator::new().evaluate(&item, &revision, &jobs, &convergences);
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

        item.approval_state = match revision.approval_policy {
            ingot_domain::revision::ApprovalPolicy::Required => ApprovalState::NotRequested,
            ingot_domain::revision::ApprovalPolicy::NotRequired => ApprovalState::NotRequired,
        };
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

    async fn verify_mutating_workspace_protocol(
        &self,
        prepared: &PreparedRun,
    ) -> Result<(), RuntimeError> {
        let repo_path = Path::new(&prepared.project.path);
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

        let repo_path = Path::new(&prepared.project.path);
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

        info!(job_id = %prepared.job.id, commit_oid, "completed authoring job");

        Ok(())
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
                    Path::new(&prepared.project.path),
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
        remove_workspace(Path::new(&project.path), Path::new(&workspace.path)).await?;
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
                        Path::new(&prepared.project.path),
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
                        Path::new(&prepared.project.path),
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
                    Path::new(&prepared.project.path),
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
            Path::new(&project.path),
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
            "extensions": { "type": ["object", "null"] }
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
            "extensions": { "type": ["object", "null"] }
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
            "extensions": { "type": ["object", "null"] }
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
    use super::*;
    use chrono::Utc;
    use ingot_domain::agent::{AdapterKind, Agent, AgentCapability, AgentStatus};
    use ingot_domain::convergence::ConvergenceStrategy;
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
            schema_property_type(&validation_schema, "extensions"),
            Some(serde_json::json!(["object", "null"]))
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
        schema
            .get("properties")
            .and_then(|value| value.get(property))
            .and_then(|value| value.get("type"))
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
            input_head_commit_oid: Some(seed_commit.clone()),
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
        git_sync(
            &repo,
            &[
                "update-ref",
                "refs/ingot/workspaces/wrk_integration_test",
                &prepared_commit,
            ],
        );
        git_sync(&repo, &["reset", "--hard", &base_commit]);
        let integration_workspace_path =
            std::env::temp_dir().join(format!("ingot-runtime-integration-{}", Uuid::now_v7()));
        git_sync(
            &repo,
            &[
                "worktree",
                "add",
                "--detach",
                integration_workspace_path.to_str().expect("workspace path"),
                "refs/ingot/workspaces/wrk_integration_test",
            ],
        );

        let db_path =
            std::env::temp_dir().join(format!("ingot-runtime-finalize-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let dispatcher = JobDispatcher::with_runner(
            db.clone(),
            ProjectLocks::default(),
            DispatcherConfig::new(
                std::env::temp_dir()
                    .join(format!("ingot-runtime-finalize-state-{}", Uuid::now_v7())),
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
            input_head_commit_oid: Some(seed_commit.clone()),
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
            input_head_commit_oid: Some(seed_commit),
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
            seed_commit_oid: seed_commit.clone(),
            seed_target_commit_oid: Some(seed_commit.clone()),
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
            input_head_commit_oid: Some(seed_commit.clone()),
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
            input_head_commit_oid: Some(seed_commit),
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
        let evaluation = Evaluator::new().evaluate(&updated_item, &revision, &jobs, &[]);
        assert_eq!(
            evaluation.dispatchable_step_id.as_deref(),
            Some(step::REVIEW_INCREMENTAL_INITIAL)
        );

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
            expected_old_oid: Some(base_commit),
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
        git_sync(
            &repo,
            &["update-ref", "refs/ingot/workspaces/remove-adopt", &head],
        );
        delete_ref(&repo, "refs/ingot/workspaces/remove-adopt")
            .await
            .expect("delete ref");

        let db_path =
            std::env::temp_dir().join(format!("ingot-runtime-remove-adopt-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let dispatcher = JobDispatcher::with_runner(
            db.clone(),
            ProjectLocks::default(),
            DispatcherConfig::new(std::env::temp_dir().join(format!(
                "ingot-runtime-remove-adopt-state-{}",
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
            std::env::temp_dir().join(format!("ingot-runtime-cleanup-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let dispatcher = JobDispatcher::with_runner(
            db.clone(),
            ProjectLocks::default(),
            DispatcherConfig::new(
                std::env::temp_dir()
                    .join(format!("ingot-runtime-cleanup-state-{}", Uuid::now_v7())),
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
            "ingot-runtime-author-cleanup-{}.db",
            Uuid::now_v7()
        ));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let dispatcher = JobDispatcher::with_runner(
            db.clone(),
            ProjectLocks::default(),
            DispatcherConfig::new(std::env::temp_dir().join(format!(
                "ingot-runtime-author-cleanup-state-{}",
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
            promoted_item_id: None,
            dismissal_reason: None,
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
            promoted_item_id: None,
            dismissal_reason: None,
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
            DispatchJobCommand { step_id: None },
        )
        .expect("dispatch author initial");
        db.create_job(&author_job).await.expect("create author job");
        dispatcher.tick().await.expect("author tick");

        let mut jobs = db.list_jobs_by_item(item.id).await.expect("jobs");
        let review_initial = dispatch_job(
            &item,
            &revision,
            &jobs,
            &[],
            DispatchJobCommand { step_id: None },
        )
        .expect("dispatch review incremental initial");
        db.create_job(&review_initial)
            .await
            .expect("create review initial");
        dispatcher.tick().await.expect("review initial tick");

        jobs = db.list_jobs_by_item(item.id).await.expect("jobs");
        let repair_job = dispatch_job(
            &item,
            &revision,
            &jobs,
            &[],
            DispatchJobCommand { step_id: None },
        )
        .expect("dispatch repair candidate");
        db.create_job(&repair_job).await.expect("create repair");
        dispatcher.tick().await.expect("repair tick");

        jobs = db.list_jobs_by_item(item.id).await.expect("jobs");
        let review_incremental_repair = dispatch_job(
            &item,
            &revision,
            &jobs,
            &[],
            DispatchJobCommand { step_id: None },
        )
        .expect("dispatch review incremental repair");
        db.create_job(&review_incremental_repair)
            .await
            .expect("create review incremental repair");
        dispatcher
            .tick()
            .await
            .expect("review incremental repair tick");

        jobs = db.list_jobs_by_item(item.id).await.expect("jobs");
        let review_candidate_repair = dispatch_job(
            &item,
            &revision,
            &jobs,
            &[],
            DispatchJobCommand { step_id: None },
        )
        .expect("dispatch review candidate repair");
        db.create_job(&review_candidate_repair)
            .await
            .expect("create review candidate repair");
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
        let evaluation = Evaluator::new().evaluate(&item, &revision, &jobs, &[]);
        assert_eq!(evaluation.next_recommended_action, "prepare_convergence");
    }

    fn prompt_value(prompt: &str, label: &str) -> Option<String> {
        prompt.lines().find_map(|line| {
            let prefix = format!("- {label}: ");
            line.strip_prefix(&prefix).map(ToOwned::to_owned)
        })
    }
}
