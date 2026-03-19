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
