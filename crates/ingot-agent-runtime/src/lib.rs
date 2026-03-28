mod autopilot;
mod bootstrap;
mod convergence;
mod dispatch;
mod execution;
mod harness;
mod preparation;
pub(crate) mod reconciliation;
mod supervisor;

use execution::run_prepared_agent_job;
use harness::{HarnessPromptContext, resolve_harness_prompt_context};
use supervisor::RunningJobResult;

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
use ingot_domain::activity::{Activity, ActivityEventType, ActivitySubject};
use ingot_domain::agent::{AdapterKind, Agent, AgentCapability};
use ingot_domain::commit_oid::CommitOid;
use ingot_domain::convergence::Convergence;
use ingot_domain::convergence_queue::ConvergenceQueueEntry;
use ingot_domain::git_operation::GitOperation;
use ingot_domain::item::EscalationReason;
use ingot_domain::job::{
    ExecutionPermission, Job, JobAssignment, JobStatus, OutcomeClass, OutputArtifactKind, PhaseKind,
};
use ingot_domain::lease_owner_id::LeaseOwnerId;
use ingot_domain::ports::{ProjectMutationLockPort, RepositoryError};
use ingot_domain::project::Project;
use ingot_domain::revision::ItemRevision;
use ingot_domain::revision_context::RevisionContext;
use ingot_domain::step_id::StepId;
use ingot_domain::workspace::{Workspace, WorkspaceKind};
use ingot_git::GitJobCompletionPort;
use ingot_git::commands::{
    FinalizeTargetRefOutcome, GitCommandError, finalize_target_ref as finalize_target_ref_in_repo,
    resolve_ref_oid,
};
use ingot_git::project_repo::{
    CheckoutFinalizationStatus, CheckoutSyncStatus, checkout_finalization_status,
    project_repo_paths, sync_checkout_to_commit,
};
use ingot_store_sqlite::Database;
use ingot_usecases::convergence::{
    CheckoutFinalizationReadiness, ConvergenceSystemActionPort, FinalizationTarget,
    FinalizePreparedTrigger, FinalizeTargetRefResult, PreparedConvergenceFinalizePort,
    SystemActionItemState, SystemActionProjectState,
};
use ingot_usecases::reconciliation::ReconciliationPort;
use ingot_usecases::{CompleteJobService, DispatchNotify, ProjectLocks};
use ingot_workspace::WorkspaceError;
use sha2::{Digest, Sha256};
use tracing::warn;

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
                AdapterKind::ClaudeCode => {
                    ClaudeCodeCliAdapter::new(agent.cli_path.clone(), agent.model.clone())
                        .launch(request, working_dir)
                        .await
                }
            }
        })
    }
}

#[derive(Clone)]
pub struct JobDispatcher {
    db: Database,
    project_locks: ProjectLocks,
    config: DispatcherConfig,
    lease_owner_id: LeaseOwnerId,
    runner: Arc<dyn AgentRunner>,
    dispatch_notify: DispatchNotify,
    #[cfg(test)]
    pre_spawn_pause_hook: Option<PreSpawnPauseHook>,
    #[cfg(test)]
    auto_queue_pause_hook: Option<AutoQueuePauseHook>,
    #[cfg(test)]
    projected_recovery_pause_hook: Option<ProjectedRecoveryPauseHook>,
}

#[cfg(test)]
type PreSpawnPauseHook = PauseHook<PreSpawnPausePoint>;

#[cfg(test)]
type AutoQueuePauseHook = PauseHook<AutoQueuePausePoint>;

#[cfg(test)]
type ProjectedRecoveryPauseHook = PauseHook<ProjectedRecoveryPausePoint>;

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreSpawnPausePoint {
    AgentBeforeSpawn,
    HarnessBeforeSpawn,
}

#[cfg(test)]
#[derive(Clone)]
struct PauseHook<P> {
    point: P,
    state: Arc<PauseHookState>,
}

#[cfg(test)]
struct PauseHookState {
    entered: std::sync::Mutex<usize>,
    released: std::sync::Mutex<bool>,
    entered_notify: tokio::sync::Notify,
    release_notify: tokio::sync::Notify,
}

