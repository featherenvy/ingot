use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use ingot_agent_runtime::{AgentRunner, DispatcherConfig, JobDispatcher};
use ingot_domain::agent::{Agent, AgentCapability};
use ingot_domain::job::{JobStatus, OutcomeClass};
use ingot_domain::workspace::{WorkspaceKind, WorkspaceStatus};
use ingot_git::commands::head_oid;
use ingot_test_support::git::unique_temp_path;
use ingot_usecases::{DispatchNotify, ProjectLocks};

mod common;
use common::*;

use ingot_agent_protocol::adapter::AgentError;
use ingot_agent_protocol::request::AgentRequest;
use ingot_agent_protocol::response::AgentResponse;
use ingot_usecases::job::{DispatchJobCommand, dispatch_job};
use ingot_usecases::job_lifecycle;
use ingot_workflow::step;

#[tokio::test]
async fn tick_executes_a_queued_authoring_job_and_creates_a_commit() {
    let h = TestHarness::new(Arc::new(FakeRunner)).await;
    h.register_mutating_agent().await;

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let seed_commit = head_oid(&h.repo_path).await.expect("seed head");

    let item = ItemBuilder::new(h.project.id, revision_id)
        .id(item_id)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .explicit_seed(&seed_commit)
        .template_map_snapshot(serde_json::json!({ "author_initial": "author-initial" }))
        .build();
    h.db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let job = test_authoring_job(h.project.id, item_id, revision_id, &seed_commit);
    h.db.create_job(&job).await.expect("create job");

    assert!(h.dispatcher.tick().await.expect("tick should run"));

    let updated_job = h.db.get_job(job.id).await.expect("updated job");
    assert_eq!(updated_job.state.status(), JobStatus::Completed);
    assert_eq!(updated_job.state.outcome_class(), Some(OutcomeClass::Clean));
    assert!(updated_job.state.output_commit_oid().is_some());

    let workspace =
        h.db.find_authoring_workspace_for_revision(revision.id)
            .await
            .expect("workspace query")
            .expect("workspace exists");
    assert_eq!(workspace.state.status(), WorkspaceStatus::Ready);
    assert_eq!(workspace.state.current_job_id(), None);
    assert_eq!(
        workspace.state.head_commit_oid(),
        updated_job.state.output_commit_oid()
    );

    let prompt_path = h
        .state_root
        .join("logs")
        .join(job.id.to_string())
        .join("prompt.txt");
    assert!(prompt_path.exists(), "prompt artifact should exist");
    let artifact_dir = h.state_root.join("logs").join(job.id.to_string());
    assert!(
        artifact_dir.join("stdout.log").exists(),
        "stdout artifact should exist"
    );
    assert!(
        artifact_dir.join("stderr.log").exists(),
        "stderr artifact should exist"
    );
    assert!(
        artifact_dir.join("result.json").exists(),
        "result artifact should exist"
    );
}

