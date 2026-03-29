use std::ffi::OsString;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use ingot_agent_protocol::adapter::AgentError;
use ingot_agent_protocol::request::AgentRequest;
use ingot_agent_protocol::response::AgentResponse;
use ingot_agent_runtime::{AgentRunner, DispatcherConfig, JobDispatcher};
use ingot_domain::agent::AgentCapability;
use ingot_domain::commit_oid::CommitOid;
use ingot_domain::job::{ExecutionPermission, JobInput, JobStatus, OutputArtifactKind, PhaseKind};
use ingot_domain::test_support::{AgentBuilder, JobBuilder};
use ingot_domain::workspace::{WorkspaceKind, WorkspaceStatus};
use ingot_test_support::git::unique_temp_path;
use ingot_usecases::{DispatchNotify, ProjectLocks};
use tower::ServiceExt;
use uuid::Uuid;

mod common;
use common::*;

fn home_env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

struct HomeEnvGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    original: Option<OsString>,
}

impl HomeEnvGuard {
    fn set(home: &Path) -> Self {
        let lock = home_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let original = std::env::var_os("HOME");
        // Guarded by a process-wide mutex and current-thread tests so no other test mutates HOME concurrently.
        unsafe { std::env::set_var("HOME", home) };
        Self {
            _lock: lock,
            original,
        }
    }
}

impl Drop for HomeEnvGuard {
    fn drop(&mut self) {
        if let Some(value) = &self.original {
            // Guarded by the same process-wide mutex acquired in `HomeEnvGuard::set`.
            unsafe { std::env::set_var("HOME", value) };
        } else {
            // Guarded by the same process-wide mutex acquired in `HomeEnvGuard::set`.
            unsafe { std::env::remove_var("HOME") };
        }
    }
}

struct DemoRunner;

impl AgentRunner for DemoRunner {
    fn launch<'a>(
        &'a self,
        _agent: &'a ingot_domain::agent::Agent,
        _request: &'a AgentRequest,
        working_dir: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<AgentResponse, AgentError>> + Send + 'a>> {
        Box::pin(async move {
            tokio::fs::write(working_dir.join("generated.txt"), "demo authoring change")
                .await
                .expect("write generated file");
            Ok(AgentResponse {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
                result: None,
            })
        })
    }
}

#[tokio::test(flavor = "current_thread")]
async fn create_demo_project_route_creates_implicit_initial_revisions_under_temp_home() {
    let home = std::env::temp_dir().join(format!("ingot-demo-home-{}", Uuid::now_v7()));
    let home_documents = home.join("Documents");
    std::fs::create_dir_all(&home_documents).expect("create temp Documents");
    let _home = HomeEnvGuard::set(&home);

    let db = migrated_test_db("ingot-http-api-demo-routes").await;
    let app = test_router(db.clone());

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/demo-project")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "name": "Finance Tracker Demo",
                        "template": "finance-tracker",
                        "stack": "express-react"
                    })
                    .to_string(),
                ))
                .expect("build request"),
        )
        .await
        .expect("demo project response");

    assert_eq!(response.status(), StatusCode::CREATED);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("demo json");
    let project_id = parse_id(json["project"]["id"].as_str().expect("project id"));
    let items_created = json["items_created"].as_u64().expect("items_created") as usize;

    let project = db.get_project(project_id).await.expect("load project");
    let canonical_documents =
        std::fs::canonicalize(&home_documents).expect("canonicalize temp Documents");
    assert!(
        project.path.starts_with(&canonical_documents),
        "demo project should stay under the temp HOME Documents directory"
    );
    assert!(project.path.exists(), "demo repo should exist on disk");

    let demo_head = CommitOid::new(git_output(&project.path, &["rev-parse", "HEAD"]));
    let items = db
        .list_items_by_project(project.id)
        .await
        .expect("list demo items");
    assert_eq!(items.len(), items_created);

    for item in items {
        let revision = db
            .get_revision(item.current_revision_id)
            .await
            .expect("load revision");
        assert_eq!(revision.target_ref.as_str(), "refs/heads/main");
        assert_eq!(revision.seed.seed_commit_oid(), None);
        assert_eq!(revision.seed.seed_target_commit_oid(), &demo_head);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn demo_project_runtime_rebinds_stale_author_initial_job_to_advanced_head() {
    let home = std::env::temp_dir().join(format!("ingot-demo-home-{}", Uuid::now_v7()));
    std::fs::create_dir_all(home.join("Documents")).expect("create temp Documents");
    let _home = HomeEnvGuard::set(&home);

    let db = migrated_test_db("ingot-http-api-demo-runtime").await;
    let app = test_router(db.clone());

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/demo-project")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "name": "Finance Tracker Runtime",
                        "template": "finance-tracker",
                        "stack": "express-react"
                    })
                    .to_string(),
                ))
                .expect("build request"),
        )
        .await
        .expect("demo project response");

    assert_eq!(response.status(), StatusCode::CREATED);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("demo json");
    let project_id = parse_id(json["project"]["id"].as_str().expect("project id"));

    let project = db.get_project(project_id).await.expect("load project");
    let items = db
        .list_items_by_project(project.id)
        .await
        .expect("list demo items");
    let mut selected = None;
    for item in items {
        let revision = db
            .get_revision(item.current_revision_id)
            .await
            .expect("load revision");
        if revision.title.starts_with("002") {
            selected = Some((item, revision));
            break;
        }
    }
    let (item, revision) = selected.expect("find later demo revision");

    let stale_head = revision.seed.seed_target_commit_oid().clone();
    let job = JobBuilder::new(project.id, item.id, revision.id, "author_initial")
        .phase_kind(PhaseKind::Author)
        .workspace_kind(WorkspaceKind::Authoring)
        .execution_permission(ExecutionPermission::MayMutate)
        .phase_template_slug("author-initial")
        .job_input(JobInput::authoring_head(stale_head.clone()))
        .output_artifact_kind(OutputArtifactKind::Commit)
        .build();
    db.create_job(&job).await.expect("create stale queued job");

    let agent = AgentBuilder::new(
        "codex",
        vec![
            AgentCapability::MutatingJobs,
            AgentCapability::ReadOnlyJobs,
            AgentCapability::StructuredOutput,
        ],
    )
    .build();
    db.create_agent(&agent).await.expect("create agent");

    std::fs::write(project.path.join("advanced.txt"), "advanced target head")
        .expect("write advanced");
    git(&project.path, &["add", "advanced.txt"]);
    git(&project.path, &["commit", "-m", "advanced target head"]);
    let advanced_head = CommitOid::new(git_output(&project.path, &["rev-parse", "HEAD"]));

    let dispatcher = JobDispatcher::with_runner(
        db.clone(),
        ProjectLocks::default(),
        DispatcherConfig::new(unique_temp_path("ingot-http-api-demo-runtime-state")),
        Arc::new(DemoRunner),
        DispatchNotify::default(),
    );

    let made_progress = dispatcher.tick().await.expect("runtime tick");
    assert!(made_progress, "runtime should process the queued demo job");

    let updated_job = db.get_job(job.id).await.expect("reload job");
    assert_eq!(updated_job.state.status(), JobStatus::Completed);
    assert_eq!(
        updated_job.job_input,
        JobInput::authoring_head(advanced_head.clone())
    );

    let workspace = db
        .find_authoring_workspace_for_revision(revision.id)
        .await
        .expect("load authoring workspace")
        .expect("authoring workspace");
    assert_eq!(workspace.state.status(), WorkspaceStatus::Ready);
    assert_eq!(workspace.state.base_commit_oid(), Some(&advanced_head));
}