#[cfg(test)]
impl<P> PauseHook<P>
where
    P: Copy + Eq,
{
    fn new(point: P) -> Self {
        Self {
            point,
            state: Arc::new(PauseHookState {
                entered: std::sync::Mutex::new(0),
                released: std::sync::Mutex::new(false),
                entered_notify: tokio::sync::Notify::new(),
                release_notify: tokio::sync::Notify::new(),
            }),
        }
    }

    async fn pause_if_matching(&self, point: P) {
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

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutoQueuePausePoint {
    BeforeGuard,
    BeforeInsert,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProjectedRecoveryPausePoint {
    BeforeGuard,
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

    fn auto_queue_convergence(
        &self,
        project_id: ingot_domain::ids::ProjectId,
        item_id: ingot_domain::ids::ItemId,
    ) -> impl Future<Output = Result<bool, ingot_usecases::UseCaseError>> + Send {
        let dispatcher = self.dispatcher.clone();
        async move {
            #[cfg(test)]
            dispatcher.pause_before_auto_queue_guard().await;
            let _guard = dispatcher
                .project_locks
                .acquire_project_mutation(project_id)
                .await;
            let project = dispatcher
                .db
                .get_project(project_id)
                .await
                .map_err(ingot_usecases::UseCaseError::Repository)?;
            if project.execution_mode != ingot_domain::project::ExecutionMode::Autopilot {
                return Ok(false);
            }
            dispatcher
                .auto_queue_convergence_inner(project_id, item_id, &project)
                .await
        }
    }
}

impl PreparedConvergenceFinalizePort for RuntimeFinalizePort {
    fn find_or_create_finalize_operation(
        &self,
        operation: &GitOperation,
    ) -> impl Future<Output = Result<GitOperation, ingot_usecases::UseCaseError>> + Send {
        let db = self.dispatcher.db.clone();
        let operation = operation.clone();
        async move {
            ingot_usecases::convergence::find_or_create_finalize_operation(&db, &operation).await
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
            lease_owner_id: LeaseOwnerId::new(format!("ingotd:{}", std::process::id())),
            runner,
            dispatch_notify,
            #[cfg(test)]
            pre_spawn_pause_hook: None,
            #[cfg(test)]
            auto_queue_pause_hook: None,
            #[cfg(test)]
            projected_recovery_pause_hook: None,
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

    #[cfg(test)]
    async fn pause_before_auto_queue_guard(&self) {
        if let Some(hook) = &self.auto_queue_pause_hook {
            hook.pause_if_matching(AutoQueuePausePoint::BeforeGuard)
                .await;
        }
    }

    #[cfg(test)]
    async fn pause_before_auto_queue_insert(&self) {
        if let Some(hook) = &self.auto_queue_pause_hook {
            hook.pause_if_matching(AutoQueuePausePoint::BeforeInsert)
                .await;
        }
    }

    #[cfg(test)]
    async fn pause_before_projected_recovery_guard(&self) {
        if let Some(hook) = &self.projected_recovery_pause_hook {
            hook.pause_if_matching(ProjectedRecoveryPausePoint::BeforeGuard)
                .await;
        }
    }

    fn project_paths(&self, project: &Project) -> ingot_git::project_repo::ProjectRepoPaths {
        project_repo_paths(self.config.state_root.as_path(), project.id, &project.path)
    }

    pub async fn refresh_project_mirror(
        &self,
        project: &Project,
    ) -> Result<ingot_git::project_repo::ProjectRepoPaths, RuntimeError> {
        ingot_git::project_repo::refresh_project_mirror(
            &self.db,
            self.config.state_root.as_path(),
            project.id,
            &project.path,
        )
        .await
        .map_err(|error| match error {
            ingot_git::project_repo::RefreshMirrorError::Repository(error) => {
                RuntimeError::Repository(error)
            }
            ingot_git::project_repo::RefreshMirrorError::Git(error) => RuntimeError::Git(error),
        })
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

// Re-export report contract from protocol crate for internal use.
pub(crate) use ingot_agent_protocol::report;

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

#[cfg(test)]
mod tests;
