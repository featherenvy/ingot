use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use ingot_agent_protocol::adapter::AgentError;
use ingot_agent_protocol::request::AgentRequest;
use ingot_agent_protocol::response::AgentResponse;
use ingot_agent_runtime::{AgentRunner, DispatcherConfig, JobDispatcher};
use ingot_domain::agent::{AdapterKind, Agent, AgentCapability, AgentStatus};
use ingot_domain::ids;
use ingot_domain::item::{
    ApprovalState, Classification, EscalationState, LifecycleState, OriginKind, ParkingState,
    Priority,
};
use ingot_domain::job::{
    ContextPolicy, ExecutionPermission, Job, JobInput, JobStatus, OutputArtifactKind, PhaseKind,
};
use ingot_domain::project::Project;
use ingot_domain::revision::{ApprovalPolicy, ItemRevision};
use ingot_domain::workspace::{
    RetentionPolicy, Workspace, WorkspaceKind, WorkspaceStatus, WorkspaceStrategy,
};
use ingot_git::commands::head_oid;
use ingot_git::project_repo::{ensure_mirror, project_repo_paths, ProjectRepoPaths};
use ingot_store_sqlite::Database;
use ingot_usecases::ProjectLocks;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// TestHarness
// ---------------------------------------------------------------------------

pub struct TestHarness {
    pub db: Database,
    pub dispatcher: JobDispatcher,
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
        let repo = temp_git_repo();
        let db_path =
            std::env::temp_dir().join(format!("ingot-runtime-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let state_root =
            std::env::temp_dir().join(format!("ingot-runtime-state-{}", Uuid::now_v7()));
        let config = config.unwrap_or_else(|| DispatcherConfig::new(state_root.clone()));
        let dispatcher = JobDispatcher::with_runner(
            db.clone(),
            ProjectLocks::default(),
            config,
            runner,
        );

        let created_at = Utc::now();
        let project = Project {
            id: ids::ProjectId::new(),
            name: "repo".into(),
            path: repo.display().to_string(),
            default_branch: "main".into(),
            color: "#000".into(),
            created_at,
            updated_at: created_at,
        };
        db.create_project(&project).await.expect("create project");

        Self {
            db,
            dispatcher,
            project,
            state_root,
            repo_path: repo,
        }
    }

    pub async fn register_mutating_agent(&self) -> Agent {
        let agent = Agent {
            id: ids::AgentId::new(),
            slug: "codex".into(),
            name: "Codex".into(),
            adapter_kind: AdapterKind::Codex,
            provider: "openai".into(),
            model: "gpt-5-codex".into(),
            cli_path: "codex".into(),
            capabilities: vec![
                AgentCapability::MutatingJobs,
                AgentCapability::StructuredOutput,
            ],
            health_check: Some("ok".into()),
            status: AgentStatus::Available,
        };
        self.db.create_agent(&agent).await.expect("create agent");
        agent
    }

    #[allow(dead_code)]
    pub async fn register_review_agent(&self) -> Agent {
        let agent = Agent {
            id: ids::AgentId::new(),
            slug: "codex-review".into(),
            name: "Codex".into(),
            adapter_kind: AdapterKind::Codex,
            provider: "openai".into(),
            model: "gpt-5-codex".into(),
            cli_path: "codex".into(),
            capabilities: vec![
                AgentCapability::ReadOnlyJobs,
                AgentCapability::StructuredOutput,
            ],
            health_check: Some("ok".into()),
            status: AgentStatus::Available,
        };
        self.db.create_agent(&agent).await.expect("create agent");
        agent
    }

    pub async fn register_full_agent(&self) -> Agent {
        let agent = Agent {
            id: ids::AgentId::new(),
            slug: "codex".into(),
            name: "Codex".into(),
            adapter_kind: AdapterKind::Codex,
            provider: "openai".into(),
            model: "gpt-5-codex".into(),
            cli_path: "codex".into(),
            capabilities: vec![
                AgentCapability::MutatingJobs,
                AgentCapability::ReadOnlyJobs,
                AgentCapability::StructuredOutput,
            ],
            health_check: Some("ok".into()),
            status: AgentStatus::Available,
        };
        self.db.create_agent(&agent).await.expect("create agent");
        agent
    }
}

// ---------------------------------------------------------------------------
// Entity builders
// ---------------------------------------------------------------------------

pub fn test_item(project_id: ids::ProjectId, revision_id: ids::ItemRevisionId) -> ingot_domain::item::Item {
    let now = Utc::now();
    ingot_domain::item::Item {
        id: ids::ItemId::new(),
        project_id,
        classification: Classification::Change,
        workflow_version: "delivery:v1".into(),
        lifecycle_state: LifecycleState::Open,
        parking_state: ParkingState::Active,
        done_reason: None,
        resolution_source: None,
        approval_state: ApprovalState::NotRequested,
        escalation_state: EscalationState::None,
        escalation_reason: None,
        current_revision_id: revision_id,
        origin_kind: OriginKind::Manual,
        origin_finding_id: None,
        priority: Priority::Major,
        labels: vec![],
        operator_notes: None,
        created_at: now,
        updated_at: now,
        closed_at: None,
    }
}

pub fn test_revision(item_id: ids::ItemId, seed_commit: &str) -> ItemRevision {
    let now = Utc::now();
    ItemRevision {
        id: ids::ItemRevisionId::new(),
        item_id,
        revision_no: 1,
        title: "Test item".into(),
        description: "Test item".into(),
        acceptance_criteria: "Test item".into(),
        target_ref: "refs/heads/main".into(),
        approval_policy: ApprovalPolicy::Required,
        policy_snapshot: serde_json::json!({}),
        template_map_snapshot: serde_json::json!({}),
        seed_commit_oid: Some(seed_commit.into()),
        seed_target_commit_oid: Some(seed_commit.into()),
        supersedes_revision_id: None,
        created_at: now,
    }
}

pub fn test_authoring_job(
    project_id: ids::ProjectId,
    item_id: ids::ItemId,
    revision_id: ids::ItemRevisionId,
    seed_commit: &str,
) -> Job {
    let now = Utc::now();
    Job {
        id: ids::JobId::new(),
        project_id,
        item_id,
        item_revision_id: revision_id,
        step_id: "author_initial".into(),
        semantic_attempt_no: 1,
        retry_no: 0,
        supersedes_job_id: None,
        status: JobStatus::Queued,
        outcome_class: None,
        phase_kind: PhaseKind::Author,
        workspace_id: None,
        workspace_kind: WorkspaceKind::Authoring,
        execution_permission: ExecutionPermission::MayMutate,
        context_policy: ContextPolicy::Fresh,
        phase_template_slug: "author-initial".into(),
        phase_template_digest: None,
        prompt_snapshot: None,
        job_input: JobInput::authoring_head(seed_commit),
        output_artifact_kind: OutputArtifactKind::Commit,
        output_commit_oid: None,
        result_schema_version: None,
        result_payload: None,
        agent_id: None,
        process_pid: None,
        lease_owner_id: None,
        heartbeat_at: None,
        lease_expires_at: None,
        error_code: None,
        error_message: None,
        created_at: now,
        started_at: None,
        ended_at: None,
    }
}

pub fn test_review_job(
    project_id: ids::ProjectId,
    item_id: ids::ItemId,
    revision_id: ids::ItemRevisionId,
    base_commit: &str,
    head_commit: &str,
) -> Job {
    let now = Utc::now();
    Job {
        id: ids::JobId::new(),
        project_id,
        item_id,
        item_revision_id: revision_id,
        step_id: "review_candidate_initial".into(),
        semantic_attempt_no: 1,
        retry_no: 0,
        supersedes_job_id: None,
        status: JobStatus::Queued,
        outcome_class: None,
        phase_kind: PhaseKind::Review,
        workspace_id: None,
        workspace_kind: WorkspaceKind::Review,
        execution_permission: ExecutionPermission::MustNotMutate,
        context_policy: ContextPolicy::Fresh,
        phase_template_slug: "review-candidate".into(),
        phase_template_digest: None,
        prompt_snapshot: None,
        job_input: JobInput::candidate_subject(base_commit, head_commit),
        output_artifact_kind: OutputArtifactKind::ReviewReport,
        output_commit_oid: None,
        result_schema_version: None,
        result_payload: None,
        agent_id: None,
        process_pid: None,
        lease_owner_id: None,
        heartbeat_at: None,
        lease_expires_at: None,
        error_code: None,
        error_message: None,
        created_at: now,
        started_at: None,
        ended_at: None,
    }
}

// ---------------------------------------------------------------------------
// Fake runners
// ---------------------------------------------------------------------------

pub struct FakeRunner;

impl AgentRunner for FakeRunner {
    fn launch<'a>(
        &'a self,
        _agent: &'a Agent,
        _request: &'a AgentRequest,
        working_dir: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<AgentResponse, AgentError>> + Send + 'a>> {
        Box::pin(async move {
            tokio::fs::write(working_dir.join("generated.txt"), "hello")
                .await
                .unwrap();
            Ok(AgentResponse {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
                result: Some(serde_json::json!({ "message": "implemented change" })),
            })
        })
    }
}

