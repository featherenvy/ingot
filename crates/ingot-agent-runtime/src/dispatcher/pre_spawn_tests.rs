use super::*;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use crate::dispatcher::prompt::output_schema_for_job;
use ingot_test_support::fixtures::{
    AgentBuilder, ItemBuilder, JobBuilder, ProjectBuilder, RevisionBuilder,
};
use ingot_test_support::git::temp_git_repo;
use ingot_test_support::git::unique_temp_path;
use ingot_test_support::sqlite::migrated_test_db;
use ingot_usecases::job_lifecycle;

#[derive(Default)]
struct BlockingRunnerState {
    launch_count: usize,
}

#[derive(Clone, Default)]
struct BlockingRunner {
    state: Arc<Mutex<BlockingRunnerState>>,
}

impl BlockingRunner {
    fn new() -> Self {
        Self::default()
    }

    fn launch_count(&self) -> usize {
        self.state.lock().expect("blocking runner state").launch_count
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
            self.state.lock().expect("blocking runner state").launch_count += 1;
            tokio::time::sleep(Duration::from_secs(60)).await;
            Err(AgentError::ProcessError("blocking runner aborted".into()))
        })
    }
}

struct TestRuntimeHarness {
    db: Database,
    repo_path: PathBuf,
    project: Project,
    dispatcher: JobDispatcher,
    dispatch_notify: DispatchNotify,
}

impl TestRuntimeHarness {
    async fn new(runner: Arc<BlockingRunner>) -> Self {
        let repo_path = temp_git_repo("ingot-runtime-repo");
        let db = migrated_test_db("ingot-runtime-pre-spawn").await;
        let state_root = unique_temp_path("ingot-runtime-pre-spawn-state");
        let project = ProjectBuilder::new(&repo_path).build();
        db.create_project(&project).await.expect("create project");

        let agent = AgentBuilder::new(
            "codex-blocking",
            vec![
                AgentCapability::MutatingJobs,
                AgentCapability::ReadOnlyJobs,
                AgentCapability::StructuredOutput,
            ],
        )
        .build();
        db.create_agent(&agent).await.expect("create agent");

        let dispatch_notify = DispatchNotify::default();
        let dispatcher = JobDispatcher::with_runner(
            db.clone(),
            ProjectLocks::default(),
            DispatcherConfig::new(state_root),
            runner as Arc<dyn AgentRunner>,
            dispatch_notify.clone(),
        );

        Self {
            db,
            repo_path,
            project,
            dispatcher,
            dispatch_notify,
        }
    }
}

fn write_harness_toml(repo_path: &Path, contents: &str) {
    let ingot_dir = repo_path.join(".ingot");
    std::fs::create_dir_all(&ingot_dir).expect("create .ingot dir");
    std::fs::write(ingot_dir.join("harness.toml"), contents).expect("write harness.toml");
}

#[tokio::test]
async fn run_with_heartbeats_does_not_spawn_agent_when_job_is_cancelled_before_spawn() {
    let runner = Arc::new(BlockingRunner::new());
    let harness = TestRuntimeHarness::new(runner.clone()).await;

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let seed_commit = head_oid(&harness.repo_path).await.expect("seed head");
    let item = ItemBuilder::new(harness.project.id, revision_id)
        .id(item_id)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .explicit_seed(&seed_commit)
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
        .job_input(ingot_domain::job::JobInput::authoring_head(seed_commit))
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

    let cancelled_job = harness.db.get_job(job.id).await.expect("reload cancelled job");
    let released_workspace = harness
        .db
        .get_workspace(workspace_id)
        .await
        .expect("reload released workspace");
    assert_eq!(cancelled_job.state.status(), JobStatus::Cancelled);
    assert_eq!(released_workspace.state.current_job_id(), None);

    pause_hook.release();

    let result = run_task.await.expect("join run_with_heartbeats task");
    assert!(matches!(
        result,
        Err(AgentError::ProcessError(message)) if message == "job cancelled"
    ));
    assert_eq!(runner.launch_count(), 0);
}

#[tokio::test]
async fn run_harness_command_with_heartbeats_does_not_spawn_command_when_job_is_cancelled_before_spawn()
 {
    let runner = Arc::new(BlockingRunner::new());
    let harness = TestRuntimeHarness::new(runner).await;

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let seed_commit = head_oid(&harness.repo_path).await.expect("seed head");
    let item = ItemBuilder::new(harness.project.id, revision_id)
        .id(item_id)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .explicit_seed(&seed_commit)
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
        ingot_workflow::step::VALIDATE_CANDIDATE_INITIAL,
    )
    .phase_kind(PhaseKind::Validate)
    .workspace_kind(WorkspaceKind::Authoring)
    .execution_permission(ExecutionPermission::DaemonOnly)
    .context_policy(ingot_domain::job::ContextPolicy::None)
    .phase_template_slug("")
    .job_input(ingot_domain::job::JobInput::candidate_subject(
        seed_commit.clone(),
        seed_commit.clone(),
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

    let cancelled_job = harness.db.get_job(job.id).await.expect("reload cancelled job");
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