#[tokio::test]
async fn tick_executes_a_review_job_and_persists_structured_report() {
    let repo = temp_git_repo("ingot-runtime-repo");
    let base_commit = head_oid(&repo).await.expect("base head");
    std::fs::write(repo.join("tracked.txt"), "next").expect("update tracked file");
    git_sync(&repo, &["add", "tracked.txt"]);
    git_sync(&repo, &["commit", "-m", "next"]);
    let head_commit = head_oid(&repo).await.expect("head oid");

    let db = migrated_test_db("ingot-runtime-review").await;
    let state_root = unique_temp_path("ingot-runtime-review-state");

    let project = ProjectBuilder::new(&repo).build();
    db.create_project(&project).await.expect("create project");

    let agent = AgentBuilder::new(
        "codex-review",
        vec![
            AgentCapability::ReadOnlyJobs,
            AgentCapability::StructuredOutput,
        ],
    )
    .build();
    db.create_agent(&agent).await.expect("create agent");

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();

    let item = ItemBuilder::new(project.id, revision_id)
        .id(item_id)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .explicit_seed(&base_commit)
        .template_map_snapshot(
            serde_json::json!({ "review_candidate_initial": "review-candidate" }),
        )
        .build();
    db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let job = test_review_job(project.id, item_id, revision_id, &base_commit, &head_commit);
    db.create_job(&job).await.expect("create job");

    let dispatcher = JobDispatcher::with_runner(
        db.clone(),
        ProjectLocks::default(),
        DispatcherConfig::new(state_root),
        Arc::new(StaticReviewRunner {
            base_commit_oid: base_commit.clone(),
            head_commit_oid: head_commit.clone(),
        }),
        DispatchNotify::default(),
    );

    assert!(dispatcher.tick().await.expect("tick should run"));

    let updated_job = db.get_job(job.id).await.expect("updated job");
    assert_eq!(updated_job.state.status(), JobStatus::Completed);
    assert_eq!(updated_job.state.outcome_class(), Some(OutcomeClass::Clean));
    assert_eq!(
        updated_job.state.result_schema_version(),
        Some("review_report:v1")
    );
    assert_eq!(
        updated_job
            .state
            .result_payload()
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
    assert_eq!(workspaces[0].state.status(), WorkspaceStatus::Abandoned);
    assert!(!Path::new(&workspaces[0].path).exists());
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
        ) -> Pin<Box<dyn Future<Output = Result<AgentResponse, AgentError>> + Send + 'a>> {
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

    let mut config = DispatcherConfig::new(unique_temp_path("ingot-runtime-timeout-state"));
    config.job_timeout = Duration::from_millis(50);
    config.heartbeat_interval = Duration::from_millis(10);
    let h = TestHarness::with_config(Arc::new(SlowRunner), Some(config)).await;
    h.register_mutating_agent().await;

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let seed_commit = head_oid(&h.repo_path).await.expect("seed head");

    let item = ItemBuilder::new(h.project.id, revision_id)
        .id(item_id)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .explicit_seed(&seed_commit)
        .template_map_snapshot(serde_json::json!({ "author_initial": "author-initial" }))
        .build();
    h.db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let job = test_authoring_job(h.project.id, item_id, revision_id, &seed_commit);
    h.db.create_job(&job).await.expect("create job");

    assert!(h.dispatcher.tick().await.expect("tick should run"));

    let updated_job = h.db.get_job(job.id).await.expect("updated job");
    assert_eq!(updated_job.state.status(), JobStatus::Failed);
    assert_eq!(
        updated_job.state.outcome_class(),
        Some(OutcomeClass::TransientFailure)
    );
    assert_eq!(updated_job.state.error_code(), Some("job_timeout"));
}

#[tokio::test]
async fn tick_runs_healthy_queued_job_even_when_another_project_is_broken() {
    let h = TestHarness::new(Arc::new(FakeRunner)).await;

    // Register agent with all capabilities
    let agent = AgentBuilder::new(
        "codex",
        vec![
            AgentCapability::MutatingJobs,
            AgentCapability::ReadOnlyJobs,
            AgentCapability::StructuredOutput,
        ],
    )
    .build();
    h.db.create_agent(&agent).await.expect("create agent");

    // Create a broken project with missing path
    let broken_project = ProjectBuilder::new(unique_temp_path("ingot-missing-project"))
        .name("broken")
        .build();
    h.db.create_project(&broken_project)
        .await
        .expect("create broken project");

    let broken_item_id = ingot_domain::ids::ItemId::new();
    let broken_revision_id = ingot_domain::ids::ItemRevisionId::new();
    let broken_item = ItemBuilder::new(broken_project.id, broken_revision_id)
        .id(broken_item_id)
        .build();
    let broken_revision = RevisionBuilder::new(broken_item_id)
        .id(broken_revision_id)
        .explicit_seed("missing-seed")
        .build();
    h.db.create_item_with_revision(&broken_item, &broken_revision)
        .await
        .expect("create broken item");

    // Create healthy item on the harness project
    let healthy_item_id = ingot_domain::ids::ItemId::new();
    let healthy_revision_id = ingot_domain::ids::ItemRevisionId::new();
    let healthy_seed_commit = head_oid(&h.repo_path).await.expect("healthy seed head");

    let healthy_item = ItemBuilder::new(h.project.id, healthy_revision_id)
        .id(healthy_item_id)
        .build();
    let healthy_revision = RevisionBuilder::new(healthy_item_id)
        .id(healthy_revision_id)
        .explicit_seed(&healthy_seed_commit)
        .build();
    h.db.create_item_with_revision(&healthy_item, &healthy_revision)
        .await
        .expect("create healthy item");

    let author_job = dispatch_job(
        &healthy_item,
        &healthy_revision,
        &[],
        &[],
        &[],
        DispatchJobCommand { step_id: None },
    )
    .expect("dispatch healthy author initial");
    h.db.create_job(&author_job)
        .await
        .expect("create healthy author job");

    assert!(h.dispatcher.tick().await.expect("tick should run"));

    let healthy_jobs =
        h.db.list_jobs_by_item(healthy_item_id)
            .await
            .expect("healthy jobs");
    let completed_author = healthy_jobs
        .iter()
        .find(|job| job.step_id == step::AUTHOR_INITIAL)
        .expect("completed healthy author job");
    assert_eq!(completed_author.state.status(), JobStatus::Completed);
    let review_job = healthy_jobs
        .iter()
        .find(|job| job.step_id == step::REVIEW_INCREMENTAL_INITIAL)
        .expect("queued healthy review job");
    assert_eq!(review_job.state.status(), JobStatus::Queued);
}

