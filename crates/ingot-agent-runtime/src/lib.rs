mod autopilot;
mod bootstrap;
mod convergence;
mod dispatch;
mod execution;
mod harness;
mod job_support;
mod preparation;
pub(crate) mod reconciliation;
mod runtime_ports;
mod supervisor;
#[cfg(test)]
mod test_support;

use execution::run_prepared_agent_job;
use harness::{HarnessPromptContext, resolve_harness_prompt_context};
use supervisor::RunningJobResult;

use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use chrono::{Duration as ChronoDuration, Utc};
use ingot_agent_protocol::adapter::AgentError;
use ingot_agent_protocol::request::AgentRequest;
use ingot_agent_protocol::response::AgentResponse;
use ingot_config::paths::job_logs_dir;
use ingot_domain::activity::{Activity, ActivityEventType, ActivitySubject};
use ingot_domain::agent::Agent;
use ingot_domain::convergence::Convergence;
use ingot_domain::job::{Job, JobStatus};
use ingot_domain::lease_owner_id::LeaseOwnerId;
use ingot_domain::ports::RepositoryError;
use ingot_domain::project::Project;
use ingot_git::GitJobCompletionPort;
use ingot_git::commands::{GitCommandError, resolve_ref_oid};
use ingot_git::project_repo::project_repo_paths;
use ingot_store_sqlite::Database;
use ingot_usecases::{CompleteJobService, DispatchNotify, ProjectLocks};
use ingot_workspace::WorkspaceError;
use tracing::warn;

pub(crate) use job_support::{
    PrepareRunOutcome, PreparedRun, WorkspaceLifecycle, built_in_template, commit_subject,
    failure_escalation_reason, format_revision_context,
    is_inert_assigned_authoring_dispatch_residue, is_supported_runtime_job, non_empty_message,
    outcome_class_name, should_clear_item_escalation_on_success, supports_job, template_digest,
};
pub(crate) use runtime_ports::{
    RuntimeConvergencePort, RuntimeFinalizePort, RuntimeReconciliationPort, drain_until_idle,
    usecase_from_runtime_error, usecase_to_runtime_error,
};

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

type AgentLaunchFuture<'a> =
    Pin<Box<dyn std::future::Future<Output = Result<AgentResponse, AgentError>> + Send + 'a>>;

pub trait AgentRunner: Send + Sync {
    fn launch<'a>(
        &'a self,
        agent: &'a Agent,
        request: &'a AgentRequest,
        working_dir: &'a Path,
    ) -> AgentLaunchFuture<'a>;
}

#[derive(Debug, Clone, Default)]
pub struct CliAgentRunner;

impl AgentRunner for CliAgentRunner {
    fn launch<'a>(
        &'a self,
        agent: &'a Agent,
        request: &'a AgentRequest,
        working_dir: &'a Path,
    ) -> AgentLaunchFuture<'a> {
        Box::pin(ingot_agent_adapters::launch_agent(
            agent,
            request,
            working_dir,
        ))
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
    test_hooks: test_support::DispatcherTestHooks,
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
            test_hooks: test_support::DispatcherTestHooks::default(),
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
        job_logs_dir(self.config.state_root.as_path(), job_id)
    }

    fn lease_ttl(&self) -> ChronoDuration {
        ChronoDuration::from_std(self.config.lease_ttl).expect("lease ttl fits chrono duration")
    }

    fn next_lease_expiration(&self) -> chrono::DateTime<Utc> {
        Utc::now() + self.lease_ttl()
    }
}

// Re-export report contract from protocol crate for internal use.
pub(crate) use ingot_agent_protocol::report;

#[cfg(test)]
mod tests;
