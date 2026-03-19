use std::collections::{BTreeSet, HashMap};
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
use ingot_domain::activity::{Activity, ActivityEventType};
use ingot_domain::agent::{AdapterKind, Agent};
use ingot_domain::convergence::{Convergence, ConvergenceStatus, PrepareFailureKind};
use ingot_domain::convergence_queue::{ConvergenceQueueEntry, ConvergenceQueueEntryStatus};
use ingot_domain::finding::FindingTriageState;
use ingot_domain::git_operation::{
    ConvergenceReplayMetadata, GitOperation, GitOperationStatus, OperationKind, OperationPayload,
};
use ingot_domain::harness::{HarnessCommand, HarnessProfile, HarnessProfileError};
use ingot_domain::ids::{GitOperationId, WorkspaceId};
use ingot_domain::item::{
    ApprovalState, DoneReason, Escalation, EscalationReason, Lifecycle, ResolutionSource,
};
use ingot_domain::job::{
    ExecutionPermission, Job, JobState, JobStatus, OutcomeClass, OutputArtifactKind,
};
use ingot_domain::ports::{JobCompletionMutation, ProjectMutationLockPort, RepositoryError};
use ingot_domain::project::Project;
use ingot_domain::revision::ItemRevision;
use ingot_domain::revision_context::RevisionContext;
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
use ingot_store_sqlite::{Database, FinishJobNonSuccessParams, StartJobExecutionParams};
use ingot_usecases::convergence::{
    ConvergenceSystemActionPort, FinalizePreparedTrigger, PreparedConvergenceFinalizePort,
    SystemActionItemState, SystemActionProjectState, finalize_prepared_convergence,
};
use ingot_usecases::job::{DispatchJobCommand, dispatch_job};
use ingot_usecases::reconciliation::ReconciliationPort;
use ingot_usecases::{
    CompleteJobCommand, CompleteJobService, ConvergenceService, DispatchNotify, ProjectLocks,
    ReconciliationService, rebuild_revision_context,
};
use ingot_workflow::{Evaluator, RecommendedAction};
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

mod agent_execution;
mod artifacts;
mod completion;
mod git_ops;
mod harness_execution;
mod ports;
#[cfg(test)]
mod pre_spawn_tests;
mod prepare;
mod projected_dispatch;
mod prompt;
mod startup;
mod supervisor;
mod system_actions;
mod workspace;

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
    pub max_concurrent_jobs: usize,
}

impl DispatcherConfig {
    pub fn new(state_root: PathBuf) -> Self {
        Self {
            state_root,
            poll_interval: Duration::from_secs(1),
            heartbeat_interval: Duration::from_secs(5),
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
    step_id: String,
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
    Agent(PreparedRun),
    HarnessValidation(PreparedHarnessValidation),
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
        project_repo_paths(
            self.config.state_root.as_path(),
            project.id,
            Path::new(&project.path),
        )
    }
}