async fn create_supervised_authoring_job(
    h: &TestHarness,
    created_at: chrono::DateTime<chrono::Utc>,
) -> (
    ingot_domain::item::Item,
    ingot_domain::revision::ItemRevision,
    ingot_domain::job::Job,
) {
    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let seed_commit = head_oid(&h.repo_path).await.expect("seed head");
    let item = ItemBuilder::new(h.project.id, revision_id)
        .id(item_id)
        .created_at(created_at)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .explicit_seed(&seed_commit)
        .template_map_snapshot(serde_json::json!({ "author_initial": "author-initial" }))
        .created_at(created_at)
        .build();
    h.db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");
    let job = ingot_domain::job::Job {
        created_at,
        ..test_authoring_job(h.project.id, item_id, revision_id, &seed_commit)
    };
    h.db.create_job(&job).await.expect("create job");
    (item, revision, job)
}

async fn stop_background_dispatcher(handle: tokio::task::JoinHandle<()>) {
    handle.abort();
    let _ = handle.await;
}

#[tokio::test]
async fn run_forever_launches_up_to_max_concurrent_jobs() {
    let runner = BlockingRunner::new();
    let mut config = DispatcherConfig::new(unique_temp_path("ingot-runtime-concurrency-state"));
    config.poll_interval = Duration::from_secs(10);
    config.max_concurrent_jobs = 2;
    let h = TestHarness::with_config(Arc::new(runner.clone()), Some(config)).await;
    h.register_mutating_agent().await;
    h.dispatcher
        .reconcile_startup()
        .await
        .expect("reconcile startup");

    let (_, _, first_job) =
        create_supervised_authoring_job(&h, parse_timestamp("2026-03-12T00:00:00Z")).await;
    let (_, _, second_job) =
        create_supervised_authoring_job(&h, parse_timestamp("2026-03-12T00:00:01Z")).await;
    let (_, _, third_job) =
        create_supervised_authoring_job(&h, parse_timestamp("2026-03-12T00:00:02Z")).await;

    let dispatcher = h.dispatcher.clone();
    let handle = tokio::spawn(async move { dispatcher.run_forever().await });
    h.dispatch_notify.notify();

    runner.wait_for_launches(2, Duration::from_secs(2)).await;
    let running_jobs = h.wait_for_running_jobs(2, Duration::from_secs(2)).await;
    assert_eq!(running_jobs.len(), 2);
    assert!(
        running_jobs.iter().all(|job| {
            job.id == first_job.id || job.id == second_job.id || job.id == third_job.id
        }),
        "unexpected running jobs: {running_jobs:?}"
    );
    assert_eq!(
        h.db.get_job(third_job.id)
            .await
            .expect("reload queued job")
            .state
            .status(),
        JobStatus::Queued
    );

    runner.release_all();
    h.wait_for_job_status(first_job.id, JobStatus::Completed, Duration::from_secs(2))
        .await;
    h.wait_for_job_status(second_job.id, JobStatus::Completed, Duration::from_secs(2))
        .await;
    h.wait_for_job_status(third_job.id, JobStatus::Completed, Duration::from_secs(2))
        .await;
    stop_background_dispatcher(handle).await;
}

#[tokio::test]
async fn run_forever_starts_next_job_on_joinset_completion() {
    let runner = BlockingRunner::new();
    let mut config = DispatcherConfig::new(unique_temp_path("ingot-runtime-joinset-state"));
    config.poll_interval = Duration::from_secs(10);
    config.max_concurrent_jobs = 2;
    let h = TestHarness::with_config(Arc::new(runner.clone()), Some(config)).await;
    h.register_mutating_agent().await;
    h.dispatcher
        .reconcile_startup()
        .await
        .expect("reconcile startup");

    let (_, _, first_job) =
        create_supervised_authoring_job(&h, parse_timestamp("2026-03-12T00:00:00Z")).await;
    let (_, _, second_job) =
        create_supervised_authoring_job(&h, parse_timestamp("2026-03-12T00:00:01Z")).await;
    let (_, _, third_job) =
        create_supervised_authoring_job(&h, parse_timestamp("2026-03-12T00:00:02Z")).await;

    let dispatcher = h.dispatcher.clone();
    let handle = tokio::spawn(async move { dispatcher.run_forever().await });
    h.dispatch_notify.notify();

    runner.wait_for_launches(2, Duration::from_secs(5)).await;
    h.wait_for_running_jobs(2, Duration::from_secs(5)).await;

    runner.release_one();

    runner.wait_for_launches(3, Duration::from_secs(5)).await;

    runner.release_all();
    h.wait_for_job_status(first_job.id, JobStatus::Completed, Duration::from_secs(5))
        .await;
    h.wait_for_job_status(second_job.id, JobStatus::Completed, Duration::from_secs(5))
        .await;
    h.wait_for_job_status(third_job.id, JobStatus::Completed, Duration::from_secs(5))
        .await;
    stop_background_dispatcher(handle).await;
}

