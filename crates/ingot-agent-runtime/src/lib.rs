mod bootstrap;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use chrono::{Duration as ChronoDuration, Utc};
use glob::glob;
use ingot_agent_adapters::claude_code::ClaudeCodeCliAdapter;
use ingot_agent_adapters::codex::CodexCliAdapter;
use ingot_agent_protocol::adapter::{AgentAdapter, AgentError};
use ingot_agent_protocol::request::AgentRequest;
use ingot_agent_protocol::response::AgentResponse;
use ingot_domain::activity::{Activity, ActivityEventType, ActivitySubject};
use ingot_domain::agent::{AdapterKind, Agent, AgentCapability, AgentStatus};
use ingot_domain::commit_oid::CommitOid;
use ingot_domain::convergence::{Convergence, ConvergenceStatus, PrepareFailureKind};
use ingot_domain::convergence_queue::{ConvergenceQueueEntry, ConvergenceQueueEntryStatus};
use ingot_domain::finding::FindingTriageState;
use ingot_domain::git_operation::{
    ConvergenceReplayMetadata, GitOperation, GitOperationEntityRef, GitOperationStatus,
    OperationKind, OperationPayload,
};
use ingot_domain::git_ref::GitRef;
use ingot_domain::harness::{HarnessCommand, HarnessProfile, HarnessProfileError};
use ingot_domain::ids::{GitOperationId, WorkspaceId};
use ingot_domain::item::{
    ApprovalState, DoneReason, Escalation, EscalationReason, Lifecycle, ResolutionSource,
};
use ingot_domain::job::{
    ExecutionPermission, Job, JobAssignment, JobState, JobStatus, OutcomeClass, OutputArtifactKind,
    PhaseKind,
};
use ingot_domain::ports::{JobCompletionMutation, ProjectMutationLockPort, RepositoryError};
use ingot_domain::project::Project;
use ingot_domain::revision::ItemRevision;
use ingot_domain::revision_context::RevisionContext;
use ingot_domain::step_id::StepId;
use ingot_domain::workspace::{
    RetentionPolicy, Workspace, WorkspaceCommitState, WorkspaceKind, WorkspaceState,
    WorkspaceStatus, WorkspaceStrategy,
};
use ingot_git::GitJobCompletionPort;
use ingot_git::commands::{
    FinalizeTargetRefOutcome, GitCommandError, delete_ref,
    finalize_target_ref as finalize_target_ref_in_repo, git, head_oid, resolve_ref_oid,
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
use ingot_store_sqlite::{
    ClaimQueuedAgentJobExecutionParams, Database, FinishJobNonSuccessParams,
    StartJobExecutionParams,
};
use ingot_usecases::convergence::{
    CheckoutFinalizationReadiness, ConvergenceSystemActionPort, FinalizationTarget,
    FinalizePreparedTrigger, FinalizeTargetRefResult, PreparedConvergenceFinalizePort,
    SystemActionItemState, SystemActionProjectState, finalize_prepared_convergence,
};
use ingot_usecases::job::{DispatchJobCommand, dispatch_job};
use ingot_usecases::reconciliation::ReconciliationPort;
use ingot_usecases::{
    CompleteJobCommand, CompleteJobService, ConvergenceService, DispatchNotify, ProjectLocks,
    ReconciliationService, rebuild_revision_context,
};
use ingot_workflow::{Evaluator, RecommendedAction, step};
use ingot_workspace::{
    WorkspaceError, ensure_authoring_workspace_state, provision_integration_workspace,
    provision_review_workspace, remove_workspace,
};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, TryAcquireError};
use tokio::task::{Id as TaskId, JoinError, JoinSet};
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
    mirror_target_ref: &'a GitRef,
    prepared_commit_oid: &'a CommitOid,
    paths: &'a ingot_git::project_repo::ProjectRepoPaths,
}

#[derive(Debug, Clone)]
pub struct DispatcherConfig {
    pub state_root: PathBuf,
    pub poll_interval: Duration,
    pub heartbeat_interval: Duration,
    pub lease_ttl: Duration,
    pub job_timeout: Duration,
    pub max_concurrent_jobs: usize,
}