pub struct StaticReviewRunner {
    pub base_commit_oid: String,
    pub head_commit_oid: String,
}

impl AgentRunner for StaticReviewRunner {
    fn launch<'a>(
        &'a self,
        _agent: &'a Agent,
        _request: &'a AgentRequest,
        _working_dir: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<AgentResponse, AgentError>> + Send + 'a>> {
        Box::pin(async move {
            Ok(AgentResponse {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
                result: Some(serde_json::json!({
                    "outcome": "clean",
                    "summary": "No issues found",
                    "review_subject": {
                        "base_commit_oid": self.base_commit_oid,
                        "head_commit_oid": self.head_commit_oid
                    },
                    "overall_risk": "low",
                    "findings": []
                })),
            })
        })
    }
}

pub struct ScriptedLoopRunner;

impl AgentRunner for ScriptedLoopRunner {
    fn launch<'a>(
        &'a self,
        _agent: &'a Agent,
        request: &'a AgentRequest,
        working_dir: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<AgentResponse, AgentError>> + Send + 'a>> {
        Box::pin(async move {
            let step = prompt_value(&request.prompt, "Step");
            match step.as_deref() {
                Some("author_initial") => {
                    tokio::fs::write(working_dir.join("feature.txt"), "initial change")
                        .await
                        .expect("write feature");
                    Ok(AgentResponse {
                        exit_code: 0,
                        stdout: String::new(),
                        stderr: String::new(),
                        result: Some(serde_json::json!({ "summary": "initial authored" })),
                    })
                }
                Some("repair_candidate") => {
                    tokio::fs::write(working_dir.join("feature.txt"), "repaired change")
                        .await
                        .expect("repair feature");
                    Ok(AgentResponse {
                        exit_code: 0,
                        stdout: String::new(),
                        stderr: String::new(),
                        result: Some(serde_json::json!({ "summary": "candidate repaired" })),
                    })
                }
                Some("review_incremental_initial") => Ok(AgentResponse {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                    result: Some(serde_json::json!({
                        "outcome": "findings",
                        "summary": "initial review found an issue",
                        "review_subject": {
                            "base_commit_oid": prompt_value(&request.prompt, "Input base commit").unwrap_or_default(),
                            "head_commit_oid": prompt_value(&request.prompt, "Input head commit").unwrap_or_default()
                        },
                        "overall_risk": "medium",
                        "findings": [{
                            "finding_key": "fix-me",
                            "code": "BUG",
                            "severity": "medium",
                            "summary": "needs repair",
                            "paths": ["feature.txt"],
                            "evidence": ["fix me"]
                        }]
                    })),
                }),
                Some("review_incremental_repair") | Some("review_candidate_repair") => {
                    Ok(AgentResponse {
                        exit_code: 0,
                        stdout: String::new(),
                        stderr: String::new(),
                        result: Some(serde_json::json!({
                            "outcome": "clean",
                            "summary": "review clean",
                            "review_subject": {
                                "base_commit_oid": prompt_value(&request.prompt, "Input base commit").unwrap_or_default(),
                                "head_commit_oid": prompt_value(&request.prompt, "Input head commit").unwrap_or_default()
                            },
                            "overall_risk": "low",
                            "findings": []
                        })),
                    })
                }
                Some("validate_candidate_repair") => Ok(AgentResponse {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                    result: Some(serde_json::json!({
                        "outcome": "clean",
                        "summary": "validation clean",
                        "checks": [],
                        "findings": []
                    })),
                }),
                other => Err(AgentError::ProtocolViolation(format!(
                    "unexpected step in scripted loop runner: {other:?}"
                ))),
            }
        })
    }
}