#[tokio::test]
async fn run_forever_starts_next_job_after_running_job_cancellation() {
    let runner = BlockingRunner::new();
    let mut config =
        DispatcherConfig::new(unique_temp_path("ingot-runtime-cancellation-wakeup-state"));
    config.poll_interval = Duration::from_secs(10);
    config.heartbeat_interval = Duration::from_secs(5);
    config.max_concurrent_jobs = 1;
    let h = TestHarness::with_config(Arc::new(runner.clone()), Some(config)).await;
    h.register_mutating_agent().await;
    h.dispatcher
        .reconcile_startup()
        .await
        .expect("reconcile startup");

    let (first_item, _, first_job) =
        create_supervised_authoring_job(&h, parse_timestamp("2026-03-12T00:00:00Z")).await;
    let (_, _, second_job) =
        create_supervised_authoring_job(&h, parse_timestamp("2026-03-12T00:00:01Z")).await;

    let dispatcher = h.dispatcher.clone();
    let handle = tokio::spawn(async move { dispatcher.run_forever().await });
    h.dispatch_notify.notify();

    runner.wait_for_launches(1, Duration::from_secs(2)).await;
    h.wait_for_job_status(first_job.id, JobStatus::Running, Duration::from_secs(2))
        .await;

    let active_job = h.db.get_job(first_job.id).await.expect("reload active job");
    job_lifecycle::cancel_job(
        &h.db,
        &h.db,
        &h.db,
        &active_job,
        &first_item,
        "operator_cancelled",
        WorkspaceStatus::Ready,
    )
    .await
    .expect("cancel running job");
    h.dispatch_notify.notify();

    runner.wait_for_launches(2, Duration::from_secs(2)).await;
    h.wait_for_job_status(second_job.id, JobStatus::Running, Duration::from_secs(2))
        .await;

    assert_eq!(
        h.db.get_job(first_job.id)
            .await
            .expect("reload cancelled job")
            .state
            .status(),
        JobStatus::Cancelled
    );
    assert_eq!(
        h.db.get_job(second_job.id)
            .await
            .expect("reload queued successor")
            .state
            .status(),
        JobStatus::Running
    );

    runner.release_all();
    h.wait_for_job_status(second_job.id, JobStatus::Completed, Duration::from_secs(2))
        .await;
    stop_background_dispatcher(handle).await;
}

#[tokio::test]
async fn run_forever_skips_unlaunchable_head_job_when_filling_capacity() {
    let mut config = DispatcherConfig::new(unique_temp_path("ingot-runtime-stale-head-state"));
    config.poll_interval = Duration::from_secs(10);
    config.max_concurrent_jobs = 2;
    let h = TestHarness::with_config(Arc::new(FakeRunner), Some(config)).await;
    h.register_mutating_agent().await;
    h.dispatcher
        .reconcile_startup()
        .await
        .expect("reconcile startup");

    let (stale_item, stale_revision, stale_job) =
        create_supervised_authoring_job(&h, parse_timestamp("2026-03-12T00:00:00Z")).await;
    let next_revision = RevisionBuilder::new(stale_item.id)
        .revision_no(2)
        .explicit_seed(head_oid(&h.repo_path).await.expect("seed head"))
        .template_map_snapshot(serde_json::json!({ "author_initial": "author-initial" }))
        .created_at(parse_timestamp("2026-03-12T00:00:03Z"))
        .build();
    h.db.create_revision(&next_revision)
        .await
        .expect("create next revision");
    let mut updated_item = h.db.get_item(stale_item.id).await.expect("load stale item");
    updated_item.current_revision_id = next_revision.id;
    h.db.update_item(&updated_item)
        .await
        .expect("update stale item");

    let (_, _, healthy_job) =
        create_supervised_authoring_job(&h, parse_timestamp("2026-03-12T00:00:01Z")).await;

    let dispatcher = h.dispatcher.clone();
    let handle = tokio::spawn(async move { dispatcher.run_forever().await });
    h.dispatch_notify.notify();

    h.wait_for_job_status(healthy_job.id, JobStatus::Completed, Duration::from_secs(5))
        .await;
    assert_eq!(
        h.db.get_job(stale_job.id)
            .await
            .expect("reload stale job")
            .state
            .status(),
        JobStatus::Queued
    );

    assert_eq!(
        h.db.get_job(stale_job.id)
            .await
            .expect("reload stale job")
            .item_revision_id,
        stale_revision.id
    );
    stop_background_dispatcher(handle).await;
}