impl DispatcherConfig {
    pub fn new(state_root: PathBuf) -> Self {
        Self {
            state_root,
            poll_interval: Duration::from_secs(1),
            heartbeat_interval: Duration::from_secs(5),
            lease_ttl: Duration::from_secs(30),
            job_timeout: Duration::from_secs(30 * 60),
            max_concurrent_jobs: 2,
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
    dispatch_notify: DispatchNotify,
    #[cfg(test)]
    pre_spawn_pause_hook: Option<PreSpawnPauseHook>,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreSpawnPausePoint {
    AgentBeforeSpawn,
    HarnessBeforeSpawn,
}

#[cfg(test)]
#[derive(Clone)]
struct PreSpawnPauseHook {
    point: PreSpawnPausePoint,
    state: Arc<PreSpawnPauseState>,
}

#[cfg(test)]
struct PreSpawnPauseState {
    entered: std::sync::Mutex<usize>,
    released: std::sync::Mutex<bool>,
    entered_notify: tokio::sync::Notify,
    release_notify: tokio::sync::Notify,
}

#[cfg(test)]
impl PreSpawnPauseHook {
    fn new(point: PreSpawnPausePoint) -> Self {
        Self {
            point,
            state: Arc::new(PreSpawnPauseState {
                entered: std::sync::Mutex::new(0),
                released: std::sync::Mutex::new(false),
                entered_notify: tokio::sync::Notify::new(),
                release_notify: tokio::sync::Notify::new(),
            }),
        }
    }

    async fn pause_if_matching(&self, point: PreSpawnPausePoint) {
        if self.point != point {
            return;
        }

        {
            let mut entered = self.state.entered.lock().expect("pause hook entered lock");
            *entered += 1;
        }
        self.state.entered_notify.notify_waiters();

        loop {
            if *self
                .state
                .released
                .lock()
                .expect("pause hook released lock")
            {
                return;
            }
            self.state.release_notify.notified().await;
        }
    }

    async fn wait_until_entered(&self, expected: usize, timeout_duration: Duration) {
        tokio::time::timeout(timeout_duration, async {
            loop {
                if *self.state.entered.lock().expect("pause hook entered lock") >= expected {
                    return;
                }
                self.state.entered_notify.notified().await;
            }
        })
        .await
        .expect("timed out waiting for pre-spawn pause hook");
    }

    fn release(&self) {
        *self
            .state
            .released
            .lock()
            .expect("pause hook released lock") = true;
        self.state.release_notify.notify_waiters();
    }
}

#[derive(Clone)]
struct RuntimeConvergencePort {
    dispatcher: JobDispatcher,
}

#[derive(Clone)]
struct RuntimeFinalizePort {
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

async fn drain_until_idle<F, Fut>(mut step: F) -> Result<(), RuntimeError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<bool, RuntimeError>>,
{
    while step().await? {}
    Ok(())
}

#[derive(Debug, Clone)]
struct PreparedRun {
    job: Job,
    item: ingot_domain::item::Item,
    revision: ItemRevision,
    project: Project,
    canonical_repo_path: PathBuf,
    agent: Agent,
    assignment: JobAssignment,
    workspace: Workspace,
    original_head_commit_oid: CommitOid,
    prompt: String,
    workspace_lifecycle: WorkspaceLifecycle,
}

enum PrepareRunOutcome {
    NotPrepared,
    FailedBeforeLaunch,
    Prepared(Box<PreparedRun>),
}

#[derive(Debug, Clone)]
struct PreparedHarnessValidation {
    harness: HarnessProfile,
    job_id: ingot_domain::ids::JobId,
    item_id: ingot_domain::ids::ItemId,
    project_id: ingot_domain::ids::ProjectId,
    revision_id: ingot_domain::ids::ItemRevisionId,
    workspace_id: WorkspaceId,
    workspace_path: PathBuf,
    step_id: ingot_domain::step_id::StepId,
}

enum PrepareHarnessValidationOutcome {
    NotPrepared,
    FailedBeforeLaunch,
    Prepared(Box<PreparedHarnessValidation>),
}

#[derive(Debug, Clone, Copy, Default)]
struct NonJobWorkProgress {
    made_progress: bool,
    system_actions_progressed: bool,
}

struct RunningJobResult {
    job_id: ingot_domain::ids::JobId,
    result: Result<(), RuntimeError>,
}

#[derive(Debug, Clone)]
enum RunningJobMeta {
    Agent(Box<PreparedRun>),
    HarnessValidation(PreparedHarnessValidation),
}

impl RunningJobMeta {
    fn job_id(&self) -> ingot_domain::ids::JobId {
        match self {
            Self::Agent(prepared) => prepared.job.id,
            Self::HarnessValidation(prepared) => prepared.job_id,
        }
    }
}

enum AgentRunOutcome {
    Completed(AgentResponse),
    TimedOut,
    Cancelled,
    OwnershipLostBeforeSpawn,
    OwnershipLostDuringRun,
    LaunchFailed(AgentError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkspaceLifecycle {
    PersistentAuthoring,
    PersistentIntegration,
    EphemeralReview,
}

#[derive(Debug, Clone, Default)]
struct HarnessPromptContext {
    commands: Vec<HarnessCommand>,
    skills: Vec<ResolvedHarnessSkill>,
}

#[derive(Debug, Clone)]
struct ResolvedHarnessSkill {
    relative_path: String,
    contents: String,
}

#[derive(Debug, thiserror::Error)]
enum HarnessLoadError {
    #[error("failed to read harness profile at {}: {source}", path.display())]
    ReadProfile { path: PathBuf, source: io::Error },
    #[error("invalid harness profile at {}: {source}", path.display())]
    InvalidProfile {
        path: PathBuf,
        source: HarnessProfileError,
    },
    #[error("failed to canonicalize project path {}: {source}", path.display())]
    CanonicalizeProjectPath { path: PathBuf, source: io::Error },
    #[error("invalid harness skill glob '{pattern}': {message}")]
    InvalidSkillGlob { pattern: String, message: String },
    #[error("failed to resolve harness skill path from pattern '{pattern}': {source}")]
    ResolveSkillPath { pattern: String, source: io::Error },
    #[error(
        "harness skill path from pattern '{pattern}' escapes project root {}: {}",
        project_path.display(),
        path.display()
    )]
    SkillPathEscapesProjectRoot {
        pattern: String,
        project_path: PathBuf,
        path: PathBuf,
    },
    #[error("failed to read harness skill {}: {source}", path.display())]
    ReadSkill { path: PathBuf, source: io::Error },
}

impl HarnessLoadError {
    fn error_code(&self) -> &'static str {
        match self {
            Self::InvalidProfile { .. }
            | Self::InvalidSkillGlob { .. }
            | Self::SkillPathEscapesProjectRoot { .. } => "invalid_harness_profile",
            Self::ReadProfile { .. }
            | Self::CanonicalizeProjectPath { .. }
            | Self::ResolveSkillPath { .. }
            | Self::ReadSkill { .. } => "harness_io_error",
        }
    }
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

    fn auto_finalize_prepared_convergence(
        &self,
        project_id: ingot_domain::ids::ProjectId,
        item_id: ingot_domain::ids::ItemId,
    ) -> impl Future<Output = Result<bool, ingot_usecases::UseCaseError>> + Send {
        let dispatcher = self.dispatcher.clone();
        async move {
            dispatcher
                .auto_finalize_prepared_convergence(project_id, item_id)
                .await
                .map_err(usecase_from_runtime_error)
        }
    }
}

impl PreparedConvergenceFinalizePort for RuntimeFinalizePort {
    fn find_or_create_finalize_operation(
        &self,
        operation: &GitOperation,
    ) -> impl Future<Output = Result<GitOperation, ingot_usecases::UseCaseError>> + Send {
        let dispatcher = self.dispatcher.clone();
        let operation = operation.clone();
        async move {
            let GitOperationEntityRef::Convergence(convergence_id) = &operation.entity else {
                return Err(ingot_usecases::UseCaseError::Internal(format!(
                    "expected convergence entity, got {:?}", operation.entity.entity_type()
                )));
            };
            let convergence_id = *convergence_id;
            if let Some(existing) = dispatcher
                .db
                .find_unresolved_finalize_for_convergence(convergence_id)
                .await
                .map_err(ingot_usecases::UseCaseError::Repository)?
            {
                return Ok(existing);
            }

            match dispatcher.db.create_git_operation(&operation).await {
                Ok(()) => {
                    dispatcher
                        .append_activity(
                            operation.project_id,
                            ActivityEventType::GitOperationPlanned,
                            ActivitySubject::GitOperation(operation.id),
                            serde_json::json!({
                                "operation_kind": operation.operation_kind(),
                                "entity_id": operation.entity.entity_id_string(),
                            }),
                        )
                        .await
                        .map_err(usecase_from_runtime_error)?;
                    Ok(operation)
                }
                Err(RepositoryError::Conflict(_)) => dispatcher
                    .db
                    .find_unresolved_finalize_for_convergence(convergence_id)
                    .await
                    .map_err(ingot_usecases::UseCaseError::Repository)?
                    .ok_or_else(|| {
                        ingot_usecases::UseCaseError::Internal(
                            "finalize git operation conflict without existing row".into(),
                        )
                    }),
                Err(other) => Err(ingot_usecases::UseCaseError::Repository(other)),
            }
        }
    }

    fn finalize_target_ref(
        &self,
        project: &Project,
        convergence: &Convergence,
    ) -> impl Future<Output = Result<FinalizeTargetRefResult, ingot_usecases::UseCaseError>> + Send
    {
        let dispatcher = self.dispatcher.clone();
        let project = project.clone();
        let convergence = convergence.clone();
        async move {
            let paths = dispatcher
                .refresh_project_mirror(&project)
                .await
                .map_err(usecase_from_runtime_error)?;
            let prepared_commit_oid = convergence
                .state
                .prepared_commit_oid()
                .map(ToOwned::to_owned)
                .ok_or_else(|| {
                    ingot_usecases::UseCaseError::Internal("prepared commit missing".into())
                })?;
            let input_target_commit_oid = convergence
                .state
                .input_target_commit_oid()
                .map(ToOwned::to_owned)
                .ok_or_else(|| {
                    ingot_usecases::UseCaseError::Internal("input target commit missing".into())
                })?;
            match finalize_target_ref_in_repo(
                paths.mirror_git_dir.as_path(),
                &convergence.target_ref,
                &prepared_commit_oid,
                &input_target_commit_oid,
            )
            .await
            .map_err(|error| usecase_from_runtime_error(RuntimeError::from(error)))?
            {
                FinalizeTargetRefOutcome::AlreadyFinalized => {
                    Ok(FinalizeTargetRefResult::AlreadyFinalized)
                }
                FinalizeTargetRefOutcome::UpdatedNow => Ok(FinalizeTargetRefResult::UpdatedNow),
                FinalizeTargetRefOutcome::Stale => Ok(FinalizeTargetRefResult::Stale),
            }
        }
    }

    fn checkout_finalization_readiness(
        &self,
        project: &Project,
        item: &ingot_domain::item::Item,
        revision: &ItemRevision,
        prepared_commit_oid: &CommitOid,
    ) -> impl Future<Output = Result<CheckoutFinalizationReadiness, ingot_usecases::UseCaseError>> + Send
    {
        let dispatcher = self.dispatcher.clone();
        let project = project.clone();
        let revision = revision.clone();
        let prepared_commit_oid = prepared_commit_oid.clone();
        async move {
            match dispatcher
                .reconcile_checkout_sync_state(&project, item.id, &revision)
                .await
                .map_err(usecase_from_runtime_error)?
            {
                CheckoutSyncStatus::Blocked { message, .. } => {
                    Ok(CheckoutFinalizationReadiness::Blocked { message })
                }
                CheckoutSyncStatus::Ready => match checkout_finalization_status(
                    &project.path,
                    &revision.target_ref,
                    &prepared_commit_oid,
                )
                .await
                .map_err(|error| usecase_from_runtime_error(RuntimeError::from(error)))?
                {
                    CheckoutFinalizationStatus::Blocked { message, .. } => {
                        Ok(CheckoutFinalizationReadiness::Blocked { message })
                    }
                    CheckoutFinalizationStatus::NeedsSync => {
                        Ok(CheckoutFinalizationReadiness::NeedsSync)
                    }
                    CheckoutFinalizationStatus::Synced => Ok(CheckoutFinalizationReadiness::Synced),
                },
            }
        }
    }

    fn sync_checkout_to_prepared_commit(
        &self,
        project: &Project,
        revision: &ItemRevision,
        prepared_commit_oid: &CommitOid,
    ) -> impl Future<Output = Result<(), ingot_usecases::UseCaseError>> + Send {
        let dispatcher = self.dispatcher.clone();
        let project = project.clone();
        let revision = revision.clone();
        let prepared_commit_oid = prepared_commit_oid.clone();
        async move {
            let paths = dispatcher
                .refresh_project_mirror(&project)
                .await
                .map_err(usecase_from_runtime_error)?;
            sync_checkout_to_commit(
                &project.path,
                paths.mirror_git_dir.as_path(),
                &revision.target_ref,
                &prepared_commit_oid,
            )
            .await
            .map_err(|error| usecase_from_runtime_error(RuntimeError::from(error)))?;
            Ok(())
        }
    }

    fn update_git_operation(
        &self,
        operation: &GitOperation,
    ) -> impl Future<Output = Result<(), ingot_usecases::UseCaseError>> + Send {
        let dispatcher = self.dispatcher.clone();
        let operation = operation.clone();
        async move {
            dispatcher
                .db
                .update_git_operation(&operation)
                .await
                .map_err(ingot_usecases::UseCaseError::Repository)?;
            Ok(())
        }
    }

    fn apply_successful_finalization(
        &self,
        _trigger: FinalizePreparedTrigger,
        project: &Project,
        item: &ingot_domain::item::Item,
        _revision: &ItemRevision,
        target: FinalizationTarget<'_>,
        operation: &GitOperation,
    ) -> impl Future<Output = Result<(), ingot_usecases::UseCaseError>> + Send {
        let dispatcher = self.dispatcher.clone();
        let convergence = target.convergence.clone();
        let operation = operation.clone();
        async move {
            dispatcher
                .adopt_finalized_target_ref(&operation)
                .await
                .map_err(usecase_from_runtime_error)?;
            dispatcher
                .append_activity(
                    project.id,
                    ActivityEventType::ConvergenceFinalized,
                    ActivitySubject::Convergence(convergence.id),
                    serde_json::json!({ "item_id": item.id }),
                )
                .await
                .map_err(usecase_from_runtime_error)?;
            Ok(())
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
    pub fn new(
        db: Database,
        project_locks: ProjectLocks,
        config: DispatcherConfig,
        dispatch_notify: DispatchNotify,
    ) -> Self {
        Self::with_runner(
            db,
            project_locks,
            config,
            Arc::new(CliAgentRunner),
            dispatch_notify,
        )
    }

    pub fn with_runner(
        db: Database,
        project_locks: ProjectLocks,
        config: DispatcherConfig,
        runner: Arc<dyn AgentRunner>,
        dispatch_notify: DispatchNotify,
    ) -> Self {
        Self {
            db,
            project_locks,
            config,
            lease_owner_id: format!("ingotd:{}", std::process::id()),
            runner,
            dispatch_notify,
            #[cfg(test)]
            pre_spawn_pause_hook: None,
        }
    }

    async fn job_is_cancelled(&self, job_id: ingot_domain::ids::JobId) -> bool {
        match self.db.get_job(job_id).await {
            Ok(job) => job.state.status() == JobStatus::Cancelled,
            Err(error) => {
                warn!(?error, job_id = %job_id, "failed to load job for cancellation guard");
                false
            }
        }
    }

    #[cfg(test)]
    async fn pause_before_pre_spawn_guard(&self, point: PreSpawnPausePoint) {
        if let Some(hook) = &self.pre_spawn_pause_hook {
            hook.pause_if_matching(point).await;
        }
    }

    fn project_paths(&self, project: &Project) -> ingot_git::project_repo::ProjectRepoPaths {
        project_repo_paths(self.config.state_root.as_path(), project.id, &project.path)
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
                    && operation.operation_kind() == OperationKind::FinalizeTargetRef
            });
        if !(has_unresolved_finalize && paths.mirror_git_dir.exists()) {
            ensure_mirror(&paths).await?;
        }
        Ok(paths)
    }

    pub async fn run_forever(&self) {
        let semaphore = Arc::new(Semaphore::new(self.config.max_concurrent_jobs));
        let mut running = JoinSet::<RunningJobResult>::new();
        let mut running_meta = HashMap::<TaskId, RunningJobMeta>::new();
        let mut running_job_ids = HashSet::new();
        let mut dispatch_listener = self.dispatch_notify.subscribe();

        loop {
            let made_progress = match self
                .run_supervisor_iteration(
                    &mut running,
                    &mut running_meta,
                    &mut running_job_ids,
                    &semaphore,
                )
                .await
            {
                Ok(made_progress) => made_progress,
                Err(error) => {
                    error!(?error, "authoring job dispatcher iteration failed");
                    false
                }
            };

            if made_progress {
                continue;
            }

            let wait_result: Result<(), RuntimeError> = if running.is_empty() {
                tokio::select! {
                    notification = dispatch_listener.notified() => {
                        debug!(
                            generation = notification.generation(),
                            reason = %notification.reason(),
                            "dispatcher woken by notification"
                        );
                        Ok(())
                    }
                    () = sleep(self.config.poll_interval) => Ok(()),
                }
            } else {
                tokio::select! {
                    join_result = running.join_next_with_id() => {
                        if let Some(join_result) = join_result {
                            self.handle_supervised_join_result(
                                join_result,
                                &mut running_meta,
                                &mut running_job_ids,
                            )
                            .await
                        } else {
                            Ok(())
                        }
                    }
                    notification = dispatch_listener.notified() => {
                        debug!(
                            generation = notification.generation(),
                            reason = %notification.reason(),
                            "dispatcher woken by notification"
                        );
                        Ok(())
                    }
                    () = sleep(self.config.poll_interval) => Ok(()),
                }
            };

            if let Err(error) = wait_result {
                error!(?error, "authoring job dispatcher wait failed");
            }
        }
    }

    pub async fn reconcile_startup(&self) -> Result<(), RuntimeError> {
        bootstrap::ensure_default_agent(&self.db).await?;
        let _ = self.reconcile_startup_assigned_jobs().await?;
        ReconciliationService::new(RuntimeReconciliationPort {
            dispatcher: self.clone(),
        })
        .reconcile_startup()
        .await
        .map_err(usecase_to_runtime_error)?;
        drain_until_idle(|| self.tick_system_action()).await?;
        let _ = self.recover_projected_review_jobs().await?;
        Ok(())
    }

    pub async fn tick(&self) -> Result<bool, RuntimeError> {
        let non_job_progress = self.drive_non_job_work().await?;
        let mut made_progress = non_job_progress.made_progress;
        if non_job_progress.system_actions_progressed {
            return Ok(true);
        }

        if let Some(job) = self.next_runnable_job().await? {
            if is_daemon_only_validation(&job) {
                self.execute_harness_validation(job).await?;
                made_progress = true;
            } else {
                match self.prepare_run(job).await? {
                    PrepareRunOutcome::Prepared(prepared) => {
                        self.execute_prepared_agent_job(*prepared).await?;
                        made_progress = true;
                    }
                    PrepareRunOutcome::FailedBeforeLaunch => {
                        made_progress = true;
                    }
                    PrepareRunOutcome::NotPrepared => {}
                }
            }
        }

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

    async fn drive_non_job_work(&self) -> Result<NonJobWorkProgress, RuntimeError> {
        let mut made_progress = ReconciliationService::new(RuntimeReconciliationPort {
            dispatcher: self.clone(),
        })
        .tick_maintenance()
        .await
        .map_err(usecase_to_runtime_error)?;
        let system_actions_progressed = ConvergenceService::new(RuntimeConvergencePort {
            dispatcher: self.clone(),
        })
        .tick_system_actions()
        .await
        .map_err(usecase_to_runtime_error)?;
        made_progress |= system_actions_progressed;
        made_progress |= self.recover_projected_review_jobs().await?;
        Ok(NonJobWorkProgress {
            made_progress,
            system_actions_progressed,
        })
    }

    async fn run_supervisor_iteration(
        &self,
        running: &mut JoinSet<RunningJobResult>,
        running_meta: &mut HashMap<TaskId, RunningJobMeta>,
        running_job_ids: &mut HashSet<ingot_domain::ids::JobId>,
        semaphore: &Arc<Semaphore>,
    ) -> Result<bool, RuntimeError> {
        let mut made_progress = self
            .reap_completed_tasks(running, running_meta, running_job_ids)
            .await?;
        made_progress |= self.drive_non_job_work().await?.made_progress;
        made_progress |= self
            .launch_supervised_jobs(running, running_meta, running_job_ids, semaphore)
            .await?;
        Ok(made_progress)
    }

    async fn reap_completed_tasks(
        &self,
        running: &mut JoinSet<RunningJobResult>,
        running_meta: &mut HashMap<TaskId, RunningJobMeta>,
        running_job_ids: &mut HashSet<ingot_domain::ids::JobId>,
    ) -> Result<bool, RuntimeError> {
        let mut made_progress = false;
        while let Some(join_result) = running.try_join_next_with_id() {
            self.handle_supervised_join_result(join_result, running_meta, running_job_ids)
                .await?;
            made_progress = true;
        }
        Ok(made_progress)
    }

    async fn handle_supervised_join_result(
        &self,
        join_result: Result<(TaskId, RunningJobResult), JoinError>,
        running_meta: &mut HashMap<TaskId, RunningJobMeta>,
        running_job_ids: &mut HashSet<ingot_domain::ids::JobId>,
    ) -> Result<(), RuntimeError> {
        match join_result {
            Ok((task_id, task_result)) => {
                let meta = running_meta.remove(&task_id);
                running_job_ids.remove(&task_result.job_id);
                match task_result.result {
                    Ok(()) => {
                        debug!(job_id = %task_result.job_id, task_id = %task_id, "supervised job task completed");
                    }
                    Err(error) => {
                        warn!(?error, job_id = %task_result.job_id, task_id = %task_id, "supervised job task returned error");
                        if let Some(meta) = meta {
                            self.cleanup_supervised_task(meta, error.to_string())
                                .await?;
                        }
                    }
                }
            }
            Err(error) => {
                let task_id = error.id();
                warn!(?error, task_id = %task_id, "supervised job task failed");
                if let Some(meta) = running_meta.remove(&task_id) {
                    running_job_ids.remove(&meta.job_id());
                    self.cleanup_supervised_task(meta, error.to_string())
                        .await?;
                }
            }
        }
        Ok(())
    }

    async fn cleanup_supervised_task(
        &self,
        meta: RunningJobMeta,
        error_message: String,
    ) -> Result<(), RuntimeError> {
        match meta {
            RunningJobMeta::Agent(prepared) => {
                let current_job = self.db.get_job(prepared.job.id).await?;
                match current_job.state.status() {
                    JobStatus::Queued => {
                        self.cleanup_unclaimed_prepared_agent_run(&prepared).await?
                    }
                    JobStatus::Assigned => self.reconcile_assigned_job(current_job).await?,
                    JobStatus::Running => {
                        self.fail_run(
                            &prepared,
                            OutcomeClass::TerminalFailure,
                            "supervised_task_failed",
                            Some(error_message),
                        )
                        .await?;
                    }
                    _ => {}
                }
            }
            RunningJobMeta::HarnessValidation(prepared) => {
                let current_job = self.db.get_job(prepared.job_id).await?;
                match current_job.state.status() {
                    JobStatus::Assigned => self.reconcile_assigned_job(current_job).await?,
                    JobStatus::Running => {
                        let _guard = self
                            .project_locks
                            .acquire_project_mutation(prepared.project_id)
                            .await;
                        let current_job = self.db.get_job(prepared.job_id).await?;
                        if current_job.state.status() != JobStatus::Running {
                            return Ok(());
                        }
                        self.db
                            .finish_job_non_success(FinishJobNonSuccessParams {
                                job_id: prepared.job_id,
                                item_id: prepared.item_id,
                                expected_item_revision_id: prepared.revision_id,
                                status: JobStatus::Failed,
                                outcome_class: Some(OutcomeClass::TerminalFailure),
                                error_code: Some("supervised_task_failed".into()),
                                error_message: Some(error_message),
                                escalation_reason: None,
                            })
                            .await?;
                        let mut workspace = self.db.get_workspace(prepared.workspace_id).await?;
                        workspace.mark_stale(Utc::now());
                        self.db.update_workspace(&workspace).await?;
                        self.append_activity(
                            prepared.project_id,
                            ActivityEventType::JobFailed,
                            ActivitySubject::Job(prepared.job_id),
                            serde_json::json!({ "item_id": prepared.item_id, "error_code": "supervised_task_failed" }),
                        )
                        .await?;
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    async fn launch_supervised_jobs(
        &self,
        running: &mut JoinSet<RunningJobResult>,
        running_meta: &mut HashMap<TaskId, RunningJobMeta>,
        running_job_ids: &mut HashSet<ingot_domain::ids::JobId>,
        semaphore: &Arc<Semaphore>,
    ) -> Result<bool, RuntimeError> {
        let mut made_progress = false;
        for job in self.db.list_queued_jobs(32).await? {
            if running_job_ids.contains(&job.id) {
                continue;
            }
            let permit = match semaphore.clone().try_acquire_owned() {
                Ok(permit) => permit,
                Err(TryAcquireError::NoPermits) => break,
                Err(TryAcquireError::Closed) => {
                    return Err(RuntimeError::InvalidState(
                        "dispatcher semaphore unexpectedly closed".into(),
                    ));
                }
            };

            if !is_supported_runtime_job(&job) {
                drop(permit);
                continue;
            }

            if is_daemon_only_validation(&job) {
                match self.prepare_harness_validation(job.clone()).await {
                    Ok(PrepareHarnessValidationOutcome::NotPrepared) => {
                        drop(permit);
                    }
                    Ok(PrepareHarnessValidationOutcome::FailedBeforeLaunch) => {
                        drop(permit);
                        made_progress = true;
                    }
                    Ok(PrepareHarnessValidationOutcome::Prepared(prepared)) => {
                        let prepared = *prepared;
                        let handle = running.spawn(run_prepared_harness_validation_job(
                            self.clone(),
                            prepared.clone(),
                            permit,
                        ));
                        running_job_ids.insert(prepared.job_id);
                        running_meta
                            .insert(handle.id(), RunningJobMeta::HarnessValidation(prepared));
                        made_progress = true;
                    }
                    Err(RuntimeError::Workspace(WorkspaceError::Busy))
                        if job.workspace_kind == WorkspaceKind::Authoring =>
                    {
                        drop(permit);
                    }
                    Err(error) => {
                        drop(permit);
                        return Err(error);
                    }
                }
                continue;
            }

            match self.prepare_run(job.clone()).await {
                Ok(PrepareRunOutcome::NotPrepared) => {
                    drop(permit);
                }
                Ok(PrepareRunOutcome::FailedBeforeLaunch) => {
                    drop(permit);
                    made_progress = true;
                }
                Ok(PrepareRunOutcome::Prepared(prepared)) => {
                    let prepared = *prepared;
                    let handle = running.spawn(run_prepared_agent_job(
                        self.clone(),
                        prepared.clone(),
                        permit,
                    ));
                    running_job_ids.insert(prepared.job.id);
                    running_meta.insert(handle.id(), RunningJobMeta::Agent(Box::new(prepared)));
                    made_progress = true;
                }
                Err(RuntimeError::Workspace(WorkspaceError::Busy))
                    if job.workspace_kind == WorkspaceKind::Authoring =>
                {
                    drop(permit);
                }
                Err(error) => {
                    drop(permit);
                    return Err(error);
                }
            }
        }

        Ok(made_progress)
    }

    pub async fn reconcile_active_jobs(&self) -> Result<bool, RuntimeError> {
        let active_jobs = self.db.list_active_jobs().await?;
        let mut made_progress = false;
        for job in active_jobs {
            match job.state.status() {
                JobStatus::Running => {
                    made_progress |= self.reconcile_running_job(job).await?;
                }
                JobStatus::Assigned => {
                    made_progress |= self.reconcile_inert_assigned_dispatch_job(job).await?;
                }
                _ => {}
            }
        }
        Ok(made_progress)
    }

    async fn reconcile_startup_assigned_jobs(&self) -> Result<bool, RuntimeError> {
        let active_jobs = self.db.list_active_jobs().await?;
        let mut made_progress = false;
        for job in active_jobs {
            if job.state.status() == JobStatus::Assigned {
                self.reconcile_assigned_job(job).await?;
                made_progress = true;
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
            if operation.operation_kind() == OperationKind::FinalizeTargetRef {
                made_progress |= self
                    .reconcile_finalize_target_ref_operation(&project, &mut operation, &paths)
                    .await?;
                continue;
            }
            let reconciled = match &operation.payload {
                OperationPayload::FinalizeTargetRef { .. } => unreachable!("handled above"),
                OperationPayload::CreateJobCommit { .. }
                | OperationPayload::PrepareConvergenceCommit { .. } => {
                    if let Some(commit_oid) = operation.effective_commit_oid() {
                        ingot_git::commands::commit_exists(repo_path, commit_oid).await?
                    } else {
                        false
                    }
                }
                OperationPayload::CreateInvestigationRef {
                    ref_name, new_oid, ..
                } => resolve_ref_oid(repo_path, ref_name).await?.as_ref() == Some(new_oid),
                OperationPayload::RemoveWorkspaceRef { ref_name, .. } => {
                    resolve_ref_oid(repo_path, ref_name).await?.is_none()
                }
                OperationPayload::RemoveInvestigationRef { ref_name, .. } => {
                    resolve_ref_oid(repo_path, ref_name).await?.is_none()
                }
                OperationPayload::ResetWorkspace {
                    workspace_id,
                    new_oid,
                    ..
                } => {
                    let workspace = self.db.get_workspace(*workspace_id).await?;
                    match head_oid(&workspace.path).await {
                        Ok(actual_head) => actual_head == *new_oid,
                        Err(_) => false,
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
            ActivitySubject::GitOperation(operation.id),
            serde_json::json!({ "operation_kind": operation.operation_kind() }),
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
        if !current_target_oid
            .as_ref()
            .is_some_and(|oid| oid == context.prepared_commit_oid)
        {
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
        match operation.operation_kind() {
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
        let GitOperationEntityRef::Job(job_id) = &operation.entity else {
            return Err(RuntimeError::InvalidState(format!(
                "expected job entity, got {:?}", operation.entity.entity_type()
            )));
        };
        let job_id = *job_id;
        let mut job = self.db.get_job(job_id).await?;
        let commit_oid = operation
            .effective_commit_oid()
            .map(ToOwned::to_owned)
            .ok_or_else(|| {
                RuntimeError::InvalidState("reconciled create_job_commit missing commit oid".into())
            })?;

        if !job.state.is_active() {
            return Ok(());
        }

        let ended_at = job.state.ended_at().unwrap_or_else(Utc::now);
        job.complete(
            OutcomeClass::Clean,
            ended_at,
            Some(commit_oid.clone()),
            None,
            None,
        );
        self.db.update_job(&job).await?;

        if let Some(workspace_id) = operation.workspace_id().or(job.state.workspace_id()) {
            let mut workspace = self.db.get_workspace(workspace_id).await?;
            let now = Utc::now();
            workspace.set_head_commit_oid(commit_oid, now);
            if workspace.state.status() == WorkspaceStatus::Busy {
                workspace.release_to(WorkspaceStatus::Ready, now);
            }
            self.db.update_workspace(&workspace).await?;
        }

        self.append_activity(
            job.project_id,
            ActivityEventType::JobCompleted,
            ActivitySubject::Job(job.id),
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
        let GitOperationEntityRef::Convergence(convergence_id) = &operation.entity else {
            return Err(RuntimeError::InvalidState(format!(
                "expected convergence entity, got {:?}", operation.entity.entity_type()
            )));
        };
        let convergence_id = *convergence_id;
        let mut convergence = self.db.get_convergence(convergence_id).await?;
        if convergence.state.status() != ConvergenceStatus::Finalized {
            let final_oid = operation
                .new_oid()
                .or(operation.commit_oid())
                .map(ToOwned::to_owned)
                .ok_or_else(|| {
                    RuntimeError::InvalidState(
                        "reconciled finalize_target_ref missing commit oid".into(),
                    )
                })?;
            convergence.transition_to_finalized(final_oid, Utc::now());
            self.db.update_convergence(&convergence).await?;
        }

        let project = self.db.get_project(convergence.project_id).await?;
        if let Some(workspace_id) = convergence.state.integration_workspace_id() {
            let workspace = self.db.get_workspace(workspace_id).await?;
            if workspace.state.status() != WorkspaceStatus::Abandoned {
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
                let (resolution_source, approval_state) = match revision.approval_policy {
                    ingot_domain::revision::ApprovalPolicy::Required => {
                        (ResolutionSource::ApprovalCommand, ApprovalState::Approved)
                    }
                    ingot_domain::revision::ApprovalPolicy::NotRequired => {
                        (ResolutionSource::SystemCommand, ApprovalState::NotRequired)
                    }
                };
                item.lifecycle = Lifecycle::Done {
                    reason: DoneReason::Completed,
                    source: resolution_source,
                    closed_at: Utc::now(),
                };
                item.approval_state = approval_state;
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
        let GitOperationEntityRef::Convergence(convergence_id) = &operation.entity else {
            return Err(RuntimeError::InvalidState(format!(
                "expected convergence entity, got {:?}", operation.entity.entity_type()
            )));
        };
        let convergence_id = *convergence_id;
        let mut convergence = self.db.get_convergence(convergence_id).await?;
        if matches!(
            convergence.state.status(),
            ConvergenceStatus::Cancelled | ConvergenceStatus::Failed | ConvergenceStatus::Finalized
        ) {
            return Ok(());
        }
        if convergence.state.status() != ConvergenceStatus::Prepared {
            let prepared_oid = operation
                .effective_commit_oid()
                .map(ToOwned::to_owned)
                .ok_or_else(|| {
                    RuntimeError::InvalidState(
                        "reconciled prepare_convergence_commit missing commit oid".into(),
                    )
                })?;
            convergence.transition_to_prepared(prepared_oid, Some(Utc::now()));
            self.db.update_convergence(&convergence).await?;
        }

        if let Some(workspace_id) = convergence.state.integration_workspace_id() {
            let mut workspace = self.db.get_workspace(workspace_id).await?;
            let now = Utc::now();
            if let Some(head_commit_oid) = operation
                .effective_commit_oid()
                .cloned()
                .or_else(|| workspace.state.head_commit_oid().cloned())
            {
                workspace.mark_ready_with_head(head_commit_oid, now);
            } else {
                workspace.release_to(WorkspaceStatus::Ready, now);
            }
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
        let GitOperationEntityRef::Convergence(convergence_id) = &operation.entity else {
            return Err(RuntimeError::InvalidState(format!(
                "expected convergence entity, got {:?}", operation.entity.entity_type()
            )));
        };
        let convergence_id = *convergence_id;
        let convergence = self.db.get_convergence(convergence_id).await?;
        let item = self.db.get_item(convergence.item_id).await?;
        let revision = self.db.get_revision(convergence.item_revision_id).await?;
        let target_ref = operation
            .ref_name()
            .cloned()
            .unwrap_or(convergence.target_ref.clone());
        let prepared_commit_oid = operation
            .new_oid()
            .or(operation.commit_oid())
            .ok_or_else(|| RuntimeError::InvalidState("finalize operation missing new oid".into()))?
            .clone();
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

    async fn adopt_reset_workspace(&self, operation: &GitOperation) -> Result<(), RuntimeError> {
        let Some(workspace_id) = operation.workspace_id() else {
            return Ok(());
        };
        let mut workspace = self.db.get_workspace(workspace_id).await?;
        let now = Utc::now();
        if let Some(head_commit_oid) = operation
            .new_oid()
            .cloned()
            .or_else(|| workspace.state.head_commit_oid().cloned())
        {
            workspace.mark_ready_with_head(head_commit_oid, now);
        } else {
            workspace.release_to(WorkspaceStatus::Ready, now);
        }
        self.db.update_workspace(&workspace).await?;
        Ok(())
    }

    async fn adopt_removed_workspace_ref(
        &self,
        operation: &GitOperation,
    ) -> Result<(), RuntimeError> {
        let Some(workspace_id) = operation.workspace_id() else {
            return Ok(());
        };
        let mut workspace = self.db.get_workspace(workspace_id).await?;
        let now = Utc::now();
        workspace.mark_abandoned(now);
        if operation.ref_name().is_some() {
            workspace.workspace_ref = None;
        }
        workspace.updated_at = now;
        self.db.update_workspace(&workspace).await?;
        Ok(())
    }

    async fn reconcile_assigned_job(&self, job: Job) -> Result<(), RuntimeError> {
        let _guard = self
            .project_locks
            .acquire_project_mutation(job.project_id)
            .await;
        let mut job = self.db.get_job(job.id).await?;
        if job.state.status() != JobStatus::Assigned {
            return Ok(());
        }

        let workspace_id = job.state.workspace_id();
        job.state = JobState::Queued;
        self.db.update_job(&job).await?;

        if let Some(workspace_id) = workspace_id {
            let mut workspace = self.db.get_workspace(workspace_id).await?;
            workspace.release_to(WorkspaceStatus::Ready, Utc::now());
            self.db.update_workspace(&workspace).await?;
        }

        Ok(())
    }

    async fn reconcile_inert_assigned_dispatch_job(&self, job: Job) -> Result<bool, RuntimeError> {
        if !is_inert_assigned_authoring_dispatch_residue(&job) {
            return Ok(false);
        }

        let _guard = self
            .project_locks
            .acquire_project_mutation(job.project_id)
            .await;
        let mut job = self.db.get_job(job.id).await?;
        if !is_inert_assigned_authoring_dispatch_residue(&job) {
            return Ok(false);
        }

        let item = self.db.get_item(job.item_id).await?;
        if item.current_revision_id != job.item_revision_id {
            return Ok(false);
        }

        let Some(workspace_id) = job.state.workspace_id() else {
            return Ok(false);
        };
        let workspace = self.db.get_workspace(workspace_id).await?;
        if workspace.kind != WorkspaceKind::Authoring
            || workspace.project_id != job.project_id
            || workspace.created_for_revision_id != Some(job.item_revision_id)
            || workspace.state.status() != WorkspaceStatus::Ready
            || workspace.state.current_job_id().is_some()
        {
            return Ok(false);
        }

        job.state = JobState::Queued;
        self.db.update_job(&job).await?;

        Ok(true)
    }

    async fn reconcile_running_job(&self, job: Job) -> Result<bool, RuntimeError> {
        let expired = job
            .state
            .lease_expires_at()
            .is_none_or(|lease| lease <= Utc::now());
        let foreign_owner = job.state.lease_owner_id() != Some(self.lease_owner_id.as_str());
        if !expired && !foreign_owner {
            return Ok(false);
        }

        let _guard = self
            .project_locks
            .acquire_project_mutation(job.project_id)
            .await;
        let job = self.db.get_job(job.id).await?;
        if job.state.status() != JobStatus::Running {
            return Ok(false);
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

        if let Some(workspace_id) = job.state.workspace_id() {
            let mut workspace = self.db.get_workspace(workspace_id).await?;
            workspace.mark_stale(Utc::now());
            self.db.update_workspace(&workspace).await?;
        }

        self.append_activity(
            job.project_id,
            ActivityEventType::JobFailed,
            ActivitySubject::Job(job.id),
            serde_json::json!({ "item_id": job.item_id, "error_code": "heartbeat_expired" }),
        )
        .await?;

        Ok(true)
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
                convergence.state.status(),
                ConvergenceStatus::Queued | ConvergenceStatus::Running
            ) {
                continue;
            }
            convergence.transition_to_failed(Some("startup_recovery_required".into()), Utc::now());
            self.db.update_convergence(&convergence).await?;

            if let Some(workspace_id) = convergence.state.integration_workspace_id() {
                let mut workspace = self.db.get_workspace(workspace_id).await?;
                workspace.mark_stale(Utc::now());
                self.db.update_workspace(&workspace).await?;
            }

            self.append_activity(
                convergence.project_id,
                ActivityEventType::ConvergenceFailed,
                ActivitySubject::Convergence(convergence.id),
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
                if workspace.state.status() != WorkspaceStatus::Abandoned
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
        let head_commit_oid = workspace.state.head_commit_oid();
        let blocked = findings.iter().any(|finding| {
            finding.source_item_revision_id == revision.id
                && finding.triage.is_unresolved()
                && head_commit_oid.is_some_and(|oid| finding.source_subject_head_commit_oid == *oid)
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
        let path = &workspace.path;
        if path.exists() {
            remove_workspace(repo_path.as_path(), path).await?;
        }

        if let Some(workspace_ref) = workspace.workspace_ref.as_ref()
            && let Some(current_oid) = resolve_ref_oid(repo_path.as_path(), workspace_ref).await?
        {
            let mut operation = GitOperation {
                id: GitOperationId::new(),
                project_id: project.id,
                entity: GitOperationEntityRef::Workspace(workspace.id),
                payload: OperationPayload::RemoveWorkspaceRef {
                    workspace_id: workspace.id,
                    ref_name: workspace_ref.clone(),
                    expected_old_oid: current_oid,
                },
                status: GitOperationStatus::Planned,
                created_at: Utc::now(),
                completed_at: None,
            };
            self.db.create_git_operation(&operation).await?;
            self.append_activity(
                project.id,
                ActivityEventType::GitOperationPlanned,
                ActivitySubject::GitOperation(operation.id),
                serde_json::json!({ "operation_kind": operation.operation_kind(), "entity_id": operation.entity.entity_id_string() }),
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
    ) -> AgentRunOutcome {
        let timeout_duration = self.config.job_timeout;
        let lease_expires_at = self.next_lease_expiration();
        let mut dispatch_listener = self.dispatch_notify.subscribe();
        if let Err(error) = self
            .db
            .claim_queued_agent_job_execution(ClaimQueuedAgentJobExecutionParams {
                job_id: prepared.job.id,
                item_id: prepared.item.id,
                expected_item_revision_id: prepared.job.item_revision_id,
                assignment: prepared.assignment.clone(),
                lease_owner_id: self.lease_owner_id.clone(),
                lease_expires_at,
            })
            .await
        {
            return match error {
                RepositoryError::Conflict(_) => match self.db.get_job(prepared.job.id).await {
                    Ok(job) if job.state.status() == JobStatus::Cancelled => {
                        AgentRunOutcome::Cancelled
                    }
                    Ok(_) => AgentRunOutcome::OwnershipLostBeforeSpawn,
                    Err(load_error) => AgentRunOutcome::LaunchFailed(AgentError::ProcessError(
                        load_error.to_string(),
                    )),
                },
                other => AgentRunOutcome::LaunchFailed(AgentError::ProcessError(other.to_string())),
            };
        }
        info!(
            job_id = %prepared.job.id,
            agent_id = %prepared.agent.id,
            workspace_id = %prepared.assignment.workspace_id,
            lease_owner_id = %self.lease_owner_id,
            timeout_seconds = timeout_duration.as_secs(),
            "job entered running state"
        );

        #[cfg(test)]
        self.pause_before_pre_spawn_guard(PreSpawnPausePoint::AgentBeforeSpawn)
            .await;

        if self.job_is_cancelled(prepared.job.id).await {
            info!(
                job_id = %prepared.job.id,
                "skipping agent launch because job was cancelled before spawn"
            );
            return AgentRunOutcome::Cancelled;
        }

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
                    let result = match result {
                        Ok(result) => result,
                        Err(error) => {
                            return AgentRunOutcome::LaunchFailed(AgentError::ProcessError(error.to_string()));
                        }
                    };
                    debug!(job_id = %prepared.job.id, "job execution future resolved");
                    return match result {
                        Ok(response) => AgentRunOutcome::Completed(response),
                        Err(error) => AgentRunOutcome::LaunchFailed(error),
                    };
                }
                _ = &mut timeout => {
                    handle.abort();
                    warn!(job_id = %prepared.job.id, timeout_seconds = timeout_duration.as_secs(), "job execution timed out");
                    return AgentRunOutcome::TimedOut;
                }
                notification = dispatch_listener.notified() => {
                    match self.db.get_job(prepared.job.id).await {
                        Ok(job) if job.state.status() == JobStatus::Cancelled => {
                            handle.abort();
                            info!(job_id = %prepared.job.id, "cancelling running job after operator request");
                            return AgentRunOutcome::Cancelled;
                        }
                        Ok(job) if job.state.status() != JobStatus::Running => {
                            handle.abort();
                            info!(job_id = %prepared.job.id, status = ?job.state.status(), "stopping runner after job lost ownership");
                            return AgentRunOutcome::OwnershipLostDuringRun;
                        }
                        Ok(_) => {
                            debug!(
                                job_id = %prepared.job.id,
                                generation = notification.generation(),
                                reason = %notification.reason(),
                                "running job woke on unrelated dispatcher notification"
                            );
                        }
                        Err(error) => {
                            warn!(
                                ?error,
                                job_id = %prepared.job.id,
                                generation = notification.generation(),
                                reason = %notification.reason(),
                                "failed to load job after dispatcher notification"
                            );
                        }
                    }
                }
                _ = ticker.tick() => {
                    match self.db.get_job(prepared.job.id).await {
                        Ok(job) if job.state.status() == JobStatus::Cancelled => {
                            handle.abort();
                            info!(job_id = %prepared.job.id, "cancelling running job after operator request");
                            return AgentRunOutcome::Cancelled;
                        }
                        Ok(job) if job.state.status() != JobStatus::Running => {
                            handle.abort();
                            info!(job_id = %prepared.job.id, status = ?job.state.status(), "stopping runner after job lost ownership");
                            return AgentRunOutcome::OwnershipLostDuringRun;
                        }
                        Ok(_) => {}
                        Err(error) => {
                            warn!(?error, job_id = %prepared.job.id, "failed to load job during heartbeat tick");
                        }
                    }
                    let lease_expires_at = self.next_lease_expiration();
                    if let Err(error) = self.db.heartbeat_job_execution(
                        prepared.job.id,
                        prepared.item.id,
                        prepared.job.item_revision_id,
                        &self.lease_owner_id,
                        lease_expires_at,
                    ).await {
                        if matches!(&error, RepositoryError::Conflict(message) if message == "job_not_active") {
                            handle.abort();
                            info!(job_id = %prepared.job.id, "stopping runner after heartbeat lost job ownership");
                            return AgentRunOutcome::OwnershipLostDuringRun;
                        }
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
        Ok(convergence.target_head_valid_for_resolved_oid(resolved.as_ref()))
    }

    async fn prepare_run(&self, queued_job: Job) -> Result<PrepareRunOutcome, RuntimeError> {
        let _guard = self
            .project_locks
            .acquire_project_mutation(queued_job.project_id)
            .await;

        let job = self.db.get_job(queued_job.id).await?;
        if job.state.status() != JobStatus::Queued || !is_supported_runtime_job(&job) {
            return Ok(PrepareRunOutcome::NotPrepared);
        }

        let item = self.db.get_item(job.item_id).await?;
        if item.current_revision_id != job.item_revision_id {
            return Ok(PrepareRunOutcome::NotPrepared);
        }

        let revision = self.db.get_revision(job.item_revision_id).await?;
        let project = self.db.get_project(job.project_id).await?;
        let harness_prompt = match resolve_harness_prompt_context(&project.path) {
            Ok(context) => context,
            Err(error) => {
                self.fail_job_preparation(
                    &job,
                    &item,
                    &project,
                    error.error_code(),
                    error.to_string(),
                )
                .await?;
                return Ok(PrepareRunOutcome::FailedBeforeLaunch);
            }
        };
        let paths = self.refresh_project_mirror(&project).await?;
        let Some(agent) = self.select_agent(&job).await? else {
            debug!(
                job_id = %job.id,
                step_id = %job.step_id,
                "queued job is waiting for a compatible available agent"
            );
            return Ok(PrepareRunOutcome::NotPrepared);
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
            .state
            .head_commit_oid()
            .map(ToOwned::to_owned)
            .ok_or_else(|| RuntimeError::InvalidState("workspace missing head".into()))?;

        workspace.attach_job(job.id, now);
        if workspace_exists {
            self.db.update_workspace(&workspace).await?;
        } else {
            self.db.create_workspace(&workspace).await?;
        }

        let template = built_in_template(&job.phase_template_slug, job.step_id);
        let phase_template_digest = template_digest(template);
        let prompt = match self
            .assemble_prompt(&job, &item, &revision, template, &harness_prompt)
            .await
        {
            Ok(prompt) => prompt,
            Err(error) => {
                self.cleanup_unclaimed_prepared_workspace(
                    job.project_id,
                    job.id,
                    &workspace,
                    workspace_lifecycle,
                    &original_head_commit_oid,
                    paths.mirror_git_dir.as_path(),
                )
                .await?;
                return Err(error);
            }
        };
        let assignment = JobAssignment::new(workspace.id)
            .with_agent(agent.id)
            .with_prompt_snapshot(prompt.clone())
            .with_phase_template_digest(phase_template_digest);

        info!(
            job_id = %job.id,
            workspace_id = %workspace.id,
            agent_id = %agent.id,
            step_id = %job.step_id,
            project_id = %project.id,
            item_id = %item.id,
            "prepared job execution"
        );

        Ok(PrepareRunOutcome::Prepared(Box::new(PreparedRun {
            job,
            item,
            revision,
            project,
            canonical_repo_path: paths.mirror_git_dir,
            agent,
            assignment,
            workspace,
            original_head_commit_oid,
            prompt,
            workspace_lifecycle,
        })))
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
            (
                WorkspaceKind::Integration,
                ExecutionPermission::MustNotMutate | ExecutionPermission::DaemonOnly,
            ) => {
                let workspace_id = self
                    .integration_workspace_id_for_job(job, revision.id)
                    .await?;
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
                workspace.path = provisioned.workspace_path.clone();
                workspace.workspace_ref = Some(provisioned.workspace_ref);
                workspace.mark_ready_with_head(provisioned.head_commit_oid, now);
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
                    path: provisioned.workspace_path.clone(),
                    created_for_revision_id: Some(revision.id),
                    parent_workspace_id: None,
                    target_ref: None,
                    workspace_ref: None,
                    retention_policy: RetentionPolicy::Ephemeral,
                    created_at: now,
                    updated_at: now,
                    state: WorkspaceState::Ready {
                        commits: WorkspaceCommitState::new(
                            job.job_input
                                .base_commit_oid()
                                .cloned()
                                .unwrap_or_else(|| provisioned.head_commit_oid.clone()),
                            provisioned.head_commit_oid,
                        ),
                    },
                };
                Ok((workspace, WorkspaceLifecycle::EphemeralReview, false))
            }
            _ => Err(RuntimeError::InvalidState(format!(
                "unsupported runtime workspace kind {:?} for step {}",
                job.workspace_kind, job.step_id
            ))),
        }
    }

    async fn integration_workspace_id_for_job(
        &self,
        job: &Job,
        revision_id: ingot_domain::ids::ItemRevisionId,
    ) -> Result<WorkspaceId, RuntimeError> {
        if let Some(workspace_id) = job.state.workspace_id() {
            return Ok(workspace_id);
        }

        if job.execution_permission != ExecutionPermission::DaemonOnly {
            return Err(RuntimeError::InvalidState(
                "integration jobs require a provisioned integration workspace".into(),
            ));
        }

        self.db
            .find_prepared_convergence_for_revision(revision_id)
            .await?
            .and_then(|convergence| convergence.state.integration_workspace_id())
            .ok_or_else(|| {
                RuntimeError::InvalidState(
                    "integration jobs require a provisioned integration workspace".into(),
                )
            })
    }

    async fn assemble_prompt(
        &self,
        job: &Job,
        item: &ingot_domain::item::Item,
        revision: &ItemRevision,
        template: &str,
        harness_prompt: &HarnessPromptContext,
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
            job.step_id,
            StepId::RepairCandidate | StepId::RepairAfterIntegration
        ) {
            let jobs = self.db.list_jobs_by_item(item.id).await?;
            let findings = self.db.list_findings_by_item(item.id).await?;
            let latest_closure_findings_job = jobs
                .iter()
                .filter(|candidate| candidate.item_revision_id == revision.id)
                .filter(|candidate| candidate.state.status().is_terminal())
                .filter(|candidate| candidate.state.outcome_class() == Some(OutcomeClass::Findings))
                .filter(|candidate| is_closure_relevant_job(candidate))
                .max_by_key(|candidate| (candidate.state.ended_at(), candidate.created_at));

            if let Some(latest_job) = latest_closure_findings_job {
                let scoped_findings = findings
                    .iter()
                    .filter(|finding| finding.source_item_revision_id == revision.id)
                    .filter(|finding| finding.source_job_id == latest_job.id)
                    .collect::<Vec<_>>();
                let fix_now_findings = scoped_findings
                    .iter()
                    .filter(|finding| finding.triage.state() == FindingTriageState::FixNow)
                    .collect::<Vec<_>>();
                let accepted_findings = scoped_findings
                    .iter()
                    .filter(|finding| !finding.triage.blocks_closure())
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
                            finding.code,
                            finding.summary,
                            finding.triage.state()
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

        // Include harness commands and skills so agents know what verification tools are available
        if !harness_prompt.commands.is_empty() {
            prompt.push_str("\nAvailable verification commands:\n");
            for cmd in &harness_prompt.commands {
                prompt.push_str(&format!("- `{}`: `{}`\n", cmd.name, cmd.run));
            }
        }
        if !harness_prompt.skills.is_empty() {
            prompt.push_str("\nRepo-local skills available:\n");
            for skill in &harness_prompt.skills {
                prompt.push_str(&format!(
                    "\nSkill file: {}\n{}\n",
                    skill.relative_path, skill.contents
                ));
            }
        }

        Ok(prompt)
    }

    async fn execute_prepared_agent_job(&self, prepared: PreparedRun) -> Result<(), RuntimeError> {
        self.write_prompt_artifact(&prepared.job, &prepared.prompt)
            .await?;
        let request = AgentRequest {
            prompt: prepared.prompt.clone(),
            working_dir: prepared.workspace.path.clone(),
            may_mutate: prepared.job.execution_permission == ExecutionPermission::MayMutate,
            timeout_seconds: Some(self.config.job_timeout.as_secs()),
            output_schema: output_schema_for_job(&prepared.job),
        };
        match self.run_with_heartbeats(&prepared, request).await {
            AgentRunOutcome::Completed(response) => {
                self.write_response_artifacts(&prepared.job, &response)
                    .await?;
                self.finish_run(prepared, response).await?;
            }
            AgentRunOutcome::TimedOut => {
                self.fail_run(
                    &prepared,
                    OutcomeClass::TransientFailure,
                    "job_timeout",
                    Some("job execution timed out".into()),
                )
                .await?;
            }
            AgentRunOutcome::Cancelled => {
                info!(
                    job_id = %prepared.job.id,
                    "job cancelled during runtime execution"
                );
                let _ = self.recover_projected_review_jobs().await?;
            }
            AgentRunOutcome::OwnershipLostBeforeSpawn => {
                self.cleanup_unclaimed_prepared_agent_run(&prepared).await?;
                info!(
                    job_id = %prepared.job.id,
                    "prepared job lost ownership before agent spawn"
                );
            }
            AgentRunOutcome::OwnershipLostDuringRun => {
                info!(
                    job_id = %prepared.job.id,
                    "running job lost ownership before completion handling"
                );
            }
            AgentRunOutcome::LaunchFailed(error) => {
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

        Ok(())
    }

    async fn finish_run(
        &self,
        prepared: PreparedRun,
        response: AgentResponse,
    ) -> Result<(), RuntimeError> {
        let current_job = self.db.get_job(prepared.job.id).await?;
        if current_job.state.status() == JobStatus::Cancelled {
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
            ActivitySubject::Job(prepared.job.id),
            serde_json::json!({ "item_id": prepared.item.id, "outcome": outcome_class_name(outcome_class) }),
        )
        .await?;
        if prepared.job.step_id == StepId::ValidateIntegrated
            && outcome_class == OutcomeClass::Clean
        {
            let updated_item = self.db.get_item(prepared.item.id).await?;
            if updated_item.approval_state == ApprovalState::Pending {
                self.append_activity(
                    prepared.project.id,
                    ActivityEventType::ApprovalRequested,
                    ActivitySubject::Item(prepared.item.id),
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
    ) -> Result<bool, RuntimeError> {
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
            return Ok(false);
        }
        let should_auto_finalize = revision.approval_policy
            == ingot_domain::revision::ApprovalPolicy::NotRequired
            && evaluation.next_recommended_action == RecommendedAction::FinalizePreparedConvergence;
        if !should_auto_finalize {
            return Ok(false);
        }

        let convergence = convergences
            .into_iter()
            .find(|convergence| {
                convergence.item_revision_id == revision.id
                    && convergence.state.status() == ConvergenceStatus::Prepared
            })
            .ok_or_else(|| RuntimeError::InvalidState("prepared convergence missing".into()))?;
        let prepared_commit_oid = convergence
            .state
            .prepared_commit_oid()
            .map(ToOwned::to_owned)
            .ok_or_else(|| RuntimeError::InvalidState("prepared commit missing".into()))?;
        let input_target_commit_oid = convergence
            .state
            .input_target_commit_oid()
            .map(ToOwned::to_owned)
            .ok_or_else(|| RuntimeError::InvalidState("input target commit missing".into()))?;
        let current_target_oid =
            resolve_ref_oid(paths.mirror_git_dir.as_path(), &convergence.target_ref).await?;
        let target_valid = current_target_oid.as_ref() == Some(&prepared_commit_oid)
            || current_target_oid.as_ref() == Some(&input_target_commit_oid);
        if !target_valid {
            return Ok(false);
        }

        match finalize_prepared_convergence(
            &RuntimeFinalizePort {
                dispatcher: self.clone(),
            },
            FinalizePreparedTrigger::SystemCommand,
            &project,
            &item,
            &revision,
            &convergence,
            queue_entry
                .as_ref()
                .expect("queue head already validated for auto-finalize"),
        )
        .await
        {
            Ok(()) => {}
            Err(ingot_usecases::UseCaseError::ProtocolViolation(_)) => return Ok(false),
            Err(error) => return Err(usecase_to_runtime_error(error)),
        }

        info!(item_id = %item.id, convergence_id = %convergence.id, "auto-finalized prepared convergence");
        Ok(true)
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
        source_commit_oids: &[CommitOid],
        prepared_commit_oids: &[CommitOid],
        summary: String,
        failure_kind: PrepareFailureKind,
    ) -> Result<(), RuntimeError> {
        integration_workspace.mark_error(Utc::now());
        self.db.update_workspace(integration_workspace).await?;

        match failure_kind {
            PrepareFailureKind::Conflicted => {
                convergence.transition_to_conflicted(summary.clone(), Utc::now());
            }
            PrepareFailureKind::Failed => {
                convergence.transition_to_failed(Some(summary.clone()), Utc::now());
            }
        }
        self.db.update_convergence(convergence).await?;

        let escalation_reason = match failure_kind {
            PrepareFailureKind::Conflicted => EscalationReason::ConvergenceConflict,
            PrepareFailureKind::Failed => EscalationReason::StepFailed,
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
        operation
            .payload
            .set_replay_metadata(ConvergenceReplayMetadata {
                source_commit_oids: source_commit_oids.to_vec(),
                prepared_commit_oids: prepared_commit_oids.to_vec(),
            });
        self.db.update_git_operation(operation).await?;

        let event_type = match failure_kind {
            PrepareFailureKind::Conflicted => ActivityEventType::ConvergenceConflicted,
            PrepareFailureKind::Failed => ActivityEventType::ConvergenceFailed,
        };
        self.append_activity(
            project.id,
            event_type,
            ActivitySubject::Convergence(convergence.id),
            serde_json::json!({ "item_id": item.id, "summary": summary }),
        )
        .await?;
        self.append_activity(
            project.id,
            ActivityEventType::ItemEscalated,
            ActivitySubject::Item(item.id),
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
            convergence.item_revision_id == revision.id && convergence.state.is_active()
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
        let integration_workspace_ref =
            GitRef::new(format!("refs/ingot/workspaces/{integration_workspace_id}"));
        let now = Utc::now();
        let mut integration_workspace = Workspace {
            id: integration_workspace_id,
            project_id: project.id,
            kind: WorkspaceKind::Integration,
            strategy: WorkspaceStrategy::Worktree,
            path: integration_workspace_path.clone(),
            created_for_revision_id: Some(revision.id),
            parent_workspace_id: Some(source_workspace.id),
            target_ref: Some(revision.target_ref.clone()),
            workspace_ref: Some(integration_workspace_ref.clone()),
            retention_policy: RetentionPolicy::Persistent,
            created_at: now,
            updated_at: now,
            state: WorkspaceState::Provisioning {
                commits: Some(WorkspaceCommitState::new(
                    input_target_commit_oid.clone(),
                    input_target_commit_oid.clone(),
                )),
            },
        };
        self.db.create_workspace(&integration_workspace).await?;

        let provisioned = provision_integration_workspace(
            repo_path,
            &integration_workspace_path,
            &integration_workspace_ref,
            &input_target_commit_oid,
        )
        .await?;
        integration_workspace.path = provisioned.workspace_path.clone();
        integration_workspace.workspace_ref = Some(provisioned.workspace_ref);
        integration_workspace.set_head_commit_oid(provisioned.head_commit_oid, Utc::now());
        self.db.update_workspace(&integration_workspace).await?;

        let mut convergence = Convergence {
            id: ingot_domain::ids::ConvergenceId::new(),
            project_id: project.id,
            item_id: item.id,
            item_revision_id: revision.id,
            source_workspace_id: source_workspace.id,
            source_head_commit_oid: source_head_commit_oid.clone(),
            target_ref: revision.target_ref.clone(),
            strategy: ingot_domain::convergence::ConvergenceStrategy::RebaseThenFastForward,
            target_head_valid: Some(true),
            created_at: now,
            state: ingot_domain::convergence::ConvergenceState::Running {
                integration_workspace_id: integration_workspace.id,
                input_target_commit_oid: input_target_commit_oid.clone(),
            },
        };
        self.db.create_convergence(&convergence).await?;
        self.append_activity(
            project.id,
            ActivityEventType::ConvergenceStarted,
            ActivitySubject::Convergence(convergence.id),
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
            entity: GitOperationEntityRef::Convergence(convergence.id),
            payload: OperationPayload::PrepareConvergenceCommit {
                workspace_id: integration_workspace.id,
                ref_name: integration_workspace.workspace_ref.clone(),
                expected_old_oid: input_target_commit_oid.clone(),
                new_oid: None,
                commit_oid: None,
                replay_metadata: Some(ConvergenceReplayMetadata {
                    source_commit_oids: source_commit_oids.clone(),
                    prepared_commit_oids: vec![],
                }),
            },
            status: GitOperationStatus::Planned,
            created_at: now,
            completed_at: None,
        };
        self.db.create_git_operation(&operation).await?;
        self.append_activity(
            project.id,
            ActivityEventType::GitOperationPlanned,
            ActivitySubject::GitOperation(operation.id),
            serde_json::json!({ "operation_kind": operation.operation_kind(), "entity_id": operation.entity.entity_id_string() }),
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
                    PrepareFailureKind::Conflicted,
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
                            PrepareFailureKind::Failed,
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
                        PrepareFailureKind::Failed,
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
                        PrepareFailureKind::Failed,
                    )
                    .await?;
                    return Ok(());
                }
            };
            if let Some(workspace_ref) = integration_workspace.workspace_ref.as_ref() {
                if let Err(error) = git(
                    repo_path,
                    &[
                        "update-ref",
                        workspace_ref.as_str(),
                        next_prepared_tip.as_str(),
                    ],
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
                        PrepareFailureKind::Failed,
                    )
                    .await?;
                    return Ok(());
                }
            }
            prepared_tip = next_prepared_tip;
            prepared_commit_oids.push(prepared_tip.clone());
        }

        integration_workspace.mark_ready_with_head(prepared_tip.clone(), Utc::now());
        self.db.update_workspace(&integration_workspace).await?;

        convergence.transition_to_prepared(prepared_tip.clone(), Some(Utc::now()));
        self.db.update_convergence(&convergence).await?;

        operation
            .payload
            .set_convergence_commit_result(prepared_tip.clone());
        operation
            .payload
            .set_replay_metadata(ConvergenceReplayMetadata {
                source_commit_oids,
                prepared_commit_oids,
            });
        self.mark_git_operation_reconciled(&mut operation).await?;

        let mut all_convergences = convergences.to_vec();
        all_convergences.push(convergence.clone());
        let validation_job = dispatch_job(
            &current_item,
            revision,
            jobs,
            findings,
            &all_convergences,
            DispatchJobCommand {
                step_id: Some(StepId::ValidateIntegrated),
            },
        )
        .map_err(|error| RuntimeError::InvalidState(error.to_string()))?;
        self.db.create_job(&validation_job).await?;
        self.append_activity(
            project.id,
            ActivityEventType::ConvergencePrepared,
            ActivitySubject::Convergence(convergence.id),
            serde_json::json!({ "item_id": item.id, "validation_job_id": validation_job.id }),
        )
        .await?;
        self.append_activity(
            project.id,
            ActivityEventType::JobDispatched,
            ActivitySubject::Job(validation_job.id),
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
        let status = checkout_sync_status(&project.path, &revision.target_ref).await?;
        let checkout_sync_blocked = matches!(
            item.escalation,
            Escalation::OperatorRequired {
                reason: EscalationReason::CheckoutSyncBlocked
            }
        );
        match &status {
            CheckoutSyncStatus::Ready => {
                if checkout_sync_blocked {
                    item.escalation = Escalation::None;
                    item.updated_at = Utc::now();
                    self.db.update_item(&item).await?;
                    self.append_activity(
                        project.id,
                        ActivityEventType::CheckoutSyncCleared,
                        ActivitySubject::Item(item.id),
                        serde_json::json!({}),
                    )
                    .await?;
                    self.append_activity(
                        project.id,
                        ActivityEventType::ItemEscalationCleared,
                        ActivitySubject::Item(item.id),
                        serde_json::json!({ "reason": "checkout_sync_ready" }),
                    )
                    .await?;
                }
            }
            CheckoutSyncStatus::Blocked { message, .. } => {
                if !checkout_sync_blocked {
                    item.escalation = Escalation::OperatorRequired {
                        reason: EscalationReason::CheckoutSyncBlocked,
                    };
                    item.updated_at = Utc::now();
                    self.db.update_item(&item).await?;
                    self.append_activity(
                        project.id,
                        ActivityEventType::CheckoutSyncBlocked,
                        ActivitySubject::Item(item.id),
                        serde_json::json!({ "message": message }),
                    )
                    .await?;
                    self.append_activity(
                        project.id,
                        ActivityEventType::ItemEscalated,
                        ActivitySubject::Item(item.id),
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
            prepared.workspace.workspace_ref.as_ref().ok_or_else(|| {
                RuntimeError::InvalidState("authoring workspace missing ref".into())
            })?;
        let actual_ref = resolve_ref_oid(repo_path, workspace_ref).await?;
        let actual_head = head_oid(Path::new(&prepared.workspace.path)).await?;

        if actual_ref.as_ref() != Some(&prepared.original_head_commit_oid)
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
    ) -> Result<CommitOid, RuntimeError> {
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
            entity: GitOperationEntityRef::Job(prepared.job.id),
            payload: OperationPayload::CreateJobCommit {
                workspace_id: prepared.workspace.id,
                ref_name: workspace_ref.clone(),
                expected_old_oid: prepared.original_head_commit_oid.clone(),
                new_oid: None,
                commit_oid: None,
            },
            status: GitOperationStatus::Planned,
            created_at: now,
            completed_at: None,
        };
        self.db.create_git_operation(&operation).await?;
        self.append_activity(
            prepared.project.id,
            ActivityEventType::GitOperationPlanned,
            ActivitySubject::GitOperation(operation.id),
            serde_json::json!({ "operation_kind": operation.operation_kind(), "entity_id": operation.entity.entity_id_string() }),
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
            &commit_subject(&prepared.revision.title, prepared.job.step_id),
            summary,
            &JobCommitTrailers {
                operation_id: operation.id,
                item_id: prepared.item.id,
                revision_no: prepared.revision.revision_no,
                job_id: prepared.job.id,
            },
        )
        .await?;
        git(
            repo_path,
            &["update-ref", workspace_ref.as_str(), commit_oid.as_str()],
        )
        .await?;

        operation.payload.set_job_commit_result(commit_oid.clone());
        operation.status = GitOperationStatus::Applied;
        operation.completed_at = Some(Utc::now());
        self.db.update_git_operation(&operation).await?;

        Ok(commit_oid)
    }

    async fn complete_commit_run(
        &self,
        prepared: &PreparedRun,
        commit_oid: &CommitOid,
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
                output_commit_oid: Some(commit_oid.clone()),
                findings: vec![],
                prepared_convergence_guard: None,
            })
            .await?;
        self.append_activity(
            prepared.project.id,
            ActivityEventType::JobCompleted,
            ActivitySubject::Job(prepared.job.id),
            serde_json::json!({ "item_id": prepared.item.id, "outcome": "clean" }),
        )
        .await?;

        self.refresh_revision_context(prepared).await?;
        self.append_escalation_cleared_activity_if_needed(prepared)
            .await?;
        self.auto_dispatch_projected_review(prepared.project.id, prepared.item.id)
            .await?;

        info!(job_id = %prepared.job.id, commit_oid = %commit_oid, "completed authoring job");

        Ok(())
    }

    async fn prepare_harness_validation(
        &self,
        queued_job: Job,
    ) -> Result<PrepareHarnessValidationOutcome, RuntimeError> {
        let _guard = self
            .project_locks
            .acquire_project_mutation(queued_job.project_id)
            .await;

        let mut job = self.db.get_job(queued_job.id).await?;
        if job.state.status() != JobStatus::Queued || !is_daemon_only_validation(&job) {
            return Ok(PrepareHarnessValidationOutcome::NotPrepared);
        }

        let item = self.db.get_item(job.item_id).await?;
        if item.current_revision_id != job.item_revision_id {
            return Ok(PrepareHarnessValidationOutcome::NotPrepared);
        }

        let revision = self.db.get_revision(job.item_revision_id).await?;
        let project = self.db.get_project(job.project_id).await?;
        let harness = match load_harness_profile(&project.path) {
            Ok(harness) => harness,
            Err(error) => {
                self.fail_job_preparation(
                    &job,
                    &item,
                    &project,
                    error.error_code(),
                    error.to_string(),
                )
                .await?;
                return Ok(PrepareHarnessValidationOutcome::FailedBeforeLaunch);
            }
        };

        let paths = self.refresh_project_mirror(&project).await?;
        let now = Utc::now();
        let (workspace, workspace_exists) = match job.workspace_kind {
            WorkspaceKind::Authoring | WorkspaceKind::Integration => {
                let (workspace, _lifecycle, workspace_exists) = self
                    .prepare_workspace(
                        &project,
                        paths.mirror_git_dir.as_path(),
                        &paths.worktree_root,
                        &revision,
                        &job,
                        now,
                    )
                    .await?;
                (workspace, workspace_exists)
            }
            _ => {
                return Err(RuntimeError::InvalidState(format!(
                    "unsupported workspace kind {:?} for harness validation",
                    job.workspace_kind
                )));
            }
        };

        if workspace_exists {
            self.db.update_workspace(&workspace).await?;
        } else {
            self.db.create_workspace(&workspace).await?;
        }

        job.assign(JobAssignment::new(workspace.id));
        self.db.update_job(&job).await?;
        self.db
            .start_job_execution(StartJobExecutionParams {
                job_id: job.id,
                item_id: job.item_id,
                expected_item_revision_id: job.item_revision_id,
                workspace_id: Some(workspace.id),
                agent_id: None,
                lease_owner_id: self.lease_owner_id.clone(),
                process_pid: None,
                lease_expires_at: now + self.lease_ttl(),
            })
            .await?;

        Ok(PrepareHarnessValidationOutcome::Prepared(Box::new(
            PreparedHarnessValidation {
                harness,
                job_id: job.id,
                item_id: job.item_id,
                project_id: project.id,
                revision_id: job.item_revision_id,
                workspace_id: workspace.id,
                workspace_path: workspace.path.clone(),
                step_id: job.step_id,
            },
        )))
    }

    async fn run_prepared_harness_validation(
        &self,
        prepared: PreparedHarnessValidation,
    ) -> Result<(), RuntimeError> {
        let mut checks = Vec::new();
        let mut findings = Vec::new();

        for command in &prepared.harness.commands {
            if self.harness_validation_cancelled(&prepared).await? {
                info!(
                    job_id = %prepared.job_id,
                    "daemon-only validation cancelled before next harness command"
                );
                return Ok(());
            }

            let result = self
                .run_harness_command_with_heartbeats(&prepared, command)
                .await;
            if result.cancelled {
                info!(
                    job_id = %prepared.job_id,
                    command = %command.name,
                    "daemon-only validation cancelled while harness command was running"
                );
                return Ok(());
            }
            let status = if result.timed_out || result.exit_code != 0 {
                "fail"
            } else {
                "pass"
            };
            let summary = if result.timed_out {
                format!(
                    "command '{}' timed out after {:?}",
                    command.name, command.timeout
                )
            } else if result.exit_code != 0 {
                format!(
                    "command '{}' exited with code {}",
                    command.name, result.exit_code
                )
            } else {
                format!("command '{}' passed", command.name)
            };
            checks.push(serde_json::json!({
                "name": command.name,
                "status": status,
                "summary": summary,
            }));
            if status == "fail" {
                let mut evidence = Vec::new();
                if !result.stdout_tail.is_empty() {
                    evidence.push(format!("stdout:\n{}", result.stdout_tail));
                }
                if !result.stderr_tail.is_empty() {
                    evidence.push(format!("stderr:\n{}", result.stderr_tail));
                }
                if evidence.is_empty() {
                    evidence.push(format!("exit code: {}", result.exit_code));
                }
                findings.push(serde_json::json!({
                    "finding_key": command.name,
                    "code": command.name,
                    "severity": "high",
                    "summary": summary,
                    "paths": [],
                    "evidence": evidence,
                }));
            }
        }

        let outcome = if findings.is_empty() {
            "clean"
        } else {
            "findings"
        };
        let result_summary = if findings.is_empty() {
            "all harness checks passed".to_string()
        } else {
            format!(
                "{} of {} harness checks failed",
                findings.len(),
                checks.len()
            )
        };
        let result_payload = serde_json::json!({
            "outcome": outcome,
            "summary": result_summary,
            "checks": checks,
            "findings": findings,
        });
        let outcome_class = if findings.is_empty() {
            OutcomeClass::Clean
        } else {
            OutcomeClass::Findings
        };

        if self.harness_validation_cancelled(&prepared).await? {
            info!(
                job_id = %prepared.job_id,
                "daemon-only validation cancelled before completion"
            );
            return Ok(());
        }

        if let Err(error) = self
            .complete_job_service()
            .execute(CompleteJobCommand {
                job_id: prepared.job_id,
                outcome_class,
                result_schema_version: Some("validation_report:v1".to_string()),
                result_payload: Some(result_payload),
                output_commit_oid: None,
            })
            .await
        {
            let current_job = self.db.get_job(prepared.job_id).await?;
            if current_job.state.status() == JobStatus::Cancelled {
                info!(
                    job_id = %prepared.job_id,
                    "daemon-only validation was cancelled before completion was persisted"
                );
                return Ok(());
            }
            warn!(?error, job_id = %prepared.job_id, "harness validation completion failed");
            self.db
                .finish_job_non_success(FinishJobNonSuccessParams {
                    job_id: prepared.job_id,
                    item_id: prepared.item_id,
                    expected_item_revision_id: prepared.revision_id,
                    status: JobStatus::Failed,
                    outcome_class: Some(OutcomeClass::TerminalFailure),
                    error_code: Some("harness_command_failed".into()),
                    error_message: Some(format!("{error:?}")),
                    escalation_reason: None,
                })
                .await?;
            return Ok(());
        }

        self.append_activity(
            prepared.project_id,
            ActivityEventType::JobCompleted,
            ActivitySubject::Job(prepared.job_id),
            serde_json::json!({ "item_id": prepared.item_id, "outcome": outcome }),
        )
        .await?;

        if prepared.step_id == StepId::ValidateIntegrated && outcome_class == OutcomeClass::Clean {
            let updated_item = self.db.get_item(prepared.item_id).await?;
            if updated_item.approval_state == ApprovalState::Pending {
                self.append_activity(
                    prepared.project_id,
                    ActivityEventType::ApprovalRequested,
                    ActivitySubject::Item(prepared.item_id),
                    serde_json::json!({ "job_id": prepared.job_id }),
                )
                .await?;
            }
        }

        let revision = self.db.get_revision(prepared.revision_id).await?;
        let item = self.db.get_item(prepared.item_id).await?;
        let jobs = self.db.list_jobs_by_item(prepared.item_id).await?;
        let authoring_workspace = self
            .db
            .find_authoring_workspace_for_revision(prepared.revision_id)
            .await?;
        let authoring_head =
            ingot_usecases::dispatch::current_authoring_head_for_revision_with_workspace(
                &revision,
                &jobs,
                authoring_workspace.as_ref(),
            );
        let changed_paths = if let Some(ref head) = authoring_head {
            let base = revision
                .seed
                .seed_commit_oid()
                .or_else(|| {
                    authoring_workspace
                        .as_ref()
                        .and_then(|ws| ws.state.base_commit_oid())
                })
                .unwrap_or(head);
            let project = self.db.get_project(prepared.project_id).await?;
            let paths = self.refresh_project_mirror(&project).await?;
            changed_paths_between(&paths.mirror_git_dir, base, head)
                .await
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let context = rebuild_revision_context(
            &item,
            &revision,
            &jobs,
            authoring_head,
            changed_paths,
            Some(prepared.job_id),
            Utc::now(),
        );
        self.db.upsert_revision_context(&context).await?;

        self.auto_dispatch_projected_review(prepared.project_id, prepared.item_id)
            .await?;

        info!(
            job_id = %prepared.job_id,
            step_id = %prepared.step_id,
            outcome = outcome,
            "completed harness validation"
        );
        Ok(())
    }

    async fn harness_validation_cancelled(
        &self,
        prepared: &PreparedHarnessValidation,
    ) -> Result<bool, RuntimeError> {
        Ok(self.db.get_job(prepared.job_id).await?.state.status() == JobStatus::Cancelled)
    }

    async fn refresh_daemon_validation_heartbeat(&self, prepared: &PreparedHarnessValidation) {
        let lease_expires_at = self.next_lease_expiration();
        if let Err(error) = self
            .db
            .heartbeat_job_execution(
                prepared.job_id,
                prepared.item_id,
                prepared.revision_id,
                &self.lease_owner_id,
                lease_expires_at,
            )
            .await
        {
            warn!(
                ?error,
                job_id = %prepared.job_id,
                "daemon-only validation heartbeat update failed"
            );
        } else {
            debug!(
                job_id = %prepared.job_id,
                "daemon-only validation heartbeat updated"
            );
        }
    }

    async fn run_harness_command_with_heartbeats(
        &self,
        prepared: &PreparedHarnessValidation,
        command_spec: &HarnessCommand,
    ) -> HarnessCommandResult {
        let mut dispatch_listener = self.dispatch_notify.subscribe();
        let mut command = tokio::process::Command::new("sh");
        command
            .arg("-c")
            .arg(&command_spec.run)
            .current_dir(&prepared.workspace_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        command.process_group(0);

        #[cfg(test)]
        self.pause_before_pre_spawn_guard(PreSpawnPausePoint::HarnessBeforeSpawn)
            .await;

        if self.job_is_cancelled(prepared.job_id).await {
            info!(
                job_id = %prepared.job_id,
                command = %command_spec.name,
                "skipping harness command because job was cancelled before spawn"
            );
            return build_harness_command_result(
                -1,
                Ok(String::new()),
                Ok(String::new()),
                false,
                true,
                vec!["command cancelled".to_string()],
            );
        }

        let child = command.spawn();

        let mut child = match child {
            Ok(child) => child,
            Err(error) => {
                return HarnessCommandResult {
                    exit_code: -1,
                    stdout_tail: String::new(),
                    stderr_tail: format!("failed to spawn command: {error}"),
                    timed_out: false,
                    cancelled: false,
                };
            }
        };

        let stdout_task = spawn_pipe_reader(child.stdout.take());
        let stderr_task = spawn_pipe_reader(child.stderr.take());
        let mut ticker = interval(self.config.heartbeat_interval);
        let timeout = tokio::time::sleep(command_spec.timeout);
        tokio::pin!(timeout);

        loop {
            tokio::select! {
                result = child.wait() => {
                    match result {
                        Ok(status) => {
                            return build_harness_command_result(
                                status.code().unwrap_or(-1),
                                collect_pipe_output(stdout_task, "stdout").await,
                                collect_pipe_output(stderr_task, "stderr").await,
                                false,
                                false,
                                Vec::new(),
                            );
                        }
                        Err(error) => {
                            return build_harness_command_result(
                                -1,
                                collect_pipe_output(stdout_task, "stdout").await,
                                collect_pipe_output(stderr_task, "stderr").await,
                                false,
                                false,
                                vec![format!("command I/O error: {error}")],
                            );
                        }
                    }
                }
                _ = &mut timeout => {
                    if let Ok(Some(status)) = child.try_wait() {
                        return build_harness_command_result(
                            status.code().unwrap_or(-1),
                            collect_pipe_output(stdout_task, "stdout").await,
                            collect_pipe_output(stderr_task, "stderr").await,
                            false,
                            false,
                            Vec::new(),
                        );
                    }
                    let mut notes = vec!["command timed out".to_string()];
                    if let Err(error) = terminate_harness_command(&mut child).await {
                        notes.push(format!("failed to terminate timed out command: {error}"));
                    }
                    if let Err(error) = child.wait().await {
                        notes.push(format!("failed to reap timed out command: {error}"));
                    }
                    return build_harness_command_result(
                        -1,
                        collect_pipe_output(stdout_task, "stdout").await,
                        collect_pipe_output(stderr_task, "stderr").await,
                        true,
                        false,
                        notes,
                    );
                }
                notification = dispatch_listener.notified() => {
                    match self.db.get_job(prepared.job_id).await {
                        Ok(job) if job.state.status() == JobStatus::Cancelled => {
                            let mut notes = vec!["command cancelled".to_string()];
                            if let Err(error) = terminate_harness_command(&mut child).await {
                                notes.push(format!("failed to terminate cancelled command: {error}"));
                            }
                            if let Err(error) = child.wait().await {
                                notes.push(format!("failed to reap cancelled command: {error}"));
                            }
                            return build_harness_command_result(
                                -1,
                                collect_pipe_output(stdout_task, "stdout").await,
                                collect_pipe_output(stderr_task, "stderr").await,
                                false,
                                true,
                                notes,
                            );
                        }
                        Ok(_) => {
                            debug!(
                                job_id = %prepared.job_id,
                                command = %command_spec.name,
                                generation = notification.generation(),
                                reason = %notification.reason(),
                                "harness command woke on unrelated dispatcher notification"
                            );
                        }
                        Err(error) => {
                            warn!(
                                ?error,
                                job_id = %prepared.job_id,
                                command = %command_spec.name,
                                generation = notification.generation(),
                                reason = %notification.reason(),
                                "failed to load harness job after dispatcher notification"
                            );
                        }
                    }
                }
                _ = ticker.tick() => {
                    match self.db.get_job(prepared.job_id).await {
                        Ok(job) if job.state.status() == JobStatus::Cancelled => {
                            let mut notes = vec!["command cancelled".to_string()];
                            if let Err(error) = terminate_harness_command(&mut child).await {
                                notes.push(format!("failed to terminate cancelled command: {error}"));
                            }
                            if let Err(error) = child.wait().await {
                                notes.push(format!("failed to reap cancelled command: {error}"));
                            }
                            return build_harness_command_result(
                                -1,
                                collect_pipe_output(stdout_task, "stdout").await,
                                collect_pipe_output(stderr_task, "stderr").await,
                                false,
                                true,
                                notes,
                            );
                        }
                        Ok(_) => {}
                        Err(error) => {
                            warn!(
                                ?error,
                                job_id = %prepared.job_id,
                                "failed to load daemon-only validation job during heartbeat tick"
                            );
                        }
                    }
                    self.refresh_daemon_validation_heartbeat(prepared).await;
                }
            }
        }
    }

    async fn execute_harness_validation(&self, queued_job: Job) -> Result<(), RuntimeError> {
        match self.prepare_harness_validation(queued_job).await? {
            PrepareHarnessValidationOutcome::NotPrepared
            | PrepareHarnessValidationOutcome::FailedBeforeLaunch => Ok(()),
            PrepareHarnessValidationOutcome::Prepared(prepared) => {
                self.run_prepared_harness_validation(*prepared).await
            }
        }
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
        } else if let Some(job) = self
            .auto_dispatch_projected_validation_job(
                project,
                &item,
                &revision,
                &jobs,
                &findings,
                &convergences,
            )
            .await?
        {
            info!(job_id = %job.id, step_id = %job.step_id, item_id = %item.id, "auto-dispatched validation");
            Ok(true)
        } else {
            Ok(false)
        }
    }

    async fn auto_dispatch_projected_validation_job(
        &self,
        project: &Project,
        item: &ingot_domain::item::Item,
        revision: &ItemRevision,
        jobs: &[Job],
        findings: &[ingot_domain::finding::Finding],
        convergences: &[Convergence],
    ) -> Result<Option<Job>, RuntimeError> {
        let evaluation = Evaluator::new().evaluate(item, revision, jobs, findings, convergences);
        let Some(step_id) = evaluation.dispatchable_step_id else {
            return Ok(None);
        };
        if !step::is_closure_relevant_validate_step(step_id) {
            return Ok(None);
        }

        let mut job = dispatch_job(
            item,
            revision,
            jobs,
            findings,
            convergences,
            DispatchJobCommand {
                step_id: Some(step_id),
            },
        )
        .map_err(|error| {
            RuntimeError::InvalidState(format!("failed to auto-dispatch validation: {error}"))
        })?;

        if ingot_usecases::dispatch::should_fill_candidate_subject_from_workspace(job.step_id) {
            let authoring_workspace = self
                .db
                .find_authoring_workspace_for_revision(revision.id)
                .await?;
            let base = job
                .job_input
                .base_commit_oid()
                .map(ToOwned::to_owned)
                .or_else(|| {
                    ingot_usecases::dispatch::effective_authoring_base_commit_oid(
                        revision,
                        authoring_workspace.as_ref(),
                    )
                });
            let head = job
                .job_input
                .head_commit_oid()
                .map(ToOwned::to_owned)
                .or_else(|| {
                    ingot_usecases::dispatch::current_authoring_head_for_revision_with_workspace(
                        revision,
                        jobs,
                        authoring_workspace.as_ref(),
                    )
                });
            match (base, head) {
                (Some(base), Some(head)) => {
                    job.job_input = ingot_domain::job::JobInput::candidate_subject(base, head);
                }
                _ => {
                    return Err(RuntimeError::InvalidState(format!(
                        "failed to auto-dispatch validation: incomplete candidate subject for {}",
                        job.step_id
                    )));
                }
            }
        }

        self.db.create_job(&job).await?;
        self.append_activity(
            project.id,
            ActivityEventType::JobDispatched,
            ActivitySubject::Job(job.id),
            serde_json::json!({ "item_id": item.id, "step_id": job.step_id }),
        )
        .await?;

        Ok(Some(job))
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
            ActivitySubject::Job(prepared.job.id),
            serde_json::json!({ "item_id": prepared.item.id, "error_code": error_code }),
        )
        .await?;
        if let Some(escalation_reason) = escalation_reason {
            self.append_activity(
                prepared.project.id,
                ActivityEventType::ItemEscalated,
                ActivitySubject::Item(prepared.item.id),
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

    async fn cleanup_unclaimed_prepared_agent_run(
        &self,
        prepared: &PreparedRun,
    ) -> Result<(), RuntimeError> {
        self.cleanup_unclaimed_prepared_workspace(
            prepared.job.project_id,
            prepared.job.id,
            &prepared.workspace,
            prepared.workspace_lifecycle,
            &prepared.original_head_commit_oid,
            prepared.canonical_repo_path.as_path(),
        )
        .await
    }

    async fn cleanup_unclaimed_prepared_workspace(
        &self,
        project_id: ingot_domain::ids::ProjectId,
        job_id: ingot_domain::ids::JobId,
        workspace: &Workspace,
        workspace_lifecycle: WorkspaceLifecycle,
        original_head_commit_oid: &CommitOid,
        canonical_repo_path: &Path,
    ) -> Result<(), RuntimeError> {
        let _guard = self
            .project_locks
            .acquire_project_mutation(project_id)
            .await;

        let current_job = self.db.get_job(job_id).await?;
        if current_job.state.status() != JobStatus::Queued {
            return Ok(());
        }

        let mut persisted_workspace = self.db.get_workspace(workspace.id).await?;
        if persisted_workspace.state.current_job_id() != Some(job_id) {
            return Ok(());
        }

        let now = Utc::now();
        match workspace_lifecycle {
            WorkspaceLifecycle::PersistentAuthoring | WorkspaceLifecycle::PersistentIntegration => {
                persisted_workspace.release_with_head(original_head_commit_oid.clone(), now);
                self.db.update_workspace(&persisted_workspace).await?;
            }
            WorkspaceLifecycle::EphemeralReview => {
                remove_workspace(canonical_repo_path, Path::new(&persisted_workspace.path)).await?;
                persisted_workspace.mark_abandoned(now);
                self.db.update_workspace(&persisted_workspace).await?;
            }
        }

        Ok(())
    }

    async fn fail_job_preparation(
        &self,
        job: &Job,
        item: &ingot_domain::item::Item,
        project: &Project,
        error_code: &'static str,
        error_message: String,
    ) -> Result<(), RuntimeError> {
        let outcome_class = OutcomeClass::TerminalFailure;
        let escalation_reason = failure_escalation_reason(job, outcome_class);

        self.db
            .finish_job_non_success(FinishJobNonSuccessParams {
                job_id: job.id,
                item_id: item.id,
                expected_item_revision_id: job.item_revision_id,
                status: JobStatus::Failed,
                outcome_class: Some(outcome_class),
                error_code: Some(error_code.into()),
                error_message: Some(error_message.clone()),
                escalation_reason,
            })
            .await?;
        self.append_activity(
            project.id,
            ActivityEventType::JobFailed,
            ActivitySubject::Job(job.id),
            serde_json::json!({ "item_id": item.id, "error_code": error_code }),
        )
        .await?;
        if let Some(escalation_reason) = escalation_reason {
            self.append_activity(
                project.id,
                ActivityEventType::ItemEscalated,
                ActivitySubject::Item(item.id),
                serde_json::json!({ "reason": escalation_reason }),
            )
            .await?;
        }
        self.refresh_revision_context_for_ids(
            project.id,
            item.id,
            job.item_revision_id,
            Some(job.id),
        )
        .await?;
        warn!(
            job_id = %job.id,
            error_code,
            error_message = %error_message,
            "job failed during preparation"
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
            ActivitySubject::Item(prepared.item.id),
            serde_json::json!({ "reason": "successful_retry", "job_id": prepared.job.id }),
        )
        .await?;

        Ok(())
    }

    async fn finalize_workspace_after_success(
        &self,
        prepared: &PreparedRun,
        head_commit_oid: Option<&CommitOid>,
    ) -> Result<(), RuntimeError> {
        let _guard = self
            .project_locks
            .acquire_project_mutation(prepared.job.project_id)
            .await;

        match prepared.workspace_lifecycle {
            WorkspaceLifecycle::PersistentAuthoring => {
                let mut workspace = self.db.get_workspace(prepared.workspace.id).await?;
                let now = Utc::now();
                workspace.release_to(WorkspaceStatus::Ready, now);
                if let Some(head_commit_oid) = head_commit_oid {
                    workspace.set_head_commit_oid(head_commit_oid.clone(), now);
                }
                self.db.update_workspace(&workspace).await?;
            }
            WorkspaceLifecycle::PersistentIntegration => {
                let mut workspace = self.db.get_workspace(prepared.workspace.id).await?;
                workspace.release_to(WorkspaceStatus::Ready, Utc::now());
                self.db.update_workspace(&workspace).await?;
            }
            WorkspaceLifecycle::EphemeralReview => {
                remove_workspace(
                    prepared.canonical_repo_path.as_path(),
                    Path::new(&prepared.workspace.path),
                )
                .await?;
                let mut workspace = self.db.get_workspace(prepared.workspace.id).await?;
                workspace.mark_abandoned(Utc::now());
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
        remove_workspace(repo_path.as_path(), &workspace.path).await?;
        let mut workspace = workspace.clone();
        workspace.mark_abandoned(Utc::now());
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
                let now = Utc::now();
                workspace.release_with_head(prepared.original_head_commit_oid.clone(), now);
                self.db.update_workspace(&workspace).await?;
            }
            WorkspaceLifecycle::PersistentIntegration => {
                let mut workspace = self.db.get_workspace(prepared.workspace.id).await?;
                let now = Utc::now();
                workspace.release_with_head(prepared.original_head_commit_oid.clone(), now);
                self.db.update_workspace(&workspace).await?;
            }
            WorkspaceLifecycle::EphemeralReview => {
                let mut workspace = self.db.get_workspace(prepared.workspace.id).await?;
                workspace.mark_abandoned(Utc::now());
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
                    &[
                        "reset",
                        "--hard",
                        prepared.original_head_commit_oid.as_str(),
                    ],
                )
                .await?;
                git(workspace_path, &["clean", "-fd"]).await?;
                if let Some(workspace_ref) = prepared.workspace.workspace_ref.as_ref() {
                    git(
                        prepared.canonical_repo_path.as_path(),
                        &[
                            "update-ref",
                            workspace_ref.as_str(),
                            prepared.original_head_commit_oid.as_str(),
                        ],
                    )
                    .await?;
                }
            }
            WorkspaceLifecycle::PersistentIntegration => {
                let workspace_path = Path::new(&prepared.workspace.path);
                git(
                    workspace_path,
                    &[
                        "reset",
                        "--hard",
                        prepared.original_head_commit_oid.as_str(),
                    ],
                )
                .await?;
                git(workspace_path, &["clean", "-fd"]).await?;
                if let Some(workspace_ref) = prepared.workspace.workspace_ref.as_ref() {
                    git(
                        prepared.canonical_repo_path.as_path(),
                        &[
                            "update-ref",
                            workspace_ref.as_str(),
                            prepared.original_head_commit_oid.as_str(),
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
            authoring_base_commit_oid.as_ref(),
            authoring_head_commit_oid.as_ref(),
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
    ) -> Result<Option<CommitOid>, RuntimeError> {
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
    ) -> Result<Option<CommitOid>, RuntimeError> {
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
        subject: ActivitySubject,
        payload: serde_json::Value,
    ) -> Result<(), RuntimeError> {
        self.db
            .append_activity(&Activity {
                id: ingot_domain::ids::ActivityId::new(),
                project_id,
                event_type,
                subject,
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

    fn lease_ttl(&self) -> ChronoDuration {
        ChronoDuration::from_std(self.config.lease_ttl).expect("lease ttl fits chrono duration")
    }

    fn next_lease_expiration(&self) -> chrono::DateTime<Utc> {
        Utc::now() + self.lease_ttl()
    }
}

async fn run_prepared_agent_job(
    dispatcher: JobDispatcher,
    prepared: PreparedRun,
    _permit: OwnedSemaphorePermit,
) -> RunningJobResult {
    let job_id = prepared.job.id;
    RunningJobResult {
        job_id,
        result: dispatcher.execute_prepared_agent_job(prepared).await,
    }
}

async fn run_prepared_harness_validation_job(
    dispatcher: JobDispatcher,
    prepared: PreparedHarnessValidation,
    _permit: OwnedSemaphorePermit,
) -> RunningJobResult {
    let job_id = prepared.job_id;
    RunningJobResult {
        job_id,
        result: dispatcher.run_prepared_harness_validation(prepared).await,
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
        ) | (
            WorkspaceKind::Authoring | WorkspaceKind::Integration,
            ExecutionPermission::DaemonOnly,
            OutputArtifactKind::ValidationReport,
        )
    )
}

fn supports_job(agent: &Agent, job: &Job) -> bool {
    if job.execution_permission == ExecutionPermission::DaemonOnly
        || !agent
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
        ExecutionPermission::DaemonOnly => unreachable!("daemon-only jobs are filtered above"),
    }
}

fn is_daemon_only_validation(job: &Job) -> bool {
    job.execution_permission == ExecutionPermission::DaemonOnly
        && job.phase_kind == PhaseKind::Validate
}

fn is_inert_assigned_authoring_dispatch_residue(job: &Job) -> bool {
    job.state.status() == JobStatus::Assigned
        && job.phase_kind == PhaseKind::Author
        && job.workspace_kind == WorkspaceKind::Authoring
        && job.execution_permission == ExecutionPermission::MayMutate
        && job.output_artifact_kind == OutputArtifactKind::Commit
        && job.state.workspace_id().is_some()
        && job.state.agent_id().is_none()
        && job.state.prompt_snapshot().is_none()
        && job.state.phase_template_digest().is_none()
        && job.state.process_pid().is_none()
        && job.state.lease_owner_id().is_none()
        && job.state.heartbeat_at().is_none()
        && job.state.lease_expires_at().is_none()
        && job.state.started_at().is_none()
}

fn read_harness_profile_if_present(
    project_path: &Path,
) -> Result<Option<HarnessProfile>, HarnessLoadError> {
    let path = project_path.join(".ingot/harness.toml");
    match std::fs::read_to_string(&path) {
        Ok(content) => HarnessProfile::from_toml(&content)
            .map(Some)
            .map_err(|source| HarnessLoadError::InvalidProfile { path, source }),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(HarnessLoadError::ReadProfile { path, source }),
    }
}

fn load_harness_profile(project_path: &Path) -> Result<HarnessProfile, HarnessLoadError> {
    Ok(read_harness_profile_if_present(project_path)?.unwrap_or_default())
}

fn resolve_harness_prompt_context(
    project_path: &Path,
) -> Result<HarnessPromptContext, HarnessLoadError> {
    let harness = load_harness_profile(project_path)?;
    Ok(HarnessPromptContext {
        commands: harness.commands,
        skills: resolve_harness_skills(project_path, &harness.skills.paths)?,
    })
}

fn resolve_harness_skills(
    project_path: &Path,
    patterns: &[String],
) -> Result<Vec<ResolvedHarnessSkill>, HarnessLoadError> {
    let canonical_project_path = std::fs::canonicalize(project_path).map_err(|source| {
        HarnessLoadError::CanonicalizeProjectPath {
            path: project_path.to_path_buf(),
            source,
        }
    })?;
    let mut seen = BTreeSet::new();
    let mut resolved = Vec::new();
    for pattern in patterns {
        let pattern_path = project_path.join(pattern);
        let pattern_glob = pattern_path.to_string_lossy().into_owned();
        let mut matches = Vec::new();
        for entry in glob(&pattern_glob).map_err(|error| HarnessLoadError::InvalidSkillGlob {
            pattern: pattern.clone(),
            message: error.msg.to_string(),
        })? {
            match entry {
                Ok(path) => matches.push(path),
                Err(error) => {
                    return Err(HarnessLoadError::ResolveSkillPath {
                        pattern: pattern.clone(),
                        source: io::Error::new(error.error().kind(), error.error().to_string()),
                    });
                }
            }
        }
        matches.sort();
        for path in matches {
            if !path.is_file() {
                continue;
            }
            let canonical_path = std::fs::canonicalize(&path).map_err(|source| {
                HarnessLoadError::ResolveSkillPath {
                    pattern: pattern.clone(),
                    source,
                }
            })?;
            let relative_path = canonical_path
                .strip_prefix(&canonical_project_path)
                .map_err(|_| HarnessLoadError::SkillPathEscapesProjectRoot {
                    pattern: pattern.clone(),
                    project_path: canonical_project_path.clone(),
                    path: canonical_path.clone(),
                })?
                .display()
                .to_string();
            if !seen.insert(relative_path.clone()) {
                continue;
            }
            let contents = std::fs::read_to_string(&canonical_path).map_err(|source| {
                HarnessLoadError::ReadSkill {
                    path: canonical_path.clone(),
                    source,
                }
            })?;
            resolved.push(ResolvedHarnessSkill {
                relative_path,
                contents,
            });
        }
    }
    Ok(resolved)
}

struct HarnessCommandResult {
    exit_code: i32,
    stdout_tail: String,
    stderr_tail: String,
    timed_out: bool,
    cancelled: bool,
}

fn build_harness_command_result(
    exit_code: i32,
    stdout: Result<String, String>,
    stderr: Result<String, String>,
    timed_out: bool,
    cancelled: bool,
    mut notes: Vec<String>,
) -> HarnessCommandResult {
    let stdout_tail = match stdout {
        Ok(output) => tail_lines(&output, 50),
        Err(error) => {
            notes.push(error);
            String::new()
        }
    };
    let stderr_tail = match stderr {
        Ok(output) => {
            if output.is_empty() {
                notes.join("\n\n")
            } else {
                let mut parts = notes;
                parts.push(tail_lines(&output, 50));
                parts.join("\n\n")
            }
        }
        Err(error) => {
            notes.push(error);
            notes.join("\n\n")
        }
    };
    HarnessCommandResult {
        exit_code,
        stdout_tail,
        stderr_tail,
        timed_out,
        cancelled,
    }
}

fn spawn_pipe_reader<R>(pipe: Option<R>) -> tokio::task::JoinHandle<io::Result<String>>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move { read_pipe_to_string(pipe).await })
}

async fn collect_pipe_output(
    handle: tokio::task::JoinHandle<io::Result<String>>,
    stream_name: &str,
) -> Result<String, String> {
    match handle.await {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(error)) => Err(format!("failed to read {stream_name}: {error}")),
        Err(error) => Err(format!("{stream_name} reader task failed: {error}")),
    }
}

async fn read_pipe_to_string<R>(pipe: Option<R>) -> io::Result<String>
where
    R: AsyncRead + Unpin,
{
    let Some(mut pipe) = pipe else {
        return Ok(String::new());
    };
    let mut output = Vec::new();
    pipe.read_to_end(&mut output).await?;
    Ok(String::from_utf8_lossy(&output).into_owned())
}

#[cfg(unix)]
async fn terminate_harness_command(child: &mut tokio::process::Child) -> io::Result<()> {
    let Some(pid) = child.id() else {
        return Ok(());
    };
    let result = unsafe { libc::killpg(pid as i32, libc::SIGKILL) };
    if result == 0 {
        Ok(())
    } else {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        }
    }
}

#[cfg(not(unix))]
async fn terminate_harness_command(child: &mut tokio::process::Child) -> io::Result<()> {
    child.kill().await
}

fn tail_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() <= n {
        s.to_string()
    } else {
        lines[lines.len() - n..].join("\n")
    }
}

fn built_in_template(template_slug: &str, step_id: StepId) -> &'static str {
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
            StepId::AuthorInitial => {
                "Implement the requested change directly in the repository. Keep the edit set focused on the acceptance criteria and preserve surrounding style."
            }
            StepId::ReviewIncrementalInitial
            | StepId::ReviewIncrementalRepair
            | StepId::ReviewIncrementalAfterIntegrationRepair => {
                "Review only the requested incremental diff and report concrete findings against the exact review subject."
            }
            StepId::ReviewCandidateInitial
            | StepId::ReviewCandidateRepair
            | StepId::ReviewAfterIntegrationRepair => {
                "Review the full candidate diff from the seed commit to the current head and report concrete findings when necessary."
            }
            StepId::ValidateCandidateInitial
            | StepId::ValidateCandidateRepair
            | StepId::ValidateAfterIntegrationRepair
            | StepId::ValidateIntegrated => {
                "Run objective validation against the current workspace subject and report failed checks or findings only when they are real."
            }
            StepId::InvestigateItem => {
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

fn commit_subject(title: &str, step_id: StepId) -> String {
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
    use std::collections::VecDeque;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use ingot_domain::agent::AgentCapability;
    use ingot_domain::job::{
        ContextPolicy, ExecutionPermission, JobInput, JobStatus, OutputArtifactKind, PhaseKind,
    };
    use ingot_domain::workspace::WorkspaceStatus;
    use ingot_git::commands::head_oid;
    use ingot_test_support::fixtures::{
        AgentBuilder, ItemBuilder, JobBuilder, ProjectBuilder, RevisionBuilder, default_timestamp,
    };
    use ingot_test_support::git::{temp_git_repo, unique_temp_path};
    use ingot_test_support::sqlite::migrated_test_db;
    use ingot_usecases::job_lifecycle;
    use ingot_workflow::step;
    use tokio::sync::Notify;

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

    #[tokio::test]
    async fn drain_until_idle_stops_after_first_idle_result() {
        let script = Arc::new(Mutex::new(VecDeque::from([Ok(false)])));
        let calls = Arc::new(Mutex::new(0usize));

        drain_until_idle({
            let script = Arc::clone(&script);
            let calls = Arc::clone(&calls);
            move || {
                *calls.lock().expect("calls lock") += 1;
                let next = script
                    .lock()
                    .expect("script lock")
                    .pop_front()
                    .expect("scripted result");
                std::future::ready(next)
            }
        })
        .await
        .expect("drain should stop");

        assert_eq!(*calls.lock().expect("calls lock"), 1);
        assert!(script.lock().expect("script lock").is_empty());
    }

    #[tokio::test]
    async fn drain_until_idle_retries_until_idle_result() {
        let script = Arc::new(Mutex::new(VecDeque::from([Ok(true), Ok(true), Ok(false)])));
        let calls = Arc::new(Mutex::new(0usize));

        drain_until_idle({
            let script = Arc::clone(&script);
            let calls = Arc::clone(&calls);
            move || {
                *calls.lock().expect("calls lock") += 1;
                let next = script
                    .lock()
                    .expect("script lock")
                    .pop_front()
                    .expect("scripted result");
                std::future::ready(next)
            }
        })
        .await
        .expect("drain should stop");

        assert_eq!(*calls.lock().expect("calls lock"), 3);
        assert!(script.lock().expect("script lock").is_empty());
    }

    #[tokio::test]
    async fn drain_until_idle_returns_first_error() {
        let script = Arc::new(Mutex::new(VecDeque::from([
            Ok(true),
            Err(RuntimeError::InvalidState("boom".into())),
        ])));
        let calls = Arc::new(Mutex::new(0usize));

        let error = drain_until_idle({
            let script = Arc::clone(&script);
            let calls = Arc::clone(&calls);
            move || {
                *calls.lock().expect("calls lock") += 1;
                let next = script
                    .lock()
                    .expect("script lock")
                    .pop_front()
                    .expect("scripted result");
                std::future::ready(next)
            }
        })
        .await
        .expect_err("drain should surface error");

        assert!(matches!(error, RuntimeError::InvalidState(message) if message == "boom"));
        assert_eq!(*calls.lock().expect("calls lock"), 2);
        assert!(script.lock().expect("script lock").is_empty());
    }

    #[derive(Default)]
    struct BlockingRunnerState {
        launches: usize,
        release_budget: usize,
    }

    #[derive(Clone, Default)]
    struct BlockingRunner {
        state: Arc<Mutex<BlockingRunnerState>>,
        launch_notify: Arc<Notify>,
        release_notify: Arc<Notify>,
    }

    impl BlockingRunner {
        fn new() -> Self {
            Self::default()
        }

        fn launch_count(&self) -> usize {
            self.state.lock().expect("blocking runner state").launches
        }
    }

    impl AgentRunner for BlockingRunner {
        fn launch<'a>(
            &'a self,
            _agent: &'a Agent,
            _request: &'a AgentRequest,
            _working_dir: &'a Path,
        ) -> Pin<Box<dyn Future<Output = Result<AgentResponse, AgentError>> + Send + 'a>> {
            Box::pin(async move {
                {
                    let mut state = self.state.lock().expect("blocking runner state");
                    state.launches += 1;
                }
                self.launch_notify.notify_waiters();

                loop {
                    let can_release = {
                        let mut state = self.state.lock().expect("blocking runner state");
                        if state.release_budget > 0 {
                            state.release_budget -= 1;
                            true
                        } else {
                            false
                        }
                    };
                    if can_release {
                        break;
                    }
                    self.release_notify.notified().await;
                }

                Ok(AgentResponse {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                    result: Some(serde_json::json!({ "message": "implemented change" })),
                })
            })
        }
    }

    struct TestRuntimeHarness {
        db: Database,
        dispatcher: JobDispatcher,
        dispatch_notify: DispatchNotify,
        project: Project,
        repo_path: PathBuf,
    }

    impl TestRuntimeHarness {
        async fn new(runner: Arc<dyn AgentRunner>) -> Self {
            Self::with_config(runner, None).await
        }

        async fn with_config(
            runner: Arc<dyn AgentRunner>,
            config: Option<DispatcherConfig>,
        ) -> Self {
            let repo_path = temp_git_repo("ingot-runtime-lib");
            let db = migrated_test_db("ingot-runtime-lib").await;
            let state_root = unique_temp_path("ingot-runtime-lib-state");
            let config = config.unwrap_or_else(|| DispatcherConfig::new(state_root));
            let dispatch_notify = DispatchNotify::default();
            let dispatcher = JobDispatcher::with_runner(
                db.clone(),
                ProjectLocks::default(),
                config,
                runner,
                dispatch_notify.clone(),
            );
            let project = ProjectBuilder::new(&repo_path)
                .created_at(default_timestamp())
                .build();
            db.create_project(&project).await.expect("create project");

            Self {
                db,
                dispatcher,
                dispatch_notify,
                project,
                repo_path,
            }
        }

        async fn register_mutating_agent(&self) -> Agent {
            let agent = AgentBuilder::new(
                "codex",
                vec![
                    AgentCapability::MutatingJobs,
                    AgentCapability::ReadOnlyJobs,
                    AgentCapability::StructuredOutput,
                ],
            )
            .build();
            self.db.create_agent(&agent).await.expect("create agent");
            agent
        }
    }

    fn write_harness_toml(repo_path: &Path, contents: &str) {
        let ingot_dir = repo_path.join(".ingot");
        std::fs::create_dir_all(&ingot_dir).expect("create .ingot dir");
        std::fs::write(ingot_dir.join("harness.toml"), contents).expect("write harness.toml");
    }

    #[tokio::test]
    async fn run_with_heartbeats_does_not_launch_runner_when_job_is_cancelled_before_spawn() {
        let runner = Arc::new(BlockingRunner::new());
        let harness = TestRuntimeHarness::new(runner.clone()).await;
        harness.register_mutating_agent().await;

        let item_id = ingot_domain::ids::ItemId::new();
        let revision_id = ingot_domain::ids::ItemRevisionId::new();
        let seed_commit = head_oid(&harness.repo_path)
            .await
            .expect("seed head")
            .into_inner();
        let item = ItemBuilder::new(harness.project.id, revision_id)
            .id(item_id)
            .build();
        let revision = RevisionBuilder::new(item_id)
            .id(revision_id)
            .explicit_seed(seed_commit.as_str())
            .build();
        harness
            .db
            .create_item_with_revision(&item, &revision)
            .await
            .expect("create item");

        let job = JobBuilder::new(harness.project.id, item_id, revision_id, "author_initial")
            .phase_kind(PhaseKind::Author)
            .workspace_kind(WorkspaceKind::Authoring)
            .execution_permission(ExecutionPermission::MayMutate)
            .phase_template_slug("author-initial")
            .job_input(JobInput::authoring_head(CommitOid::new(seed_commit)))
            .output_artifact_kind(OutputArtifactKind::Commit)
            .build();
        harness.db.create_job(&job).await.expect("create job");

        let mut dispatcher = harness.dispatcher.clone();
        let pause_hook = PreSpawnPauseHook::new(PreSpawnPausePoint::AgentBeforeSpawn);
        dispatcher.pre_spawn_pause_hook = Some(pause_hook.clone());

        let prepared = match dispatcher
            .prepare_run(harness.db.get_job(job.id).await.expect("load queued job"))
            .await
            .expect("prepare run")
        {
            PrepareRunOutcome::Prepared(prepared) => *prepared,
            _ => panic!("expected prepared run"),
        };
        let workspace_id = prepared.workspace.id;
        let request = AgentRequest {
            prompt: prepared.prompt.clone(),
            working_dir: prepared.workspace.path.clone(),
            may_mutate: prepared.job.execution_permission == ExecutionPermission::MayMutate,
            timeout_seconds: Some(dispatcher.config.job_timeout.as_secs()),
            output_schema: output_schema_for_job(&prepared.job),
        };

        let run_task = tokio::spawn({
            let dispatcher = dispatcher.clone();
            let prepared = prepared.clone();
            async move { dispatcher.run_with_heartbeats(&prepared, request).await }
        });

        pause_hook
            .wait_until_entered(1, Duration::from_secs(2))
            .await;

        let active_job = harness.db.get_job(job.id).await.expect("reload active job");
        job_lifecycle::cancel_job(
            &harness.db,
            &harness.db,
            &harness.db,
            &active_job,
            &item,
            "operator_cancelled",
            WorkspaceStatus::Ready,
        )
        .await
        .expect("cancel active job");
        harness.dispatch_notify.notify();

        let cancelled_job = harness
            .db
            .get_job(job.id)
            .await
            .expect("reload cancelled job");
        let released_workspace = harness
            .db
            .get_workspace(workspace_id)
            .await
            .expect("reload released workspace");
        assert_eq!(cancelled_job.state.status(), JobStatus::Cancelled);
        assert_eq!(released_workspace.state.current_job_id(), None);

        pause_hook.release();

        let result = run_task.await.expect("join run_with_heartbeats task");
        assert!(matches!(result, AgentRunOutcome::Cancelled));
        assert_eq!(runner.launch_count(), 0);
    }

    #[tokio::test]
    async fn run_with_heartbeats_claims_running_job_with_configured_lease_ttl() {
        let runner = Arc::new(BlockingRunner::new());
        let mut config = DispatcherConfig::new(unique_temp_path("ingot-runtime-lease-ttl-state"));
        config.lease_ttl = Duration::from_secs(17);
        let harness = TestRuntimeHarness::with_config(runner.clone(), Some(config)).await;
        harness.register_mutating_agent().await;

        let item_id = ingot_domain::ids::ItemId::new();
        let revision_id = ingot_domain::ids::ItemRevisionId::new();
        let seed_commit = head_oid(&harness.repo_path)
            .await
            .expect("seed head")
            .into_inner();
        let item = ItemBuilder::new(harness.project.id, revision_id)
            .id(item_id)
            .build();
        let revision = RevisionBuilder::new(item_id)
            .id(revision_id)
            .explicit_seed(seed_commit.as_str())
            .build();
        harness
            .db
            .create_item_with_revision(&item, &revision)
            .await
            .expect("create item");

        let job = JobBuilder::new(harness.project.id, item_id, revision_id, "author_initial")
            .phase_kind(PhaseKind::Author)
            .workspace_kind(WorkspaceKind::Authoring)
            .execution_permission(ExecutionPermission::MayMutate)
            .phase_template_slug("author-initial")
            .job_input(JobInput::authoring_head(CommitOid::new(seed_commit)))
            .output_artifact_kind(OutputArtifactKind::Commit)
            .build();
        harness.db.create_job(&job).await.expect("create job");

        let mut dispatcher = harness.dispatcher.clone();
        let pause_hook = PreSpawnPauseHook::new(PreSpawnPausePoint::AgentBeforeSpawn);
        dispatcher.pre_spawn_pause_hook = Some(pause_hook.clone());

        let prepared = match dispatcher
            .prepare_run(harness.db.get_job(job.id).await.expect("load queued job"))
            .await
            .expect("prepare run")
        {
            PrepareRunOutcome::Prepared(prepared) => *prepared,
            _ => panic!("expected prepared run"),
        };
        let request = AgentRequest {
            prompt: prepared.prompt.clone(),
            working_dir: prepared.workspace.path.clone(),
            may_mutate: prepared.job.execution_permission == ExecutionPermission::MayMutate,
            timeout_seconds: Some(dispatcher.config.job_timeout.as_secs()),
            output_schema: output_schema_for_job(&prepared.job),
        };

        let run_task = tokio::spawn({
            let dispatcher = dispatcher.clone();
            let prepared = prepared.clone();
            async move { dispatcher.run_with_heartbeats(&prepared, request).await }
        });

        pause_hook
            .wait_until_entered(1, Duration::from_secs(2))
            .await;

        let running_job = harness
            .db
            .get_job(job.id)
            .await
            .expect("reload running job");
        let started_at = running_job.state.started_at().expect("started_at");
        let lease_expires_at = running_job
            .state
            .lease_expires_at()
            .expect("lease_expires_at");
        let lease_duration = lease_expires_at.signed_duration_since(started_at);
        assert!(lease_duration <= ChronoDuration::seconds(17));
        assert!(lease_duration >= ChronoDuration::seconds(16));

        pause_hook.release();
        {
            let mut state = runner.state.lock().expect("blocking runner state");
            state.release_budget = 1;
        }
        runner.release_notify.notify_waiters();

        let result = run_task.await.expect("join run_with_heartbeats task");
        assert!(matches!(result, AgentRunOutcome::Completed(_)));
    }

    #[tokio::test]
    async fn cleanup_supervised_task_releases_workspace_for_unclaimed_prepared_agent_job() {
        let harness = TestRuntimeHarness::new(Arc::new(BlockingRunner::new())).await;
        harness.register_mutating_agent().await;

        let item_id = ingot_domain::ids::ItemId::new();
        let revision_id = ingot_domain::ids::ItemRevisionId::new();
        let seed_commit = head_oid(&harness.repo_path)
            .await
            .expect("seed head")
            .into_inner();
        let item = ItemBuilder::new(harness.project.id, revision_id)
            .id(item_id)
            .build();
        let revision = RevisionBuilder::new(item_id)
            .id(revision_id)
            .explicit_seed(seed_commit.as_str())
            .build();
        harness
            .db
            .create_item_with_revision(&item, &revision)
            .await
            .expect("create item");

        let job = JobBuilder::new(harness.project.id, item_id, revision_id, "author_initial")
            .phase_kind(PhaseKind::Author)
            .workspace_kind(WorkspaceKind::Authoring)
            .execution_permission(ExecutionPermission::MayMutate)
            .phase_template_slug("author-initial")
            .job_input(JobInput::authoring_head(CommitOid::new(seed_commit)))
            .output_artifact_kind(OutputArtifactKind::Commit)
            .build();
        harness.db.create_job(&job).await.expect("create job");

        let prepared = match harness
            .dispatcher
            .prepare_run(harness.db.get_job(job.id).await.expect("load queued job"))
            .await
            .expect("prepare run")
        {
            PrepareRunOutcome::Prepared(prepared) => *prepared,
            _ => panic!("expected prepared run"),
        };

        let workspace_before = harness
            .db
            .get_workspace(prepared.workspace.id)
            .await
            .expect("workspace before cleanup");
        assert_eq!(workspace_before.state.status(), WorkspaceStatus::Busy);
        assert_eq!(workspace_before.state.current_job_id(), Some(job.id));

        harness
            .dispatcher
            .cleanup_supervised_task(
                RunningJobMeta::Agent(Box::new(prepared.clone())),
                "supervised task failed".into(),
            )
            .await
            .expect("cleanup supervised task");

        let updated_job = harness.db.get_job(job.id).await.expect("reload job");
        let updated_workspace = harness
            .db
            .get_workspace(prepared.workspace.id)
            .await
            .expect("reload workspace");
        assert_eq!(updated_job.state.status(), JobStatus::Queued);
        assert_eq!(updated_workspace.state.status(), WorkspaceStatus::Ready);
        assert_eq!(updated_workspace.state.current_job_id(), None);
    }

    #[tokio::test]
    async fn supervisor_does_not_launch_same_job_twice_during_pre_spawn_pause() {
        let runner = Arc::new(BlockingRunner::new());
        let harness = TestRuntimeHarness::new(runner.clone()).await;
        harness.register_mutating_agent().await;

        let item_id = ingot_domain::ids::ItemId::new();
        let revision_id = ingot_domain::ids::ItemRevisionId::new();
        let seed_commit = head_oid(&harness.repo_path)
            .await
            .expect("seed head")
            .into_inner();
        let item = ItemBuilder::new(harness.project.id, revision_id)
            .id(item_id)
            .build();
        let revision = RevisionBuilder::new(item_id)
            .id(revision_id)
            .explicit_seed(seed_commit.as_str())
            .build();
        harness
            .db
            .create_item_with_revision(&item, &revision)
            .await
            .expect("create item");

        let job = JobBuilder::new(harness.project.id, item_id, revision_id, "author_initial")
            .phase_kind(PhaseKind::Author)
            .workspace_kind(WorkspaceKind::Authoring)
            .execution_permission(ExecutionPermission::MayMutate)
            .phase_template_slug("author-initial")
            .job_input(JobInput::authoring_head(CommitOid::new(
                seed_commit.clone(),
            )))
            .output_artifact_kind(OutputArtifactKind::Commit)
            .build();
        harness.db.create_job(&job).await.expect("create job");

        let mut dispatcher = harness.dispatcher.clone();
        let pause_hook = PreSpawnPauseHook::new(PreSpawnPausePoint::AgentBeforeSpawn);
        dispatcher.pre_spawn_pause_hook = Some(pause_hook.clone());

        let semaphore = Arc::new(Semaphore::new(1));
        let mut running = JoinSet::<RunningJobResult>::new();
        let mut running_meta = HashMap::<TaskId, RunningJobMeta>::new();
        let mut running_job_ids = HashSet::new();

        let made_progress = dispatcher
            .run_supervisor_iteration(
                &mut running,
                &mut running_meta,
                &mut running_job_ids,
                &semaphore,
            )
            .await
            .expect("first supervisor iteration");
        assert!(made_progress);

        pause_hook
            .wait_until_entered(1, Duration::from_secs(2))
            .await;

        let running_job = harness
            .db
            .get_job(job.id)
            .await
            .expect("reload running job");
        assert_eq!(running_job.state.status(), JobStatus::Running);
        assert_eq!(running_meta.len(), 1);
        assert_eq!(running_job_ids.len(), 1);
        assert_eq!(runner.launch_count(), 0);

        dispatcher
            .run_supervisor_iteration(
                &mut running,
                &mut running_meta,
                &mut running_job_ids,
                &semaphore,
            )
            .await
            .expect("second supervisor iteration");

        assert_eq!(running_meta.len(), 1);
        assert_eq!(running_job_ids.len(), 1);
        assert_eq!(runner.launch_count(), 0);

        {
            let mut state = runner.state.lock().expect("blocking runner state");
            state.release_budget = 1;
        }
        pause_hook.release();
        runner.release_notify.notify_waiters();

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                dispatcher
                    .reap_completed_tasks(&mut running, &mut running_meta, &mut running_job_ids)
                    .await
                    .expect("reap completed tasks");
                if running_meta.is_empty() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("supervised task should finish");

        assert_eq!(runner.launch_count(), 1);
        assert!(running_job_ids.is_empty());
        let completed_job = harness.db.get_job(job.id).await.expect("completed job");
        assert!(completed_job.state.status().is_terminal());
    }

    #[tokio::test]
    async fn run_harness_command_with_heartbeats_does_not_spawn_command_when_job_is_cancelled_before_spawn()
     {
        let runner = Arc::new(BlockingRunner::new());
        let harness = TestRuntimeHarness::new(runner).await;

        let item_id = ingot_domain::ids::ItemId::new();
        let revision_id = ingot_domain::ids::ItemRevisionId::new();
        let seed_commit = head_oid(&harness.repo_path)
            .await
            .expect("seed head")
            .into_inner();
        let item = ItemBuilder::new(harness.project.id, revision_id)
            .id(item_id)
            .build();
        let revision = RevisionBuilder::new(item_id)
            .id(revision_id)
            .explicit_seed(seed_commit.as_str())
            .build();
        harness
            .db
            .create_item_with_revision(&item, &revision)
            .await
            .expect("create item");

        write_harness_toml(
            &harness.repo_path,
            r#"
[commands.marker]
run = "printf spawned > pre_spawn_marker.txt"
timeout = "30s"
"#,
        );

        let job = JobBuilder::new(
            harness.project.id,
            item_id,
            revision_id,
            step::VALIDATE_CANDIDATE_INITIAL,
        )
        .phase_kind(PhaseKind::Validate)
        .workspace_kind(WorkspaceKind::Authoring)
        .execution_permission(ExecutionPermission::DaemonOnly)
        .context_policy(ContextPolicy::None)
        .phase_template_slug("")
        .job_input(JobInput::candidate_subject(
            CommitOid::new(seed_commit.clone()),
            CommitOid::new(seed_commit.clone()),
        ))
        .output_artifact_kind(OutputArtifactKind::ValidationReport)
        .build();
        harness.db.create_job(&job).await.expect("create job");

        let mut dispatcher = harness.dispatcher.clone();
        let pause_hook = PreSpawnPauseHook::new(PreSpawnPausePoint::HarnessBeforeSpawn);
        dispatcher.pre_spawn_pause_hook = Some(pause_hook.clone());

        let prepared = match dispatcher
            .prepare_harness_validation(harness.db.get_job(job.id).await.expect("load queued job"))
            .await
            .expect("prepare harness validation")
        {
            PrepareHarnessValidationOutcome::Prepared(prepared) => *prepared,
            _ => panic!("expected prepared harness validation"),
        };
        let workspace_id = prepared.workspace_id;
        let marker_path = prepared.workspace_path.join("pre_spawn_marker.txt");
        let command = prepared
            .harness
            .commands
            .first()
            .expect("prepared harness command")
            .clone();

        let run_task = tokio::spawn({
            let dispatcher = dispatcher.clone();
            let prepared = prepared.clone();
            let command = command.clone();
            async move {
                dispatcher
                    .run_harness_command_with_heartbeats(&prepared, &command)
                    .await
            }
        });

        pause_hook
            .wait_until_entered(1, Duration::from_secs(2))
            .await;

        let active_job = harness.db.get_job(job.id).await.expect("reload active job");
        job_lifecycle::cancel_job(
            &harness.db,
            &harness.db,
            &harness.db,
            &active_job,
            &item,
            "operator_cancelled",
            WorkspaceStatus::Ready,
        )
        .await
        .expect("cancel active job");
        harness.dispatch_notify.notify();

        let cancelled_job = harness
            .db
            .get_job(job.id)
            .await
            .expect("reload cancelled job");
        let released_workspace = harness
            .db
            .get_workspace(workspace_id)
            .await
            .expect("reload released workspace");
        assert_eq!(cancelled_job.state.status(), JobStatus::Cancelled);
        assert_eq!(released_workspace.state.current_job_id(), None);

        pause_hook.release();

        let result = run_task
            .await
            .expect("join run_harness_command_with_heartbeats task");
        assert!(result.cancelled);
        assert!(!marker_path.exists());
    }

    #[tokio::test]
    async fn prepare_harness_validation_uses_configured_lease_ttl() {
        let mut config = DispatcherConfig::new(unique_temp_path("ingot-runtime-harness-lease"));
        config.lease_ttl = Duration::from_secs(23);
        let harness =
            TestRuntimeHarness::with_config(Arc::new(BlockingRunner::new()), Some(config)).await;

        let item_id = ingot_domain::ids::ItemId::new();
        let revision_id = ingot_domain::ids::ItemRevisionId::new();
        let seed_commit = head_oid(&harness.repo_path)
            .await
            .expect("seed head")
            .into_inner();
        let item = ItemBuilder::new(harness.project.id, revision_id)
            .id(item_id)
            .build();
        let revision = RevisionBuilder::new(item_id)
            .id(revision_id)
            .explicit_seed(seed_commit.as_str())
            .build();
        harness
            .db
            .create_item_with_revision(&item, &revision)
            .await
            .expect("create item");

        write_harness_toml(
            &harness.repo_path,
            r#"
[commands.marker]
run = "printf spawned > pre_spawn_marker.txt"
timeout = "30s"
"#,
        );

        let job = JobBuilder::new(
            harness.project.id,
            item_id,
            revision_id,
            step::VALIDATE_CANDIDATE_INITIAL,
        )
        .phase_kind(PhaseKind::Validate)
        .workspace_kind(WorkspaceKind::Authoring)
        .execution_permission(ExecutionPermission::DaemonOnly)
        .context_policy(ContextPolicy::None)
        .phase_template_slug("")
        .job_input(JobInput::candidate_subject(
            CommitOid::new(seed_commit.clone()),
            CommitOid::new(seed_commit.clone()),
        ))
        .output_artifact_kind(OutputArtifactKind::ValidationReport)
        .build();
        harness.db.create_job(&job).await.expect("create job");

        let prepared = match harness
            .dispatcher
            .prepare_harness_validation(harness.db.get_job(job.id).await.expect("load queued job"))
            .await
            .expect("prepare harness validation")
        {
            PrepareHarnessValidationOutcome::Prepared(prepared) => *prepared,
            _ => panic!("expected prepared harness validation"),
        };

        let running_job = harness
            .db
            .get_job(job.id)
            .await
            .expect("reload running job");
        let started_at = running_job.state.started_at().expect("started_at");
        let lease_expires_at = running_job
            .state
            .lease_expires_at()
            .expect("lease_expires_at");
        let lease_duration = lease_expires_at.signed_duration_since(started_at);
        assert!(lease_duration <= ChronoDuration::seconds(23));
        assert!(lease_duration >= ChronoDuration::seconds(22));

        let workspace = harness
            .db
            .get_workspace(prepared.workspace_id)
            .await
            .expect("reload workspace");
        assert_eq!(workspace.state.status(), WorkspaceStatus::Ready);
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
