use super::*;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::execution::AgentRunOutcome;
use crate::harness::PrepareHarnessValidationOutcome;
use crate::supervisor::RunningJobMeta;
use tokio::sync::Semaphore;
use tokio::task::{Id as TaskId, JoinSet};

use ingot_domain::activity::ActivityEventType;
use ingot_domain::agent::AgentCapability;
use ingot_domain::finding::FindingSeverity;
use ingot_domain::item::ApprovalState;
use ingot_domain::job::{
    ContextPolicy, ExecutionPermission, JobInput, JobStatus, OutputArtifactKind, PhaseKind,
};
use ingot_domain::project::{AutoTriageDecision, AutoTriagePolicy, ExecutionMode};
use ingot_domain::revision::ApprovalPolicy;
use ingot_domain::workspace::WorkspaceStatus;
use ingot_git::commands::head_oid;
use ingot_test_support::fixtures::{
    AgentBuilder, ConvergenceQueueEntryBuilder, FindingBuilder, ItemBuilder, JobBuilder,
    ProjectBuilder, RevisionBuilder, default_timestamp,
};
use ingot_test_support::git::{run_git as git_sync, temp_git_repo, unique_temp_path};
use ingot_test_support::sqlite::migrated_test_db;
use ingot_usecases::job_lifecycle;
use ingot_workflow::step;
use sqlx::query;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
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

#[derive(Clone)]
struct StaticResponseRunner {
    response: AgentResponse,
}

impl StaticResponseRunner {
    fn new(response: AgentResponse) -> Self {
        Self { response }
    }
}

impl AgentRunner for StaticResponseRunner {
    fn launch<'a>(
        &'a self,
        _agent: &'a Agent,
        _request: &'a AgentRequest,
        _working_dir: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<AgentResponse, AgentError>> + Send + 'a>> {
        Box::pin(async move { Ok(self.response.clone()) })
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

    async fn with_config(runner: Arc<dyn AgentRunner>, config: Option<DispatcherConfig>) -> Self {
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
        .seed_commit_oid(None::<String>)
        .seed_target_commit_oid(Some(seed_commit.clone()))
        .build();
    harness
        .db
        .create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    std::fs::write(harness.repo_path.join("advanced.txt"), "advanced head")
        .expect("write advanced");
    git_sync(&harness.repo_path, &["add", "advanced.txt"]);
    git_sync(&harness.repo_path, &["commit", "-m", "advanced head"]);
    let rebound_head = head_oid(&harness.repo_path)
        .await
        .expect("rebound head")
        .into_inner();

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

    let prepared = match harness
        .dispatcher
        .prepare_run(harness.db.get_job(job.id).await.expect("load queued job"))
        .await
        .expect("prepare run")
    {
        PrepareRunOutcome::Prepared(prepared) => *prepared,
        _ => panic!("expected prepared run"),
    };
    assert_eq!(
        prepared.job.job_input,
        JobInput::authoring_head(CommitOid::new(rebound_head.clone()))
    );

    let workspace_before = harness
        .db
        .get_workspace(prepared.workspace.id)
        .await
        .expect("workspace before cleanup");
    assert_eq!(workspace_before.state.status(), WorkspaceStatus::Busy);
    assert_eq!(workspace_before.state.current_job_id(), Some(job.id));
    assert_eq!(
        workspace_before.state.base_commit_oid(),
        Some(&CommitOid::new(rebound_head.clone()))
    );
    assert_eq!(
        workspace_before.state.head_commit_oid(),
        Some(&CommitOid::new(rebound_head.clone()))
    );

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
    assert_eq!(
        updated_job.job_input,
        JobInput::authoring_head(CommitOid::new(rebound_head))
    );
    assert_eq!(updated_workspace.state.status(), WorkspaceStatus::Ready);
    assert_eq!(updated_workspace.state.current_job_id(), None);
}

#[tokio::test]
async fn prepare_run_rebinds_implicit_author_initial_head_after_target_advances() {
    let harness = TestRuntimeHarness::new(Arc::new(BlockingRunner::new())).await;
    harness.register_mutating_agent().await;

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let seeded_target_head = head_oid(&harness.repo_path)
        .await
        .expect("seed target head")
        .into_inner();
    let item = ItemBuilder::new(harness.project.id, revision_id)
        .id(item_id)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .seed_commit_oid(None::<String>)
        .seed_target_commit_oid(Some(seeded_target_head.clone()))
        .template_map_snapshot(serde_json::json!({ "author_initial": "author-initial" }))
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
            seeded_target_head.clone(),
        )))
        .output_artifact_kind(OutputArtifactKind::Commit)
        .build();
    harness.db.create_job(&job).await.expect("create job");

    std::fs::write(
        harness.repo_path.join("advanced.txt"),
        "advanced target head",
    )
    .expect("write advanced");
    git_sync(&harness.repo_path, &["add", "advanced.txt"]);
    git_sync(
        &harness.repo_path,
        &["commit", "-m", "advanced target head"],
    );
    let advanced_target_head = head_oid(&harness.repo_path)
        .await
        .expect("advanced target head")
        .into_inner();

    let prepared = match harness
        .dispatcher
        .prepare_run(harness.db.get_job(job.id).await.expect("load queued job"))
        .await
        .expect("prepare run")
    {
        PrepareRunOutcome::Prepared(prepared) => *prepared,
        _ => panic!("expected prepared run"),
    };

    assert_eq!(
        prepared.job.job_input,
        JobInput::authoring_head(CommitOid::new(advanced_target_head.clone()))
    );

    let reloaded_job = harness.db.get_job(job.id).await.expect("reload job");
    assert_eq!(
        reloaded_job.job_input,
        JobInput::authoring_head(CommitOid::new(advanced_target_head.clone()))
    );

    let workspace = harness
        .db
        .get_workspace(prepared.workspace.id)
        .await
        .expect("reload workspace");
    assert_eq!(
        workspace.state.base_commit_oid(),
        Some(&CommitOid::new(advanced_target_head.clone()))
    );
    assert_eq!(
        workspace.state.head_commit_oid(),
        Some(&CommitOid::new(advanced_target_head))
    );
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