#[tokio::test]
async fn run_forever_skips_workspace_busy_head_job_when_filling_capacity() {
    let mut config = DispatcherConfig::new(unique_temp_path("ingot-runtime-busy-head-state"));
    config.poll_interval = Duration::from_secs(10);
    config.max_concurrent_jobs = 2;
    let h = TestHarness::with_config(Arc::new(FakeRunner), Some(config)).await;
    h.register_mutating_agent().await;
    h.dispatcher
        .reconcile_startup()
        .await
        .expect("reconcile startup");

    let (_, busy_revision, busy_job) =
        create_supervised_authoring_job(&h, parse_timestamp("2026-03-12T00:00:00Z")).await;
    let seed_commit = head_oid(&h.repo_path).await.expect("seed head");
    let busy_workspace = WorkspaceBuilder::new(h.project.id, WorkspaceKind::Authoring)
        .created_for_revision_id(busy_revision.id)
        .current_job_id(busy_job.id)
        .status(WorkspaceStatus::Busy)
        .base_commit_oid(seed_commit.clone())
        .head_commit_oid(seed_commit)
        .created_at(parse_timestamp("2026-03-12T00:00:00Z"))
        .build();
    h.db.create_workspace(&busy_workspace)
        .await
        .expect("create busy workspace");

    let (_, _, healthy_job) =
        create_supervised_authoring_job(&h, parse_timestamp("2026-03-12T00:00:01Z")).await;

    let dispatcher = h.dispatcher.clone();
    let handle = tokio::spawn(async move { dispatcher.run_forever().await });
    h.dispatch_notify.notify();

    h.wait_for_job_status(healthy_job.id, JobStatus::Completed, Duration::from_secs(5))
        .await;
    assert_eq!(
        h.db.get_job(busy_job.id)
            .await
            .expect("reload busy job")
            .state
            .status(),
        JobStatus::Queued
    );

    stop_background_dispatcher(handle).await;
}

#[tokio::test]
async fn run_forever_refreshes_heartbeat_while_job_is_running() {
    let runner = BlockingRunner::new();
    let mut config = DispatcherConfig::new(unique_temp_path("ingot-runtime-heartbeat-state"));
    config.poll_interval = Duration::from_secs(10);
    config.heartbeat_interval = Duration::from_millis(20);
    config.job_timeout = Duration::from_secs(5);
    config.max_concurrent_jobs = 1;
    let h = TestHarness::with_config(Arc::new(runner.clone()), Some(config)).await;
    h.register_mutating_agent().await;
    h.dispatcher
        .reconcile_startup()
        .await
        .expect("reconcile startup");

    let (_, _, job) =
        create_supervised_authoring_job(&h, parse_timestamp("2026-03-12T00:00:00Z")).await;

    let dispatcher = h.dispatcher.clone();
    let handle = tokio::spawn(async move { dispatcher.run_forever().await });
    h.dispatch_notify.notify();

    runner.wait_for_launches(1, Duration::from_secs(5)).await;
    let running_job = h
        .wait_for_job_status(job.id, JobStatus::Running, Duration::from_secs(5))
        .await;
    let initial_heartbeat = running_job
        .state
        .heartbeat_at()
        .expect("initial running heartbeat");

    let refreshed_job = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let job = h.db.get_job(job.id).await.expect("reload job");
            if job
                .state
                .heartbeat_at()
                .is_some_and(|heartbeat| heartbeat > initial_heartbeat)
            {
                return job;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("timed out waiting for heartbeat refresh");
    assert!(
        refreshed_job
            .state
            .heartbeat_at()
            .is_some_and(|heartbeat| heartbeat > initial_heartbeat)
    );

    runner.release_all();
    h.wait_for_job_status(job.id, JobStatus::Completed, Duration::from_secs(5))
        .await;
    stop_background_dispatcher(handle).await;
}