pub struct CleanInitialReviewRunner;

impl AgentRunner for CleanInitialReviewRunner {
    fn launch<'a>(
        &'a self,
        _agent: &'a Agent,
        request: &'a AgentRequest,
        _working_dir: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<AgentResponse, AgentError>> + Send + 'a>> {
        Box::pin(async move {
            match prompt_value(&request.prompt, "Step").as_deref() {
                Some("review_incremental_initial") => Ok(AgentResponse {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                    result: Some(serde_json::json!({
                        "outcome": "clean",
                        "summary": "incremental review clean",
                        "review_subject": {
                            "base_commit_oid": prompt_value(&request.prompt, "Input base commit").unwrap_or_default(),
                            "head_commit_oid": prompt_value(&request.prompt, "Input head commit").unwrap_or_default()
                        },
                        "overall_risk": "low",
                        "findings": []
                    })),
                }),
                other => Err(AgentError::ProtocolViolation(format!(
                    "unexpected step in clean initial review runner: {other:?}"
                ))),
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Mirror / git helpers
// ---------------------------------------------------------------------------

pub async fn ensure_test_mirror(
    state_root: &Path,
    project: &Project,
) -> ProjectRepoPaths {
    let paths = project_repo_paths(
        state_root,
        project.id,
        Path::new(&project.path),
    );
    ensure_mirror(&paths).await.expect("ensure mirror");
    paths
}

pub async fn create_mirror_only_commit(
    mirror_git_dir: &Path,
    base_commit: &str,
    workspace_ref: &str,
    message: &str,
) -> (PathBuf, String) {
    let worktree_path =
        std::env::temp_dir().join(format!("ingot-runtime-mirror-only-{}", Uuid::now_v7()));
    git_sync(
        mirror_git_dir,
        &[
            "worktree",
            "add",
            "--detach",
            worktree_path.to_str().expect("worktree path"),
            base_commit,
        ],
    );
    git_sync(&worktree_path, &["config", "user.name", "Ingot Test"]);
    git_sync(
        &worktree_path,
        &["config", "user.email", "ingot@example.com"],
    );
    std::fs::write(worktree_path.join("tracked.txt"), message).expect("write tracked file");
    git_sync(&worktree_path, &["add", "tracked.txt"]);
    git_sync(&worktree_path, &["commit", "-m", message]);
    let commit_oid = head_oid(&worktree_path).await.expect("mirror-only head");
    git_sync(mirror_git_dir, &["update-ref", workspace_ref, &commit_oid]);
    (worktree_path, commit_oid)
}

pub fn temp_git_repo() -> PathBuf {
    let path = std::env::temp_dir().join(format!("ingot-runtime-repo-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&path).expect("create temp repo dir");
    git_sync(&path, &["init"]);
    git_sync(&path, &["branch", "-M", "main"]);
    git_sync(&path, &["config", "user.name", "Ingot Test"]);
    git_sync(&path, &["config", "user.email", "ingot@example.com"]);
    std::fs::write(path.join("tracked.txt"), "initial").expect("write tracked file");
    git_sync(&path, &["add", "tracked.txt"]);
    git_sync(&path, &["commit", "-m", "initial"]);
    path
}

pub fn git_sync(path: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(path)
        .status()
        .expect("run git");
    assert!(status.success(), "git {:?} failed", args);
}

pub fn git_output(path: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .expect("run git output");
    assert!(output.status.success(), "git {:?} failed", args);
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

pub fn prompt_value(prompt: &str, label: &str) -> Option<String> {
    prompt.lines().find_map(|line| {
        let prefix = format!("- {label}: ");
        line.strip_prefix(&prefix).map(ToOwned::to_owned)
    })
}

// ---------------------------------------------------------------------------
// Legacy make_runtime_* helpers (used by reconciliation.rs mixed-state tests)
// ---------------------------------------------------------------------------

pub fn make_runtime_item(
    project_id: ids::ProjectId,
    revision_id: ids::ItemRevisionId,
    item_id: ids::ItemId,
    created_at: DateTime<Utc>,
) -> ingot_domain::item::Item {
    ingot_domain::item::Item {
        id: item_id,
        project_id,
        classification: Classification::Change,
        workflow_version: "delivery:v1".into(),
        lifecycle_state: LifecycleState::Open,
        parking_state: ParkingState::Active,
        done_reason: None,
        resolution_source: None,
        approval_state: ApprovalState::NotRequested,
        escalation_state: EscalationState::None,
        escalation_reason: None,
        current_revision_id: revision_id,
        origin_kind: OriginKind::Manual,
        origin_finding_id: None,
        priority: Priority::Major,
        labels: vec![],
        operator_notes: None,
        created_at,
        updated_at: created_at,
        closed_at: None,
    }
}

pub fn make_runtime_revision(
    item_id: ids::ItemId,
    revision_no: u32,
    seed_commit_oid: &str,
    created_at: DateTime<Utc>,
) -> ItemRevision {
    ItemRevision {
        id: ids::ItemRevisionId::new(),
        item_id,
        revision_no,
        title: "Runtime".into(),
        description: "runtime".into(),
        acceptance_criteria: "runtime".into(),
        target_ref: "refs/heads/main".into(),
        approval_policy: ApprovalPolicy::Required,
        policy_snapshot: serde_json::json!({}),
        template_map_snapshot: serde_json::json!({}),
        seed_commit_oid: Some(seed_commit_oid.into()),
        seed_target_commit_oid: Some(seed_commit_oid.into()),
        supersedes_revision_id: None,
        created_at,
    }
}

pub fn make_runtime_workspace(
    project_id: ids::ProjectId,
    revision_id: Option<ids::ItemRevisionId>,
    kind: WorkspaceKind,
    status: WorkspaceStatus,
    head_commit_oid: &str,
    created_at: DateTime<Utc>,
) -> Workspace {
    Workspace {
        id: ids::WorkspaceId::new(),
        project_id,
        kind,
        strategy: WorkspaceStrategy::Worktree,
        path: std::env::temp_dir()
            .join(format!("ingot-runtime-mixed-workspace-{}", Uuid::now_v7()))
            .display()
            .to_string(),
        created_for_revision_id: revision_id,
        parent_workspace_id: None,
        target_ref: Some("refs/heads/main".into()),
        workspace_ref: Some(format!("refs/ingot/workspaces/{}", Uuid::now_v7().simple())),
        base_commit_oid: Some(head_commit_oid.into()),
        head_commit_oid: Some(head_commit_oid.into()),
        retention_policy: RetentionPolicy::Persistent,
        status,
        current_job_id: None,
        created_at,
        updated_at: created_at,
    }
}

pub fn make_runtime_job(
    project_id: ids::ProjectId,
    item_id: ids::ItemId,
    revision_id: ids::ItemRevisionId,
    step_id: &str,
    workspace_kind: WorkspaceKind,
    output_artifact_kind: OutputArtifactKind,
    created_at: DateTime<Utc>,
) -> Job {
    Job {
        id: ids::JobId::new(),
        project_id,
        item_id,
        item_revision_id: revision_id,
        step_id: step_id.into(),
        semantic_attempt_no: 1,
        retry_no: 0,
        supersedes_job_id: None,
        status: JobStatus::Queued,
        outcome_class: None,
        phase_kind: PhaseKind::Author,
        workspace_id: None,
        workspace_kind,
        execution_permission: ExecutionPermission::MayMutate,
        context_policy: ContextPolicy::Fresh,
        phase_template_slug: "template".into(),
        phase_template_digest: None,
        prompt_snapshot: None,
        job_input: JobInput::authoring_head("seed"),
        output_artifact_kind,
        output_commit_oid: None,
        result_schema_version: None,
        result_payload: None,
        agent_id: None,
        process_pid: None,
        lease_owner_id: None,
        heartbeat_at: None,
        lease_expires_at: None,
        error_code: None,
        error_message: None,
        created_at,
        started_at: None,
        ended_at: None,
    }
}