#[tokio::test]
async fn auto_queue_convergence_treats_conflicting_insert_as_noop() {
    let repo_path = temp_git_repo("ingot-runtime-lib-autopilot");
    let db_path = unique_temp_path("ingot-runtime-lib-autopilot-db").with_extension("db");
    let options = SqliteConnectOptions::new()
        .filename(&db_path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(5));
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await
        .expect("connect db");
    let db = Database::from_pool(pool);
    db.migrate().await.expect("migrate db");

    let dispatcher = JobDispatcher::with_runner(
        db.clone(),
        ProjectLocks::default(),
        DispatcherConfig::new(unique_temp_path("ingot-runtime-lib-autopilot-state")),
        Arc::new(BlockingRunner::new()),
        DispatchNotify::default(),
    );

    let created_at = default_timestamp();
    let project = ProjectBuilder::new(&repo_path)
        .execution_mode(ExecutionMode::Autopilot)
        .created_at(created_at)
        .build();
    db.create_project(&project).await.expect("create project");

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let seed_commit = head_oid(&repo_path).await.expect("seed head").into_inner();
    let item = ItemBuilder::new(project.id, revision_id)
        .id(item_id)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .explicit_seed(seed_commit.as_str())
        .build();
    db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");
    let validate_job = JobBuilder::new(
        project.id,
        item_id,
        revision_id,
        step::VALIDATE_CANDIDATE_INITIAL,
    )
    .status(JobStatus::Completed)
    .outcome_class(OutcomeClass::Clean)
    .phase_kind(PhaseKind::Validate)
    .workspace_kind(WorkspaceKind::Integration)
    .execution_permission(ExecutionPermission::MustNotMutate)
    .context_policy(ContextPolicy::ResumeContext)
    .phase_template_slug("validate-candidate")
    .job_input(JobInput::None)
    .output_artifact_kind(OutputArtifactKind::ValidationReport)
    .created_at(created_at)
    .started_at(created_at)
    .ended_at(created_at)
    .build();
    db.create_job(&validate_job).await.expect("create job");

    let mut writer = db.raw_pool().acquire().await.expect("acquire writer");
    query("BEGIN IMMEDIATE")
        .execute(&mut *writer)
        .await
        .expect("begin write transaction");

    let mut dispatcher = dispatcher.clone();
    let pause_hook = AutoQueuePauseHook::new(AutoQueuePausePoint::BeforeInsert);
    dispatcher.auto_queue_pause_hook = Some(pause_hook.clone());

    let queue_task = tokio::spawn({
        let port = RuntimeConvergencePort { dispatcher };
        async move { port.auto_queue_convergence(project.id, item.id).await }
    });

    pause_hook
        .wait_until_entered(1, Duration::from_secs(2))
        .await;

    let queue_entry = ConvergenceQueueEntryBuilder::new(project.id, item.id, revision.id)
        .created_at(created_at)
        .build();
    query(
        "INSERT INTO convergence_queue_entries (
            id, project_id, item_id, item_revision_id, target_ref, status, head_acquired_at,
            created_at, updated_at, released_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(queue_entry.id)
    .bind(queue_entry.project_id)
    .bind(queue_entry.item_id)
    .bind(queue_entry.item_revision_id)
    .bind(&queue_entry.target_ref)
    .bind(queue_entry.status)
    .bind(queue_entry.head_acquired_at)
    .bind(queue_entry.created_at)
    .bind(queue_entry.updated_at)
    .bind(queue_entry.released_at)
    .execute(&mut *writer)
    .await
    .expect("insert conflicting queue entry");
    query("COMMIT")
        .execute(&mut *writer)
        .await
        .expect("commit conflicting queue entry");
    pause_hook.release();

    let queued = queue_task
        .await
        .expect("join queue task")
        .expect("concurrent autopilot queueing should complete without surfacing the race");
    assert!(
        !queued,
        "concurrent autopilot queueing should degrade to a no-op"
    );

    let active_entries = db
        .list_active_queue_entries_for_lane(project.id, &revision.target_ref)
        .await
        .expect("list queue entries");
    assert_eq!(active_entries.len(), 1);
    assert_eq!(active_entries[0].item_revision_id, revision.id);
}

#[tokio::test]
async fn auto_triage_job_findings_treats_missing_policy_as_disabled() {
    let harness = TestRuntimeHarness::new(Arc::new(BlockingRunner::new())).await;

    let mut project = harness
        .db
        .get_project(harness.project.id)
        .await
        .expect("project");
    project.execution_mode = ExecutionMode::Autopilot;
    project.auto_triage_policy = None;
    harness
        .db
        .update_project(&project)
        .await
        .expect("update project");

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let item = ItemBuilder::new(project.id, revision_id)
        .id(item_id)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .approval_policy(ApprovalPolicy::Required)
        .build();
    harness
        .db
        .create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let job = JobBuilder::new(project.id, item_id, revision_id, step::INVESTIGATE_ITEM)
        .phase_kind(PhaseKind::Investigate)
        .workspace_kind(WorkspaceKind::Review)
        .execution_permission(ExecutionPermission::MustNotMutate)
        .phase_template_slug("investigate-item")
        .output_artifact_kind(OutputArtifactKind::FindingReport)
        .build();
    harness.db.create_job(&job).await.expect("create job");

    let finding = FindingBuilder::new(project.id, item_id, revision_id, job.id)
        .source_step_id(step::INVESTIGATE_ITEM)
        .severity(FindingSeverity::Low)
        .build();
    harness
        .db
        .create_finding(&finding)
        .await
        .expect("create finding");

    harness
        .dispatcher
        .auto_triage_job_findings(&project, job.id, &item)
        .await
        .expect("auto triage findings");

    let findings = harness
        .db
        .list_findings_by_item(item_id)
        .await
        .expect("list findings");
    assert_eq!(findings.len(), 1);
    assert!(
        findings[0].triage.is_unresolved(),
        "disabled auto-triage should leave findings unresolved"
    );

    let items = harness
        .db
        .list_items_by_project(project.id)
        .await
        .expect("list items");
    assert_eq!(
        items.len(),
        1,
        "disabled auto-triage should not create backlog items"
    );

    let activity = harness
        .db
        .list_activity_by_project(project.id, 20, 0)
        .await
        .expect("list activity");
    assert!(
        activity.is_empty(),
        "disabled auto-triage should not append finding activity"
    );
}

#[tokio::test]
async fn auto_triage_job_findings_only_requests_approval_for_validate_integrated() {
    let harness = TestRuntimeHarness::new(Arc::new(BlockingRunner::new())).await;

    let mut project = harness
        .db
        .get_project(harness.project.id)
        .await
        .expect("project");
    project.execution_mode = ExecutionMode::Autopilot;
    project.auto_triage_policy = Some(AutoTriagePolicy {
        critical: AutoTriageDecision::Backlog,
        high: AutoTriageDecision::Backlog,
        medium: AutoTriageDecision::Backlog,
        low: AutoTriageDecision::Backlog,
    });
    harness
        .db
        .update_project(&project)
        .await
        .expect("update project");

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let item = ItemBuilder::new(project.id, revision_id)
        .id(item_id)
        .approval_state(ApprovalState::NotRequested)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .approval_policy(ApprovalPolicy::Required)
        .build();
    harness
        .db
        .create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let job = JobBuilder::new(project.id, item_id, revision_id, step::INVESTIGATE_ITEM)
        .phase_kind(PhaseKind::Investigate)
        .workspace_kind(WorkspaceKind::Review)
        .execution_permission(ExecutionPermission::MustNotMutate)
        .phase_template_slug("investigate-item")
        .output_artifact_kind(OutputArtifactKind::FindingReport)
        .build();
    harness.db.create_job(&job).await.expect("create job");

    let finding = FindingBuilder::new(project.id, item_id, revision_id, job.id)
        .source_step_id(step::INVESTIGATE_ITEM)
        .severity(FindingSeverity::High)
        .build();
    harness
        .db
        .create_finding(&finding)
        .await
        .expect("create finding");

    harness
        .dispatcher
        .auto_triage_job_findings(&project, job.id, &item)
        .await
        .expect("auto triage findings");

    let updated_item = harness.db.get_item(item_id).await.expect("reload item");
    assert_eq!(
        updated_item.approval_state,
        ApprovalState::NotRequested,
        "non-validate findings must not move an item into approval"
    );

    let activity = harness
        .db
        .list_activity_by_project(project.id, 20, 0)
        .await
        .expect("list activity");
    assert!(
        !activity
            .iter()
            .any(|entry| entry.event_type == ActivityEventType::ApprovalRequested),
        "non-validate findings must not emit approval_requested"
    );
}

#[tokio::test]
async fn finish_report_run_reloads_project_before_auto_triage() {
    let runner = Arc::new(StaticResponseRunner::new(AgentResponse {
        exit_code: 0,
        stdout: String::new(),
        stderr: String::new(),
        result: Some(serde_json::json!({
            "outcome": "findings",
            "summary": "Found an issue",
            "findings": [{
                "finding_key": "f-1",
                "code": "INV001",
                "severity": "low",
                "summary": "Needs follow-up",
                "paths": ["src/lib.rs"],
                "evidence": ["broken"],
            }],
        })),
    }));
    let harness = TestRuntimeHarness::new(runner).await;

    let agent = AgentBuilder::new(
        "codex-review",
        vec![
            AgentCapability::ReadOnlyJobs,
            AgentCapability::StructuredOutput,
        ],
    )
    .build();
    harness.db.create_agent(&agent).await.expect("create agent");

    let mut project = harness
        .db
        .get_project(harness.project.id)
        .await
        .expect("project");
    project.execution_mode = ExecutionMode::Autopilot;
    project.auto_triage_policy = Some(AutoTriagePolicy {
        critical: AutoTriageDecision::Backlog,
        high: AutoTriageDecision::Backlog,
        medium: AutoTriageDecision::Backlog,
        low: AutoTriageDecision::Backlog,
    });
    harness
        .db
        .update_project(&project)
        .await
        .expect("enable auto triage");

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let seed_commit = head_oid(&harness.repo_path)
        .await
        .expect("seed head")
        .into_inner();
    let item = ItemBuilder::new(project.id, revision_id)
        .id(item_id)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .approval_policy(ApprovalPolicy::Required)
        .explicit_seed(seed_commit.as_str())
        .build();
    harness
        .db
        .create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let job = JobBuilder::new(project.id, item_id, revision_id, step::INVESTIGATE_ITEM)
        .phase_kind(PhaseKind::Investigate)
        .workspace_kind(WorkspaceKind::Review)
        .execution_permission(ExecutionPermission::MustNotMutate)
        .phase_template_slug("investigate-item")
        .job_input(JobInput::candidate_subject(
            CommitOid::new(seed_commit.clone()),
            CommitOid::new(seed_commit.clone()),
        ))
        .output_artifact_kind(OutputArtifactKind::FindingReport)
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

    project.execution_mode = ExecutionMode::Manual;
    project.auto_triage_policy = None;
    harness
        .db
        .update_project(&project)
        .await
        .expect("disable auto triage");

    harness
        .dispatcher
        .execute_prepared_agent_job(prepared)
        .await
        .expect("execute report job");

    let findings = harness
        .db
        .list_findings_by_item(item_id)
        .await
        .expect("list findings");
    assert_eq!(findings.len(), 1);
    assert!(
        findings[0].triage.is_unresolved(),
        "latest project config should be respected when findings complete"
    );

    let items = harness
        .db
        .list_items_by_project(project.id)
        .await
        .expect("list items");
    assert_eq!(
        items.len(),
        1,
        "stale prepared project snapshots must not create backlog items"
    );
}

#[tokio::test]
async fn tick_system_action_does_not_queue_stale_autopilot_prepare_decision() {
    let harness = TestRuntimeHarness::new(Arc::new(BlockingRunner::new())).await;

    let mut project = harness
        .db
        .get_project(harness.project.id)
        .await
        .expect("project");
    project.execution_mode = ExecutionMode::Autopilot;
    harness
        .db
        .update_project(&project)
        .await
        .expect("update project");

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let seed_commit = head_oid(&harness.repo_path)
        .await
        .expect("seed head")
        .into_inner();
    let item = ItemBuilder::new(project.id, revision_id)
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

    let validate_job = JobBuilder::new(
        project.id,
        item_id,
        revision_id,
        step::VALIDATE_CANDIDATE_INITIAL,
    )
    .status(JobStatus::Completed)
    .outcome_class(OutcomeClass::Clean)
    .phase_kind(PhaseKind::Validate)
    .workspace_kind(WorkspaceKind::Integration)
    .execution_permission(ExecutionPermission::MustNotMutate)
    .context_policy(ContextPolicy::ResumeContext)
    .phase_template_slug("validate-candidate")
    .job_input(JobInput::None)
    .output_artifact_kind(OutputArtifactKind::ValidationReport)
    .created_at(default_timestamp())
    .started_at(default_timestamp())
    .ended_at(default_timestamp())
    .build();
    harness
        .db
        .create_job(&validate_job)
        .await
        .expect("create job");

    let mut dispatcher = harness.dispatcher.clone();
    let pause_hook = AutoQueuePauseHook::new(AutoQueuePausePoint::BeforeGuard);
    dispatcher.auto_queue_pause_hook = Some(pause_hook.clone());

    let tick_task = tokio::spawn({
        let dispatcher = dispatcher.clone();
        async move { dispatcher.tick_system_action().await }
    });

    pause_hook
        .wait_until_entered(1, Duration::from_secs(2))
        .await;

    {
        let _guard = dispatcher
            .project_locks
            .acquire_project_mutation(project.id)
            .await;
        let active_job = JobBuilder::new(project.id, item_id, revision_id, step::AUTHOR_INITIAL)
            .status(JobStatus::Queued)
            .phase_kind(PhaseKind::Author)
            .workspace_kind(WorkspaceKind::Authoring)
            .execution_permission(ExecutionPermission::MayMutate)
            .phase_template_slug("author-initial")
            .job_input(JobInput::authoring_head(CommitOid::new(
                seed_commit.clone(),
            )))
            .output_artifact_kind(OutputArtifactKind::Commit)
            .created_at(default_timestamp())
            .build();
        harness
            .db
            .create_job(&active_job)
            .await
            .expect("create active job");
    }

    pause_hook.release();

    let made_progress = tick_task
        .await
        .expect("join tick task")
        .expect("tick system action");
    assert!(
        !made_progress,
        "stale prepare decision should not count as progress"
    );

    let queue_entry = harness
        .db
        .find_active_queue_entry_for_revision(revision.id)
        .await
        .expect("find queue entry");
    assert!(
        queue_entry.is_none(),
        "runtime should not queue convergence after the item becomes non-preparable"
    );
}

#[tokio::test]
async fn tick_system_action_does_not_queue_after_execution_mode_switches_to_manual() {
    let harness = TestRuntimeHarness::new(Arc::new(BlockingRunner::new())).await;

    let mut project = harness
        .db
        .get_project(harness.project.id)
        .await
        .expect("project");
    project.execution_mode = ExecutionMode::Autopilot;
    harness
        .db
        .update_project(&project)
        .await
        .expect("update project");

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let seed_commit = head_oid(&harness.repo_path)
        .await
        .expect("seed head")
        .into_inner();
    let item = ItemBuilder::new(project.id, revision_id)
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

    let validate_job = JobBuilder::new(
        project.id,
        item_id,
        revision_id,
        step::VALIDATE_CANDIDATE_INITIAL,
    )
    .status(JobStatus::Completed)
    .outcome_class(OutcomeClass::Clean)
    .phase_kind(PhaseKind::Validate)
    .workspace_kind(WorkspaceKind::Integration)
    .execution_permission(ExecutionPermission::MustNotMutate)
    .context_policy(ContextPolicy::ResumeContext)
    .phase_template_slug("validate-candidate")
    .job_input(JobInput::None)
    .output_artifact_kind(OutputArtifactKind::ValidationReport)
    .created_at(default_timestamp())
    .started_at(default_timestamp())
    .ended_at(default_timestamp())
    .build();
    harness
        .db
        .create_job(&validate_job)
        .await
        .expect("create job");

    let mut dispatcher = harness.dispatcher.clone();
    let pause_hook = AutoQueuePauseHook::new(AutoQueuePausePoint::BeforeGuard);
    dispatcher.auto_queue_pause_hook = Some(pause_hook.clone());

    let tick_task = tokio::spawn({
        let dispatcher = dispatcher.clone();
        async move { dispatcher.tick_system_action().await }
    });

    pause_hook
        .wait_until_entered(1, Duration::from_secs(2))
        .await;

    project.execution_mode = ExecutionMode::Manual;
    harness
        .db
        .update_project(&project)
        .await
        .expect("switch execution mode");

    pause_hook.release();

    let made_progress = tick_task
        .await
        .expect("join tick task")
        .expect("tick system action");
    assert!(
        !made_progress,
        "stale autopilot mode should not queue convergence after switching to manual"
    );

    let queue_entry = harness
        .db
        .find_active_queue_entry_for_revision(revision.id)
        .await
        .expect("find queue entry");
    assert!(
        queue_entry.is_none(),
        "runtime should not queue convergence after autopilot is disabled"
    );
}

#[tokio::test]
async fn recover_projected_jobs_reloads_execution_mode_after_lock() {
    let harness = TestRuntimeHarness::new(Arc::new(BlockingRunner::new())).await;

    let mut project = harness
        .db
        .get_project(harness.project.id)
        .await
        .expect("project");
    project.execution_mode = ExecutionMode::Autopilot;
    harness
        .db
        .update_project(&project)
        .await
        .expect("update project");

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let seed_commit = head_oid(&harness.repo_path)
        .await
        .expect("seed head")
        .into_inner();
    let item = ItemBuilder::new(project.id, revision_id)
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

    let mut dispatcher = harness.dispatcher.clone();
    let pause_hook = ProjectedRecoveryPauseHook::new(ProjectedRecoveryPausePoint::BeforeGuard);
    dispatcher.projected_recovery_pause_hook = Some(pause_hook.clone());

    let recovery_task = tokio::spawn({
        let dispatcher = dispatcher.clone();
        async move { dispatcher.recover_projected_jobs().await }
    });

    pause_hook
        .wait_until_entered(1, Duration::from_secs(2))
        .await;

    project.execution_mode = ExecutionMode::Manual;
    harness
        .db
        .update_project(&project)
        .await
        .expect("switch execution mode");

    pause_hook.release();

    let recovered = recovery_task
        .await
        .expect("join recovery task")
        .expect("recover projected jobs");
    assert!(
        !recovered,
        "stale autopilot mode should not dispatch projected recovery work after switching to manual"
    );

    let jobs = harness
        .db
        .list_jobs_by_item(item.id)
        .await
        .expect("list jobs");
    assert!(
        jobs.is_empty(),
        "runtime should not auto-dispatch new work after autopilot is disabled"
    );
}

#[tokio::test]
async fn recover_projected_jobs_only_queues_one_autopilot_item_while_another_is_active() {
    let harness = TestRuntimeHarness::new(Arc::new(BlockingRunner::new())).await;

    let mut project = harness
        .db
        .get_project(harness.project.id)
        .await
        .expect("project");
    project.execution_mode = ExecutionMode::Autopilot;
    harness
        .db
        .update_project(&project)
        .await
        .expect("update project");

    let seed_commit = head_oid(&harness.repo_path)
        .await
        .expect("seed head")
        .into_inner();

    let first_item_id = ingot_domain::ids::ItemId::new();
    let first_revision_id = ingot_domain::ids::ItemRevisionId::new();
    let first_item = ItemBuilder::new(project.id, first_revision_id)
        .id(first_item_id)
        .sort_key("10")
        .build();
    let first_revision = RevisionBuilder::new(first_item_id)
        .id(first_revision_id)
        .explicit_seed(seed_commit.as_str())
        .build();
    harness
        .db
        .create_item_with_revision(&first_item, &first_revision)
        .await
        .expect("create first item");

    let second_item_id = ingot_domain::ids::ItemId::new();
    let second_revision_id = ingot_domain::ids::ItemRevisionId::new();
    let second_item = ItemBuilder::new(project.id, second_revision_id)
        .id(second_item_id)
        .sort_key("20")
        .build();
    let second_revision = RevisionBuilder::new(second_item_id)
        .id(second_revision_id)
        .explicit_seed(seed_commit.as_str())
        .build();
    harness
        .db
        .create_item_with_revision(&second_item, &second_revision)
        .await
        .expect("create second item");

    let recovered = harness
        .dispatcher
        .recover_projected_jobs()
        .await
        .expect("recover projected jobs");
    assert!(
        recovered,
        "autopilot recovery should queue work for the first eligible item"
    );

    let first_jobs = harness
        .db
        .list_jobs_by_item(first_item.id)
        .await
        .expect("list first item jobs");
    assert_eq!(first_jobs.len(), 1, "first item should have one queued job");
    assert_eq!(first_jobs[0].step_id, step::AUTHOR_INITIAL);
    assert_eq!(first_jobs[0].state.status(), JobStatus::Queued);

    let second_jobs = harness
        .db
        .list_jobs_by_item(second_item.id)
        .await
        .expect("list second item jobs");
    assert!(
        second_jobs.is_empty(),
        "second item should remain undispatched while the first autopilot item is active"
    );

    let recovered_again = harness
        .dispatcher
        .recover_projected_jobs()
        .await
        .expect("recover projected jobs again");
    assert!(
        !recovered_again,
        "autopilot recovery should not queue the second item while the first job is still active"
    );

    let second_jobs = harness
        .db
        .list_jobs_by_item(second_item.id)
        .await
        .expect("list second item jobs after second recovery");
    assert!(
        second_jobs.is_empty(),
        "second item should still be waiting while the first item remains active"
    );
}

#[tokio::test]
async fn recover_projected_jobs_does_not_skip_escalated_item_to_dispatch_next() {
    let harness = TestRuntimeHarness::new(Arc::new(BlockingRunner::new())).await;

    let mut project = harness
        .db
        .get_project(harness.project.id)
        .await
        .expect("project");
    project.execution_mode = ExecutionMode::Autopilot;
    harness
        .db
        .update_project(&project)
        .await
        .expect("update project");

    let seed_commit = head_oid(&harness.repo_path)
        .await
        .expect("seed head")
        .into_inner();

    // Item 1: escalated — evaluator returns nothing-to-dispatch
    let first_item_id = ingot_domain::ids::ItemId::new();
    let first_revision_id = ingot_domain::ids::ItemRevisionId::new();
    let first_item = ItemBuilder::new(project.id, first_revision_id)
        .id(first_item_id)
        .sort_key("10")
        .escalated(ingot_domain::item::EscalationReason::StepFailed)
        .build();
    let first_revision = RevisionBuilder::new(first_item_id)
        .id(first_revision_id)
        .explicit_seed(seed_commit.as_str())
        .build();
    harness
        .db
        .create_item_with_revision(&first_item, &first_revision)
        .await
        .expect("create first item");

    // Item 2: normal — would be dispatchable if not blocked
    let second_item_id = ingot_domain::ids::ItemId::new();
    let second_revision_id = ingot_domain::ids::ItemRevisionId::new();
    let second_item = ItemBuilder::new(project.id, second_revision_id)
        .id(second_item_id)
        .sort_key("20")
        .build();
    let second_revision = RevisionBuilder::new(second_item_id)
        .id(second_revision_id)
        .explicit_seed(seed_commit.as_str())
        .build();
    harness
        .db
        .create_item_with_revision(&second_item, &second_revision)
        .await
        .expect("create second item");

    let recovered = harness
        .dispatcher
        .recover_projected_jobs()
        .await
        .expect("recover projected jobs");
    assert!(
        !recovered,
        "escalated first item should block dispatch; nothing should be dispatched"
    );

    let second_jobs = harness
        .db
        .list_jobs_by_item(second_item.id)
        .await
        .expect("list second item jobs");
    assert!(
        second_jobs.is_empty(),
        "second item must NOT be dispatched when the first open item is escalated"
    );
}

#[tokio::test]
async fn auto_dispatch_projected_review_does_not_queue_autopilot_item_while_project_has_active_work()
 {
    let harness = TestRuntimeHarness::new(Arc::new(BlockingRunner::new())).await;

    let mut project = harness
        .db
        .get_project(harness.project.id)
        .await
        .expect("project");
    project.execution_mode = ExecutionMode::Autopilot;
    harness
        .db
        .update_project(&project)
        .await
        .expect("update project");

    let seed_commit = head_oid(&harness.repo_path)
        .await
        .expect("seed head")
        .into_inner();

    let first_item_id = ingot_domain::ids::ItemId::new();
    let first_revision_id = ingot_domain::ids::ItemRevisionId::new();
    let first_item = ItemBuilder::new(project.id, first_revision_id)
        .id(first_item_id)
        .sort_key("10")
        .build();
    let first_revision = RevisionBuilder::new(first_item_id)
        .id(first_revision_id)
        .explicit_seed(seed_commit.as_str())
        .build();
    harness
        .db
        .create_item_with_revision(&first_item, &first_revision)
        .await
        .expect("create first item");

    let second_item_id = ingot_domain::ids::ItemId::new();
    let second_revision_id = ingot_domain::ids::ItemRevisionId::new();
    let second_item = ItemBuilder::new(project.id, second_revision_id)
        .id(second_item_id)
        .sort_key("20")
        .build();
    let second_revision = RevisionBuilder::new(second_item_id)
        .id(second_revision_id)
        .explicit_seed(seed_commit.as_str())
        .build();
    harness
        .db
        .create_item_with_revision(&second_item, &second_revision)
        .await
        .expect("create second item");

    let recovered = harness
        .dispatcher
        .recover_projected_jobs()
        .await
        .expect("recover projected jobs");
    assert!(
        recovered,
        "autopilot recovery should queue work for the first eligible item"
    );

    let dispatched = harness
        .dispatcher
        .auto_dispatch_projected_review(project.id, second_item.id)
        .await
        .expect("auto-dispatch projected review");
    assert!(
        !dispatched,
        "shared projected-review entry should not queue a second autopilot item while the project already has active work"
    );

    let second_jobs = harness
        .db
        .list_jobs_by_item(second_item.id)
        .await
        .expect("list second item jobs");
    assert!(
        second_jobs.is_empty(),
        "second item should remain undispatched while another autopilot item is active"
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

fn schema_property_type(schema: &serde_json::Value, property: &str) -> Option<serde_json::Value> {
    schema_property(schema, property).and_then(|value| value.get("type").cloned())
}

fn schema_property(schema: &serde_json::Value, property: &str) -> Option<serde_json::Value> {
    schema
        .get("properties")
        .and_then(|value| value.get(property))
        .cloned()
}
