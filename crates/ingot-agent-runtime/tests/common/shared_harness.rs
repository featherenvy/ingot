use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ingot_agent_protocol::adapter::AgentError;
use ingot_agent_protocol::request::AgentRequest;
use ingot_agent_protocol::response::AgentResponse;
use ingot_domain::agent::{Agent, AgentCapability};
use ingot_domain::ids;
use ingot_domain::job::{Job, JobStatus};
use ingot_domain::project::Project;
use ingot_domain::test_support::{AgentBuilder, ProjectBuilder, default_timestamp};
use ingot_store_sqlite::Database;
use ingot_usecases::{DispatchNotify, ProjectLocks};
use runtime_crate::{AgentRunner, DispatcherConfig, JobDispatcher};
use tokio::sync::Notify;
use tokio::time::{sleep, timeout};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestAgentProfile {
    Mutating,
    ReviewOnly,
    Full,
}

impl TestAgentProfile {
    fn capabilities(self) -> Vec<AgentCapability> {
        match self {
            Self::Mutating => vec![
                AgentCapability::MutatingJobs,
                AgentCapability::StructuredOutput,
            ],
            Self::ReviewOnly => vec![
                AgentCapability::ReadOnlyJobs,
                AgentCapability::StructuredOutput,
            ],
            Self::Full => vec![
                AgentCapability::MutatingJobs,
                AgentCapability::ReadOnlyJobs,
                AgentCapability::StructuredOutput,
            ],
        }
    }
}

pub fn agent_fixture(name: &str, profile: TestAgentProfile) -> Agent {
    AgentBuilder::new(name, profile.capabilities()).build()
}

pub struct TestHarness {
    pub db: Database,
    pub dispatcher: JobDispatcher,
    pub dispatch_notify: DispatchNotify,
    pub project: Project,
    pub state_root: PathBuf,
    pub repo_path: PathBuf,
}

impl TestHarness {
    pub async fn new(runner: Arc<dyn AgentRunner>) -> Self {
        Self::with_config(runner, None).await
    }

    pub async fn with_config(
        runner: Arc<dyn AgentRunner>,
        config: Option<DispatcherConfig>,
    ) -> Self {
        let repo_path = ingot_test_support::git::temp_git_repo("ingot-runtime-repo");
        let db = ingot_test_support::sqlite::migrated_test_db("ingot-runtime").await;
        let state_root = ingot_test_support::git::unique_temp_path("ingot-runtime-state");
        let config = config.unwrap_or_else(|| DispatcherConfig::new(state_root.clone()));
        let dispatch_notify = DispatchNotify::default();
        let dispatcher = JobDispatcher::with_runner(
            db.clone(),
            ProjectLocks::default(),
            config.clone(),
            runner,
            dispatch_notify.clone(),
        );

        let project = ProjectBuilder::new(&repo_path)
            .id(ids::ProjectId::new())
            .created_at(default_timestamp())
            .build();
        db.create_project(&project).await.expect("create project");

        Self {
            db,
            dispatcher,
            dispatch_notify,
            project,
            state_root: config.state_root.clone(),
            repo_path,
        }
    }

    pub async fn register_agent(&self, name: &str, profile: TestAgentProfile) -> Agent {
        let agent = agent_fixture(name, profile);
        self.db.create_agent(&agent).await.expect("create agent");
        agent
    }

    pub async fn register_mutating_agent(&self) -> Agent {
        self.register_agent("codex", TestAgentProfile::Mutating)
            .await
    }

    pub async fn register_review_agent(&self) -> Agent {
        self.register_agent("codex-review", TestAgentProfile::ReviewOnly)
            .await
    }

    pub async fn register_full_agent(&self) -> Agent {
        self.register_agent("codex", TestAgentProfile::Full).await
    }

    pub async fn wait_for_job_status(
        &self,
        job_id: ids::JobId,
        expected: JobStatus,
        timeout_duration: Duration,
    ) -> Job {
        match timeout(timeout_duration, async {
            loop {
                let job = self.db.get_job(job_id).await.expect("load job");
                if job.state.status() == expected {
                    return job;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        {
            Ok(job) => job,
            Err(_) => {
                let current = self.db.get_job(job_id).await.expect("load timed out job");
                panic!(
                    "timed out waiting for job {job_id} to reach {expected:?}; last status was {:?}",
                    current.state.status()
                );
            }
        }
    }

    pub async fn wait_for_running_jobs(
        &self,
        expected: usize,
        timeout_duration: Duration,
    ) -> Vec<Job> {
        match timeout(timeout_duration, async {
            loop {
                let jobs = self
                    .db
                    .list_jobs_by_project(self.project.id)
                    .await
                    .expect("list project jobs")
                    .into_iter()
                    .filter(|job| job.state.status() == JobStatus::Running)
                    .collect::<Vec<_>>();
                if jobs.len() == expected {
                    return jobs;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        {
            Ok(jobs) => jobs,
            Err(_) => {
                let jobs = self
                    .db
                    .list_jobs_by_project(self.project.id)
                    .await
                    .expect("list jobs after timeout");
                panic!(
                    "timed out waiting for {expected} running jobs; current statuses: {:?}",
                    jobs.into_iter()
                        .map(|job| (job.id, job.state.status()))
                        .collect::<Vec<_>>()
                );
            }
        }
    }
}

#[derive(Default)]
struct BlockingRunnerState {
    launches: usize,
    release_budget: usize,
}

#[derive(Clone, Default)]
pub struct BlockingRunner {
    state: Arc<Mutex<BlockingRunnerState>>,
    launch_notify: Arc<Notify>,
    release_notify: Arc<Notify>,
}

impl BlockingRunner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn launch_count(&self) -> usize {
        self.state.lock().expect("blocking runner state").launches
    }

    pub async fn wait_for_launches(&self, expected: usize, timeout_duration: Duration) {
        timeout(timeout_duration, async {
            loop {
                if self.state.lock().expect("blocking runner state").launches >= expected {
                    return;
                }
                self.launch_notify.notified().await;
            }
        })
        .await
        .expect("timed out waiting for runner launches");
    }

    pub fn release_one(&self) {
        let mut state = self.state.lock().expect("blocking runner state");
        state.release_budget += 1;
        drop(state);
        self.release_notify.notify_one();
    }

    pub fn release_all(&self) {
        let mut state = self.state.lock().expect("blocking runner state");
        state.release_budget = usize::MAX / 2;
        drop(state);
        self.release_notify.notify_waiters();
    }
}

impl AgentRunner for BlockingRunner {
    fn launch<'a>(
        &'a self,
        _agent: &'a Agent,
        _request: &'a AgentRequest,
        working_dir: &'a Path,
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

            tokio::fs::write(working_dir.join("generated.txt"), "hello")
                .await
                .expect("write generated file");
            Ok(AgentResponse {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
                result: Some(serde_json::json!({ "message": "implemented change" })),
            })
        })
    }
}
