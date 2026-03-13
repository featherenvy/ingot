use std::path::Path as FsPath;
use std::path::PathBuf;
use std::str::FromStr;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use chrono::Utc;
use ingot_agent_adapters::registry::{default_agent_capabilities, probe_and_apply};
use ingot_config::IngotConfig;
use ingot_config::loader::load_config;
use ingot_domain::activity::{Activity, ActivityEventType};
use ingot_domain::agent::{AdapterKind, Agent, AgentCapability, AgentStatus};
use serde::{Deserialize, Serialize};

use ingot_domain::convergence::Convergence;
use ingot_domain::finding::{Finding, FindingTriageState};
use ingot_domain::git_operation::{GitEntityType, GitOperation, GitOperationStatus, OperationKind};
use ingot_domain::ids::{AgentId, FindingId, ItemId, JobId, ProjectId, WorkspaceId};
use ingot_domain::item::{
    ApprovalState, Classification, DoneReason, EscalationReason, Item, LifecycleState, OriginKind,
    Priority, ResolutionSource,
};
use ingot_domain::job::{Job, JobStatus, OutcomeClass};
use ingot_domain::ports::{ProjectMutationLockPort, RepositoryError};
use ingot_domain::project::Project;
use ingot_domain::revision::{ApprovalPolicy, ItemRevision};
use ingot_domain::revision_context::RevisionContextSummary;
use ingot_domain::workspace::{Workspace, WorkspaceKind, WorkspaceStatus};
use ingot_git::GitJobCompletionPort;
use ingot_git::commands::{
    compare_and_swap_ref, current_branch_name, delete_ref, git, is_commit_reachable_from_any_ref,
    resolve_ref_oid,
};
use ingot_git::commit::{
    ConvergenceCommitTrailers, abort_cherry_pick, cherry_pick_no_commit, commit_message,
    create_daemon_convergence_commit, list_commits_oldest_first,
};
use ingot_git::diff::changed_paths_between;
use ingot_store_sqlite::{Database, FinishJobNonSuccessParams, StartJobExecutionParams};
use ingot_usecases::finding::{
    BacklogFindingOverrides, TriageFindingInput, backlog_finding, parse_revision_context_summary,
    triage_finding,
};
use ingot_usecases::item::{
    CreateItemInput, approval_state_for_policy, create_manual_item, default_policy_snapshot,
    default_template_map_snapshot, normalize_target_ref, rework_budgets_from_policy_snapshot,
};
use ingot_usecases::job::{DispatchJobCommand, dispatch_job, retry_job};
use ingot_usecases::{
    CompleteJobCommand, CompleteJobError, CompleteJobService, ProjectLocks, UseCaseError,
    rebuild_revision_context,
};
use ingot_workflow::{Evaluation, Evaluator};
use ingot_workspace::{
    WorkspaceError, ensure_authoring_workspace_state, provision_integration_workspace,
    provision_review_workspace, remove_workspace, workspace_root_path,
};

use crate::error::ApiError;

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) db: Database,
    complete_job_service: CompleteJobService<Database, GitJobCompletionPort, ProjectLocks>,
    pub(crate) project_locks: ProjectLocks,
}

#[derive(Debug, Serialize)]
pub struct ItemSummaryResponse {
    pub item: Item,
    pub title: String,
    pub evaluation: Evaluation,
}

#[derive(Debug, Serialize)]
pub struct ConvergenceResponse {
    pub id: String,
    pub status: String,
    pub input_target_commit_oid: Option<String>,
    pub prepared_commit_oid: Option<String>,
    pub final_target_commit_oid: Option<String>,
    pub target_head_valid: bool,
}

#[derive(Debug, Serialize)]
pub struct ItemDetailResponse {
    pub item: Item,
    pub current_revision: ItemRevision,
    pub evaluation: Evaluation,
    pub revision_history: Vec<ItemRevision>,
    pub jobs: Vec<Job>,
    pub findings: Vec<Finding>,
    pub workspaces: Vec<Workspace>,
    pub convergences: Vec<ConvergenceResponse>,
    pub revision_context_summary: Option<RevisionContextSummary>,
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct PromoteFindingResponse {
    pub item: Item,
    pub current_revision: ItemRevision,
    pub finding: Finding,
}

#[derive(Debug, Serialize)]
pub struct CompleteJobResponse {
    pub finding_count: usize,
}

#[derive(Debug, Serialize)]
pub struct PrepareConvergenceResponse {
    pub convergence: ConvergenceResponse,
    pub validation_job: Job,
}

#[derive(Debug, Serialize)]
pub struct JobLogsResponse {
    pub prompt: Option<String>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub result: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct ActivityQuery {
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct CreateProjectRequest {
    pub name: Option<String>,
    pub path: String,
    pub default_branch: Option<String>,
    pub color: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct UpdateProjectRequest {
    pub name: Option<String>,
    pub path: Option<String>,
    pub default_branch: Option<String>,
    pub color: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct UpdateItemRequest {
    pub classification: Option<Classification>,
    pub priority: Option<Priority>,
    pub labels: Option<Vec<String>>,
    pub operator_notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateAgentRequest {
    pub slug: Option<String>,
    pub name: String,
    pub adapter_kind: AdapterKind,
    pub provider: String,
    pub model: String,
    pub cli_path: String,
    pub capabilities: Option<Vec<AgentCapability>>,
}

#[derive(Debug, Default, Deserialize)]
pub struct UpdateAgentRequest {
    pub slug: Option<String>,
    pub name: Option<String>,
    pub adapter_kind: Option<AdapterKind>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub cli_path: Option<String>,
    pub capabilities: Option<Vec<AgentCapability>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateItemRequest {
    pub title: String,
    pub description: String,
    pub acceptance_criteria: String,
    pub classification: Option<Classification>,
    pub priority: Option<Priority>,
    pub labels: Option<Vec<String>>,
    pub operator_notes: Option<String>,
    pub target_ref: Option<String>,
    pub approval_policy: Option<ApprovalPolicy>,
    pub seed_commit_oid: Option<String>,
    pub seed_target_commit_oid: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DismissFindingRequest {
    pub dismissal_reason: String,
}

#[derive(Debug, Deserialize)]
pub struct TriageFindingRequest {
    pub triage_state: FindingTriageState,
    pub triage_note: Option<String>,
    pub linked_item_id: Option<String>,
    pub target_ref: Option<String>,
    pub approval_policy: Option<ApprovalPolicy>,
}

#[derive(Debug, Default, Deserialize)]
pub struct DispatchJobRequest {
    pub step_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AssignJobRequest {
    pub agent_id: String,
}

#[derive(Debug, Deserialize)]
pub struct StartJobRequest {
    pub lease_owner_id: String,
    pub process_pid: Option<u32>,
    pub lease_duration_seconds: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct HeartbeatJobRequest {
    pub lease_owner_id: String,
    pub lease_duration_seconds: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct PromoteFindingRequest {
    pub target_ref: Option<String>,
    pub approval_policy: Option<ApprovalPolicy>,
}

#[derive(Debug, Default, Deserialize)]
pub struct RejectApprovalRequest {
    pub title: Option<String>,
    pub description: Option<String>,
    pub acceptance_criteria: Option<String>,
    pub target_ref: Option<String>,
    pub approval_policy: Option<ApprovalPolicy>,
    pub seed_commit_oid: Option<String>,
    pub seed_target_commit_oid: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ReviseItemRequest {
    pub title: Option<String>,
    pub description: Option<String>,
    pub acceptance_criteria: Option<String>,
    pub target_ref: Option<String>,
    pub approval_policy: Option<ApprovalPolicy>,
    pub seed_commit_oid: Option<String>,
    pub seed_target_commit_oid: Option<String>,
}

impl From<RejectApprovalRequest> for ReviseItemRequest {
    fn from(request: RejectApprovalRequest) -> Self {
        Self {
            title: request.title,
            description: request.description,
            acceptance_criteria: request.acceptance_criteria,
            target_ref: request.target_ref,
            approval_policy: request.approval_policy,
            seed_commit_oid: request.seed_commit_oid,
            seed_target_commit_oid: request.seed_target_commit_oid,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct CompleteJobRequest {
    pub outcome_class: OutcomeClass,
    pub result_schema_version: Option<String>,
    pub result_payload: Option<serde_json::Value>,
    pub output_commit_oid: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct FailJobRequest {
    pub outcome_class: OutcomeClass,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
}

/// Build the Axum router with all API routes.
pub fn build_router(db: Database) -> Router {
    build_router_with_project_locks(db, ProjectLocks::default())
}

pub fn build_router_with_project_locks(db: Database, project_locks: ProjectLocks) -> Router {
    let state = AppState {
        db: db.clone(),
        complete_job_service: CompleteJobService::new(
            db,
            GitJobCompletionPort,
            project_locks.clone(),
        ),
        project_locks,
    };

    Router::new()
        .route("/api/health", get(health))
        .route("/api/config", get(get_global_config))
        .route("/api/projects", get(list_projects).post(create_project))
        .route("/api/demo-project", post(crate::demo::create_demo_project))
        .route(
            "/api/projects/{project_id}/activity",
            get(list_project_activity),
        )
        .route(
            "/api/projects/{project_id}/workspaces",
            get(list_project_workspaces),
        )
        .route(
            "/api/projects/{project_id}/workspaces/{workspace_id}/reset",
            post(reset_workspace_route),
        )
        .route(
            "/api/projects/{project_id}/workspaces/{workspace_id}/abandon",
            post(abandon_workspace_route),
        )
        .route(
            "/api/projects/{project_id}/workspaces/{workspace_id}/remove",
            post(remove_workspace_route),
        )
        .route(
            "/api/projects/{project_id}",
            put(update_project).delete(delete_project),
        )
        .route("/api/projects/{project_id}/config", get(get_project_config))
        .route(
            "/api/projects/{project_id}/items",
            get(list_items).post(create_item),
        )
        .route(
            "/api/projects/{project_id}/items/{item_id}",
            get(get_item).patch(update_item),
        )
        .route("/api/agents", get(list_agents).post(create_agent))
        .route(
            "/api/agents/{agent_id}",
            put(update_agent).delete(delete_agent),
        )
        .route("/api/agents/{agent_id}/reprobe", post(reprobe_agent))
        .route(
            "/api/projects/{project_id}/items/{item_id}/revise",
            post(revise_item),
        )
        .route(
            "/api/projects/{project_id}/items/{item_id}/defer",
            post(defer_item),
        )
        .route(
            "/api/projects/{project_id}/items/{item_id}/resume",
            post(resume_item),
        )
        .route(
            "/api/projects/{project_id}/items/{item_id}/dismiss",
            post(dismiss_item),
        )
        .route(
            "/api/projects/{project_id}/items/{item_id}/invalidate",
            post(invalidate_item),
        )
        .route(
            "/api/projects/{project_id}/items/{item_id}/reopen",
            post(reopen_item),
        )
        .route(
            "/api/projects/{project_id}/items/{item_id}/findings",
            get(list_item_findings),
        )
        .route(
            "/api/projects/{project_id}/items/{item_id}/jobs",
            post(dispatch_item_job),
        )
        .route("/api/projects/{project_id}/jobs", get(list_project_jobs))
        .route(
            "/api/projects/{project_id}/items/{item_id}/jobs/{job_id}/retry",
            post(retry_item_job),
        )
        .route(
            "/api/projects/{project_id}/items/{item_id}/jobs/{job_id}/cancel",
            post(cancel_item_job),
        )
        .route(
            "/api/projects/{project_id}/items/{item_id}/convergence/prepare",
            post(prepare_item_convergence),
        )
        .route(
            "/api/projects/{project_id}/items/{item_id}/approval/approve",
            post(approve_item),
        )
        .route(
            "/api/projects/{project_id}/items/{item_id}/approval/reject",
            post(reject_item_approval),
        )
        .route("/api/jobs/{job_id}/assign", post(assign_job))
        .route("/api/jobs/{job_id}/start", post(start_job))
        .route("/api/jobs/{job_id}/heartbeat", post(heartbeat_job))
        .route("/api/jobs/{job_id}/logs", get(get_job_logs))
        .route("/api/findings/{finding_id}", get(get_finding))
        .route(
            "/api/findings/{finding_id}/triage",
            post(triage_item_finding),
        )
        .route(
            "/api/findings/{finding_id}/promote",
            post(promote_item_from_finding),
        )
        .route(
            "/api/findings/{finding_id}/dismiss",
            post(dismiss_item_finding),
        )
        .route("/api/jobs/{job_id}/complete", post(complete_job))
        .route("/api/jobs/{job_id}/fail", post(fail_job))
        .route("/api/jobs/{job_id}/expire", post(expire_job))
        .with_state(state)
}

async fn health() -> &'static str {
    "ok"
}

async fn get_global_config() -> Result<Json<IngotConfig>, ApiError> {
    Ok(Json(load_effective_config(None)?))
}

async fn list_project_activity(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
    Query(query): Query<ActivityQuery>,
) -> Result<Json<Vec<Activity>>, ApiError> {
    let project_id = parse_id::<ProjectId>(&project_id, "project")?;
    state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;
    let activity = state
        .db
        .list_activity_by_project(
            project_id,
            query.limit.unwrap_or(50),
            query.offset.unwrap_or(0),
        )
        .await
        .map_err(repo_to_internal)?;
    Ok(Json(activity))
}

async fn list_project_workspaces(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> Result<Json<Vec<Workspace>>, ApiError> {
    let project_id = parse_id::<ProjectId>(&project_id, "project")?;
    state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;
    let workspaces = state
        .db
        .list_workspaces_by_project(project_id)
        .await
        .map_err(repo_to_internal)?;
    Ok(Json(workspaces))
}

async fn reset_workspace_route(
    State(state): State<AppState>,
    Path((project_id, workspace_id)): Path<(String, String)>,
) -> Result<Json<Workspace>, ApiError> {
    let project_id = parse_id::<ProjectId>(&project_id, "project")?;
    let workspace_id = parse_id::<WorkspaceId>(&workspace_id, "workspace")?;
    let project = state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;
    let _guard = state
        .project_locks
        .acquire_project_mutation(project_id)
        .await;
    let mut workspace = state
        .db
        .get_workspace(workspace_id)
        .await
        .map_err(repo_to_internal)?;
    if workspace.project_id != project_id {
        return Err(ApiError::NotFound {
            code: "workspace_not_found",
            message: "Workspace not found".into(),
        });
    }
    ensure_workspace_not_busy(&workspace)?;

    let expected_head = workspace.head_commit_oid.clone().ok_or_else(|| {
        ApiError::from(UseCaseError::Internal(
            "workspace missing head_commit_oid".into(),
        ))
    })?;
    let now = Utc::now();
    let mut operation = GitOperation {
        id: ingot_domain::ids::GitOperationId::new(),
        project_id,
        operation_kind: OperationKind::ResetWorkspace,
        entity_type: GitEntityType::Workspace,
        entity_id: workspace.id.to_string(),
        workspace_id: Some(workspace.id),
        ref_name: workspace.workspace_ref.clone(),
        expected_old_oid: workspace.head_commit_oid.clone(),
        new_oid: Some(expected_head.clone()),
        commit_oid: None,
        status: GitOperationStatus::Planned,
        metadata: None,
        created_at: now,
        completed_at: None,
    };
    state
        .db
        .create_git_operation(&operation)
        .await
        .map_err(repo_to_internal)?;
    append_activity(
        &state,
        project_id,
        ActivityEventType::GitOperationPlanned,
        "git_operation",
        operation.id,
        serde_json::json!({ "operation_kind": operation.operation_kind, "entity_id": operation.entity_id }),
    )
    .await?;

    match workspace.kind {
        WorkspaceKind::Authoring | WorkspaceKind::Integration => {
            git(
                FsPath::new(&workspace.path),
                &["reset", "--hard", &expected_head],
            )
            .await
            .map_err(git_to_internal)?;
            git(FsPath::new(&workspace.path), &["clean", "-fd"])
                .await
                .map_err(git_to_internal)?;
            if let Some(workspace_ref) = workspace.workspace_ref.as_deref() {
                ingot_git::commands::git(
                    FsPath::new(&project.path),
                    &["update-ref", workspace_ref, &expected_head],
                )
                .await
                .map_err(git_to_internal)?;
            }
        }
        WorkspaceKind::Review => {
            provision_review_workspace(
                FsPath::new(&project.path),
                FsPath::new(&workspace.path),
                &expected_head,
            )
            .await
            .map_err(workspace_to_api_error)?;
        }
    }

    workspace.status = WorkspaceStatus::Ready;
    workspace.current_job_id = None;
    workspace.updated_at = Utc::now();
    state
        .db
        .update_workspace(&workspace)
        .await
        .map_err(repo_to_internal)?;
    operation.status = GitOperationStatus::Applied;
    operation.completed_at = Some(Utc::now());
    state
        .db
        .update_git_operation(&operation)
        .await
        .map_err(repo_to_internal)?;

    Ok(Json(workspace))
}

async fn abandon_workspace_route(
    State(state): State<AppState>,
    Path((project_id, workspace_id)): Path<(String, String)>,
) -> Result<Json<Workspace>, ApiError> {
    let project_id = parse_id::<ProjectId>(&project_id, "project")?;
    let workspace_id = parse_id::<WorkspaceId>(&workspace_id, "workspace")?;
    state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;
    let _guard = state
        .project_locks
        .acquire_project_mutation(project_id)
        .await;
    let mut workspace = state
        .db
        .get_workspace(workspace_id)
        .await
        .map_err(repo_to_internal)?;
    if workspace.project_id != project_id {
        return Err(ApiError::NotFound {
            code: "workspace_not_found",
            message: "Workspace not found".into(),
        });
    }
    ensure_workspace_not_busy(&workspace)?;
    workspace.status = WorkspaceStatus::Abandoned;
    workspace.current_job_id = None;
    workspace.updated_at = Utc::now();
    state
        .db
        .update_workspace(&workspace)
        .await
        .map_err(repo_to_internal)?;
    Ok(Json(workspace))
}

async fn remove_workspace_route(
    State(state): State<AppState>,
    Path((project_id, workspace_id)): Path<(String, String)>,
) -> Result<Json<Workspace>, ApiError> {
    let project_id = parse_id::<ProjectId>(&project_id, "project")?;
    let workspace_id = parse_id::<WorkspaceId>(&workspace_id, "workspace")?;
    let project = state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;
    let _guard = state
        .project_locks
        .acquire_project_mutation(project_id)
        .await;
    let mut workspace = state
        .db
        .get_workspace(workspace_id)
        .await
        .map_err(repo_to_internal)?;
    if workspace.project_id != project_id {
        return Err(ApiError::NotFound {
            code: "workspace_not_found",
            message: "Workspace not found".into(),
        });
    }
    ensure_workspace_not_busy(&workspace)?;
    workspace.status = WorkspaceStatus::Removing;
    workspace.updated_at = Utc::now();
    state
        .db
        .update_workspace(&workspace)
        .await
        .map_err(repo_to_internal)?;

    if PathBuf::from(&workspace.path).exists() {
        remove_workspace(FsPath::new(&project.path), FsPath::new(&workspace.path))
            .await
            .map_err(workspace_to_api_error)?;
    }
    if let Some(workspace_ref) = workspace.workspace_ref.as_deref()
        && resolve_ref_oid(FsPath::new(&project.path), workspace_ref)
            .await
            .map_err(git_to_internal)?
            .is_some()
    {
        let now = Utc::now();
        let mut operation = GitOperation {
            id: ingot_domain::ids::GitOperationId::new(),
            project_id,
            operation_kind: OperationKind::RemoveWorkspaceRef,
            entity_type: GitEntityType::Workspace,
            entity_id: workspace.id.to_string(),
            workspace_id: Some(workspace.id),
            ref_name: Some(workspace_ref.into()),
            expected_old_oid: workspace.head_commit_oid.clone(),
            new_oid: None,
            commit_oid: None,
            status: GitOperationStatus::Planned,
            metadata: None,
            created_at: now,
            completed_at: None,
        };
        state
            .db
            .create_git_operation(&operation)
            .await
            .map_err(repo_to_internal)?;
        append_activity(
            &state,
            project_id,
            ActivityEventType::GitOperationPlanned,
            "git_operation",
            operation.id,
            serde_json::json!({ "operation_kind": operation.operation_kind, "entity_id": operation.entity_id }),
        )
        .await?;
        delete_ref(FsPath::new(&project.path), workspace_ref)
            .await
            .map_err(git_to_internal)?;
        operation.status = GitOperationStatus::Applied;
        operation.completed_at = Some(Utc::now());
        state
            .db
            .update_git_operation(&operation)
            .await
            .map_err(repo_to_internal)?;
    }

    workspace.status = WorkspaceStatus::Abandoned;
    workspace.current_job_id = None;
    workspace.updated_at = Utc::now();
    state
        .db
        .update_workspace(&workspace)
        .await
        .map_err(repo_to_internal)?;
    Ok(Json(workspace))
}

async fn list_projects(State(state): State<AppState>) -> Result<Json<Vec<Project>>, ApiError> {
    let projects = state.db.list_projects().await.map_err(repo_to_internal)?;
    Ok(Json(projects))
}

async fn create_project(
    State(state): State<AppState>,
    Json(request): Json<CreateProjectRequest>,
) -> Result<(StatusCode, Json<Project>), ApiError> {
    let path = canonicalize_repo_path(&request.path)?;
    let default_branch = resolve_default_branch(&path, request.default_branch.as_deref()).await?;
    let now = Utc::now();
    let project = Project {
        id: ProjectId::new(),
        name: normalize_project_name(request.name.as_deref(), &path)?,
        path: path.display().to_string(),
        default_branch,
        color: normalize_project_color(request.color.as_deref())?,
        created_at: now,
        updated_at: now,
    };

    state
        .db
        .create_project(&project)
        .await
        .map_err(repo_to_project_mutation)?;

    Ok((StatusCode::CREATED, Json(project)))
}

async fn update_project(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
    Json(request): Json<UpdateProjectRequest>,
) -> Result<Json<Project>, ApiError> {
    let project_id = parse_id::<ProjectId>(&project_id, "project")?;
    let existing = state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;
    let existing_name = existing.name.clone();
    let existing_default_branch = existing.default_branch.clone();
    let existing_color = existing.color.clone();
    let path = match request.path.as_deref() {
        Some(path) => canonicalize_repo_path(path)?,
        None => PathBuf::from(&existing.path),
    };

    let project = Project {
        id: existing.id,
        name: match request.name.as_deref() {
            Some(name) => normalize_non_empty("project name", name)?,
            None => existing_name,
        },
        path: path.display().to_string(),
        default_branch: if request.default_branch.is_some() || request.path.is_some() {
            resolve_default_branch(&path, request.default_branch.as_deref()).await?
        } else {
            existing_default_branch
        },
        color: match request.color.as_deref() {
            Some(color) => normalize_project_color(Some(color))?,
            None => existing_color,
        },
        created_at: existing.created_at,
        updated_at: Utc::now(),
    };

    state
        .db
        .update_project(&project)
        .await
        .map_err(repo_to_project_mutation)?;

    Ok(Json(project))
}

async fn delete_project(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let project_id = parse_id::<ProjectId>(&project_id, "project")?;
    state
        .db
        .delete_project(project_id)
        .await
        .map_err(repo_to_project_mutation)?;

    Ok(StatusCode::NO_CONTENT)
}

async fn get_project_config(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> Result<Json<IngotConfig>, ApiError> {
    let project_id = parse_id::<ProjectId>(&project_id, "project")?;
    let project = state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;
    Ok(Json(load_effective_config(Some(&project))?))
}

async fn list_agents(State(state): State<AppState>) -> Result<Json<Vec<Agent>>, ApiError> {
    let agents = state.db.list_agents().await.map_err(repo_to_internal)?;
    Ok(Json(agents))
}

async fn create_agent(
    State(state): State<AppState>,
    Json(request): Json<CreateAgentRequest>,
) -> Result<(StatusCode, Json<Agent>), ApiError> {
    let mut agent = Agent {
        id: AgentId::new(),
        slug: normalize_agent_slug(request.slug.as_deref(), &request.name)?,
        name: normalize_non_empty("agent name", &request.name)?,
        adapter_kind: request.adapter_kind,
        provider: normalize_non_empty("provider", &request.provider)?,
        model: normalize_non_empty("model", &request.model)?,
        cli_path: normalize_non_empty("cli path", &request.cli_path)?,
        capabilities: request
            .capabilities
            .unwrap_or_else(|| default_agent_capabilities(request.adapter_kind)),
        health_check: None,
        status: AgentStatus::Probing,
    };
    probe_and_apply(&mut agent).await;

    state
        .db
        .create_agent(&agent)
        .await
        .map_err(repo_to_agent_mutation)?;

    Ok((StatusCode::CREATED, Json(agent)))
}

async fn update_agent(
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
    Json(request): Json<UpdateAgentRequest>,
) -> Result<Json<Agent>, ApiError> {
    let agent_id = parse_id::<AgentId>(&agent_id, "agent")?;
    let existing = state.db.get_agent(agent_id).await.map_err(repo_to_agent)?;
    let existing_name = existing.name.clone();
    let existing_slug = existing.slug.clone();
    let existing_provider = existing.provider.clone();
    let existing_model = existing.model.clone();
    let existing_cli_path = existing.cli_path.clone();
    let existing_capabilities = existing.capabilities.clone();
    let existing_health_check = existing.health_check.clone();
    let adapter_kind = request.adapter_kind.unwrap_or(existing.adapter_kind);
    let name = match request.name.as_deref() {
        Some(name) => normalize_non_empty("agent name", name)?,
        None => existing_name,
    };
    let mut agent = Agent {
        id: existing.id,
        slug: match request.slug.as_deref() {
            Some(slug) => normalize_agent_slug(Some(slug), &name)?,
            None => existing_slug,
        },
        name,
        adapter_kind,
        provider: match request.provider.as_deref() {
            Some(provider) => normalize_non_empty("provider", provider)?,
            None => existing_provider,
        },
        model: match request.model.as_deref() {
            Some(model) => normalize_non_empty("model", model)?,
            None => existing_model,
        },
        cli_path: match request.cli_path.as_deref() {
            Some(cli_path) => normalize_non_empty("cli path", cli_path)?,
            None => existing_cli_path,
        },
        capabilities: request.capabilities.unwrap_or(existing_capabilities),
        health_check: existing_health_check,
        status: AgentStatus::Probing,
    };
    probe_and_apply(&mut agent).await;

    state
        .db
        .update_agent(&agent)
        .await
        .map_err(repo_to_agent_mutation)?;

    Ok(Json(agent))
}

async fn delete_agent(
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let agent_id = parse_id::<AgentId>(&agent_id, "agent")?;
    state
        .db
        .delete_agent(agent_id)
        .await
        .map_err(repo_to_agent_mutation)?;

    Ok(StatusCode::NO_CONTENT)
}

async fn reprobe_agent(
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
) -> Result<Json<Agent>, ApiError> {
    let agent_id = parse_id::<AgentId>(&agent_id, "agent")?;
    let mut agent = state.db.get_agent(agent_id).await.map_err(repo_to_agent)?;
    probe_and_apply(&mut agent).await;

    state
        .db
        .update_agent(&agent)
        .await
        .map_err(repo_to_agent_mutation)?;

    Ok(Json(agent))
}

async fn create_item(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
    Json(request): Json<CreateItemRequest>,
) -> Result<(StatusCode, Json<ItemDetailResponse>), ApiError> {
    let project_id = parse_id::<ProjectId>(&project_id, "project")?;
    let project = state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;
    let _guard = state
        .project_locks
        .acquire_project_mutation(project_id)
        .await;
    let config = load_effective_config(Some(&project))?;
    let configured_approval_policy = parse_config_approval_policy(&config)?;

    let target_ref = normalize_target_ref(
        request
            .target_ref
            .as_deref()
            .unwrap_or(project.default_branch.as_str()),
    );
    let repo_path = FsPath::new(&project.path);
    let resolved_target_head = resolve_ref_oid(repo_path, &target_ref)
        .await
        .map_err(git_to_internal)?
        .ok_or_else(|| UseCaseError::TargetRefUnresolved(target_ref.clone()))?;

    let seed_commit_oid = if let Some(seed_commit_oid) = request.seed_commit_oid {
        ensure_reachable_seed(repo_path, "seed_commit_oid", &seed_commit_oid).await?;
        seed_commit_oid
    } else {
        resolved_target_head.clone()
    };

    let seed_target_commit_oid = if let Some(seed_target_commit_oid) =
        request.seed_target_commit_oid
    {
        ensure_reachable_seed(repo_path, "seed_target_commit_oid", &seed_target_commit_oid).await?;
        Some(seed_target_commit_oid)
    } else {
        Some(resolved_target_head)
    };

    let (item, revision) = create_manual_item(
        &project,
        CreateItemInput {
            classification: request.classification.unwrap_or(Classification::Change),
            priority: request.priority.unwrap_or(Priority::Major),
            labels: request.labels.unwrap_or_default(),
            operator_notes: request.operator_notes,
            title: request.title,
            description: request.description,
            acceptance_criteria: request.acceptance_criteria,
            target_ref,
            approval_policy: request
                .approval_policy
                .unwrap_or(configured_approval_policy),
            candidate_rework_budget: config.defaults.candidate_rework_budget,
            integration_rework_budget: config.defaults.integration_rework_budget,
            seed_commit_oid,
            seed_target_commit_oid,
        },
        Utc::now(),
    );

    state
        .db
        .create_item_with_revision(&item, &revision)
        .await
        .map_err(repo_to_internal)?;
    append_activity(
        &state,
        project_id,
        ActivityEventType::ItemCreated,
        "item",
        item.id,
        serde_json::json!({ "revision_id": revision.id }),
    )
    .await?;

    let detail = load_item_detail(&state, project_id, item.id).await?;
    Ok((StatusCode::CREATED, Json(detail)))
}

async fn list_items(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> Result<Json<Vec<ItemSummaryResponse>>, ApiError> {
    let project_id = parse_id::<ProjectId>(&project_id, "project")?;
    let project = state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;
    let items = state
        .db
        .list_items_by_project(project_id)
        .await
        .map_err(repo_to_internal)?;
    let evaluator = Evaluator::new();
    let mut summaries = Vec::with_capacity(items.len());

    for item in items {
        let current_revision = state
            .db
            .get_revision(item.current_revision_id)
            .await
            .map_err(repo_to_internal)?;
        let jobs = state
            .db
            .list_jobs_by_item(item.id)
            .await
            .map_err(repo_to_internal)?;
        let findings = state
            .db
            .list_findings_by_item(item.id)
            .await
            .map_err(repo_to_internal)?;
        let convergences = state
            .db
            .list_convergences_by_item(item.id)
            .await
            .map_err(repo_to_internal)?;
        let convergences = hydrate_convergence_validity(&project, convergences).await?;
        let evaluation =
            evaluator.evaluate(&item, &current_revision, &jobs, &findings, &convergences);

        let title = current_revision.title.clone();
        summaries.push(ItemSummaryResponse {
            item,
            title,
            evaluation,
        });
    }

    Ok(Json(summaries))
}

async fn list_project_jobs(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> Result<Json<Vec<Job>>, ApiError> {
    let project_id = parse_id::<ProjectId>(&project_id, "project")?;
    state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;
    let jobs = state
        .db
        .list_jobs_by_project(project_id)
        .await
        .map_err(repo_to_internal)?;
    Ok(Json(jobs))
}

async fn update_item(
    State(state): State<AppState>,
    Path((project_id, item_id)): Path<(String, String)>,
    Json(request): Json<UpdateItemRequest>,
) -> Result<Json<ItemDetailResponse>, ApiError> {
    let project_id = parse_id::<ProjectId>(&project_id, "project")?;
    let item_id = parse_id::<ItemId>(&item_id, "item")?;
    let _project = state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;
    let _guard = state
        .project_locks
        .acquire_project_mutation(project_id)
        .await;
    let mut item = state.db.get_item(item_id).await.map_err(repo_to_item)?;
    if item.project_id != project_id {
        return Err(UseCaseError::ItemNotFound.into());
    }
    if let Some(classification) = request.classification {
        item.classification = classification;
    }
    if let Some(priority) = request.priority {
        item.priority = priority;
    }
    if let Some(labels) = request.labels {
        item.labels = labels;
    }
    if request.operator_notes.is_some() {
        item.operator_notes = request.operator_notes;
    }
    item.updated_at = Utc::now();
    state
        .db
        .update_item(&item)
        .await
        .map_err(repo_to_internal)?;
    append_activity(
        &state,
        project_id,
        ActivityEventType::ItemUpdated,
        "item",
        item.id,
        serde_json::json!({}),
    )
    .await?;
    let detail = load_item_detail(&state, project_id, item.id).await?;
    Ok(Json(detail))
}

async fn get_item(
    State(state): State<AppState>,
    Path((project_id, item_id)): Path<(String, String)>,
) -> Result<Json<ItemDetailResponse>, ApiError> {
    let project_id = parse_id::<ProjectId>(&project_id, "project")?;
    let item_id = parse_id::<ItemId>(&item_id, "item")?;
    state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;
    let response = load_item_detail(&state, project_id, item_id).await?;
    Ok(Json(response))
}

async fn revise_item(
    State(state): State<AppState>,
    Path((project_id, item_id)): Path<(String, String)>,
    maybe_request: Option<Json<ReviseItemRequest>>,
) -> Result<Json<ItemDetailResponse>, ApiError> {
    let request: ReviseItemRequest = maybe_request
        .map(|Json(request)| request)
        .unwrap_or_default()
        .into();
    let project_id = parse_id::<ProjectId>(&project_id, "project")?;
    let item_id = parse_id::<ItemId>(&item_id, "item")?;
    let project = state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;
    let _guard = state
        .project_locks
        .acquire_project_mutation(project_id)
        .await;
    let mut item = state.db.get_item(item_id).await.map_err(repo_to_item)?;
    if item.project_id != project_id {
        return Err(UseCaseError::ItemNotFound.into());
    }
    ensure_item_open_idle(&item)?;
    let current_revision = state
        .db
        .get_revision(item.current_revision_id)
        .await
        .map_err(repo_to_internal)?;
    let jobs = state
        .db
        .list_jobs_by_item(item.id)
        .await
        .map_err(repo_to_internal)?;
    let next_revision =
        build_superseding_revision(&project, &item, &current_revision, &jobs, request).await?;
    state
        .db
        .create_revision(&next_revision)
        .await
        .map_err(repo_to_internal)?;
    item.current_revision_id = next_revision.id;
    let cleared_escalation =
        item.escalation_state == ingot_domain::item::EscalationState::OperatorRequired;
    item.approval_state = approval_state_for_policy(next_revision.approval_policy);
    item.escalation_state = ingot_domain::item::EscalationState::None;
    item.escalation_reason = None;
    item.updated_at = Utc::now();
    state
        .db
        .update_item(&item)
        .await
        .map_err(repo_to_internal)?;
    append_activity(
        &state,
        project_id,
        ActivityEventType::ItemRevisionCreated,
        "item",
        item.id,
        serde_json::json!({ "revision_id": next_revision.id, "kind": "revise" }),
    )
    .await?;
    if cleared_escalation {
        append_activity(
            &state,
            project_id,
            ActivityEventType::ItemEscalationCleared,
            "item",
            item.id,
            serde_json::json!({ "reason": "revise" }),
        )
        .await?;
    }
    let detail = load_item_detail(&state, project_id, item.id).await?;
    Ok(Json(detail))
}

async fn defer_item(
    State(state): State<AppState>,
    Path((project_id, item_id)): Path<(String, String)>,
) -> Result<Json<ItemDetailResponse>, ApiError> {
    let project_id = parse_id::<ProjectId>(&project_id, "project")?;
    let item_id = parse_id::<ItemId>(&item_id, "item")?;
    state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;
    let _guard = state
        .project_locks
        .acquire_project_mutation(project_id)
        .await;
    let mut item = state.db.get_item(item_id).await.map_err(repo_to_item)?;
    if item.project_id != project_id {
        return Err(UseCaseError::ItemNotFound.into());
    }
    ensure_item_open_idle(&item)?;
    if item.approval_state == ApprovalState::Pending {
        return Err(ApiError::Conflict {
            code: "item_pending_approval",
            message: "Pending approval items cannot be deferred".into(),
        });
    }
    item.parking_state = ingot_domain::item::ParkingState::Deferred;
    item.updated_at = Utc::now();
    state
        .db
        .update_item(&item)
        .await
        .map_err(repo_to_internal)?;
    append_activity(
        &state,
        project_id,
        ActivityEventType::ItemDeferred,
        "item",
        item.id,
        serde_json::json!({}),
    )
    .await?;
    let detail = load_item_detail(&state, project_id, item.id).await?;
    Ok(Json(detail))
}

async fn resume_item(
    State(state): State<AppState>,
    Path((project_id, item_id)): Path<(String, String)>,
) -> Result<Json<ItemDetailResponse>, ApiError> {
    let project_id = parse_id::<ProjectId>(&project_id, "project")?;
    let item_id = parse_id::<ItemId>(&item_id, "item")?;
    state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;
    let _guard = state
        .project_locks
        .acquire_project_mutation(project_id)
        .await;
    let mut item = state.db.get_item(item_id).await.map_err(repo_to_item)?;
    if item.project_id != project_id {
        return Err(UseCaseError::ItemNotFound.into());
    }
    if item.parking_state != ingot_domain::item::ParkingState::Deferred {
        return Err(ApiError::Conflict {
            code: "item_not_deferred",
            message: "Item is not deferred".into(),
        });
    }
    item.parking_state = ingot_domain::item::ParkingState::Active;
    item.updated_at = Utc::now();
    state
        .db
        .update_item(&item)
        .await
        .map_err(repo_to_internal)?;
    append_activity(
        &state,
        project_id,
        ActivityEventType::ItemResumed,
        "item",
        item.id,
        serde_json::json!({}),
    )
    .await?;
    let detail = load_item_detail(&state, project_id, item.id).await?;
    Ok(Json(detail))
}

async fn dismiss_item(
    State(state): State<AppState>,
    Path((project_id, item_id)): Path<(String, String)>,
) -> Result<Json<ItemDetailResponse>, ApiError> {
    finish_item_manually(
        state,
        parse_id::<ProjectId>(&project_id, "project")?,
        parse_id::<ItemId>(&item_id, "item")?,
        DoneReason::Dismissed,
        ActivityEventType::ItemDismissed,
    )
    .await
}

async fn invalidate_item(
    State(state): State<AppState>,
    Path((project_id, item_id)): Path<(String, String)>,
) -> Result<Json<ItemDetailResponse>, ApiError> {
    finish_item_manually(
        state,
        parse_id::<ProjectId>(&project_id, "project")?,
        parse_id::<ItemId>(&item_id, "item")?,
        DoneReason::Invalidated,
        ActivityEventType::ItemInvalidated,
    )
    .await
}

async fn reopen_item(
    State(state): State<AppState>,
    Path((project_id, item_id)): Path<(String, String)>,
    maybe_request: Option<Json<ReviseItemRequest>>,
) -> Result<Json<ItemDetailResponse>, ApiError> {
    let request: ReviseItemRequest = maybe_request
        .map(|Json(request)| request)
        .unwrap_or_default()
        .into();
    let project_id = parse_id::<ProjectId>(&project_id, "project")?;
    let item_id = parse_id::<ItemId>(&item_id, "item")?;
    let project = state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;
    let _guard = state
        .project_locks
        .acquire_project_mutation(project_id)
        .await;
    let mut item = state.db.get_item(item_id).await.map_err(repo_to_item)?;
    if item.project_id != project_id {
        return Err(UseCaseError::ItemNotFound.into());
    }
    match item.done_reason {
        Some(DoneReason::Dismissed | DoneReason::Invalidated) => {}
        Some(DoneReason::Completed) => return Err(UseCaseError::CompletedItemCannotReopen.into()),
        None => {
            return Err(ApiError::Conflict {
                code: "item_not_reopenable",
                message: "Only dismissed or invalidated items can be reopened".into(),
            });
        }
    }
    let current_revision = state
        .db
        .get_revision(item.current_revision_id)
        .await
        .map_err(repo_to_internal)?;
    let jobs = state
        .db
        .list_jobs_by_item(item.id)
        .await
        .map_err(repo_to_internal)?;
    let next_revision =
        build_superseding_revision(&project, &item, &current_revision, &jobs, request).await?;
    state
        .db
        .create_revision(&next_revision)
        .await
        .map_err(repo_to_internal)?;
    let cleared_escalation =
        item.escalation_state == ingot_domain::item::EscalationState::OperatorRequired;
    item.current_revision_id = next_revision.id;
    item.lifecycle_state = LifecycleState::Open;
    item.parking_state = ingot_domain::item::ParkingState::Active;
    item.done_reason = None;
    item.resolution_source = None;
    item.closed_at = None;
    item.approval_state = approval_state_for_policy(next_revision.approval_policy);
    item.escalation_state = ingot_domain::item::EscalationState::None;
    item.escalation_reason = None;
    item.updated_at = Utc::now();
    state
        .db
        .update_item(&item)
        .await
        .map_err(repo_to_internal)?;
    append_activity(
        &state,
        project_id,
        ActivityEventType::ItemReopened,
        "item",
        item.id,
        serde_json::json!({ "revision_id": next_revision.id }),
    )
    .await?;
    if cleared_escalation {
        append_activity(
            &state,
            project_id,
            ActivityEventType::ItemEscalationCleared,
            "item",
            item.id,
            serde_json::json!({ "reason": "reopen" }),
        )
        .await?;
    }
    let detail = load_item_detail(&state, project_id, item.id).await?;
    Ok(Json(detail))
}

async fn list_item_findings(
    State(state): State<AppState>,
    Path((project_id, item_id)): Path<(String, String)>,
) -> Result<Json<Vec<Finding>>, ApiError> {
    let project_id = parse_id::<ProjectId>(&project_id, "project")?;
    let item_id = parse_id::<ItemId>(&item_id, "item")?;
    state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;
    let item = state.db.get_item(item_id).await.map_err(repo_to_item)?;
    if item.project_id != project_id {
        return Err(UseCaseError::ItemNotFound.into());
    }

    let findings = state
        .db
        .list_findings_by_item(item_id)
        .await
        .map_err(repo_to_internal)?;

    Ok(Json(findings))
}

async fn dispatch_item_job(
    State(state): State<AppState>,
    Path((project_id, item_id)): Path<(String, String)>,
    maybe_request: Option<Json<DispatchJobRequest>>,
) -> Result<(StatusCode, Json<Job>), ApiError> {
    let project_id = parse_id::<ProjectId>(&project_id, "project")?;
    let item_id = parse_id::<ItemId>(&item_id, "item")?;
    let project = state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;
    let _guard = state
        .project_locks
        .acquire_project_mutation(project_id)
        .await;

    let item = state.db.get_item(item_id).await.map_err(repo_to_item)?;
    if item.project_id != project_id {
        return Err(UseCaseError::ItemNotFound.into());
    }

    let current_revision = state
        .db
        .get_revision(item.current_revision_id)
        .await
        .map_err(repo_to_internal)?;
    let jobs = state
        .db
        .list_jobs_by_item(item.id)
        .await
        .map_err(repo_to_internal)?;
    let findings = state
        .db
        .list_findings_by_item(item.id)
        .await
        .map_err(repo_to_internal)?;
    let convergences = state
        .db
        .list_convergences_by_item(item.id)
        .await
        .map_err(repo_to_internal)?;
    let convergences = hydrate_convergence_validity(&project, convergences).await?;
    let command = DispatchJobCommand {
        step_id: maybe_request.and_then(|Json(request)| request.step_id),
    };
    let mut job = dispatch_job(
        &item,
        &current_revision,
        &jobs,
        &findings,
        &convergences,
        command,
    )?;

    state.db.create_job(&job).await.map_err(repo_to_internal)?;

    if job.workspace_kind == WorkspaceKind::Authoring {
        let workspace =
            ensure_authoring_workspace(&state, &project, &current_revision, &job).await?;
        job.workspace_id = Some(workspace.id);
        state.db.update_job(&job).await.map_err(repo_to_internal)?;
    }
    append_activity(
        &state,
        project_id,
        ActivityEventType::JobDispatched,
        "job",
        job.id,
        serde_json::json!({ "item_id": item.id, "step_id": job.step_id }),
    )
    .await?;

    Ok((StatusCode::CREATED, Json(job)))
}

async fn retry_item_job(
    State(state): State<AppState>,
    Path((project_id, item_id, job_id)): Path<(String, String, String)>,
) -> Result<(StatusCode, Json<Job>), ApiError> {
    let project_id = parse_id::<ProjectId>(&project_id, "project")?;
    let item_id = parse_id::<ItemId>(&item_id, "item")?;
    let job_id = parse_id::<JobId>(&job_id, "job")?;
    let project = state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;
    let _guard = state
        .project_locks
        .acquire_project_mutation(project_id)
        .await;

    let item = state.db.get_item(item_id).await.map_err(repo_to_item)?;
    if item.project_id != project_id {
        return Err(UseCaseError::ItemNotFound.into());
    }
    let current_revision = state
        .db
        .get_revision(item.current_revision_id)
        .await
        .map_err(repo_to_internal)?;
    let jobs = state
        .db
        .list_jobs_by_item(item.id)
        .await
        .map_err(repo_to_internal)?;
    let findings = state
        .db
        .list_findings_by_item(item.id)
        .await
        .map_err(repo_to_internal)?;
    let previous_job = jobs
        .iter()
        .find(|job| job.id == job_id)
        .cloned()
        .ok_or_else(|| ApiError::NotFound {
            code: "job_not_found",
            message: "Job not found".into(),
        })?;
    let convergences = state
        .db
        .list_convergences_by_item(item.id)
        .await
        .map_err(repo_to_internal)?;
    let convergences = hydrate_convergence_validity(&project, convergences).await?;

    let mut job = retry_job(
        &item,
        &current_revision,
        &jobs,
        &findings,
        &convergences,
        &previous_job,
    )?;
    state.db.create_job(&job).await.map_err(repo_to_internal)?;
    if job.workspace_kind == WorkspaceKind::Authoring {
        let workspace =
            ensure_authoring_workspace(&state, &project, &current_revision, &job).await?;
        job.workspace_id = Some(workspace.id);
        state.db.update_job(&job).await.map_err(repo_to_internal)?;
    }
    append_activity(
        &state,
        project_id,
        ActivityEventType::JobDispatched,
        "job",
        job.id,
        serde_json::json!({
            "item_id": item.id,
            "step_id": job.step_id,
            "supersedes_job_id": previous_job.id,
            "retry_no": job.retry_no
        }),
    )
    .await?;

    Ok((StatusCode::CREATED, Json(job)))
}

async fn cancel_item_job(
    State(state): State<AppState>,
    Path((project_id, item_id, job_id)): Path<(String, String, String)>,
) -> Result<Json<()>, ApiError> {
    let project_id = parse_id::<ProjectId>(&project_id, "project")?;
    let item_id = parse_id::<ItemId>(&item_id, "item")?;
    let job_id = parse_id::<JobId>(&job_id, "job")?;
    let _project = state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;
    let _guard = state
        .project_locks
        .acquire_project_mutation(project_id)
        .await;

    let item = state.db.get_item(item_id).await.map_err(repo_to_item)?;
    if item.project_id != project_id {
        return Err(UseCaseError::ItemNotFound.into());
    }
    let job = state.db.get_job(job_id).await.map_err(repo_to_internal)?;
    if job.item_id != item.id {
        return Err(ApiError::NotFound {
            code: "job_not_found",
            message: "Job not found".into(),
        });
    }
    if !job.status.is_active() {
        return Err(UseCaseError::JobNotActive.into());
    }
    if job.item_revision_id != item.current_revision_id {
        return Err(UseCaseError::ProtocolViolation(
            "job cancellation does not match the current item revision".into(),
        )
        .into());
    }

    state
        .db
        .finish_job_non_success(FinishJobNonSuccessParams {
            job_id: job.id,
            item_id: item.id,
            expected_item_revision_id: job.item_revision_id,
            status: JobStatus::Cancelled,
            outcome_class: Some(OutcomeClass::Cancelled),
            error_code: Some("operator_cancelled"),
            error_message: None,
            escalation_reason: None,
        })
        .await
        .map_err(repo_to_job_failure)?;

    if let Some(workspace_id) = job.workspace_id {
        let mut workspace = state
            .db
            .get_workspace(workspace_id)
            .await
            .map_err(repo_to_internal)?;
        workspace.current_job_id = None;
        if workspace.status == ingot_domain::workspace::WorkspaceStatus::Busy {
            workspace.status = ingot_domain::workspace::WorkspaceStatus::Ready;
        }
        workspace.updated_at = Utc::now();
        state
            .db
            .update_workspace(&workspace)
            .await
            .map_err(repo_to_internal)?;
    }

    refresh_revision_context_for_job(&state, job.id).await?;
    append_activity(
        &state,
        project_id,
        ActivityEventType::JobCancelled,
        "job",
        job.id,
        serde_json::json!({ "item_id": item.id }),
    )
    .await?;

    Ok(Json(()))
}

async fn assign_job(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    Json(request): Json<AssignJobRequest>,
) -> Result<Json<Job>, ApiError> {
    let job_id = parse_id::<JobId>(&job_id, "job")?;
    let agent_id = parse_id::<AgentId>(&request.agent_id, "agent")?;
    let mut job = state.db.get_job(job_id).await.map_err(repo_to_internal)?;
    if job.status == JobStatus::Assigned {
        return Ok(Json(job));
    }
    if job.status != JobStatus::Queued {
        return Err(ApiError::Conflict {
            code: "job_not_assignable",
            message: "Only queued jobs can be assigned".into(),
        });
    }
    if job.workspace_kind != WorkspaceKind::Authoring {
        return Err(ApiError::BadRequest {
            code: "unsupported_workspace_kind",
            message: "This milestone only provisions authoring workspaces".into(),
        });
    }

    let agent = state.db.get_agent(agent_id).await.map_err(repo_to_agent)?;
    if agent.status != AgentStatus::Available {
        return Err(ApiError::Conflict {
            code: "agent_unavailable",
            message: "Agent is not available".into(),
        });
    }

    let item = state.db.get_item(job.item_id).await.map_err(repo_to_item)?;
    if item.current_revision_id != job.item_revision_id {
        return Err(UseCaseError::ProtocolViolation(
            "job assignment does not match the current item revision".into(),
        )
        .into());
    }
    let revision = state
        .db
        .get_revision(job.item_revision_id)
        .await
        .map_err(repo_to_internal)?;
    let project = state
        .db
        .get_project(job.project_id)
        .await
        .map_err(repo_to_project)?;
    let _guard = state
        .project_locks
        .acquire_project_mutation(project.id)
        .await;
    let workspace = ensure_authoring_workspace(&state, &project, &revision, &job).await?;

    job.status = JobStatus::Assigned;
    job.workspace_id = Some(workspace.id);
    job.agent_id = Some(agent.id);
    state.db.update_job(&job).await.map_err(repo_to_internal)?;

    Ok(Json(job))
}

async fn start_job(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    Json(request): Json<StartJobRequest>,
) -> Result<Json<Job>, ApiError> {
    let job_id = parse_id::<JobId>(&job_id, "job")?;
    let mut job = state.db.get_job(job_id).await.map_err(repo_to_internal)?;
    if job.status == JobStatus::Running {
        return Ok(Json(job));
    }
    let item = state.db.get_item(job.item_id).await.map_err(repo_to_item)?;
    let _guard = state
        .project_locks
        .acquire_project_mutation(job.project_id)
        .await;
    let lease_expires_at =
        Utc::now() + chrono::Duration::seconds(request.lease_duration_seconds.unwrap_or(1800));
    state
        .db
        .start_job_execution(StartJobExecutionParams {
            job_id: job.id,
            item_id: item.id,
            expected_item_revision_id: job.item_revision_id,
            workspace_id: job.workspace_id,
            agent_id: job.agent_id,
            lease_owner_id: &request.lease_owner_id,
            process_pid: request.process_pid,
            lease_expires_at,
        })
        .await
        .map_err(repo_to_job_failure)?;
    job = state.db.get_job(job.id).await.map_err(repo_to_internal)?;
    Ok(Json(job))
}

async fn heartbeat_job(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    Json(request): Json<HeartbeatJobRequest>,
) -> Result<Json<Job>, ApiError> {
    let job_id = parse_id::<JobId>(&job_id, "job")?;
    let job = state.db.get_job(job_id).await.map_err(repo_to_internal)?;
    let item = state.db.get_item(job.item_id).await.map_err(repo_to_item)?;
    let _guard = state
        .project_locks
        .acquire_project_mutation(job.project_id)
        .await;
    let lease_expires_at =
        Utc::now() + chrono::Duration::seconds(request.lease_duration_seconds.unwrap_or(1800));
    state
        .db
        .heartbeat_job_execution(
            job.id,
            item.id,
            job.item_revision_id,
            &request.lease_owner_id,
            lease_expires_at,
        )
        .await
        .map_err(repo_to_job_failure)?;
    let job = state.db.get_job(job.id).await.map_err(repo_to_internal)?;
    Ok(Json(job))
}

async fn get_job_logs(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> Result<Json<JobLogsResponse>, ApiError> {
    let job_id = parse_id::<JobId>(&job_id, "job")?;
    state.db.get_job(job_id).await.map_err(repo_to_internal)?;
    let logs_dir = logs_root().join(job_id.to_string());

    let prompt = read_optional_text(logs_dir.join("prompt.txt")).await?;
    let stdout = read_optional_text(logs_dir.join("stdout.log")).await?;
    let stderr = read_optional_text(logs_dir.join("stderr.log")).await?;
    let result = read_optional_json(logs_dir.join("result.json")).await?;

    Ok(Json(JobLogsResponse {
        prompt,
        stdout,
        stderr,
        result,
    }))
}

async fn prepare_item_convergence(
    State(state): State<AppState>,
    Path((project_id, item_id)): Path<(String, String)>,
) -> Result<(StatusCode, Json<PrepareConvergenceResponse>), ApiError> {
    let project_id = parse_id::<ProjectId>(&project_id, "project")?;
    let item_id = parse_id::<ItemId>(&item_id, "item")?;
    let project = state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;
    let _guard = state
        .project_locks
        .acquire_project_mutation(project_id)
        .await;

    let item = state.db.get_item(item_id).await.map_err(repo_to_item)?;
    if item.project_id != project_id {
        return Err(UseCaseError::ItemNotFound.into());
    }

    let current_revision = state
        .db
        .get_revision(item.current_revision_id)
        .await
        .map_err(repo_to_internal)?;
    let jobs = state
        .db
        .list_jobs_by_item(item.id)
        .await
        .map_err(repo_to_internal)?;
    let findings = state
        .db
        .list_findings_by_item(item.id)
        .await
        .map_err(repo_to_internal)?;
    let convergences = state
        .db
        .list_convergences_by_item(item.id)
        .await
        .map_err(repo_to_internal)?;
    let convergences = hydrate_convergence_validity(&project, convergences).await?;
    let evaluation =
        Evaluator::new().evaluate(&item, &current_revision, &jobs, &findings, &convergences);

    if evaluation.next_recommended_action != "prepare_convergence" {
        return Err(ApiError::Conflict {
            code: "convergence_not_preparable",
            message: "Convergence cannot be prepared in the current item state".into(),
        });
    }

    let source_workspace = state
        .db
        .find_authoring_workspace_for_revision(current_revision.id)
        .await
        .map_err(repo_to_internal)?
        .ok_or_else(|| ApiError::Conflict {
            code: "authoring_workspace_missing",
            message: "Authoring workspace is required before preparing convergence".into(),
        })?;

    let source_head_commit_oid = current_authoring_head_for_revision(&jobs, &current_revision);
    let prepared = prepare_convergence_workspace(
        &state,
        &project,
        &item,
        &current_revision,
        &source_workspace,
        &source_head_commit_oid,
    )
    .await?;
    let all_convergences = {
        let mut all = convergences.clone();
        all.push(prepared.clone());
        all
    };
    let mut validation_job = dispatch_job(
        &item,
        &current_revision,
        &jobs,
        &findings,
        &all_convergences,
        DispatchJobCommand {
            step_id: Some("validate_integrated".into()),
        },
    )?;
    validation_job.workspace_id = prepared.integration_workspace_id;
    state
        .db
        .create_job(&validation_job)
        .await
        .map_err(repo_to_internal)?;
    append_activity(
        &state,
        project_id,
        ActivityEventType::ConvergencePrepared,
        "convergence",
        prepared.id,
        serde_json::json!({ "item_id": item.id, "validation_job_id": validation_job.id }),
    )
    .await?;
    append_activity(
        &state,
        project_id,
        ActivityEventType::JobDispatched,
        "job",
        validation_job.id,
        serde_json::json!({ "item_id": item.id, "step_id": validation_job.step_id }),
    )
    .await?;
    refresh_revision_context_for_job_like(&state, &item, &current_revision, project.path.as_str())
        .await?;

    Ok((
        StatusCode::CREATED,
        Json(PrepareConvergenceResponse {
            convergence: convergence_response(prepared),
            validation_job,
        }),
    ))
}

async fn approve_item(
    State(state): State<AppState>,
    Path((project_id, item_id)): Path<(String, String)>,
) -> Result<Json<ItemDetailResponse>, ApiError> {
    let project_id = parse_id::<ProjectId>(&project_id, "project")?;
    let item_id = parse_id::<ItemId>(&item_id, "item")?;
    let project = state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;
    let _guard = state
        .project_locks
        .acquire_project_mutation(project_id)
        .await;

    let mut item = state.db.get_item(item_id).await.map_err(repo_to_item)?;
    if item.project_id != project_id {
        return Err(UseCaseError::ItemNotFound.into());
    }
    if item.approval_state != ApprovalState::Pending {
        return Err(UseCaseError::ApprovalNotPending.into());
    }

    let current_revision = state
        .db
        .get_revision(item.current_revision_id)
        .await
        .map_err(repo_to_internal)?;
    let jobs = state
        .db
        .list_jobs_by_item(item.id)
        .await
        .map_err(repo_to_internal)?;
    if jobs
        .iter()
        .any(|job| job.item_revision_id == current_revision.id && job.status.is_active())
    {
        return Err(UseCaseError::ActiveJobExists.into());
    }
    let convergences = state
        .db
        .list_convergences_by_item(item.id)
        .await
        .map_err(repo_to_internal)?;
    if convergences.iter().any(|convergence| {
        convergence.item_revision_id == current_revision.id
            && matches!(
                convergence.status,
                ingot_domain::convergence::ConvergenceStatus::Queued
                    | ingot_domain::convergence::ConvergenceStatus::Running
            )
    }) {
        return Err(UseCaseError::ActiveConvergenceExists.into());
    }
    let mut convergence = convergences
        .into_iter()
        .filter(|convergence| convergence.item_revision_id == current_revision.id)
        .find(|convergence| {
            convergence.status == ingot_domain::convergence::ConvergenceStatus::Prepared
        })
        .ok_or(UseCaseError::PreparedConvergenceMissing)?;
    convergence.target_head_valid =
        compute_target_head_valid(FsPath::new(&project.path), &convergence).await?;
    if convergence.target_head_valid == Some(false) {
        return Err(UseCaseError::PreparedConvergenceStale.into());
    }

    let prepared_commit_oid = convergence
        .prepared_commit_oid
        .clone()
        .ok_or(UseCaseError::PreparedConvergenceMissing)?;
    let input_target_commit_oid = convergence
        .input_target_commit_oid
        .clone()
        .ok_or(UseCaseError::PreparedConvergenceMissing)?;

    let mut operation = GitOperation {
        id: ingot_domain::ids::GitOperationId::new(),
        project_id: project.id,
        operation_kind: OperationKind::FinalizeTargetRef,
        entity_type: GitEntityType::Convergence,
        entity_id: convergence.id.to_string(),
        workspace_id: convergence.integration_workspace_id,
        ref_name: Some(convergence.target_ref.clone()),
        expected_old_oid: Some(input_target_commit_oid.clone()),
        new_oid: Some(prepared_commit_oid.clone()),
        commit_oid: Some(prepared_commit_oid.clone()),
        status: GitOperationStatus::Planned,
        metadata: None,
        created_at: Utc::now(),
        completed_at: None,
    };
    state
        .db
        .create_git_operation(&operation)
        .await
        .map_err(repo_to_internal)?;
    append_activity(
        &state,
        project_id,
        ActivityEventType::GitOperationPlanned,
        "git_operation",
        operation.id,
        serde_json::json!({ "operation_kind": operation.operation_kind, "entity_id": operation.entity_id }),
    )
    .await?;

    compare_and_swap_ref(
        FsPath::new(&project.path),
        &convergence.target_ref,
        &prepared_commit_oid,
        &input_target_commit_oid,
    )
    .await
    .map_err(git_to_internal)?;

    operation.status = GitOperationStatus::Applied;
    operation.completed_at = Some(Utc::now());
    state
        .db
        .update_git_operation(&operation)
        .await
        .map_err(repo_to_internal)?;

    convergence.status = ingot_domain::convergence::ConvergenceStatus::Finalized;
    convergence.final_target_commit_oid = Some(prepared_commit_oid);
    convergence.completed_at = Some(Utc::now());
    state
        .db
        .update_convergence(&convergence)
        .await
        .map_err(repo_to_internal)?;

    item.approval_state = ApprovalState::Approved;
    item.lifecycle_state = LifecycleState::Done;
    item.done_reason = Some(DoneReason::Completed);
    item.resolution_source = Some(ResolutionSource::ApprovalCommand);
    item.closed_at = Some(Utc::now());
    item.updated_at = Utc::now();
    state
        .db
        .update_item(&item)
        .await
        .map_err(repo_to_internal)?;
    append_activity(
        &state,
        project_id,
        ActivityEventType::ApprovalApproved,
        "item",
        item.id,
        serde_json::json!({ "convergence_id": convergence.id }),
    )
    .await?;
    append_activity(
        &state,
        project_id,
        ActivityEventType::ConvergenceFinalized,
        "convergence",
        convergence.id,
        serde_json::json!({ "item_id": item.id }),
    )
    .await?;

    let detail = load_item_detail(&state, project_id, item.id).await?;
    Ok(Json(detail))
}

async fn reject_item_approval(
    State(state): State<AppState>,
    Path((project_id, item_id)): Path<(String, String)>,
    maybe_request: Option<Json<RejectApprovalRequest>>,
) -> Result<Json<ItemDetailResponse>, ApiError> {
    let project_id = parse_id::<ProjectId>(&project_id, "project")?;
    let item_id = parse_id::<ItemId>(&item_id, "item")?;
    let project = state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;
    let _guard = state
        .project_locks
        .acquire_project_mutation(project_id)
        .await;

    let mut item = state.db.get_item(item_id).await.map_err(repo_to_item)?;
    if item.project_id != project_id {
        return Err(UseCaseError::ItemNotFound.into());
    }
    if item.approval_state != ApprovalState::Pending {
        return Err(UseCaseError::ApprovalNotPending.into());
    }

    let current_revision = state
        .db
        .get_revision(item.current_revision_id)
        .await
        .map_err(repo_to_internal)?;
    let jobs = state
        .db
        .list_jobs_by_item(item.id)
        .await
        .map_err(repo_to_internal)?;
    if jobs
        .iter()
        .any(|job| job.item_revision_id == current_revision.id && job.status.is_active())
    {
        return Err(UseCaseError::ActiveJobExists.into());
    }
    let mut convergences = state
        .db
        .list_convergences_by_item(item.id)
        .await
        .map_err(repo_to_internal)?;
    if convergences.iter().any(|convergence| {
        convergence.item_revision_id == current_revision.id
            && matches!(
                convergence.status,
                ingot_domain::convergence::ConvergenceStatus::Queued
                    | ingot_domain::convergence::ConvergenceStatus::Running
            )
    }) {
        return Err(UseCaseError::ActiveConvergenceExists.into());
    }
    let mut prepared_convergence = convergences
        .iter_mut()
        .find(|convergence| {
            convergence.item_revision_id == current_revision.id
                && convergence.status == ingot_domain::convergence::ConvergenceStatus::Prepared
        })
        .ok_or(UseCaseError::PreparedConvergenceMissing)?
        .clone();

    prepared_convergence.status = ingot_domain::convergence::ConvergenceStatus::Cancelled;
    prepared_convergence.completed_at = Some(Utc::now());
    state
        .db
        .update_convergence(&prepared_convergence)
        .await
        .map_err(repo_to_internal)?;

    if let Some(workspace_id) = prepared_convergence.integration_workspace_id {
        let workspace = state
            .db
            .get_workspace(workspace_id)
            .await
            .map_err(repo_to_internal)?;
        if PathBuf::from(&workspace.path).exists() {
            let _ = ingot_workspace::remove_workspace(
                FsPath::new(&project.path),
                FsPath::new(&workspace.path),
            )
            .await;
        }
        let mut abandoned_workspace = workspace;
        abandoned_workspace.status = ingot_domain::workspace::WorkspaceStatus::Abandoned;
        abandoned_workspace.current_job_id = None;
        abandoned_workspace.updated_at = Utc::now();
        state
            .db
            .update_workspace(&abandoned_workspace)
            .await
            .map_err(repo_to_internal)?;
    }

    let request: ReviseItemRequest = maybe_request
        .map(|Json(request)| request)
        .unwrap_or_default()
        .into();
    let next_revision =
        build_superseding_revision(&project, &item, &current_revision, &jobs, request).await?;
    state
        .db
        .create_revision(&next_revision)
        .await
        .map_err(repo_to_internal)?;

    let cleared_escalation =
        item.escalation_state == ingot_domain::item::EscalationState::OperatorRequired;
    item.current_revision_id = next_revision.id;
    item.approval_state = approval_state_for_policy(next_revision.approval_policy);
    item.lifecycle_state = LifecycleState::Open;
    item.parking_state = ingot_domain::item::ParkingState::Active;
    item.done_reason = None;
    item.resolution_source = None;
    item.closed_at = None;
    item.updated_at = Utc::now();
    state
        .db
        .update_item(&item)
        .await
        .map_err(repo_to_internal)?;
    append_activity(
        &state,
        project_id,
        ActivityEventType::ApprovalRejected,
        "item",
        item.id,
        serde_json::json!({ "new_revision_id": next_revision.id, "cancelled_convergence_id": prepared_convergence.id }),
    )
    .await?;
    if cleared_escalation {
        append_activity(
            &state,
            project_id,
            ActivityEventType::ItemEscalationCleared,
            "item",
            item.id,
            serde_json::json!({ "reason": "approval_reject" }),
        )
        .await?;
    }

    let detail = load_item_detail(&state, project_id, item.id).await?;
    Ok(Json(detail))
}

async fn get_finding(
    State(state): State<AppState>,
    Path(finding_id): Path<String>,
) -> Result<Json<Finding>, ApiError> {
    let finding_id = parse_id::<FindingId>(&finding_id, "finding")?;
    let finding = state
        .db
        .get_finding(finding_id)
        .await
        .map_err(repo_to_finding)?;
    Ok(Json(finding))
}

#[derive(Debug)]
struct AppliedFindingTriage {
    finding: Finding,
    linked_item: Option<Item>,
    linked_revision: Option<ItemRevision>,
}

async fn triage_item_finding(
    State(state): State<AppState>,
    Path(finding_id): Path<String>,
    Json(request): Json<TriageFindingRequest>,
) -> Result<Json<Finding>, ApiError> {
    let finding_id = parse_id::<FindingId>(&finding_id, "finding")?;
    let applied = apply_finding_triage(&state, finding_id, request).await?;
    Ok(Json(applied.finding))
}

async fn dismiss_item_finding(
    State(state): State<AppState>,
    Path(finding_id): Path<String>,
    Json(request): Json<DismissFindingRequest>,
) -> Result<Json<Finding>, ApiError> {
    let finding_id = parse_id::<FindingId>(&finding_id, "finding")?;
    let applied = apply_finding_triage(
        &state,
        finding_id,
        TriageFindingRequest {
            triage_state: FindingTriageState::DismissedInvalid,
            triage_note: Some(request.dismissal_reason),
            linked_item_id: None,
            target_ref: None,
            approval_policy: None,
        },
    )
    .await?;
    Ok(Json(applied.finding))
}

async fn promote_item_from_finding(
    State(state): State<AppState>,
    Path(finding_id): Path<String>,
    maybe_request: Option<Json<PromoteFindingRequest>>,
) -> Result<Json<PromoteFindingResponse>, ApiError> {
    let finding_id = parse_id::<FindingId>(&finding_id, "finding")?;
    let request = maybe_request
        .map(|Json(request)| TriageFindingRequest {
            triage_state: FindingTriageState::Backlog,
            triage_note: None,
            linked_item_id: None,
            target_ref: request.target_ref,
            approval_policy: request.approval_policy,
        })
        .unwrap_or(TriageFindingRequest {
            triage_state: FindingTriageState::Backlog,
            triage_note: None,
            linked_item_id: None,
            target_ref: None,
            approval_policy: None,
        });
    let applied = apply_finding_triage(&state, finding_id, request).await?;
    let item = applied.linked_item.ok_or_else(|| ApiError::Conflict {
        code: "linked_item_missing",
        message: "Backlog promotion did not create a linked item".into(),
    })?;
    let current_revision = applied.linked_revision.ok_or_else(|| ApiError::Conflict {
        code: "linked_revision_missing",
        message: "Backlog promotion did not create a linked revision".into(),
    })?;

    Ok(Json(PromoteFindingResponse {
        item,
        current_revision,
        finding: applied.finding,
    }))
}

async fn apply_finding_triage(
    state: &AppState,
    finding_id: FindingId,
    request: TriageFindingRequest,
) -> Result<AppliedFindingTriage, ApiError> {
    let finding = state
        .db
        .get_finding(finding_id)
        .await
        .map_err(repo_to_finding)?;
    let source_item = state
        .db
        .get_item(finding.source_item_id)
        .await
        .map_err(repo_to_item)?;
    let source_revision = state
        .db
        .get_revision(finding.source_item_revision_id)
        .await
        .map_err(repo_to_internal)?;
    let project = state
        .db
        .get_project(source_item.project_id)
        .await
        .map_err(repo_to_project)?;
    let _guard = state
        .project_locks
        .acquire_project_mutation(project.id)
        .await;

    let parsed_linked_item_id = request
        .linked_item_id
        .as_deref()
        .map(|value| parse_id::<ItemId>(value, "linked_item"))
        .transpose()?;
    let detached_origin_item_id =
        find_detached_origin_item_id(state, &finding, parsed_linked_item_id).await?;

    let applied = match request.triage_state {
        FindingTriageState::Backlog => {
            ensure_finding_subject_reachable(&project, &finding).await?;
            if let Some(linked_item_id) = parsed_linked_item_id {
                let linked_item =
                    load_linked_item_for_finding(state, &source_item, linked_item_id).await?;
                if linked_item.id == source_item.id {
                    return Err(ApiError::UseCase(UseCaseError::InvalidFindingTriage(
                        "backlog triage must link to a different item".into(),
                    )));
                }
                let triaged = triage_finding(
                    &finding,
                    TriageFindingInput {
                        triage_state: FindingTriageState::Backlog,
                        triage_note: request.triage_note,
                        linked_item_id: Some(linked_item.id),
                    },
                )?;
                state
                    .db
                    .triage_finding_with_origin_detached(&triaged, detached_origin_item_id)
                    .await
                    .map_err(repo_to_internal)?;
                AppliedFindingTriage {
                    finding: triaged,
                    linked_item: Some(linked_item),
                    linked_revision: None,
                }
            } else {
                let overrides = BacklogFindingOverrides {
                    target_ref: request.target_ref,
                    approval_policy: request.approval_policy,
                };
                let (linked_item, linked_revision, triaged) = backlog_finding(
                    &finding,
                    &source_item,
                    &source_revision,
                    overrides,
                    request.triage_note,
                )?;
                state
                    .db
                    .link_backlog_finding(
                        &triaged,
                        &linked_item,
                        &linked_revision,
                        detached_origin_item_id,
                    )
                    .await
                    .map_err(repo_to_internal)?;
                AppliedFindingTriage {
                    finding: triaged,
                    linked_item: Some(linked_item),
                    linked_revision: Some(linked_revision),
                }
            }
        }
        FindingTriageState::Duplicate => {
            let linked_item_id = parsed_linked_item_id.ok_or_else(|| {
                ApiError::UseCase(UseCaseError::InvalidFindingTriage(
                    "duplicate triage requires linked_item_id".into(),
                ))
            })?;
            let linked_item =
                load_linked_item_for_finding(state, &source_item, linked_item_id).await?;
            if linked_item.id == source_item.id {
                return Err(ApiError::UseCase(UseCaseError::InvalidFindingTriage(
                    "duplicate triage must link to a different item".into(),
                )));
            }
            let triaged = triage_finding(
                &finding,
                TriageFindingInput {
                    triage_state: FindingTriageState::Duplicate,
                    triage_note: request.triage_note,
                    linked_item_id: Some(linked_item.id),
                },
            )?;
            state
                .db
                .triage_finding_with_origin_detached(&triaged, detached_origin_item_id)
                .await
                .map_err(repo_to_internal)?;
            AppliedFindingTriage {
                finding: triaged,
                linked_item: Some(linked_item),
                linked_revision: None,
            }
        }
        _ => {
            let triaged = triage_finding(
                &finding,
                TriageFindingInput {
                    triage_state: request.triage_state,
                    triage_note: request.triage_note,
                    linked_item_id: parsed_linked_item_id,
                },
            )?;
            state
                .db
                .triage_finding_with_origin_detached(&triaged, detached_origin_item_id)
                .await
                .map_err(repo_to_internal)?;
            AppliedFindingTriage {
                finding: triaged,
                linked_item: None,
                linked_revision: None,
            }
        }
    };

    maybe_enter_approval_after_finding_triage(
        state,
        &source_item,
        &source_revision,
        &applied.finding,
    )
    .await?;

    append_activity(
        state,
        source_item.project_id,
        ActivityEventType::FindingTriaged,
        "finding",
        applied.finding.id,
        serde_json::json!({
            "item_id": source_item.id,
            "triage_state": applied.finding.triage_state,
            "linked_item_id": applied.finding.linked_item_id,
        }),
    )
    .await?;

    Ok(applied)
}

async fn find_detached_origin_item_id(
    state: &AppState,
    finding: &Finding,
    next_linked_item_id: Option<ItemId>,
) -> Result<Option<ItemId>, ApiError> {
    let Some(current_linked_item_id) = finding.linked_item_id else {
        return Ok(None);
    };
    if finding.triage_state != FindingTriageState::Backlog {
        return Ok(None);
    }
    if next_linked_item_id == Some(current_linked_item_id) {
        return Ok(None);
    }

    let linked_item = state
        .db
        .get_item(current_linked_item_id)
        .await
        .map_err(repo_to_internal)?;

    if linked_item.origin_kind == OriginKind::PromotedFinding
        && linked_item.origin_finding_id == Some(finding.id)
    {
        Ok(Some(linked_item.id))
    } else {
        Ok(None)
    }
}

async fn load_linked_item_for_finding(
    state: &AppState,
    source_item: &Item,
    linked_item_id: ItemId,
) -> Result<Item, ApiError> {
    let linked_item = state
        .db
        .get_item(linked_item_id)
        .await
        .map_err(|error| match error {
            RepositoryError::NotFound => ApiError::UseCase(UseCaseError::LinkedItemNotFound),
            other => repo_to_internal(other),
        })?;

    if linked_item.project_id != source_item.project_id {
        return Err(UseCaseError::LinkedItemProjectMismatch.into());
    }

    Ok(linked_item)
}

async fn maybe_enter_approval_after_finding_triage(
    state: &AppState,
    source_item: &Item,
    source_revision: &ItemRevision,
    finding: &Finding,
) -> Result<(), ApiError> {
    if finding.source_step_id != "validate_integrated"
        || source_item.current_revision_id != source_revision.id
    {
        return Ok(());
    }

    let jobs = state
        .db
        .list_jobs_by_item(source_item.id)
        .await
        .map_err(repo_to_internal)?;
    let latest_closure_findings_job = jobs
        .iter()
        .filter(|job| job.item_revision_id == source_revision.id)
        .filter(|job| job.status.is_terminal() && job.outcome_class == Some(OutcomeClass::Findings))
        .filter(|job| {
            matches!(
                ingot_workflow::step::find_step(&job.step_id)
                    .map(|contract| contract.closure_relevance),
                Some(ingot_workflow::ClosureRelevance::ClosureRelevant)
            )
        })
        .max_by_key(|job| (job.ended_at, job.created_at));

    let Some(latest_job) = latest_closure_findings_job else {
        return Ok(());
    };
    if latest_job.id != finding.source_job_id {
        return Ok(());
    }

    let findings = state
        .db
        .list_findings_by_item(source_item.id)
        .await
        .map_err(repo_to_internal)?;
    let latest_job_findings = findings
        .iter()
        .filter(|row| row.source_item_revision_id == source_revision.id)
        .filter(|row| row.source_job_id == latest_job.id)
        .collect::<Vec<_>>();

    if latest_job_findings.is_empty()
        || latest_job_findings.iter().any(|row| {
            row.triage_state.is_unresolved() || row.triage_state == FindingTriageState::FixNow
        })
    {
        return Ok(());
    }

    let mut item = state
        .db
        .get_item(source_item.id)
        .await
        .map_err(repo_to_item)?;
    let next_approval_state = match source_revision.approval_policy {
        ApprovalPolicy::Required => ApprovalState::Pending,
        ApprovalPolicy::NotRequired => ApprovalState::NotRequired,
    };
    if item.approval_state != next_approval_state {
        item.approval_state = next_approval_state;
        item.updated_at = Utc::now();
        state
            .db
            .update_item(&item)
            .await
            .map_err(repo_to_internal)?;

        if next_approval_state == ApprovalState::Pending {
            append_activity(
                state,
                item.project_id,
                ActivityEventType::ApprovalRequested,
                "item",
                item.id,
                serde_json::json!({ "source": "finding_triage" }),
            )
            .await?;
        }
    }

    Ok(())
}

async fn complete_job(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    Json(request): Json<CompleteJobRequest>,
) -> Result<Json<CompleteJobResponse>, ApiError> {
    let job_id = parse_id::<JobId>(&job_id, "job")?;
    let prior_job = state.db.get_job(job_id).await.map_err(repo_to_internal)?;
    let prior_item = state
        .db
        .get_item(prior_job.item_id)
        .await
        .map_err(repo_to_item)?;
    let result = state
        .complete_job_service
        .execute(CompleteJobCommand {
            job_id,
            outcome_class: request.outcome_class,
            result_schema_version: request.result_schema_version,
            result_payload: request.result_payload,
            output_commit_oid: request.output_commit_oid,
        })
        .await
        .map_err(complete_job_error_to_api_error)?;
    refresh_revision_context_for_job(&state, job_id).await?;
    let job = state.db.get_job(job_id).await.map_err(repo_to_internal)?;
    let item = state.db.get_item(job.item_id).await.map_err(repo_to_item)?;
    append_activity(
        &state,
        job.project_id,
        ActivityEventType::JobCompleted,
        "job",
        job.id,
        serde_json::json!({ "item_id": job.item_id, "outcome": job.outcome_class }),
    )
    .await?;
    if prior_item.escalation_state == ingot_domain::item::EscalationState::OperatorRequired
        && item.current_revision_id == job.item_revision_id
        && item.escalation_state == ingot_domain::item::EscalationState::None
        && item.escalation_reason.is_none()
    {
        append_activity(
            &state,
            job.project_id,
            ActivityEventType::ItemEscalationCleared,
            "item",
            item.id,
            serde_json::json!({ "reason": "successful_retry", "job_id": job.id }),
        )
        .await?;
    }
    if job.step_id == "validate_integrated"
        && job.outcome_class == Some(OutcomeClass::Clean)
        && item.approval_state == ApprovalState::Pending
    {
        append_activity(
            &state,
            job.project_id,
            ActivityEventType::ApprovalRequested,
            "item",
            item.id,
            serde_json::json!({ "job_id": job.id }),
        )
        .await?;
    }

    Ok(Json(CompleteJobResponse {
        finding_count: result.finding_count,
    }))
}

async fn fail_job(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    Json(request): Json<FailJobRequest>,
) -> Result<Json<()>, ApiError> {
    let status = failure_status(request.outcome_class)?;
    let job_id = parse_id::<JobId>(&job_id, "job")?;
    let job = state.db.get_job(job_id).await.map_err(repo_to_internal)?;
    if !job.status.is_active() {
        return Err(UseCaseError::JobNotActive.into());
    }
    let item = state.db.get_item(job.item_id).await.map_err(repo_to_item)?;
    if job.item_revision_id != item.current_revision_id {
        return Err(UseCaseError::ProtocolViolation(
            "job failure does not match the current item revision".into(),
        )
        .into());
    }
    let escalation_reason = failure_escalation_reason(&job, request.outcome_class);

    state
        .db
        .finish_job_non_success(FinishJobNonSuccessParams {
            job_id: job.id,
            item_id: item.id,
            expected_item_revision_id: job.item_revision_id,
            status,
            outcome_class: Some(request.outcome_class),
            error_code: request.error_code.as_deref(),
            error_message: request.error_message.as_deref(),
            escalation_reason,
        })
        .await
        .map_err(repo_to_job_failure)?;
    refresh_revision_context_for_job(&state, job.id).await?;
    if escalation_reason.is_some() {
        append_activity(
            &state,
            job.project_id,
            ActivityEventType::ItemEscalated,
            "item",
            item.id,
            serde_json::json!({ "reason": escalation_reason }),
        )
        .await?;
    }
    append_activity(
        &state,
        job.project_id,
        ActivityEventType::JobFailed,
        "job",
        job.id,
        serde_json::json!({ "item_id": job.item_id, "error_code": request.error_code }),
    )
    .await?;

    Ok(Json(()))
}

async fn expire_job(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> Result<Json<()>, ApiError> {
    let job_id = parse_id::<JobId>(&job_id, "job")?;
    let job = state.db.get_job(job_id).await.map_err(repo_to_internal)?;
    if !job.status.is_active() {
        return Err(UseCaseError::JobNotActive.into());
    }
    let item = state.db.get_item(job.item_id).await.map_err(repo_to_item)?;
    if job.item_revision_id != item.current_revision_id {
        return Err(UseCaseError::ProtocolViolation(
            "job expiration does not match the current item revision".into(),
        )
        .into());
    }

    state
        .db
        .finish_job_non_success(FinishJobNonSuccessParams {
            job_id: job.id,
            item_id: item.id,
            expected_item_revision_id: job.item_revision_id,
            status: JobStatus::Expired,
            outcome_class: Some(OutcomeClass::TransientFailure),
            error_code: Some("job_expired"),
            error_message: None,
            escalation_reason: None,
        })
        .await
        .map_err(repo_to_job_expiration)?;
    refresh_revision_context_for_job(&state, job.id).await?;
    append_activity(
        &state,
        job.project_id,
        ActivityEventType::JobFailed,
        "job",
        job.id,
        serde_json::json!({ "item_id": job.item_id, "error_code": "job_expired" }),
    )
    .await?;

    Ok(Json(()))
}

async fn load_item_detail(
    state: &AppState,
    project_id: ProjectId,
    item_id: ItemId,
) -> Result<ItemDetailResponse, ApiError> {
    let item = state.db.get_item(item_id).await.map_err(repo_to_item)?;
    if item.project_id != project_id {
        return Err(UseCaseError::ItemNotFound.into());
    }
    let project = state
        .db
        .get_project(item.project_id)
        .await
        .map_err(repo_to_project)?;

    let current_revision = state
        .db
        .get_revision(item.current_revision_id)
        .await
        .map_err(repo_to_internal)?;
    let revision_history = state
        .db
        .list_revisions_by_item(item.id)
        .await
        .map_err(repo_to_internal)?;
    let jobs = state
        .db
        .list_jobs_by_item(item.id)
        .await
        .map_err(repo_to_internal)?;
    let findings = state
        .db
        .list_findings_by_item(item.id)
        .await
        .map_err(repo_to_internal)?;
    let workspaces = state
        .db
        .list_workspaces_by_item(item.id)
        .await
        .map_err(repo_to_internal)?;
    let convergences = state
        .db
        .list_convergences_by_item(item.id)
        .await
        .map_err(repo_to_internal)?;
    let convergences = hydrate_convergence_validity(&project, convergences).await?;
    let revision_context = state
        .db
        .get_revision_context(item.current_revision_id)
        .await
        .map_err(repo_to_internal)?;
    let revision_context_summary = parse_revision_context_summary(revision_context.as_ref())?;
    let evaluation =
        Evaluator::new().evaluate(&item, &current_revision, &jobs, &findings, &convergences);
    let diagnostics = evaluation.diagnostics.clone();

    Ok(ItemDetailResponse {
        item,
        current_revision,
        evaluation,
        revision_history,
        jobs,
        findings,
        workspaces,
        convergences: convergences.into_iter().map(convergence_response).collect(),
        revision_context_summary,
        diagnostics,
    })
}

async fn refresh_revision_context_for_job(state: &AppState, job_id: JobId) -> Result<(), ApiError> {
    let job = state.db.get_job(job_id).await.map_err(repo_to_internal)?;
    let item = state.db.get_item(job.item_id).await.map_err(repo_to_item)?;
    let revision = state
        .db
        .get_revision(job.item_revision_id)
        .await
        .map_err(repo_to_internal)?;
    let project = state
        .db
        .get_project(job.project_id)
        .await
        .map_err(repo_to_project)?;
    refresh_revision_context_for_job_like(state, &item, &revision, &project.path).await
}

async fn refresh_revision_context_for_job_like(
    state: &AppState,
    item: &Item,
    revision: &ItemRevision,
    project_path: &str,
) -> Result<(), ApiError> {
    let jobs = state
        .db
        .list_jobs_by_item(item.id)
        .await
        .map_err(repo_to_internal)?;
    let authoring_head_commit_oid = current_authoring_head_for_revision(&jobs, revision);
    let changed_paths = changed_paths_between(
        FsPath::new(project_path),
        &revision.seed_commit_oid,
        &authoring_head_commit_oid,
    )
    .await
    .map_err(git_to_internal)?;
    let context = rebuild_revision_context(
        item,
        revision,
        &jobs,
        changed_paths,
        jobs.first().map(|job| job.id),
        Utc::now(),
    );
    state
        .db
        .upsert_revision_context(&context)
        .await
        .map_err(repo_to_internal)?;
    Ok(())
}

pub(crate) async fn append_activity(
    state: &AppState,
    project_id: ProjectId,
    event_type: ActivityEventType,
    entity_type: &'static str,
    entity_id: impl ToString,
    payload: serde_json::Value,
) -> Result<(), ApiError> {
    state
        .db
        .append_activity(&Activity {
            id: ingot_domain::ids::ActivityId::new(),
            project_id,
            event_type,
            entity_type: entity_type.into(),
            entity_id: entity_id.to_string(),
            payload,
            created_at: Utc::now(),
        })
        .await
        .map_err(repo_to_internal)
}

async fn read_optional_text(path: PathBuf) -> Result<Option<String>, ApiError> {
    match tokio::fs::read_to_string(path).await {
        Ok(contents) => Ok(Some(contents)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(ApiError::from(UseCaseError::Internal(error.to_string()))),
    }
}

async fn read_optional_json(path: PathBuf) -> Result<Option<serde_json::Value>, ApiError> {
    let Some(contents) = read_optional_text(path).await? else {
        return Ok(None);
    };

    serde_json::from_str(&contents)
        .map(Some)
        .map_err(|error| ApiError::from(UseCaseError::Internal(error.to_string())))
}

fn convergence_response(convergence: Convergence) -> ConvergenceResponse {
    ConvergenceResponse {
        id: convergence.id.to_string(),
        status: serde_json::to_value(convergence.status)
            .ok()
            .and_then(|value| value.as_str().map(ToOwned::to_owned))
            .unwrap_or_else(|| "unknown".into()),
        input_target_commit_oid: convergence.input_target_commit_oid,
        prepared_commit_oid: convergence.prepared_commit_oid,
        final_target_commit_oid: convergence.final_target_commit_oid,
        target_head_valid: convergence.target_head_valid.unwrap_or(true),
    }
}

async fn hydrate_convergence_validity(
    project: &Project,
    convergences: Vec<Convergence>,
) -> Result<Vec<Convergence>, ApiError> {
    let mut hydrated = Vec::with_capacity(convergences.len());

    for mut convergence in convergences {
        convergence.target_head_valid =
            compute_target_head_valid(FsPath::new(&project.path), &convergence).await?;
        hydrated.push(convergence);
    }

    Ok(hydrated)
}

async fn compute_target_head_valid(
    repo_path: &FsPath,
    convergence: &Convergence,
) -> Result<Option<bool>, ApiError> {
    let Some(expected_target_oid) = convergence.input_target_commit_oid.as_deref() else {
        return Ok(None);
    };

    let resolved = resolve_ref_oid(repo_path, &convergence.target_ref)
        .await
        .map_err(|err| ApiError::from(UseCaseError::Internal(err.to_string())))?;

    Ok(Some(resolved.as_deref() == Some(expected_target_oid)))
}

async fn ensure_finding_subject_reachable(
    project: &Project,
    finding: &Finding,
) -> Result<(), ApiError> {
    let repo_path = FsPath::new(&project.path);
    let head_reachable =
        is_commit_reachable_from_any_ref(repo_path, &finding.source_subject_head_commit_oid)
            .await
            .map_err(|err| ApiError::from(UseCaseError::Internal(err.to_string())))?;

    if !head_reachable {
        return Err(UseCaseError::FindingSubjectUnreachable.into());
    }

    if finding.source_subject_kind == ingot_domain::finding::FindingSubjectKind::Integrated {
        let Some(base_commit_oid) = finding.source_subject_base_commit_oid.as_deref() else {
            return Err(UseCaseError::FindingSubjectUnreachable.into());
        };
        let base_reachable = is_commit_reachable_from_any_ref(repo_path, base_commit_oid)
            .await
            .map_err(|err| ApiError::from(UseCaseError::Internal(err.to_string())))?;

        if !base_reachable {
            return Err(UseCaseError::FindingSubjectUnreachable.into());
        }
    }

    Ok(())
}

async fn ensure_reachable_seed(
    repo_path: &FsPath,
    seed_name: &str,
    commit_oid: &str,
) -> Result<(), ApiError> {
    let reachable = is_commit_reachable_from_any_ref(repo_path, commit_oid)
        .await
        .map_err(git_to_internal)?;

    if !reachable {
        return Err(UseCaseError::RevisionSeedUnreachable(seed_name.into()).into());
    }

    Ok(())
}

fn ensure_item_open_idle(item: &Item) -> Result<(), ApiError> {
    if item.lifecycle_state != LifecycleState::Open {
        return Err(UseCaseError::ItemNotOpen.into());
    }
    if item.parking_state != ingot_domain::item::ParkingState::Active {
        return Err(UseCaseError::ItemNotIdle.into());
    }
    Ok(())
}

async fn finish_item_manually(
    state: AppState,
    project_id: ProjectId,
    item_id: ItemId,
    done_reason: DoneReason,
    event_type: ActivityEventType,
) -> Result<Json<ItemDetailResponse>, ApiError> {
    state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;
    let _guard = state
        .project_locks
        .acquire_project_mutation(project_id)
        .await;
    let mut item = state.db.get_item(item_id).await.map_err(repo_to_item)?;
    if item.project_id != project_id {
        return Err(UseCaseError::ItemNotFound.into());
    }
    ensure_item_open_idle(&item)?;
    item.lifecycle_state = LifecycleState::Done;
    item.done_reason = Some(done_reason);
    item.resolution_source = Some(ResolutionSource::ManualCommand);
    item.closed_at = Some(Utc::now());
    let revision = state
        .db
        .get_revision(item.current_revision_id)
        .await
        .map_err(repo_to_internal)?;
    item.approval_state = approval_state_for_policy(revision.approval_policy);
    item.updated_at = Utc::now();
    state
        .db
        .update_item(&item)
        .await
        .map_err(repo_to_internal)?;
    append_activity(
        &state,
        project_id,
        event_type,
        "item",
        item.id,
        serde_json::json!({ "done_reason": item.done_reason }),
    )
    .await?;
    let detail = load_item_detail(&state, project_id, item.id).await?;
    Ok(Json(detail))
}

async fn build_superseding_revision(
    project: &Project,
    item: &Item,
    current_revision: &ItemRevision,
    jobs: &[Job],
    request: ReviseItemRequest,
) -> Result<ItemRevision, ApiError> {
    let target_ref = normalize_target_ref(
        request
            .target_ref
            .as_deref()
            .unwrap_or(current_revision.target_ref.as_str()),
    );
    let repo_path = FsPath::new(&project.path);
    let derived_target_head = resolve_ref_oid(repo_path, &target_ref)
        .await
        .map_err(git_to_internal)?
        .ok_or_else(|| UseCaseError::TargetRefUnresolved(target_ref.clone()))?;

    let seed_commit_oid = if let Some(seed_commit_oid) = request.seed_commit_oid {
        ensure_reachable_seed(repo_path, "seed_commit_oid", &seed_commit_oid).await?;
        seed_commit_oid
    } else {
        current_authoring_head_for_revision(jobs, current_revision)
    };
    let seed_target_commit_oid = if let Some(seed_target_commit_oid) =
        request.seed_target_commit_oid
    {
        ensure_reachable_seed(repo_path, "seed_target_commit_oid", &seed_target_commit_oid).await?;
        Some(seed_target_commit_oid)
    } else {
        Some(derived_target_head)
    };
    let approval_policy = request
        .approval_policy
        .unwrap_or(current_revision.approval_policy);
    let policy_snapshot = build_superseding_policy_snapshot(current_revision, approval_policy);

    Ok(ItemRevision {
        id: ingot_domain::ids::ItemRevisionId::new(),
        item_id: item.id,
        revision_no: current_revision.revision_no + 1,
        title: request.title.unwrap_or(current_revision.title.clone()),
        description: request
            .description
            .unwrap_or(current_revision.description.clone()),
        acceptance_criteria: request
            .acceptance_criteria
            .unwrap_or(current_revision.acceptance_criteria.clone()),
        target_ref,
        approval_policy,
        policy_snapshot,
        template_map_snapshot: default_template_map_snapshot(),
        seed_commit_oid,
        seed_target_commit_oid,
        supersedes_revision_id: Some(current_revision.id),
        created_at: Utc::now(),
    })
}

fn build_superseding_policy_snapshot(
    current_revision: &ItemRevision,
    approval_policy: ApprovalPolicy,
) -> serde_json::Value {
    match rework_budgets_from_policy_snapshot(&current_revision.policy_snapshot) {
        Some((candidate_rework_budget, integration_rework_budget)) => default_policy_snapshot(
            approval_policy,
            candidate_rework_budget,
            integration_rework_budget,
        ),
        None => {
            let mut policy_snapshot = current_revision.policy_snapshot.clone();
            if let Some(object) = policy_snapshot.as_object_mut() {
                object.insert(
                    "approval_policy".into(),
                    serde_json::to_value(approval_policy)
                        .expect("approval policy should serialize into JSON"),
                );
            }
            policy_snapshot
        }
    }
}

async fn ensure_authoring_workspace(
    state: &AppState,
    project: &Project,
    revision: &ItemRevision,
    job: &Job,
) -> Result<Workspace, ApiError> {
    let now = Utc::now();
    let existing = state
        .db
        .find_authoring_workspace_for_revision(revision.id)
        .await
        .map_err(repo_to_internal)?;
    let workspace_exists = existing.is_some();
    let workspace = ensure_authoring_workspace_state(
        existing,
        project.id,
        FsPath::new(&project.path),
        revision,
        job,
        now,
    )
    .await
    .map_err(workspace_to_api_error)?;

    if workspace_exists {
        state
            .db
            .update_workspace(&workspace)
            .await
            .map_err(repo_to_internal)?;
    } else {
        state
            .db
            .create_workspace(&workspace)
            .await
            .map_err(repo_to_internal)?;
    }

    Ok(workspace)
}

async fn prepare_convergence_workspace(
    state: &AppState,
    project: &Project,
    item: &Item,
    revision: &ItemRevision,
    source_workspace: &Workspace,
    source_head_commit_oid: &str,
) -> Result<Convergence, ApiError> {
    let repo_path = FsPath::new(&project.path);
    let input_target_commit_oid = resolve_ref_oid(repo_path, &revision.target_ref)
        .await
        .map_err(git_to_internal)?
        .ok_or_else(|| UseCaseError::TargetRefUnresolved(revision.target_ref.clone()))?;

    let integration_workspace_id = WorkspaceId::new();
    let integration_workspace_path =
        workspace_root_path(repo_path).join(integration_workspace_id.to_string());
    let integration_workspace_ref = format!("refs/ingot/workspaces/{integration_workspace_id}");
    let now = Utc::now();
    let mut integration_workspace = Workspace {
        id: integration_workspace_id,
        project_id: project.id,
        kind: WorkspaceKind::Integration,
        strategy: ingot_domain::workspace::WorkspaceStrategy::Worktree,
        path: integration_workspace_path.display().to_string(),
        created_for_revision_id: Some(revision.id),
        parent_workspace_id: Some(source_workspace.id),
        target_ref: Some(revision.target_ref.clone()),
        workspace_ref: Some(integration_workspace_ref.clone()),
        base_commit_oid: Some(input_target_commit_oid.clone()),
        head_commit_oid: Some(input_target_commit_oid.clone()),
        retention_policy: ingot_domain::workspace::RetentionPolicy::Persistent,
        status: ingot_domain::workspace::WorkspaceStatus::Provisioning,
        current_job_id: None,
        created_at: now,
        updated_at: now,
    };
    state
        .db
        .create_workspace(&integration_workspace)
        .await
        .map_err(repo_to_internal)?;

    let provisioned = provision_integration_workspace(
        repo_path,
        &integration_workspace_path,
        &integration_workspace_ref,
        &input_target_commit_oid,
    )
    .await
    .map_err(workspace_to_api_error)?;
    integration_workspace.path = provisioned.workspace_path.display().to_string();
    integration_workspace.workspace_ref = Some(provisioned.workspace_ref);
    integration_workspace.head_commit_oid = Some(provisioned.head_commit_oid);
    integration_workspace.status = ingot_domain::workspace::WorkspaceStatus::Busy;
    integration_workspace.updated_at = Utc::now();
    state
        .db
        .update_workspace(&integration_workspace)
        .await
        .map_err(repo_to_internal)?;

    let mut convergence = Convergence {
        id: ingot_domain::ids::ConvergenceId::new(),
        project_id: project.id,
        item_id: item.id,
        item_revision_id: revision.id,
        source_workspace_id: source_workspace.id,
        integration_workspace_id: Some(integration_workspace.id),
        source_head_commit_oid: source_head_commit_oid.into(),
        target_ref: revision.target_ref.clone(),
        strategy: ingot_domain::convergence::ConvergenceStrategy::RebaseThenFastForward,
        status: ingot_domain::convergence::ConvergenceStatus::Running,
        input_target_commit_oid: Some(input_target_commit_oid.clone()),
        prepared_commit_oid: None,
        final_target_commit_oid: None,
        target_head_valid: Some(true),
        conflict_summary: None,
        created_at: now,
        completed_at: None,
    };
    state
        .db
        .create_convergence(&convergence)
        .await
        .map_err(repo_to_internal)?;
    append_activity(
        state,
        project.id,
        ActivityEventType::ConvergenceStarted,
        "convergence",
        convergence.id,
        serde_json::json!({ "item_id": item.id }),
    )
    .await?;

    let mut operation = GitOperation {
        id: ingot_domain::ids::GitOperationId::new(),
        project_id: project.id,
        operation_kind: OperationKind::PrepareConvergenceCommit,
        entity_type: GitEntityType::Convergence,
        entity_id: convergence.id.to_string(),
        workspace_id: Some(integration_workspace.id),
        ref_name: integration_workspace.workspace_ref.clone(),
        expected_old_oid: Some(input_target_commit_oid.clone()),
        new_oid: None,
        commit_oid: None,
        status: GitOperationStatus::Planned,
        metadata: None,
        created_at: now,
        completed_at: None,
    };
    state
        .db
        .create_git_operation(&operation)
        .await
        .map_err(repo_to_internal)?;
    append_activity(
        state,
        project.id,
        ActivityEventType::GitOperationPlanned,
        "git_operation",
        operation.id,
        serde_json::json!({ "operation_kind": operation.operation_kind, "entity_id": operation.entity_id }),
    )
    .await?;

    let source_commit_oids =
        list_commits_oldest_first(repo_path, &revision.seed_commit_oid, source_head_commit_oid)
            .await
            .map_err(git_to_internal)?;
    operation.metadata = Some(serde_json::json!({
        "source_commit_oids": source_commit_oids,
        "prepared_commit_oids": [],
    }));
    state
        .db
        .update_git_operation(&operation)
        .await
        .map_err(repo_to_internal)?;

    let integration_workspace_dir = PathBuf::from(&integration_workspace.path);
    let mut prepared_tip = input_target_commit_oid.clone();
    let mut prepared_commit_oids = Vec::with_capacity(source_commit_oids.len());

    for source_commit_oid in &source_commit_oids {
        if let Err(error) =
            cherry_pick_no_commit(&integration_workspace_dir, source_commit_oid).await
        {
            let _ = abort_cherry_pick(&integration_workspace_dir).await;
            integration_workspace.status = ingot_domain::workspace::WorkspaceStatus::Error;
            integration_workspace.updated_at = Utc::now();
            let _ = state.db.update_workspace(&integration_workspace).await;

            convergence.status = ingot_domain::convergence::ConvergenceStatus::Conflicted;
            convergence.conflict_summary = Some(error.to_string());
            convergence.completed_at = Some(Utc::now());
            let _ = state.db.update_convergence(&convergence).await;
            let mut escalated_item = item.clone();
            escalated_item.escalation_state = ingot_domain::item::EscalationState::OperatorRequired;
            escalated_item.escalation_reason = Some(EscalationReason::ConvergenceConflict);
            escalated_item.updated_at = Utc::now();
            let _ = state.db.update_item(&escalated_item).await;
            let _ = append_activity(
                state,
                project.id,
                ActivityEventType::ConvergenceConflicted,
                "convergence",
                convergence.id,
                serde_json::json!({ "item_id": item.id }),
            )
            .await;
            let _ = append_activity(
                state,
                project.id,
                ActivityEventType::ItemEscalated,
                "item",
                item.id,
                serde_json::json!({ "reason": EscalationReason::ConvergenceConflict }),
            )
            .await;

            operation.status = GitOperationStatus::Failed;
            operation.completed_at = Some(Utc::now());
            operation.metadata = Some(serde_json::json!({
                "source_commit_oids": source_commit_oids,
                "prepared_commit_oids": prepared_commit_oids,
            }));
            let _ = state.db.update_git_operation(&operation).await;

            return Err(ApiError::Conflict {
                code: "convergence_conflicted",
                message: "Convergence replay conflicted".into(),
            });
        }

        let original_message = commit_message(repo_path, source_commit_oid)
            .await
            .map_err(git_to_internal)?;
        prepared_tip = create_daemon_convergence_commit(
            &integration_workspace_dir,
            &original_message,
            &ConvergenceCommitTrailers {
                operation_id: operation.id,
                item_id: item.id,
                revision_no: revision.revision_no,
                convergence_id: convergence.id,
                source_commit_oid: source_commit_oid.clone(),
            },
        )
        .await
        .map_err(git_to_internal)?;
        prepared_commit_oids.push(prepared_tip.clone());
        if let Some(workspace_ref) = integration_workspace.workspace_ref.as_deref() {
            ingot_git::commands::git(repo_path, &["update-ref", workspace_ref, &prepared_tip])
                .await
                .map_err(git_to_internal)?;
        }
    }

    integration_workspace.head_commit_oid = Some(prepared_tip.clone());
    integration_workspace.status = ingot_domain::workspace::WorkspaceStatus::Ready;
    integration_workspace.updated_at = Utc::now();
    state
        .db
        .update_workspace(&integration_workspace)
        .await
        .map_err(repo_to_internal)?;

    convergence.status = ingot_domain::convergence::ConvergenceStatus::Prepared;
    convergence.prepared_commit_oid = Some(prepared_tip.clone());
    convergence.completed_at = Some(Utc::now());
    state
        .db
        .update_convergence(&convergence)
        .await
        .map_err(repo_to_internal)?;

    operation.new_oid = Some(prepared_tip.clone());
    operation.commit_oid = Some(prepared_tip);
    operation.status = GitOperationStatus::Applied;
    operation.completed_at = Some(Utc::now());
    operation.metadata = Some(serde_json::json!({
        "source_commit_oids": source_commit_oids,
        "prepared_commit_oids": prepared_commit_oids,
    }));
    state
        .db
        .update_git_operation(&operation)
        .await
        .map_err(repo_to_internal)?;

    Ok(convergence)
}

pub(crate) fn load_effective_config(project: Option<&Project>) -> Result<IngotConfig, ApiError> {
    let project_path = project.map(project_config_path);
    load_config(global_config_path().as_path(), project_path.as_deref()).map_err(|error| {
        ApiError::BadRequest {
            code: "config_invalid",
            message: error.to_string(),
        }
    })
}

fn global_config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".ingot").join("config.yml")
}

fn logs_root() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".ingot").join("logs")
}

fn project_config_path(project: &Project) -> PathBuf {
    FsPath::new(&project.path).join(".ingot").join("config.yml")
}

pub(crate) fn parse_config_approval_policy(
    config: &IngotConfig,
) -> Result<ApprovalPolicy, ApiError> {
    match config.defaults.approval_policy.as_str() {
        "required" => Ok(ApprovalPolicy::Required),
        "not_required" => Ok(ApprovalPolicy::NotRequired),
        other => Err(ApiError::BadRequest {
            code: "config_invalid",
            message: format!("Unsupported approval policy in config: {other}"),
        }),
    }
}

fn canonicalize_repo_path(path: &str) -> Result<PathBuf, ApiError> {
    let path = normalize_non_empty("project path", path)?;
    std::fs::canonicalize(path).map_err(|error| ApiError::BadRequest {
        code: "invalid_project_path",
        message: error.to_string(),
    })
}

pub(crate) async fn resolve_default_branch(
    repo_path: &FsPath,
    requested_branch: Option<&str>,
) -> Result<String, ApiError> {
    let branch = if let Some(branch) = requested_branch {
        normalize_branch_name(branch)?
    } else {
        current_branch_name(repo_path)
            .await
            .map_err(|error| ApiError::BadRequest {
                code: "invalid_project_repo",
                message: error.to_string(),
            })?
    };

    let target_ref = normalize_target_ref(&branch);
    let resolved = resolve_ref_oid(repo_path, &target_ref)
        .await
        .map_err(|error| ApiError::BadRequest {
            code: "invalid_project_repo",
            message: error.to_string(),
        })?;

    if resolved.is_none() {
        return Err(ApiError::BadRequest {
            code: "invalid_default_branch",
            message: format!("Branch does not exist: {branch}"),
        });
    }

    Ok(branch)
}

fn normalize_project_name(name: Option<&str>, path: &FsPath) -> Result<String, ApiError> {
    match name {
        Some(name) => normalize_non_empty("project name", name),
        None => path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .ok_or_else(|| ApiError::BadRequest {
                code: "invalid_project_name",
                message: "Project name is required".into(),
            }),
    }
}

fn normalize_project_color(color: Option<&str>) -> Result<String, ApiError> {
    let color = color.unwrap_or("#6366f1").trim().to_lowercase();
    let valid_length = matches!(color.len(), 4 | 7);
    let valid_hex = color.starts_with('#') && color[1..].chars().all(|ch| ch.is_ascii_hexdigit());

    if valid_length && valid_hex {
        Ok(color)
    } else {
        Err(ApiError::BadRequest {
            code: "invalid_project_color",
            message: format!("Invalid project color: {color}"),
        })
    }
}

fn normalize_branch_name(branch: &str) -> Result<String, ApiError> {
    let branch = normalize_non_empty("default branch", branch)?;
    Ok(branch
        .strip_prefix("refs/heads/")
        .unwrap_or(branch.as_str())
        .to_string())
}

fn normalize_agent_slug(slug: Option<&str>, fallback_name: &str) -> Result<String, ApiError> {
    let raw = slug.unwrap_or(fallback_name).trim().to_lowercase();
    let mut normalized = String::with_capacity(raw.len());
    let mut previous_dash = false;

    for ch in raw.chars() {
        let next = if ch.is_ascii_alphanumeric() {
            previous_dash = false;
            Some(ch)
        } else if !previous_dash {
            previous_dash = true;
            Some('-')
        } else {
            None
        };

        if let Some(ch) = next {
            normalized.push(ch);
        }
    }

    let normalized = normalized.trim_matches('-').to_string();
    if normalized.is_empty() {
        return Err(ApiError::BadRequest {
            code: "invalid_agent_slug",
            message: "Agent slug must contain at least one letter or digit".into(),
        });
    }

    Ok(normalized)
}

fn normalize_non_empty(field: &'static str, value: &str) -> Result<String, ApiError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(ApiError::BadRequest {
            code: "invalid_input",
            message: format!("{field} is required"),
        });
    }

    Ok(trimmed.to_string())
}

fn workspace_to_api_error(error: WorkspaceError) -> ApiError {
    match error {
        WorkspaceError::Busy => ApiError::Conflict {
            code: "workspace_busy",
            message: error.to_string(),
        },
        WorkspaceError::MissingInputHeadCommitOid => {
            ApiError::from(UseCaseError::Internal(error.to_string()))
        }
        WorkspaceError::WorkspaceRefMismatch { .. }
        | WorkspaceError::WorkspaceHeadMismatch { .. } => ApiError::Conflict {
            code: "workspace_state_mismatch",
            message: error.to_string(),
        },
        other => ApiError::from(UseCaseError::Internal(other.to_string())),
    }
}

fn ensure_workspace_not_busy(workspace: &Workspace) -> Result<(), ApiError> {
    if workspace.status == WorkspaceStatus::Busy {
        return Err(ApiError::Conflict {
            code: "workspace_busy",
            message: "Workspace is busy".into(),
        });
    }
    Ok(())
}

fn parse_id<T>(value: &str, entity: &'static str) -> Result<T, ApiError>
where
    T: FromStr,
{
    value
        .parse()
        .map_err(|_| ApiError::invalid_id(entity, value))
}

pub(crate) fn repo_to_internal(error: RepositoryError) -> ApiError {
    ApiError::from(UseCaseError::Repository(error))
}

pub(crate) fn git_to_internal(error: ingot_git::commands::GitCommandError) -> ApiError {
    ApiError::from(UseCaseError::Internal(error.to_string()))
}

fn repo_to_job_completion(error: RepositoryError) -> ApiError {
    match error {
        RepositoryError::Conflict(message) if message == "job_not_active" => {
            UseCaseError::JobNotActive.into()
        }
        other => repo_to_internal(other),
    }
}

fn repo_to_job_failure(error: RepositoryError) -> ApiError {
    match error {
        RepositoryError::Conflict(message) if message == "job_revision_stale" => {
            UseCaseError::ProtocolViolation(
                "job failure does not match the current item revision".into(),
            )
            .into()
        }
        other => repo_to_job_completion(other),
    }
}

fn repo_to_job_expiration(error: RepositoryError) -> ApiError {
    match error {
        RepositoryError::Conflict(message) if message == "job_revision_stale" => {
            UseCaseError::ProtocolViolation(
                "job expiration does not match the current item revision".into(),
            )
            .into()
        }
        other => repo_to_job_completion(other),
    }
}

fn repo_to_item(error: RepositoryError) -> ApiError {
    match error {
        RepositoryError::NotFound => UseCaseError::ItemNotFound.into(),
        other => ApiError::from(UseCaseError::Repository(other)),
    }
}

fn repo_to_project(error: RepositoryError) -> ApiError {
    match error {
        RepositoryError::NotFound => UseCaseError::ProjectNotFound.into(),
        other => ApiError::from(UseCaseError::Repository(other)),
    }
}

pub(crate) fn repo_to_project_mutation(error: RepositoryError) -> ApiError {
    match error {
        RepositoryError::NotFound => UseCaseError::ProjectNotFound.into(),
        RepositoryError::Conflict(message) if message.contains("projects.path") => {
            ApiError::Conflict {
                code: "project_path_conflict",
                message: "A project is already registered for that path".into(),
            }
        }
        RepositoryError::Conflict(message) if message.contains("FOREIGN KEY") => {
            ApiError::Conflict {
                code: "project_in_use",
                message: "Project cannot be deleted while related items still exist".into(),
            }
        }
        other => ApiError::from(UseCaseError::Repository(other)),
    }
}

fn repo_to_agent(error: RepositoryError) -> ApiError {
    match error {
        RepositoryError::NotFound => ApiError::NotFound {
            code: "agent_not_found",
            message: "Agent not found".into(),
        },
        other => ApiError::from(UseCaseError::Repository(other)),
    }
}

fn repo_to_agent_mutation(error: RepositoryError) -> ApiError {
    match error {
        RepositoryError::NotFound => repo_to_agent(RepositoryError::NotFound),
        RepositoryError::Conflict(message) if message.contains("agents.slug") => {
            ApiError::Conflict {
                code: "agent_slug_conflict",
                message: "An agent with that slug already exists".into(),
            }
        }
        RepositoryError::Conflict(message) if message.contains("FOREIGN KEY") => {
            ApiError::Conflict {
                code: "agent_in_use",
                message: "Agent cannot be deleted while related jobs still exist".into(),
            }
        }
        other => ApiError::from(UseCaseError::Repository(other)),
    }
}

fn repo_to_finding(error: RepositoryError) -> ApiError {
    match error {
        RepositoryError::NotFound => UseCaseError::FindingNotFound.into(),
        other => ApiError::from(UseCaseError::Repository(other)),
    }
}

fn complete_job_error_to_api_error(error: CompleteJobError) -> ApiError {
    match error {
        CompleteJobError::BadRequest { code, message } => ApiError::BadRequest { code, message },
        CompleteJobError::UseCase(error) => error.into(),
    }
}

fn failure_status(outcome_class: OutcomeClass) -> Result<JobStatus, ApiError> {
    match outcome_class {
        OutcomeClass::TransientFailure
        | OutcomeClass::TerminalFailure
        | OutcomeClass::ProtocolViolation => Ok(JobStatus::Failed),
        OutcomeClass::Cancelled => Ok(JobStatus::Cancelled),
        OutcomeClass::Clean | OutcomeClass::Findings => Err(ApiError::BadRequest {
            code: "invalid_outcome_class",
            message:
                "Failure endpoints only accept transient_failure, terminal_failure, protocol_violation, or cancelled"
                    .into(),
        }),
    }
}

fn failure_escalation_reason(job: &Job, outcome_class: OutcomeClass) -> Option<EscalationReason> {
    if !is_closure_relevant_job(job) {
        return None;
    }

    match outcome_class {
        OutcomeClass::TerminalFailure => Some(EscalationReason::StepFailed),
        OutcomeClass::ProtocolViolation => Some(EscalationReason::ProtocolViolation),
        OutcomeClass::Clean
        | OutcomeClass::Findings
        | OutcomeClass::TransientFailure
        | OutcomeClass::Cancelled => None,
    }
}

fn is_closure_relevant_job(job: &Job) -> bool {
    matches!(
        ingot_workflow::step::find_step(&job.step_id).map(|step| step.closure_relevance),
        Some(ingot_workflow::ClosureRelevance::ClosureRelevant)
    )
}

fn current_authoring_head_for_revision(jobs: &[Job], revision: &ItemRevision) -> String {
    jobs.iter()
        .filter(|job| job.item_revision_id == revision.id)
        .filter(|job| job.status == JobStatus::Completed)
        .filter(|job| job.output_artifact_kind == ingot_domain::job::OutputArtifactKind::Commit)
        .filter_map(|job| {
            job.output_commit_oid
                .as_ref()
                .map(|commit_oid| ((job.ended_at, job.created_at), commit_oid.clone()))
        })
        .max_by_key(|(sort_key, _)| *sort_key)
        .map(|(_, commit_oid)| commit_oid)
        .unwrap_or_else(|| revision.seed_commit_oid.clone())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::process::Command;
    use std::str::FromStr;

    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode, header};
    use chrono::Utc;
    use ingot_domain::activity::ActivityEventType;
    use ingot_domain::convergence::{Convergence, ConvergenceStatus, ConvergenceStrategy};
    use ingot_domain::finding::{Finding, FindingSeverity, FindingSubjectKind, FindingTriageState};
    use ingot_domain::ids::{
        ConvergenceId, FindingId, ItemId, ItemRevisionId, JobId, ProjectId, WorkspaceId,
    };
    use ingot_domain::job::{JobStatus, OutcomeClass};
    use ingot_domain::ports::RepositoryError;
    use ingot_domain::project::Project;
    use ingot_store_sqlite::Database;
    use ingot_usecases::UseCaseError;
    use tower::ServiceExt;
    use uuid::Uuid;

    use crate::error::ApiError;

    use super::{
        build_router, compute_target_head_valid, ensure_finding_subject_reachable, failure_status,
        repo_to_job_expiration, repo_to_job_failure, repo_to_project,
    };

    #[tokio::test]
    async fn target_head_valid_tracks_ref_movement() {
        let repo = temp_git_repo();
        let first = git_output(&repo, &["rev-parse", "HEAD"]);
        let mut convergence = test_prepared_convergence();
        convergence.target_ref = "refs/heads/main".into();
        convergence.input_target_commit_oid = Some(first.clone());

        let valid = compute_target_head_valid(&repo, &convergence)
            .await
            .expect("compute validity");
        assert_eq!(valid, Some(true));

        write_file(&repo.join("tracked.txt"), "next");
        git(&repo, &["add", "tracked.txt"]);
        git(&repo, &["commit", "-m", "next"]);

        let stale = compute_target_head_valid(&repo, &convergence)
            .await
            .expect("compute stale validity");
        assert_eq!(stale, Some(false));
    }

    #[tokio::test]
    async fn promotion_rejects_unreachable_subject_commits() {
        let repo = temp_git_repo();
        let project = test_project(repo.clone());
        let mut finding = test_finding();
        finding.source_subject_head_commit_oid = "deadbeef".into();

        let result = ensure_finding_subject_reachable(&project, &finding).await;

        assert!(matches!(
            result,
            Err(ApiError::UseCase(UseCaseError::FindingSubjectUnreachable))
        ));
    }

    #[tokio::test]
    async fn candidate_promotions_only_require_head_reachability() {
        let repo = temp_git_repo();
        let project = test_project(repo.clone());
        let head = git_output(&repo, &["rev-parse", "HEAD"]);
        let mut finding = test_finding();
        finding.source_subject_kind = FindingSubjectKind::Candidate;
        finding.source_subject_head_commit_oid = head;
        finding.source_subject_base_commit_oid = Some("deadbeef".into());

        ensure_finding_subject_reachable(&project, &finding)
            .await
            .expect("candidate finding should remain promotable");
    }

    #[tokio::test]
    async fn triaging_final_integrated_finding_enters_pending_approval() {
        let repo = temp_git_repo();
        let head = git_output(&repo, &["rev-parse", "HEAD"]);
        let db_path =
            std::env::temp_dir().join(format!("ingot-http-api-triage-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");

        let project_id = "prj_11111111111111111111111111111111";
        let item_id = "itm_11111111111111111111111111111111";
        let revision_id = "rev_11111111111111111111111111111111";
        let job_id = "job_11111111111111111111111111111111";
        let convergence_id = "cnv_11111111111111111111111111111111";
        let workspace_id = "wrk_11111111111111111111111111111111";
        let finding_id = "fnd_11111111111111111111111111111111";

        sqlx::query(
            "INSERT INTO projects (id, name, path, default_branch, color, created_at, updated_at)
             VALUES (?, 'Test', ?, 'main', '#000', '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(project_id)
        .bind(repo.display().to_string())
        .execute(&db.pool)
        .await
        .expect("insert project");
        sqlx::query(
            "INSERT INTO items (
                id, project_id, classification, workflow_version, lifecycle_state, parking_state,
                approval_state, escalation_state, current_revision_id, origin_kind, origin_finding_id,
                priority, labels, created_at, updated_at
             ) VALUES (?, ?, 'change', 'delivery:v1', 'open', 'active', 'not_requested', 'none', ?, 'manual', NULL, 'major', '[]', '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(item_id)
        .bind(project_id)
        .bind(revision_id)
        .execute(&db.pool)
        .await
        .expect("insert item");
        sqlx::query(
            "INSERT INTO item_revisions (
                id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
                approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid,
                seed_target_commit_oid, supersedes_revision_id, created_at
             ) VALUES (?, ?, 1, 'Title', 'Desc', 'AC', 'refs/heads/main', 'required', '{}', '{}', ?, ?, NULL, '2026-03-12T00:00:00Z')",
        )
        .bind(revision_id)
        .bind(item_id)
        .bind(&head)
        .bind(&head)
        .execute(&db.pool)
        .await
        .expect("insert revision");
        sqlx::query(
            "INSERT INTO jobs (
                id, project_id, item_id, item_revision_id, step_id, semantic_attempt_no, retry_no,
                status, outcome_class, phase_kind, workspace_kind, execution_permission, context_policy,
                phase_template_slug, output_artifact_kind, input_base_commit_oid, input_head_commit_oid,
                created_at, ended_at
             ) VALUES (?, ?, ?, ?, 'validate_integrated', 1, 0, 'completed', 'findings', 'validate', 'integration', 'must_not_mutate', 'resume_context', 'validate-integrated', 'validation_report', ?, ?, '2026-03-12T00:00:00Z', '2026-03-12T00:01:00Z')",
        )
        .bind(job_id)
        .bind(project_id)
        .bind(item_id)
        .bind(revision_id)
        .bind(&head)
        .bind(&head)
        .execute(&db.pool)
        .await
        .expect("insert job");
        sqlx::query(
            "INSERT INTO workspaces (
                id, project_id, kind, strategy, path, created_for_revision_id, parent_workspace_id,
                target_ref, workspace_ref, base_commit_oid, head_commit_oid, retention_policy,
                status, current_job_id, created_at, updated_at
             ) VALUES (?, ?, 'authoring', 'worktree', ?, ?, NULL, 'refs/heads/main', NULL, ?, ?, 'ephemeral', 'ready', NULL, '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(workspace_id)
        .bind(project_id)
        .bind(repo.join("workspace").display().to_string())
        .bind(revision_id)
        .bind(&head)
        .bind(&head)
        .execute(&db.pool)
        .await
        .expect("insert workspace");
        sqlx::query(
            "INSERT INTO convergences (
                id, project_id, item_id, item_revision_id, source_workspace_id, integration_workspace_id,
                source_head_commit_oid, target_ref, strategy, status, input_target_commit_oid,
                prepared_commit_oid, final_target_commit_oid, conflict_summary, created_at, completed_at
             ) VALUES (?, ?, ?, ?, ?, NULL, ?, 'refs/heads/main', 'rebase_then_fast_forward', 'prepared', ?, ?, NULL, NULL, '2026-03-12T00:00:00Z', NULL)",
        )
        .bind(convergence_id)
        .bind(project_id)
        .bind(item_id)
        .bind(revision_id)
        .bind(workspace_id)
        .bind(&head)
        .bind(&head)
        .bind(&head)
        .execute(&db.pool)
        .await
        .expect("insert convergence");
        sqlx::query(
            "INSERT INTO findings (
                id, project_id, source_item_id, source_item_revision_id, source_job_id, source_step_id,
                source_report_schema_version, source_finding_key, source_subject_kind,
                source_subject_base_commit_oid, source_subject_head_commit_oid, code, severity, summary,
                paths, evidence, triage_state, linked_item_id, triage_note, created_at, triaged_at
             ) VALUES (?, ?, ?, ?, ?, 'validate_integrated', 'validation_report:v1', 'finding-1', 'integrated', ?, ?, 'BUG001', 'high', 'summary', '[]', '[]', 'untriaged', NULL, NULL, '2026-03-12T00:00:00Z', NULL)",
        )
        .bind(finding_id)
        .bind(project_id)
        .bind(item_id)
        .bind(revision_id)
        .bind(job_id)
        .bind(&head)
        .bind(&head)
        .execute(&db.pool)
        .await
        .expect("insert finding");

        let response = build_router(db.clone())
            .oneshot(
                Request::builder()
                    .uri(format!("/api/findings/{finding_id}/triage"))
                    .method("POST")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "triage_state": "wont_fix",
                            "triage_note": "accepted risk"
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("triage request");

        assert_eq!(response.status(), StatusCode::OK);
        let approval_state: String =
            sqlx::query_scalar("SELECT approval_state FROM items WHERE id = ?")
                .bind(item_id)
                .fetch_one(&db.pool)
                .await
                .expect("load approval state");
        assert_eq!(approval_state, "pending");
    }

    #[tokio::test]
    async fn backlog_triage_rejects_self_linked_item() {
        let repo = temp_git_repo();
        let head = git_output(&repo, &["rev-parse", "HEAD"]);
        let db_path =
            std::env::temp_dir().join(format!("ingot-http-api-backlog-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");

        let project_id = "prj_22222222222222222222222222222222";
        let item_id = "itm_22222222222222222222222222222222";
        let revision_id = "rev_22222222222222222222222222222222";
        let finding_id = "fnd_22222222222222222222222222222222";
        let job_id = "job_22222222222222222222222222222222";

        sqlx::query(
            "INSERT INTO projects (id, name, path, default_branch, color, created_at, updated_at)
             VALUES (?, 'Test', ?, 'main', '#000', '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(project_id)
        .bind(repo.display().to_string())
        .execute(&db.pool)
        .await
        .expect("insert project");
        sqlx::query(
            "INSERT INTO items (
                id, project_id, classification, workflow_version, lifecycle_state, parking_state,
                approval_state, escalation_state, current_revision_id, origin_kind, origin_finding_id,
                priority, labels, created_at, updated_at
             ) VALUES (?, ?, 'change', 'delivery:v1', 'open', 'active', 'not_requested', 'none', ?, 'manual', NULL, 'major', '[]', '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(item_id)
        .bind(project_id)
        .bind(revision_id)
        .execute(&db.pool)
        .await
        .expect("insert item");
        sqlx::query(
            "INSERT INTO item_revisions (
                id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
                approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid,
                seed_target_commit_oid, supersedes_revision_id, created_at
             ) VALUES (?, ?, 1, 'Title', 'Desc', 'AC', 'refs/heads/main', 'required', '{}', '{}', ?, ?, NULL, '2026-03-12T00:00:00Z')",
        )
        .bind(revision_id)
        .bind(item_id)
        .bind(&head)
        .bind(&head)
        .execute(&db.pool)
        .await
        .expect("insert revision");
        sqlx::query(
            "INSERT INTO jobs (
                id, project_id, item_id, item_revision_id, step_id, semantic_attempt_no, retry_no,
                status, outcome_class, phase_kind, workspace_kind, execution_permission, context_policy,
                phase_template_slug, output_artifact_kind, input_base_commit_oid, input_head_commit_oid,
                created_at, ended_at
             ) VALUES (?, ?, ?, ?, 'review_candidate_initial', 1, 0, 'completed', 'findings', 'review', 'review', 'must_not_mutate', 'fresh', 'review-candidate', 'review_report', ?, ?, '2026-03-12T00:00:00Z', '2026-03-12T00:01:00Z')",
        )
        .bind(job_id)
        .bind(project_id)
        .bind(item_id)
        .bind(revision_id)
        .bind(&head)
        .bind(&head)
        .execute(&db.pool)
        .await
        .expect("insert job");
        sqlx::query(
            "INSERT INTO findings (
                id, project_id, source_item_id, source_item_revision_id, source_job_id, source_step_id,
                source_report_schema_version, source_finding_key, source_subject_kind,
                source_subject_base_commit_oid, source_subject_head_commit_oid, code, severity, summary,
                paths, evidence, triage_state, linked_item_id, triage_note, created_at, triaged_at
             ) VALUES (?, ?, ?, ?, ?, 'review_candidate_initial', 'review_report:v1', 'finding-1', 'candidate', ?, ?, 'BUG001', 'high', 'summary', '[]', '[]', 'untriaged', NULL, NULL, '2026-03-12T00:00:00Z', NULL)",
        )
        .bind(finding_id)
        .bind(project_id)
        .bind(item_id)
        .bind(revision_id)
        .bind(job_id)
        .bind(&head)
        .bind(&head)
        .execute(&db.pool)
        .await
        .expect("insert finding");

        let response = build_router(db.clone())
            .oneshot(
                Request::builder()
                    .uri(format!("/api/findings/{finding_id}/triage"))
                    .method("POST")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "triage_state": "backlog",
                            "linked_item_id": item_id
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("triage request");

        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn retriaging_backlog_created_item_clears_origin_backlink() {
        let repo = temp_git_repo();
        let head = git_output(&repo, &["rev-parse", "HEAD"]);
        let db_path =
            std::env::temp_dir().join(format!("ingot-http-api-retriage-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");

        let project_id = "prj_33333333333333333333333333333333";
        let item_id = "itm_33333333333333333333333333333333";
        let revision_id = "rev_33333333333333333333333333333333";
        let finding_id = "fnd_33333333333333333333333333333333";
        let job_id = "job_33333333333333333333333333333333";
        let linked_item_id = "itm_44444444444444444444444444444444";
        let linked_revision_id = "rev_44444444444444444444444444444444";

        sqlx::query(
            "INSERT INTO projects (id, name, path, default_branch, color, created_at, updated_at)
             VALUES (?, 'Test', ?, 'main', '#000', '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(project_id)
        .bind(repo.display().to_string())
        .execute(&db.pool)
        .await
        .expect("insert project");
        sqlx::query(
            "INSERT INTO items (
                id, project_id, classification, workflow_version, lifecycle_state, parking_state,
                approval_state, escalation_state, current_revision_id, origin_kind, origin_finding_id,
                priority, labels, created_at, updated_at
             ) VALUES (?, ?, 'change', 'delivery:v1', 'open', 'active', 'not_requested', 'none', ?, 'manual', NULL, 'major', '[]', '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(item_id)
        .bind(project_id)
        .bind(revision_id)
        .execute(&db.pool)
        .await
        .expect("insert item");
        sqlx::query(
            "INSERT INTO item_revisions (
                id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
                approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid,
                seed_target_commit_oid, supersedes_revision_id, created_at
             ) VALUES (?, ?, 1, 'Title', 'Desc', 'AC', 'refs/heads/main', 'required', '{}', '{}', ?, ?, NULL, '2026-03-12T00:00:00Z')",
        )
        .bind(revision_id)
        .bind(item_id)
        .bind(&head)
        .bind(&head)
        .execute(&db.pool)
        .await
        .expect("insert revision");
        sqlx::query(
            "INSERT INTO jobs (
                id, project_id, item_id, item_revision_id, step_id, semantic_attempt_no, retry_no,
                status, outcome_class, phase_kind, workspace_kind, execution_permission, context_policy,
                phase_template_slug, output_artifact_kind, input_base_commit_oid, input_head_commit_oid,
                created_at, ended_at
             ) VALUES (?, ?, ?, ?, 'review_candidate_initial', 1, 0, 'completed', 'findings', 'review', 'review', 'must_not_mutate', 'fresh', 'review-candidate', 'review_report', ?, ?, '2026-03-12T00:00:00Z', '2026-03-12T00:01:00Z')",
        )
        .bind(job_id)
        .bind(project_id)
        .bind(item_id)
        .bind(revision_id)
        .bind(&head)
        .bind(&head)
        .execute(&db.pool)
        .await
        .expect("insert job");
        sqlx::query(
            "INSERT INTO findings (
                id, project_id, source_item_id, source_item_revision_id, source_job_id, source_step_id,
                source_report_schema_version, source_finding_key, source_subject_kind,
                source_subject_base_commit_oid, source_subject_head_commit_oid, code, severity, summary,
                paths, evidence, triage_state, linked_item_id, triage_note, created_at, triaged_at
             ) VALUES (?, ?, ?, ?, ?, 'review_candidate_initial', 'review_report:v1', 'finding-1', 'candidate', ?, ?, 'BUG001', 'high', 'summary', '[]', '[]', 'untriaged', NULL, NULL, '2026-03-12T00:00:00Z', NULL)",
        )
        .bind(finding_id)
        .bind(project_id)
        .bind(item_id)
        .bind(revision_id)
        .bind(job_id)
        .bind(&head)
        .bind(&head)
        .execute(&db.pool)
        .await
        .expect("insert finding");
        sqlx::query(
            "INSERT INTO items (
                id, project_id, classification, workflow_version, lifecycle_state, parking_state,
                approval_state, escalation_state, current_revision_id, origin_kind, origin_finding_id,
                priority, labels, created_at, updated_at
             ) VALUES (?, ?, 'bug', 'delivery:v1', 'open', 'active', 'not_requested', 'none', ?, 'promoted_finding', ?, 'major', '[]', '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(linked_item_id)
        .bind(project_id)
        .bind(linked_revision_id)
        .bind(finding_id)
        .execute(&db.pool)
        .await
        .expect("insert linked item");
        sqlx::query(
            "INSERT INTO item_revisions (
                id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
                approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid,
                seed_target_commit_oid, supersedes_revision_id, created_at
             ) VALUES (?, ?, 1, 'Bug', 'Desc', 'AC', 'refs/heads/main', 'required', '{}', '{}', ?, ?, NULL, '2026-03-12T00:00:00Z')",
        )
        .bind(linked_revision_id)
        .bind(linked_item_id)
        .bind(&head)
        .bind(&head)
        .execute(&db.pool)
        .await
        .expect("insert linked revision");
        sqlx::query(
            "UPDATE findings
             SET triage_state = 'backlog', linked_item_id = ?, triaged_at = '2026-03-12T00:01:00Z'
             WHERE id = ?",
        )
        .bind(linked_item_id)
        .bind(finding_id)
        .execute(&db.pool)
        .await
        .expect("mark finding backlog");

        let response = build_router(db.clone())
            .oneshot(
                Request::builder()
                    .uri(format!("/api/findings/{finding_id}/triage"))
                    .method("POST")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "triage_state": "fix_now"
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("triage request");

        assert_eq!(response.status(), StatusCode::OK);
        let origin_kind: String = sqlx::query_scalar("SELECT origin_kind FROM items WHERE id = ?")
            .bind(linked_item_id)
            .fetch_one(&db.pool)
            .await
            .expect("load origin kind");
        let origin_finding_id: Option<String> =
            sqlx::query_scalar("SELECT origin_finding_id FROM items WHERE id = ?")
                .bind(linked_item_id)
                .fetch_one(&db.pool)
                .await
                .expect("load origin finding id");
        assert_eq!(origin_kind, "manual");
        assert_eq!(origin_finding_id, None);
    }

    #[tokio::test]
    async fn invalid_repo_paths_surface_internal_errors_during_reachability_checks() {
        let project =
            test_project(std::env::temp_dir().join(format!("not-a-repo-{}", Uuid::now_v7())));
        let finding = test_finding();

        let result = ensure_finding_subject_reachable(&project, &finding).await;

        assert!(matches!(
            result,
            Err(ApiError::UseCase(UseCaseError::Internal(_)))
        ));
    }

    #[test]
    fn project_not_found_maps_to_project_error() {
        let error = repo_to_project(RepositoryError::NotFound);

        assert!(matches!(
            error,
            ApiError::UseCase(UseCaseError::ProjectNotFound)
        ));
    }

    #[test]
    fn failure_revision_stale_maps_to_protocol_violation() {
        let error = repo_to_job_failure(RepositoryError::Conflict("job_revision_stale".into()));

        assert!(matches!(
            error,
            ApiError::UseCase(UseCaseError::ProtocolViolation(message))
                if message == "job failure does not match the current item revision"
        ));
    }

    #[test]
    fn expiration_revision_stale_maps_to_protocol_violation() {
        let error = repo_to_job_expiration(RepositoryError::Conflict("job_revision_stale".into()));

        assert!(matches!(
            error,
            ApiError::UseCase(UseCaseError::ProtocolViolation(message))
                if message == "job expiration does not match the current item revision"
        ));
    }

    #[test]
    fn failure_status_maps_cancelled_to_cancelled_and_failures_to_failed() {
        assert!(matches!(
            failure_status(OutcomeClass::Cancelled),
            Ok(JobStatus::Cancelled)
        ));
        assert!(matches!(
            failure_status(OutcomeClass::TransientFailure),
            Ok(JobStatus::Failed)
        ));
        assert!(matches!(
            failure_status(OutcomeClass::TerminalFailure),
            Ok(JobStatus::Failed)
        ));
        assert!(matches!(
            failure_status(OutcomeClass::ProtocolViolation),
            Ok(JobStatus::Failed)
        ));
        assert!(matches!(
            failure_status(OutcomeClass::Clean),
            Err(ApiError::BadRequest {
                code: "invalid_outcome_class",
                ..
            })
        ));
    }

    #[tokio::test]
    async fn fail_route_persists_escalation_and_item_detail_projection() {
        let (_repo, db, project_id, item_id, job_id) = seeded_route_test_app().await;
        let app = build_router(db.clone());

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/jobs/{job_id}/fail"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "outcome_class": "terminal_failure",
                            "error_code": "worker_failed",
                            "error_message": "boom"
                        })
                        .to_string(),
                    ))
                    .expect("build request"),
            )
            .await
            .expect("fail route response");

        assert_eq!(response.status(), StatusCode::OK);

        let detail_response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/projects/{project_id}/items/{item_id}"))
                    .body(Body::empty())
                    .expect("build detail request"),
            )
            .await
            .expect("detail route response");

        assert_eq!(detail_response.status(), StatusCode::OK);
        let body = to_bytes(detail_response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("detail json");

        assert_eq!(
            json["item"]["escalation_state"].as_str(),
            Some("operator_required")
        );
        assert_eq!(
            json["item"]["escalation_reason"].as_str(),
            Some("step_failed")
        );
        assert_eq!(
            json["evaluation"]["phase_status"].as_str(),
            Some("escalated")
        );
        assert_eq!(json["jobs"][0]["status"].as_str(), Some("failed"));
        assert_eq!(
            json["jobs"][0]["outcome_class"].as_str(),
            Some("terminal_failure")
        );
    }

    #[tokio::test]
    async fn expire_route_persists_terminal_job_without_auto_redispatch() {
        let (_repo, db, project_id, item_id, job_id) = seeded_route_test_app().await;
        let app = build_router(db.clone());

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/jobs/{job_id}/expire"))
                    .body(Body::empty())
                    .expect("build expire request"),
            )
            .await
            .expect("expire route response");

        assert_eq!(response.status(), StatusCode::OK);

        let detail_response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/projects/{project_id}/items/{item_id}"))
                    .body(Body::empty())
                    .expect("build detail request"),
            )
            .await
            .expect("detail route response");

        assert_eq!(detail_response.status(), StatusCode::OK);
        let body = to_bytes(detail_response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("detail json");

        assert_eq!(json["item"]["escalation_state"].as_str(), Some("none"));
        assert!(json["evaluation"]["dispatchable_step_id"].is_null());
        assert_eq!(
            json["evaluation"]["next_recommended_action"].as_str(),
            Some("none")
        );
        assert_eq!(
            json["evaluation"]["current_step_id"].as_str(),
            Some("validate_candidate_initial")
        );
        assert_eq!(json["jobs"][0]["status"].as_str(), Some("expired"));
        assert_eq!(
            json["jobs"][0]["outcome_class"].as_str(),
            Some("transient_failure")
        );
    }

    #[tokio::test]
    async fn create_project_route_registers_repo_and_exposes_project_config() {
        let repo = temp_git_repo();
        let db_path = std::env::temp_dir().join(format!("ingot-http-api-db-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");

        fs::create_dir_all(repo.join(".ingot")).expect("create config dir");
        write_file(
            &repo.join(".ingot/config.yml"),
            "defaults:\n  candidate_rework_budget: 7\n  integration_rework_budget: 9\n  approval_policy: not_required\n  overflow_strategy: truncate\n",
        );

        let app = build_router(db.clone());
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/projects")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "path": repo.display().to_string(),
                            "color": "#123abc"
                        })
                        .to_string(),
                    ))
                    .expect("build request"),
            )
            .await
            .expect("create project response");

        assert_eq!(response.status(), StatusCode::CREATED);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("project json");
        let project_id = json["id"].as_str().expect("project id");

        assert_eq!(json["default_branch"].as_str(), Some("main"));
        assert_eq!(json["color"].as_str(), Some("#123abc"));
        assert_eq!(
            json["name"].as_str(),
            repo.file_name().and_then(|name| name.to_str())
        );

        let config_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/projects/{project_id}/config"))
                    .body(Body::empty())
                    .expect("build config request"),
            )
            .await
            .expect("project config response");

        assert_eq!(config_response.status(), StatusCode::OK);
        let config_body = to_bytes(config_response.into_body(), usize::MAX)
            .await
            .expect("read config body");
        let config_json: serde_json::Value =
            serde_json::from_slice(&config_body).expect("config json");

        assert_eq!(
            config_json["defaults"]["approval_policy"].as_str(),
            Some("not_required")
        );
        assert_eq!(
            config_json["defaults"]["candidate_rework_budget"].as_u64(),
            Some(7)
        );

        let list_response = app
            .oneshot(
                Request::builder()
                    .uri("/api/projects")
                    .body(Body::empty())
                    .expect("build list request"),
            )
            .await
            .expect("list projects response");

        assert_eq!(list_response.status(), StatusCode::OK);
        let list_body = to_bytes(list_response.into_body(), usize::MAX)
            .await
            .expect("read list body");
        let list_json: serde_json::Value = serde_json::from_slice(&list_body).expect("list json");
        assert_eq!(list_json.as_array().map(Vec::len), Some(1));
    }

    #[tokio::test]
    async fn project_activity_route_lists_recorded_activity() {
        let repo = temp_git_repo();
        let db_path = std::env::temp_dir().join(format!("ingot-http-api-db-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let app = build_router(db.clone());

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/projects")
                    .method("POST")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "name": "Test",
                            "path": repo.display().to_string()
                        })
                        .to_string(),
                    ))
                    .expect("build project request"),
            )
            .await
            .expect("project route response");
        let project_body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("project body");
        let project_json: serde_json::Value =
            serde_json::from_slice(&project_body).expect("project json");
        let project_id = project_json["id"].as_str().expect("project id");

        let item_response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/projects/{project_id}/items"))
                    .method("POST")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "title": "Title",
                            "description": "Desc",
                            "acceptance_criteria": "AC"
                        })
                        .to_string(),
                    ))
                    .expect("build item request"),
            )
            .await
            .expect("item route response");
        assert_eq!(item_response.status(), StatusCode::CREATED);

        let activity_response = build_router(db.clone())
            .oneshot(
                Request::builder()
                    .uri(format!("/api/projects/{project_id}/activity"))
                    .method("GET")
                    .body(Body::empty())
                    .expect("build activity request"),
            )
            .await
            .expect("activity route response");

        assert_eq!(activity_response.status(), StatusCode::OK);
        let activity_body = to_bytes(activity_response.into_body(), usize::MAX)
            .await
            .expect("activity body");
        let activity_json: serde_json::Value =
            serde_json::from_slice(&activity_body).expect("activity json");
        assert_eq!(activity_json.as_array().map(Vec::len), Some(1));
        assert_eq!(
            activity_json[0]["event_type"].as_str(),
            Some("item_created")
        );
    }

    #[tokio::test]
    async fn update_and_delete_project_routes_mutate_registered_project() {
        let repo = temp_git_repo();
        let db_path = std::env::temp_dir().join(format!("ingot-http-api-db-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let app = build_router(db.clone());

        let create_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/projects")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "name": "Original",
                            "path": repo.display().to_string()
                        })
                        .to_string(),
                    ))
                    .expect("build create request"),
            )
            .await
            .expect("create project response");
        let create_body = to_bytes(create_response.into_body(), usize::MAX)
            .await
            .expect("read create body");
        let create_json: serde_json::Value =
            serde_json::from_slice(&create_body).expect("create json");
        let project_id = create_json["id"].as_str().expect("project id");

        let update_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/projects/{project_id}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "name": "Renamed",
                            "color": "#abcdef"
                        })
                        .to_string(),
                    ))
                    .expect("build update request"),
            )
            .await
            .expect("update project response");

        assert_eq!(update_response.status(), StatusCode::OK);
        let update_body = to_bytes(update_response.into_body(), usize::MAX)
            .await
            .expect("read update body");
        let update_json: serde_json::Value =
            serde_json::from_slice(&update_body).expect("update json");
        assert_eq!(update_json["name"].as_str(), Some("Renamed"));
        assert_eq!(update_json["color"].as_str(), Some("#abcdef"));

        let delete_response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/projects/{project_id}"))
                    .body(Body::empty())
                    .expect("build delete request"),
            )
            .await
            .expect("delete project response");

        assert_eq!(delete_response.status(), StatusCode::NO_CONTENT);

        let projects: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM projects")
            .fetch_one(&db.pool)
            .await
            .expect("project count");
        assert_eq!(projects, 0);
    }

    #[tokio::test]
    async fn create_item_route_uses_project_config_defaults_when_policy_is_omitted() {
        let repo = temp_git_repo();
        let db_path = std::env::temp_dir().join(format!("ingot-http-api-db-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let app = build_router(db.clone());

        fs::create_dir_all(repo.join(".ingot")).expect("create config dir");
        write_file(
            &repo.join(".ingot/config.yml"),
            "defaults:\n  candidate_rework_budget: 7\n  integration_rework_budget: 9\n  approval_policy: not_required\n  overflow_strategy: truncate\n",
        );

        let create_project_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/projects")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "path": repo.display().to_string()
                        })
                        .to_string(),
                    ))
                    .expect("build project request"),
            )
            .await
            .expect("project response");
        let project_body = to_bytes(create_project_response.into_body(), usize::MAX)
            .await
            .expect("read project body");
        let project_json: serde_json::Value =
            serde_json::from_slice(&project_body).expect("project json");
        let project_id = project_json["id"].as_str().expect("project id");

        let item_response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{project_id}/items"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "title": "Config-backed item",
                            "description": "Load defaults from project config",
                            "acceptance_criteria": "The revision freezes config defaults"
                        })
                        .to_string(),
                    ))
                    .expect("build item request"),
            )
            .await
            .expect("item response");

        assert_eq!(item_response.status(), StatusCode::CREATED);
        let item_body = to_bytes(item_response.into_body(), usize::MAX)
            .await
            .expect("read item body");
        let item_json: serde_json::Value = serde_json::from_slice(&item_body).expect("item json");

        assert_eq!(
            item_json["current_revision"]["approval_policy"].as_str(),
            Some("not_required")
        );
        assert_eq!(
            item_json["item"]["approval_state"].as_str(),
            Some("not_required")
        );
        assert_eq!(
            item_json["current_revision"]["policy_snapshot"]["candidate_rework_budget"].as_u64(),
            Some(7)
        );
        assert_eq!(
            item_json["current_revision"]["policy_snapshot"]["integration_rework_budget"].as_u64(),
            Some(9)
        );
    }

    #[tokio::test]
    async fn create_agent_route_probes_cli_and_lists_agents() {
        let db_path = std::env::temp_dir().join(format!("ingot-http-api-db-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let app = build_router(db.clone());
        let fake_codex = fake_codex_probe_script();

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/agents")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "name": "Codex CLI",
                            "adapter_kind": "codex",
                            "provider": "openai",
                            "model": "gpt-5-codex",
                            "cli_path": fake_codex.display().to_string()
                        })
                        .to_string(),
                    ))
                    .expect("build create request"),
            )
            .await
            .expect("create agent response");

        assert_eq!(response.status(), StatusCode::CREATED);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read create body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("agent json");
        assert_eq!(json["status"].as_str(), Some("available"));
        assert_eq!(json["slug"].as_str(), Some("codex-cli"));
        assert!(
            json["health_check"]
                .as_str()
                .is_some_and(|value| value.contains("codex exec help ok"))
        );

        let list_response = app
            .oneshot(
                Request::builder()
                    .uri("/api/agents")
                    .body(Body::empty())
                    .expect("build list request"),
            )
            .await
            .expect("list agents response");
        assert_eq!(list_response.status(), StatusCode::OK);
        let list_body = to_bytes(list_response.into_body(), usize::MAX)
            .await
            .expect("read list body");
        let list_json: serde_json::Value = serde_json::from_slice(&list_body).expect("list json");
        assert_eq!(list_json.as_array().map(Vec::len), Some(1));
    }

    #[tokio::test]
    async fn update_reprobe_and_delete_agent_routes_mutate_bootstrap_state() {
        let db_path = std::env::temp_dir().join(format!("ingot-http-api-db-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let app = build_router(db.clone());
        let fake_codex = fake_codex_probe_script();

        let create_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/agents")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "name": "Codex CLI",
                            "adapter_kind": "codex",
                            "provider": "openai",
                            "model": "gpt-5-codex",
                            "cli_path": fake_codex.display().to_string()
                        })
                        .to_string(),
                    ))
                    .expect("build create request"),
            )
            .await
            .expect("create agent response");
        let create_body = to_bytes(create_response.into_body(), usize::MAX)
            .await
            .expect("read create body");
        let create_json: serde_json::Value =
            serde_json::from_slice(&create_body).expect("create json");
        let agent_id = create_json["id"].as_str().expect("agent id");

        let update_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/agents/{agent_id}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "slug": "codex-primary",
                            "model": "gpt-5"
                        })
                        .to_string(),
                    ))
                    .expect("build update request"),
            )
            .await
            .expect("update agent response");

        assert_eq!(update_response.status(), StatusCode::OK);
        let update_body = to_bytes(update_response.into_body(), usize::MAX)
            .await
            .expect("read update body");
        let update_json: serde_json::Value =
            serde_json::from_slice(&update_body).expect("update json");
        assert_eq!(update_json["slug"].as_str(), Some("codex-primary"));
        assert_eq!(update_json["model"].as_str(), Some("gpt-5"));

        sqlx::query("UPDATE agents SET cli_path = '/definitely/missing/ingot-cli' WHERE id = ?")
            .bind(agent_id)
            .execute(&db.pool)
            .await
            .expect("update cli path");

        let reprobe_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/agents/{agent_id}/reprobe"))
                    .body(Body::empty())
                    .expect("build reprobe request"),
            )
            .await
            .expect("reprobe response");

        assert_eq!(reprobe_response.status(), StatusCode::OK);
        let reprobe_body = to_bytes(reprobe_response.into_body(), usize::MAX)
            .await
            .expect("read reprobe body");
        let reprobe_json: serde_json::Value =
            serde_json::from_slice(&reprobe_body).expect("reprobe json");
        assert_eq!(reprobe_json["status"].as_str(), Some("unavailable"));

        let delete_response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/agents/{agent_id}"))
                    .body(Body::empty())
                    .expect("build delete request"),
            )
            .await
            .expect("delete response");

        assert_eq!(delete_response.status(), StatusCode::NO_CONTENT);

        let agents: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agents")
            .fetch_one(&db.pool)
            .await
            .expect("agent count");
        assert_eq!(agents, 0);
    }

    #[tokio::test]
    async fn create_item_route_derives_initial_revision_from_target_head() {
        let repo = temp_git_repo();
        let seed_head = git_output(&repo, &["rev-parse", "HEAD"]);
        let db_path = std::env::temp_dir().join(format!("ingot-http-api-db-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");

        let project_id = "prj_00000000000000000000000000000021".to_string();
        sqlx::query(
            "INSERT INTO projects (id, name, path, default_branch, color, created_at, updated_at)
             VALUES (?, 'Test', ?, 'main', '#000', '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(&project_id)
        .bind(repo.display().to_string())
        .execute(&db.pool)
        .await
        .expect("insert project");

        let app = build_router(db.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{project_id}/items"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "title": "Implement feature",
                            "description": "Add the MVP path",
                            "acceptance_criteria": "The route creates an item"
                        })
                        .to_string(),
                    ))
                    .expect("build create request"),
            )
            .await
            .expect("create item response");

        assert_eq!(response.status(), StatusCode::CREATED);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("detail json");

        assert_eq!(
            json["current_revision"]["target_ref"].as_str(),
            Some("refs/heads/main")
        );
        assert_eq!(
            json["current_revision"]["seed_commit_oid"].as_str(),
            Some(seed_head.as_str())
        );
        assert_eq!(
            json["evaluation"]["dispatchable_step_id"].as_str(),
            Some("author_initial")
        );

        let revision_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM item_revisions")
            .fetch_one(&db.pool)
            .await
            .expect("revision count");
        assert_eq!(revision_count, 1);
    }

    #[tokio::test]
    async fn defer_and_resume_routes_toggle_parking_state() {
        let repo = temp_git_repo();
        let db_path = std::env::temp_dir().join(format!("ingot-http-api-db-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let project_id = "prj_00000000000000000000000000000055".to_string();
        let item_id = "itm_00000000000000000000000000000055".to_string();
        let revision_id = "rev_00000000000000000000000000000055".to_string();
        let head = git_output(&repo, &["rev-parse", "HEAD"]);

        sqlx::query(
            "INSERT INTO projects (id, name, path, default_branch, color, created_at, updated_at)
             VALUES (?, 'Test', ?, 'main', '#000', '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(&project_id)
        .bind(repo.display().to_string())
        .execute(&db.pool)
        .await
        .expect("insert project");
        sqlx::query(
            "INSERT INTO items (
                id, project_id, classification, workflow_version, lifecycle_state, parking_state,
                approval_state, escalation_state, current_revision_id, origin_kind, origin_finding_id,
                priority, labels, created_at, updated_at
             ) VALUES (?, ?, 'change', 'delivery:v1', 'open', 'active', 'not_requested', 'none', ?, 'manual', NULL, 'major', '[]', '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(&item_id)
        .bind(&project_id)
        .bind(&revision_id)
        .execute(&db.pool)
        .await
        .expect("insert item");
        sqlx::query(
            "INSERT INTO item_revisions (
                id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
                approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid,
                seed_target_commit_oid, supersedes_revision_id, created_at
             ) VALUES (?, ?, 1, 'Title', 'Desc', 'AC', 'refs/heads/main', 'required', '{\"workflow_version\":\"delivery:v1\",\"approval_policy\":\"required\",\"candidate_rework_budget\":7,\"integration_rework_budget\":8}', '{\"author_initial\":\"author-initial\"}', ?, ?, NULL, '2026-03-12T00:00:00Z')",
        )
        .bind(&revision_id)
        .bind(&item_id)
        .bind(&head)
        .bind(&head)
        .execute(&db.pool)
        .await
        .expect("insert revision");

        let app = build_router(db.clone());
        let defer_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/projects/{project_id}/items/{item_id}/defer"))
                    .method("POST")
                    .body(Body::empty())
                    .expect("build defer request"),
            )
            .await
            .expect("defer route response");
        assert_eq!(defer_response.status(), StatusCode::OK);

        let resume_response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/projects/{project_id}/items/{item_id}/resume"))
                    .method("POST")
                    .body(Body::empty())
                    .expect("build resume request"),
            )
            .await
            .expect("resume route response");
        assert_eq!(resume_response.status(), StatusCode::OK);
        let body = to_bytes(resume_response.into_body(), usize::MAX)
            .await
            .expect("resume body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("resume json");
        assert_eq!(json["item"]["parking_state"].as_str(), Some("active"));
    }

    #[tokio::test]
    async fn revise_route_creates_superseding_revision() {
        let repo = temp_git_repo();
        let db_path = std::env::temp_dir().join(format!("ingot-http-api-db-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let project_id = "prj_00000000000000000000000000000054".to_string();
        let item_id = "itm_00000000000000000000000000000054".to_string();
        let revision_id = "rev_00000000000000000000000000000054".to_string();
        let head = git_output(&repo, &["rev-parse", "HEAD"]);

        sqlx::query(
            "INSERT INTO projects (id, name, path, default_branch, color, created_at, updated_at)
             VALUES (?, 'Test', ?, 'main', '#000', '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(&project_id)
        .bind(repo.display().to_string())
        .execute(&db.pool)
        .await
        .expect("insert project");
        sqlx::query(
            "INSERT INTO items (
                id, project_id, classification, workflow_version, lifecycle_state, parking_state,
                approval_state, escalation_state, escalation_reason, current_revision_id, origin_kind, origin_finding_id,
                priority, labels, created_at, updated_at
             ) VALUES (?, ?, 'change', 'delivery:v1', 'open', 'active', 'not_requested', 'operator_required', 'step_failed', ?, 'manual', NULL, 'major', '[]', '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(&item_id)
        .bind(&project_id)
        .bind(&revision_id)
        .execute(&db.pool)
        .await
        .expect("insert item");
        sqlx::query(
            "INSERT INTO item_revisions (
                id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
                approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid,
                seed_target_commit_oid, supersedes_revision_id, created_at
             ) VALUES (?, ?, 1, 'Title', 'Desc', 'AC', 'refs/heads/main', 'required', '{\"workflow_version\":\"delivery:v1\",\"approval_policy\":\"required\",\"candidate_rework_budget\":3,\"integration_rework_budget\":4}', '{\"author_initial\":\"author-initial\"}', ?, ?, NULL, '2026-03-12T00:00:00Z')",
        )
        .bind(&revision_id)
        .bind(&item_id)
        .bind(&head)
        .bind(&head)
        .execute(&db.pool)
        .await
        .expect("insert revision");

        let app = build_router(db.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/projects/{project_id}/items/{item_id}/revise"))
                    .method("POST")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "title": "Revised Title",
                            "approval_policy": "not_required"
                        })
                        .to_string(),
                    ))
                    .expect("build revise request"),
            )
            .await
            .expect("revise route response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("revise body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("revise json");
        assert_eq!(
            json["current_revision"]["title"].as_str(),
            Some("Revised Title")
        );
        assert_eq!(
            json["current_revision"]["approval_policy"].as_str(),
            Some("not_required")
        );
        assert_eq!(
            json["current_revision"]["policy_snapshot"]["approval_policy"].as_str(),
            Some("not_required")
        );
        assert_eq!(
            json["current_revision"]["policy_snapshot"]["candidate_rework_budget"].as_u64(),
            Some(3)
        );
        assert_eq!(
            json["current_revision"]["supersedes_revision_id"].as_str(),
            Some(revision_id.as_str())
        );
        assert_eq!(json["item"]["escalation_state"].as_str(), Some("none"));
        assert_eq!(
            json["item"]["approval_state"].as_str(),
            Some("not_required")
        );

        let revision_policy_snapshot: String = sqlx::query_scalar(
            "SELECT policy_snapshot FROM item_revisions WHERE item_id = ? AND revision_no = 2",
        )
        .bind(&item_id)
        .fetch_one(&db.pool)
        .await
        .expect("load revised policy snapshot");
        let revision_policy_snapshot: serde_json::Value =
            serde_json::from_str(&revision_policy_snapshot).expect("revised policy snapshot json");
        assert_eq!(
            revision_policy_snapshot["approval_policy"].as_str(),
            Some("not_required")
        );
        assert_eq!(
            revision_policy_snapshot["candidate_rework_budget"].as_u64(),
            Some(3)
        );
    }

    #[tokio::test]
    async fn dismiss_and_reopen_routes_close_and_reopen_item() {
        let repo = temp_git_repo();
        let db_path = std::env::temp_dir().join(format!("ingot-http-api-db-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let project_id = "prj_00000000000000000000000000000053".to_string();
        let item_id = "itm_00000000000000000000000000000053".to_string();
        let revision_id = "rev_00000000000000000000000000000053".to_string();
        let head = git_output(&repo, &["rev-parse", "HEAD"]);

        sqlx::query(
            "INSERT INTO projects (id, name, path, default_branch, color, created_at, updated_at)
             VALUES (?, 'Test', ?, 'main', '#000', '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(&project_id)
        .bind(repo.display().to_string())
        .execute(&db.pool)
        .await
        .expect("insert project");
        sqlx::query(
            "INSERT INTO items (
                id, project_id, classification, workflow_version, lifecycle_state, parking_state,
                approval_state, escalation_state, current_revision_id, origin_kind, origin_finding_id,
                priority, labels, created_at, updated_at
             ) VALUES (?, ?, 'change', 'delivery:v1', 'open', 'active', 'not_requested', 'none', ?, 'manual', NULL, 'major', '[]', '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(&item_id)
        .bind(&project_id)
        .bind(&revision_id)
        .execute(&db.pool)
        .await
        .expect("insert item");
        sqlx::query(
            "INSERT INTO item_revisions (
                id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
                approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid,
                seed_target_commit_oid, supersedes_revision_id, created_at
             ) VALUES (?, ?, 1, 'Title', 'Desc', 'AC', 'refs/heads/main', 'required', '{\"workflow_version\":\"delivery:v1\",\"approval_policy\":\"required\",\"candidate_rework_budget\":5,\"integration_rework_budget\":6}', '{\"author_initial\":\"author-initial\"}', ?, ?, NULL, '2026-03-12T00:00:00Z')",
        )
        .bind(&revision_id)
        .bind(&item_id)
        .bind(&head)
        .bind(&head)
        .execute(&db.pool)
        .await
        .expect("insert revision");

        let app = build_router(db.clone());
        let dismiss_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/api/projects/{project_id}/items/{item_id}/dismiss"
                    ))
                    .method("POST")
                    .body(Body::empty())
                    .expect("build dismiss request"),
            )
            .await
            .expect("dismiss route response");
        assert_eq!(dismiss_response.status(), StatusCode::OK);

        let reopen_response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/projects/{project_id}/items/{item_id}/reopen"))
                    .method("POST")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "approval_policy": "not_required"
                        })
                        .to_string(),
                    ))
                    .expect("build reopen request"),
            )
            .await
            .expect("reopen route response");
        assert_eq!(reopen_response.status(), StatusCode::OK);
        let body = to_bytes(reopen_response.into_body(), usize::MAX)
            .await
            .expect("reopen body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("reopen json");
        assert_eq!(json["item"]["lifecycle_state"].as_str(), Some("open"));
        assert_eq!(json["item"]["done_reason"], serde_json::Value::Null);
        assert_eq!(
            json["current_revision"]["approval_policy"].as_str(),
            Some("not_required")
        );
        assert_eq!(
            json["current_revision"]["policy_snapshot"]["approval_policy"].as_str(),
            Some("not_required")
        );
        assert_eq!(
            json["current_revision"]["policy_snapshot"]["candidate_rework_budget"].as_u64(),
            Some(5)
        );
        assert_eq!(
            json["current_revision"]["supersedes_revision_id"].as_str(),
            Some(revision_id.as_str())
        );
        assert_eq!(
            json["item"]["approval_state"].as_str(),
            Some("not_required")
        );

        let revision_policy_snapshot: String = sqlx::query_scalar(
            "SELECT policy_snapshot FROM item_revisions WHERE item_id = ? AND revision_no = 2",
        )
        .bind(&item_id)
        .fetch_one(&db.pool)
        .await
        .expect("load reopened policy snapshot");
        let revision_policy_snapshot: serde_json::Value =
            serde_json::from_str(&revision_policy_snapshot).expect("reopened policy snapshot json");
        assert_eq!(
            revision_policy_snapshot["approval_policy"].as_str(),
            Some("not_required")
        );
        assert_eq!(
            revision_policy_snapshot["candidate_rework_budget"].as_u64(),
            Some(5)
        );
    }

    #[tokio::test]
    async fn dispatch_item_job_route_creates_queued_author_initial_job_and_workspace() {
        let repo = temp_git_repo();
        let seed_head = git_output(&repo, &["rev-parse", "HEAD"]);
        let db_path = std::env::temp_dir().join(format!("ingot-http-api-db-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");

        let project_id = "prj_00000000000000000000000000000031".to_string();
        let item_id = "itm_00000000000000000000000000000031".to_string();
        let revision_id = "rev_00000000000000000000000000000031".to_string();

        sqlx::query(
            "INSERT INTO projects (id, name, path, default_branch, color, created_at, updated_at)
             VALUES (?, 'Test', ?, 'main', '#000', '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(&project_id)
        .bind(repo.display().to_string())
        .execute(&db.pool)
        .await
        .expect("insert project");

        sqlx::query(
            "INSERT INTO items (
                id, project_id, classification, workflow_version, lifecycle_state, parking_state,
                approval_state, escalation_state, current_revision_id, origin_kind, origin_finding_id,
                priority, labels, created_at, updated_at
             ) VALUES (?, ?, 'change', 'delivery:v1', 'open', 'active', 'not_requested', 'none', ?, 'manual', NULL, 'major', '[]', '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(&item_id)
        .bind(&project_id)
        .bind(&revision_id)
        .execute(&db.pool)
        .await
        .expect("insert item");

        sqlx::query(
            "INSERT INTO item_revisions (
                id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
                approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid,
                seed_target_commit_oid, supersedes_revision_id, created_at
             ) VALUES (?, ?, 1, 'Title', 'Desc', 'AC', 'refs/heads/main', 'required', '{}', '{\"author_initial\":\"author-initial\"}', ?, ?, NULL, '2026-03-12T00:00:00Z')",
        )
        .bind(&revision_id)
        .bind(&item_id)
        .bind(&seed_head)
        .bind(&seed_head)
        .execute(&db.pool)
        .await
        .expect("insert revision");

        let app = build_router(db.clone());
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{project_id}/items/{item_id}/jobs"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .expect("build dispatch request"),
            )
            .await
            .expect("dispatch route response");

        assert_eq!(response.status(), StatusCode::CREATED);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("dispatch json");

        assert_eq!(json["step_id"].as_str(), Some("author_initial"));
        assert_eq!(json["status"].as_str(), Some("queued"));
        assert_eq!(json["phase_template_slug"].as_str(), Some("author-initial"));
        assert_eq!(
            json["input_head_commit_oid"].as_str(),
            Some(seed_head.as_str())
        );
        let workspace_id = json["workspace_id"]
            .as_str()
            .expect("workspace id assigned on dispatch");

        let detail_response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/projects/{project_id}/items/{item_id}"))
                    .body(Body::empty())
                    .expect("build detail request"),
            )
            .await
            .expect("detail response");

        let detail_body = to_bytes(detail_response.into_body(), usize::MAX)
            .await
            .expect("read detail body");
        let detail_json: serde_json::Value =
            serde_json::from_slice(&detail_body).expect("detail json");

        assert_eq!(
            detail_json["evaluation"]["current_step_id"].as_str(),
            Some("author_initial")
        );
        assert_eq!(
            detail_json["evaluation"]["phase_status"].as_str(),
            Some("running")
        );
        assert_eq!(detail_json["workspaces"].as_array().map(Vec::len), Some(1));
        assert_eq!(
            detail_json["workspaces"][0]["id"].as_str(),
            Some(workspace_id)
        );
        assert_eq!(
            detail_json["workspaces"][0]["kind"].as_str(),
            Some("authoring")
        );
        assert_eq!(
            detail_json["workspaces"][0]["status"].as_str(),
            Some("ready")
        );
        assert_eq!(
            detail_json["workspaces"][0]["head_commit_oid"].as_str(),
            Some(seed_head.as_str())
        );
        let workspace_path = detail_json["workspaces"][0]["path"]
            .as_str()
            .expect("workspace path");
        assert!(PathBuf::from(workspace_path).exists());
        assert_eq!(
            git_output(&PathBuf::from(workspace_path), &["rev-parse", "HEAD"]),
            seed_head
        );
    }

    #[tokio::test]
    async fn dispatch_item_job_route_reuses_existing_authoring_workspace_for_revision() {
        let repo = temp_git_repo();
        let seed_head = git_output(&repo, &["rev-parse", "HEAD"]);
        let db_path = std::env::temp_dir().join(format!("ingot-http-api-db-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");

        let project_id = "prj_00000000000000000000000000000032".to_string();
        let item_id = "itm_00000000000000000000000000000032".to_string();
        let revision_id = "rev_00000000000000000000000000000032".to_string();
        let workspace_id = "wrk_00000000000000000000000000000032".to_string();
        let workspace_path =
            std::env::temp_dir().join(format!("ingot-authoring-existing-{}", Uuid::now_v7()));

        sqlx::query(
            "INSERT INTO projects (id, name, path, default_branch, color, created_at, updated_at)
             VALUES (?, 'Test', ?, 'main', '#000', '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(&project_id)
        .bind(repo.display().to_string())
        .execute(&db.pool)
        .await
        .expect("insert project");

        sqlx::query(
            "INSERT INTO items (
                id, project_id, classification, workflow_version, lifecycle_state, parking_state,
                approval_state, escalation_state, current_revision_id, origin_kind, origin_finding_id,
                priority, labels, created_at, updated_at
             ) VALUES (?, ?, 'change', 'delivery:v1', 'open', 'active', 'not_requested', 'none', ?, 'manual', NULL, 'major', '[]', '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(&item_id)
        .bind(&project_id)
        .bind(&revision_id)
        .execute(&db.pool)
        .await
        .expect("insert item");

        sqlx::query(
            "INSERT INTO item_revisions (
                id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
                approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid,
                seed_target_commit_oid, supersedes_revision_id, created_at
             ) VALUES (?, ?, 1, 'Title', 'Desc', 'AC', 'refs/heads/main', 'required', '{}', '{\"author_initial\":\"author-initial\"}', ?, ?, NULL, '2026-03-12T00:00:00Z')",
        )
        .bind(&revision_id)
        .bind(&item_id)
        .bind(&seed_head)
        .bind(&seed_head)
        .execute(&db.pool)
        .await
        .expect("insert revision");

        sqlx::query(
            "INSERT INTO workspaces (
                id, project_id, kind, strategy, path, created_for_revision_id, parent_workspace_id,
                target_ref, workspace_ref, base_commit_oid, head_commit_oid, retention_policy,
                status, current_job_id, created_at, updated_at
             ) VALUES (?, ?, 'authoring', 'worktree', ?, ?, NULL, 'refs/heads/main', ?, ?, ?, 'ephemeral', 'ready', NULL, '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(&workspace_id)
        .bind(&project_id)
        .bind(workspace_path.display().to_string())
        .bind(&revision_id)
        .bind(format!("refs/ingot/workspaces/{workspace_id}"))
        .bind(&seed_head)
        .bind(&seed_head)
        .execute(&db.pool)
        .await
        .expect("insert workspace");

        let app = build_router(db.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{project_id}/items/{item_id}/jobs"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .expect("build dispatch request"),
            )
            .await
            .expect("dispatch route response");

        assert_eq!(response.status(), StatusCode::CREATED);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("dispatch json");
        assert_eq!(json["workspace_id"].as_str(), Some(workspace_id.as_str()));

        let workspace_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM workspaces WHERE created_for_revision_id = ?")
                .bind(&revision_id)
                .fetch_one(&db.pool)
                .await
                .expect("workspace count");
        assert_eq!(workspace_count, 1);
    }

    #[tokio::test]
    async fn prepare_convergence_route_replays_authoring_chain_and_queues_integrated_validation() {
        let repo = temp_git_repo();
        let base_commit_oid = git_output(&repo, &["rev-parse", "HEAD"]);
        write_file(&repo.join("tracked.txt"), "candidate change");
        git(&repo, &["add", "tracked.txt"]);
        git(&repo, &["commit", "-m", "candidate commit"]);
        let source_commit_oid = git_output(&repo, &["rev-parse", "HEAD"]);
        git(
            &repo,
            &[
                "update-ref",
                "refs/ingot/workspaces/wrk_source",
                &source_commit_oid,
            ],
        );
        git(&repo, &["reset", "--hard", &base_commit_oid]);

        let db_path = std::env::temp_dir().join(format!("ingot-http-api-db-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let project_id = "prj_00000000000000000000000000000099".to_string();
        let item_id = "itm_00000000000000000000000000000099".to_string();
        let revision_id = "rev_00000000000000000000000000000099".to_string();
        let author_job_id = "job_00000000000000000000000000000098".to_string();
        let validate_job_id = "job_00000000000000000000000000000097".to_string();
        let workspace_id = "wrk_00000000000000000000000000000099".to_string();

        sqlx::query(
            "INSERT INTO projects (id, name, path, default_branch, color, created_at, updated_at)
             VALUES (?, 'Test', ?, 'main', '#000', '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(&project_id)
        .bind(repo.display().to_string())
        .execute(&db.pool)
        .await
        .expect("insert project");

        sqlx::query(
            "INSERT INTO items (
                id, project_id, classification, workflow_version, lifecycle_state, parking_state,
                approval_state, escalation_state, current_revision_id, origin_kind, origin_finding_id,
                priority, labels, created_at, updated_at
             ) VALUES (?, ?, 'change', 'delivery:v1', 'open', 'active', 'not_requested', 'none', ?, 'manual', NULL, 'major', '[]', '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(&item_id)
        .bind(&project_id)
        .bind(&revision_id)
        .execute(&db.pool)
        .await
        .expect("insert item");

        sqlx::query(
            "INSERT INTO item_revisions (
                id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
                approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid,
                seed_target_commit_oid, supersedes_revision_id, created_at
             ) VALUES (?, ?, 1, 'Title', 'Desc', 'AC', 'refs/heads/main', 'required', '{\"workflow_version\":\"delivery:v1\",\"approval_policy\":\"required\",\"candidate_rework_budget\":7,\"integration_rework_budget\":8}', '{\"author_initial\":\"author-initial\"}', ?, ?, NULL, '2026-03-12T00:00:00Z')",
        )
        .bind(&revision_id)
        .bind(&item_id)
        .bind(&base_commit_oid)
        .bind(&base_commit_oid)
        .execute(&db.pool)
        .await
        .expect("insert revision");

        sqlx::query(
            "INSERT INTO workspaces (
                id, project_id, kind, strategy, path, created_for_revision_id, parent_workspace_id,
                target_ref, workspace_ref, base_commit_oid, head_commit_oid, retention_policy,
                status, current_job_id, created_at, updated_at
             ) VALUES (?, ?, 'authoring', 'worktree', ?, ?, NULL, 'refs/heads/main', 'refs/ingot/workspaces/wrk_source', ?, ?, 'persistent', 'ready', NULL, '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(&workspace_id)
        .bind(&project_id)
        .bind(repo.join("source-workspace").display().to_string())
        .bind(&revision_id)
        .bind(&base_commit_oid)
        .bind(&source_commit_oid)
        .execute(&db.pool)
        .await
        .expect("insert source workspace");

        sqlx::query(
            "INSERT INTO jobs (
                id, project_id, item_id, item_revision_id, step_id, semantic_attempt_no, retry_no,
                status, outcome_class, phase_kind, workspace_id, workspace_kind, execution_permission, context_policy,
                phase_template_slug, output_artifact_kind, output_commit_oid, created_at, ended_at
             ) VALUES (?, ?, ?, ?, 'author_initial', 1, 0, 'completed', 'clean', 'author', ?, 'authoring', 'may_mutate', 'fresh', 'author-initial', 'commit', ?, '2026-03-12T00:00:00Z', '2026-03-12T00:05:00Z')",
        )
        .bind(&author_job_id)
        .bind(&project_id)
        .bind(&item_id)
        .bind(&revision_id)
        .bind(&workspace_id)
        .bind(&source_commit_oid)
        .execute(&db.pool)
        .await
        .expect("insert author job");

        sqlx::query(
            "INSERT INTO jobs (
                id, project_id, item_id, item_revision_id, step_id, semantic_attempt_no, retry_no,
                status, outcome_class, phase_kind, workspace_kind, execution_permission, context_policy,
                phase_template_slug, output_artifact_kind, input_base_commit_oid, input_head_commit_oid,
                result_schema_version, result_payload, created_at, ended_at
             ) VALUES (?, ?, ?, ?, 'validate_candidate_initial', 1, 0, 'completed', 'clean', 'validate', 'authoring', 'must_not_mutate', 'resume_context', 'validate-candidate', 'validation_report', ?, ?, 'validation_report:v1', ?, '2026-03-12T00:06:00Z', '2026-03-12T00:07:00Z')",
        )
        .bind(&validate_job_id)
        .bind(&project_id)
        .bind(&item_id)
        .bind(&revision_id)
        .bind(&base_commit_oid)
        .bind(&source_commit_oid)
        .bind(serde_json::json!({
            "outcome": "clean",
            "summary": "validation clean",
            "checks": [],
            "findings": []
        }).to_string())
        .execute(&db.pool)
        .await
        .expect("insert validate job");

        let app = build_router(db.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/api/projects/{project_id}/items/{item_id}/convergence/prepare"
                    ))
                    .method("POST")
                    .body(Body::empty())
                    .expect("build request"),
            )
            .await
            .expect("route response");

        assert_eq!(response.status(), StatusCode::CREATED);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("json body");

        assert_eq!(json["convergence"]["status"].as_str(), Some("prepared"));
        assert_eq!(
            json["validation_job"]["step_id"].as_str(),
            Some("validate_integrated")
        );
        assert_eq!(json["validation_job"]["status"].as_str(), Some("queued"));
        let prepared_commit_oid = json["convergence"]["prepared_commit_oid"]
            .as_str()
            .expect("prepared commit oid");
        let integration_workspace_id = json["validation_job"]["workspace_id"]
            .as_str()
            .expect("integration workspace id");

        let integration_workspace = db
            .get_workspace(
                integration_workspace_id
                    .parse::<WorkspaceId>()
                    .expect("parse workspace id"),
            )
            .await
            .expect("integration workspace");
        assert_eq!(
            integration_workspace.kind,
            ingot_domain::workspace::WorkspaceKind::Integration
        );
        assert!(PathBuf::from(&integration_workspace.path).exists());
        assert_eq!(
            git_output(
                &PathBuf::from(&integration_workspace.path),
                &["rev-parse", "HEAD"]
            ),
            prepared_commit_oid
        );

        let convergence_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM convergences WHERE item_revision_id = ?")
                .bind(&revision_id)
                .fetch_one(&db.pool)
                .await
                .expect("convergence count");
        assert_eq!(convergence_count, 1);
    }

    #[tokio::test]
    async fn approve_route_finalizes_prepared_convergence_and_closes_item() {
        let repo = temp_git_repo();
        let base_commit_oid = git_output(&repo, &["rev-parse", "HEAD"]);
        write_file(&repo.join("tracked.txt"), "prepared change");
        git(&repo, &["add", "tracked.txt"]);
        git(&repo, &["commit", "-m", "prepared commit"]);
        let prepared_commit_oid = git_output(&repo, &["rev-parse", "HEAD"]);
        git(
            &repo,
            &[
                "update-ref",
                "refs/ingot/workspaces/wrk_integration",
                &prepared_commit_oid,
            ],
        );
        git(&repo, &["reset", "--hard", &base_commit_oid]);

        let db_path = std::env::temp_dir().join(format!("ingot-http-api-db-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let project_id = "prj_00000000000000000000000000000088".to_string();
        let item_id = "itm_00000000000000000000000000000088".to_string();
        let revision_id = "rev_00000000000000000000000000000088".to_string();
        let workspace_id = "wrk_00000000000000000000000000000088".to_string();
        let convergence_id = "conv_00000000000000000000000000000088".to_string();

        sqlx::query(
            "INSERT INTO projects (id, name, path, default_branch, color, created_at, updated_at)
             VALUES (?, 'Test', ?, 'main', '#000', '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(&project_id)
        .bind(repo.display().to_string())
        .execute(&db.pool)
        .await
        .expect("insert project");

        sqlx::query(
            "INSERT INTO items (
                id, project_id, classification, workflow_version, lifecycle_state, parking_state,
                approval_state, escalation_state, current_revision_id, origin_kind, origin_finding_id,
                priority, labels, created_at, updated_at
             ) VALUES (?, ?, 'change', 'delivery:v1', 'open', 'active', 'pending', 'none', ?, 'manual', NULL, 'major', '[]', '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(&item_id)
        .bind(&project_id)
        .bind(&revision_id)
        .execute(&db.pool)
        .await
        .expect("insert item");

        sqlx::query(
            "INSERT INTO item_revisions (
                id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
                approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid,
                seed_target_commit_oid, supersedes_revision_id, created_at
             ) VALUES (?, ?, 1, 'Title', 'Desc', 'AC', 'refs/heads/main', 'required', '{\"workflow_version\":\"delivery:v1\",\"approval_policy\":\"required\",\"candidate_rework_budget\":7,\"integration_rework_budget\":8}', '{\"author_initial\":\"author-initial\"}', ?, ?, NULL, '2026-03-12T00:00:00Z')",
        )
        .bind(&revision_id)
        .bind(&item_id)
        .bind(&base_commit_oid)
        .bind(&base_commit_oid)
        .execute(&db.pool)
        .await
        .expect("insert revision");

        sqlx::query(
            "INSERT INTO workspaces (
                id, project_id, kind, strategy, path, created_for_revision_id, parent_workspace_id,
                target_ref, workspace_ref, base_commit_oid, head_commit_oid, retention_policy,
                status, current_job_id, created_at, updated_at
             ) VALUES (?, ?, 'integration', 'worktree', ?, ?, NULL, 'refs/heads/main', 'refs/ingot/workspaces/wrk_integration', ?, ?, 'persistent', 'ready', NULL, '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(&workspace_id)
        .bind(&project_id)
        .bind(repo.join("integration-workspace").display().to_string())
        .bind(&revision_id)
        .bind(&base_commit_oid)
        .bind(&prepared_commit_oid)
        .execute(&db.pool)
        .await
        .expect("insert integration workspace");

        sqlx::query(
            "INSERT INTO convergences (
                id, project_id, item_id, item_revision_id, source_workspace_id, integration_workspace_id,
                source_head_commit_oid, target_ref, strategy, status, input_target_commit_oid,
                prepared_commit_oid, final_target_commit_oid, conflict_summary, created_at, completed_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, 'refs/heads/main', 'rebase_then_fast_forward', 'prepared', ?, ?, NULL, NULL, '2026-03-12T00:00:00Z', NULL)",
        )
        .bind(&convergence_id)
        .bind(&project_id)
        .bind(&item_id)
        .bind(&revision_id)
        .bind(&workspace_id)
        .bind(&workspace_id)
        .bind(&prepared_commit_oid)
        .bind(&base_commit_oid)
        .bind(&prepared_commit_oid)
        .execute(&db.pool)
        .await
        .expect("insert convergence");

        let app = build_router(db.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/api/projects/{project_id}/items/{item_id}/approval/approve"
                    ))
                    .method("POST")
                    .body(Body::empty())
                    .expect("build request"),
            )
            .await
            .expect("route response");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            git_output(&repo, &["rev-parse", "refs/heads/main"]),
            prepared_commit_oid
        );

        let item_state: (String, String, String) = sqlx::query_as(
            "SELECT lifecycle_state, approval_state, resolution_source FROM items WHERE id = ?",
        )
        .bind(&item_id)
        .fetch_one(&db.pool)
        .await
        .expect("item state");
        assert_eq!(item_state.0, "done");
        assert_eq!(item_state.1, "approved");
        assert_eq!(item_state.2, "approval_command");

        let convergence_state: (String, String) =
            sqlx::query_as("SELECT status, final_target_commit_oid FROM convergences WHERE id = ?")
                .bind(&convergence_id)
                .fetch_one(&db.pool)
                .await
                .expect("convergence state");
        assert_eq!(convergence_state.0, "finalized");
        assert_eq!(convergence_state.1, prepared_commit_oid);
    }

    #[tokio::test]
    async fn prepare_convergence_conflict_escalates_item() {
        let repo = temp_git_repo();
        let base_commit_oid = git_output(&repo, &["rev-parse", "HEAD"]);
        write_file(&repo.join("tracked.txt"), "source change");
        git(&repo, &["add", "tracked.txt"]);
        git(&repo, &["commit", "-m", "source commit"]);
        let source_commit_oid = git_output(&repo, &["rev-parse", "HEAD"]);
        git(
            &repo,
            &[
                "update-ref",
                "refs/ingot/workspaces/wrk_source_conflict",
                &source_commit_oid,
            ],
        );
        git(&repo, &["reset", "--hard", &base_commit_oid]);
        write_file(&repo.join("tracked.txt"), "target change");
        git(&repo, &["add", "tracked.txt"]);
        git(&repo, &["commit", "-m", "target commit"]);

        let db_path = std::env::temp_dir().join(format!("ingot-http-api-db-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let project_id = "prj_00000000000000000000000000000042".to_string();
        let item_id = "itm_00000000000000000000000000000042".to_string();
        let revision_id = "rev_00000000000000000000000000000042".to_string();
        let workspace_id = "wrk_00000000000000000000000000000042".to_string();
        let author_job_id = "job_00000000000000000000000000000042".to_string();
        let validate_job_id = "job_00000000000000000000000000000041".to_string();

        sqlx::query(
            "INSERT INTO projects (id, name, path, default_branch, color, created_at, updated_at)
             VALUES (?, 'Test', ?, 'main', '#000', '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(&project_id)
        .bind(repo.display().to_string())
        .execute(&db.pool)
        .await
        .expect("insert project");

        sqlx::query(
            "INSERT INTO items (
                id, project_id, classification, workflow_version, lifecycle_state, parking_state,
                approval_state, escalation_state, current_revision_id, origin_kind, origin_finding_id,
                priority, labels, created_at, updated_at
             ) VALUES (?, ?, 'change', 'delivery:v1', 'open', 'active', 'not_requested', 'none', ?, 'manual', NULL, 'major', '[]', '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(&item_id)
        .bind(&project_id)
        .bind(&revision_id)
        .execute(&db.pool)
        .await
        .expect("insert item");

        sqlx::query(
            "INSERT INTO item_revisions (
                id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
                approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid,
                seed_target_commit_oid, supersedes_revision_id, created_at
             ) VALUES (?, ?, 1, 'Title', 'Desc', 'AC', 'refs/heads/main', 'required', '{\"workflow_version\":\"delivery:v1\",\"approval_policy\":\"required\",\"candidate_rework_budget\":7,\"integration_rework_budget\":8}', '{\"author_initial\":\"author-initial\"}', ?, ?, NULL, '2026-03-12T00:00:00Z')",
        )
        .bind(&revision_id)
        .bind(&item_id)
        .bind(&base_commit_oid)
        .bind(&base_commit_oid)
        .execute(&db.pool)
        .await
        .expect("insert revision");

        sqlx::query(
            "INSERT INTO workspaces (
                id, project_id, kind, strategy, path, created_for_revision_id, parent_workspace_id,
                target_ref, workspace_ref, base_commit_oid, head_commit_oid, retention_policy,
                status, current_job_id, created_at, updated_at
             ) VALUES (?, ?, 'authoring', 'worktree', ?, ?, NULL, 'refs/heads/main', 'refs/ingot/workspaces/wrk_source_conflict', ?, ?, 'persistent', 'ready', NULL, '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(&workspace_id)
        .bind(&project_id)
        .bind(repo.join("source-conflict").display().to_string())
        .bind(&revision_id)
        .bind(&base_commit_oid)
        .bind(&source_commit_oid)
        .execute(&db.pool)
        .await
        .expect("insert source workspace");

        sqlx::query(
            "INSERT INTO jobs (
                id, project_id, item_id, item_revision_id, step_id, semantic_attempt_no, retry_no,
                status, outcome_class, phase_kind, workspace_id, workspace_kind, execution_permission, context_policy,
                phase_template_slug, output_artifact_kind, output_commit_oid, created_at, ended_at
             ) VALUES (?, ?, ?, ?, 'author_initial', 1, 0, 'completed', 'clean', 'author', ?, 'authoring', 'may_mutate', 'fresh', 'author-initial', 'commit', ?, '2026-03-12T00:00:00Z', '2026-03-12T00:05:00Z')",
        )
        .bind(&author_job_id)
        .bind(&project_id)
        .bind(&item_id)
        .bind(&revision_id)
        .bind(&workspace_id)
        .bind(&source_commit_oid)
        .execute(&db.pool)
        .await
        .expect("insert author job");

        sqlx::query(
            "INSERT INTO jobs (
                id, project_id, item_id, item_revision_id, step_id, semantic_attempt_no, retry_no,
                status, outcome_class, phase_kind, workspace_kind, execution_permission, context_policy,
                phase_template_slug, output_artifact_kind, input_base_commit_oid, input_head_commit_oid,
                result_schema_version, result_payload, created_at, ended_at
             ) VALUES (?, ?, ?, ?, 'validate_candidate_initial', 1, 0, 'completed', 'clean', 'validate', 'authoring', 'must_not_mutate', 'resume_context', 'validate-candidate', 'validation_report', ?, ?, 'validation_report:v1', ?, '2026-03-12T00:06:00Z', '2026-03-12T00:07:00Z')",
        )
        .bind(&validate_job_id)
        .bind(&project_id)
        .bind(&item_id)
        .bind(&revision_id)
        .bind(&base_commit_oid)
        .bind(&source_commit_oid)
        .bind(serde_json::json!({
            "outcome": "clean",
            "summary": "validation clean",
            "checks": [],
            "findings": []
        }).to_string())
        .execute(&db.pool)
        .await
        .expect("insert validate job");

        let app = build_router(db.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/api/projects/{project_id}/items/{item_id}/convergence/prepare"
                    ))
                    .method("POST")
                    .body(Body::empty())
                    .expect("build request"),
            )
            .await
            .expect("route response");

        assert_eq!(response.status(), StatusCode::CONFLICT);
        let item_state: (String, String) =
            sqlx::query_as("SELECT escalation_state, escalation_reason FROM items WHERE id = ?")
                .bind(&item_id)
                .fetch_one(&db.pool)
                .await
                .expect("item state");
        assert_eq!(item_state.0, "operator_required");
        assert_eq!(item_state.1, "convergence_conflict");
    }

    #[tokio::test]
    async fn reject_approval_route_cancels_prepared_convergence_and_creates_superseding_revision() {
        let repo = temp_git_repo();
        let base_commit_oid = git_output(&repo, &["rev-parse", "HEAD"]);
        write_file(&repo.join("tracked.txt"), "candidate change");
        git(&repo, &["add", "tracked.txt"]);
        git(&repo, &["commit", "-m", "candidate"]);
        let candidate_commit_oid = git_output(&repo, &["rev-parse", "HEAD"]);
        git(&repo, &["reset", "--hard", &base_commit_oid]);

        let db_path = std::env::temp_dir().join(format!("ingot-http-api-db-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let project_id = "prj_00000000000000000000000000000077".to_string();
        let item_id = "itm_00000000000000000000000000000077".to_string();
        let revision_id = "rev_00000000000000000000000000000077".to_string();
        let workspace_id = "wrk_00000000000000000000000000000077".to_string();
        let convergence_id = "conv_00000000000000000000000000000077".to_string();
        let author_job_id = "job_00000000000000000000000000000077".to_string();

        sqlx::query(
            "INSERT INTO projects (id, name, path, default_branch, color, created_at, updated_at)
             VALUES (?, 'Test', ?, 'main', '#000', '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(&project_id)
        .bind(repo.display().to_string())
        .execute(&db.pool)
        .await
        .expect("insert project");

        sqlx::query(
            "INSERT INTO items (
                id, project_id, classification, workflow_version, lifecycle_state, parking_state,
                approval_state, escalation_state, current_revision_id, origin_kind, origin_finding_id,
                priority, labels, created_at, updated_at
             ) VALUES (?, ?, 'change', 'delivery:v1', 'open', 'active', 'pending', 'none', ?, 'manual', NULL, 'major', '[]', '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(&item_id)
        .bind(&project_id)
        .bind(&revision_id)
        .execute(&db.pool)
        .await
        .expect("insert item");

        sqlx::query(
            "INSERT INTO item_revisions (
                id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
                approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid,
                seed_target_commit_oid, supersedes_revision_id, created_at
             ) VALUES (?, ?, 1, 'Title', 'Desc', 'AC', 'refs/heads/main', 'required', '{\"workflow_version\":\"delivery:v1\",\"approval_policy\":\"required\",\"candidate_rework_budget\":7,\"integration_rework_budget\":8}', '{\"author_initial\":\"author-initial\"}', ?, ?, NULL, '2026-03-12T00:00:00Z')",
        )
        .bind(&revision_id)
        .bind(&item_id)
        .bind(&base_commit_oid)
        .bind(&base_commit_oid)
        .execute(&db.pool)
        .await
        .expect("insert revision");

        sqlx::query(
            "INSERT INTO workspaces (
                id, project_id, kind, strategy, path, created_for_revision_id, parent_workspace_id,
                target_ref, workspace_ref, base_commit_oid, head_commit_oid, retention_policy,
                status, current_job_id, created_at, updated_at
             ) VALUES (?, ?, 'integration', 'worktree', ?, ?, NULL, 'refs/heads/main', 'refs/ingot/workspaces/wrk_integration_reject', ?, ?, 'persistent', 'ready', NULL, '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(&workspace_id)
        .bind(&project_id)
        .bind(repo.join("integration-reject").display().to_string())
        .bind(&revision_id)
        .bind(&base_commit_oid)
        .bind(&candidate_commit_oid)
        .execute(&db.pool)
        .await
        .expect("insert integration workspace");

        sqlx::query(
            "INSERT INTO jobs (
                id, project_id, item_id, item_revision_id, step_id, semantic_attempt_no, retry_no,
                status, outcome_class, phase_kind, workspace_kind, execution_permission, context_policy,
                phase_template_slug, output_artifact_kind, output_commit_oid, created_at, ended_at
             ) VALUES (?, ?, ?, ?, 'author_initial', 1, 0, 'completed', 'clean', 'author', 'authoring', 'may_mutate', 'fresh', 'author-initial', 'commit', ?, '2026-03-12T00:00:00Z', '2026-03-12T00:05:00Z')",
        )
        .bind(&author_job_id)
        .bind(&project_id)
        .bind(&item_id)
        .bind(&revision_id)
        .bind(&candidate_commit_oid)
        .execute(&db.pool)
        .await
        .expect("insert author job");

        sqlx::query(
            "INSERT INTO convergences (
                id, project_id, item_id, item_revision_id, source_workspace_id, integration_workspace_id,
                source_head_commit_oid, target_ref, strategy, status, input_target_commit_oid,
                prepared_commit_oid, final_target_commit_oid, conflict_summary, created_at, completed_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, 'refs/heads/main', 'rebase_then_fast_forward', 'prepared', ?, ?, NULL, NULL, '2026-03-12T00:00:00Z', NULL)",
        )
        .bind(&convergence_id)
        .bind(&project_id)
        .bind(&item_id)
        .bind(&revision_id)
        .bind(&workspace_id)
        .bind(&workspace_id)
        .bind(&candidate_commit_oid)
        .bind(&base_commit_oid)
        .bind(&candidate_commit_oid)
        .execute(&db.pool)
        .await
        .expect("insert convergence");

        let app = build_router(db.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/api/projects/{project_id}/items/{item_id}/approval/reject"
                    ))
                    .method("POST")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "approval_policy": "not_required"
                        })
                        .to_string(),
                    ))
                    .expect("build request"),
            )
            .await
            .expect("route response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(
            json["item"]["approval_state"].as_str(),
            Some("not_required")
        );
        assert_eq!(json["item"]["lifecycle_state"].as_str(), Some("open"));
        assert_eq!(
            json["current_revision"]["approval_policy"].as_str(),
            Some("not_required")
        );
        assert_eq!(
            json["current_revision"]["policy_snapshot"]["approval_policy"].as_str(),
            Some("not_required")
        );
        assert_eq!(
            json["current_revision"]["policy_snapshot"]["candidate_rework_budget"].as_u64(),
            Some(7)
        );
        assert_ne!(
            json["current_revision"]["id"].as_str(),
            Some(revision_id.as_str())
        );
        assert_eq!(
            json["current_revision"]["supersedes_revision_id"].as_str(),
            Some(revision_id.as_str())
        );
        assert_eq!(
            json["current_revision"]["seed_commit_oid"].as_str(),
            Some(candidate_commit_oid.as_str())
        );

        let revision_policy_snapshot: String = sqlx::query_scalar(
            "SELECT policy_snapshot FROM item_revisions WHERE item_id = ? AND revision_no = 2",
        )
        .bind(&item_id)
        .fetch_one(&db.pool)
        .await
        .expect("load rejected policy snapshot");
        let revision_policy_snapshot: serde_json::Value =
            serde_json::from_str(&revision_policy_snapshot).expect("rejected policy snapshot json");
        assert_eq!(
            revision_policy_snapshot["approval_policy"].as_str(),
            Some("not_required")
        );
        assert_eq!(
            revision_policy_snapshot["candidate_rework_budget"].as_u64(),
            Some(7)
        );

        let convergence_status: String =
            sqlx::query_scalar("SELECT status FROM convergences WHERE id = ?")
                .bind(&convergence_id)
                .fetch_one(&db.pool)
                .await
                .expect("convergence status");
        assert_eq!(convergence_status, "cancelled");
    }

    #[tokio::test]
    async fn retry_route_requeues_terminal_non_success_job_on_current_revision() {
        let repo = temp_git_repo();
        let base_commit_oid = git_output(&repo, &["rev-parse", "HEAD"]);

        let db_path = std::env::temp_dir().join(format!("ingot-http-api-db-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let project_id = "prj_00000000000000000000000000000066".to_string();
        let item_id = "itm_00000000000000000000000000000066".to_string();
        let revision_id = "rev_00000000000000000000000000000066".to_string();
        let job_id = "job_00000000000000000000000000000066".to_string();

        sqlx::query(
            "INSERT INTO projects (id, name, path, default_branch, color, created_at, updated_at)
             VALUES (?, 'Test', ?, 'main', '#000', '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(&project_id)
        .bind(repo.display().to_string())
        .execute(&db.pool)
        .await
        .expect("insert project");

        sqlx::query(
            "INSERT INTO items (
                id, project_id, classification, workflow_version, lifecycle_state, parking_state,
                approval_state, escalation_state, escalation_reason, current_revision_id, origin_kind, origin_finding_id,
                priority, labels, created_at, updated_at
             ) VALUES (?, ?, 'change', 'delivery:v1', 'open', 'active', 'not_requested', 'operator_required', 'step_failed', ?, 'manual', NULL, 'major', '[]', '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(&item_id)
        .bind(&project_id)
        .bind(&revision_id)
        .execute(&db.pool)
        .await
        .expect("insert item");

        sqlx::query(
            "INSERT INTO item_revisions (
                id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
                approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid,
                seed_target_commit_oid, supersedes_revision_id, created_at
             ) VALUES (?, ?, 1, 'Title', 'Desc', 'AC', 'refs/heads/main', 'required', '{}', '{}', ?, ?, NULL, '2026-03-12T00:00:00Z')",
        )
        .bind(&revision_id)
        .bind(&item_id)
        .bind(&base_commit_oid)
        .bind(&base_commit_oid)
        .execute(&db.pool)
        .await
        .expect("insert revision");

        sqlx::query(
            "INSERT INTO jobs (
                id, project_id, item_id, item_revision_id, step_id, semantic_attempt_no, retry_no,
                status, outcome_class, phase_kind, workspace_kind, execution_permission, context_policy,
                phase_template_slug, output_artifact_kind, input_base_commit_oid, input_head_commit_oid,
                error_code, created_at, ended_at
             ) VALUES (?, ?, ?, ?, 'validate_candidate_initial', 1, 0, 'failed', 'terminal_failure', 'validate', 'authoring', 'must_not_mutate', 'resume_context', 'validate-candidate', 'validation_report', ?, ?, 'step_failed', '2026-03-12T00:00:00Z', '2026-03-12T00:05:00Z')",
        )
        .bind(&job_id)
        .bind(&project_id)
        .bind(&item_id)
        .bind(&revision_id)
        .bind(&base_commit_oid)
        .bind(&base_commit_oid)
        .execute(&db.pool)
        .await
        .expect("insert failed job");

        let app = build_router(db.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/api/projects/{project_id}/items/{item_id}/jobs/{job_id}/retry"
                    ))
                    .method("POST")
                    .body(Body::empty())
                    .expect("build request"),
            )
            .await
            .expect("route response");

        assert_eq!(response.status(), StatusCode::CREATED);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(json["step_id"].as_str(), Some("validate_candidate_initial"));
        assert_eq!(json["semantic_attempt_no"].as_u64(), Some(1));
        assert_eq!(json["retry_no"].as_u64(), Some(1));
        assert_eq!(json["supersedes_job_id"].as_str(), Some(job_id.as_str()));
        assert_eq!(json["status"].as_str(), Some("queued"));
    }

    #[tokio::test]
    async fn cancel_route_marks_active_job_cancelled_and_clears_workspace_attachment() {
        let repo = temp_git_repo();
        let base_commit_oid = git_output(&repo, &["rev-parse", "HEAD"]);

        let db_path = std::env::temp_dir().join(format!("ingot-http-api-db-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let project_id = "prj_00000000000000000000000000000065".to_string();
        let item_id = "itm_00000000000000000000000000000065".to_string();
        let revision_id = "rev_00000000000000000000000000000065".to_string();
        let job_id = "job_00000000000000000000000000000065".to_string();
        let workspace_id = "wrk_00000000000000000000000000000065".to_string();

        sqlx::query(
            "INSERT INTO projects (id, name, path, default_branch, color, created_at, updated_at)
             VALUES (?, 'Test', ?, 'main', '#000', '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(&project_id)
        .bind(repo.display().to_string())
        .execute(&db.pool)
        .await
        .expect("insert project");

        sqlx::query(
            "INSERT INTO items (
                id, project_id, classification, workflow_version, lifecycle_state, parking_state,
                approval_state, escalation_state, current_revision_id, origin_kind, origin_finding_id,
                priority, labels, created_at, updated_at
             ) VALUES (?, ?, 'change', 'delivery:v1', 'open', 'active', 'not_requested', 'none', ?, 'manual', NULL, 'major', '[]', '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(&item_id)
        .bind(&project_id)
        .bind(&revision_id)
        .execute(&db.pool)
        .await
        .expect("insert item");

        sqlx::query(
            "INSERT INTO item_revisions (
                id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
                approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid,
                seed_target_commit_oid, supersedes_revision_id, created_at
             ) VALUES (?, ?, 1, 'Title', 'Desc', 'AC', 'refs/heads/main', 'required', '{}', '{}', ?, ?, NULL, '2026-03-12T00:00:00Z')",
        )
        .bind(&revision_id)
        .bind(&item_id)
        .bind(&base_commit_oid)
        .bind(&base_commit_oid)
        .execute(&db.pool)
        .await
        .expect("insert revision");

        sqlx::query(
            "INSERT INTO workspaces (
                id, project_id, kind, strategy, path, created_for_revision_id, parent_workspace_id,
                target_ref, workspace_ref, base_commit_oid, head_commit_oid, retention_policy,
                status, current_job_id, created_at, updated_at
             ) VALUES (?, ?, 'authoring', 'worktree', ?, ?, NULL, 'refs/heads/main', 'refs/ingot/workspaces/wrk_cancel', ?, ?, 'persistent', 'busy', ?, '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(&workspace_id)
        .bind(&project_id)
        .bind(repo.join("cancel-workspace").display().to_string())
        .bind(&revision_id)
        .bind(&base_commit_oid)
        .bind(&base_commit_oid)
        .bind(&job_id)
        .execute(&db.pool)
        .await
        .expect("insert workspace");

        sqlx::query(
            "INSERT INTO jobs (
                id, project_id, item_id, item_revision_id, step_id, semantic_attempt_no, retry_no,
                status, phase_kind, workspace_id, workspace_kind, execution_permission, context_policy,
                phase_template_slug, output_artifact_kind, input_head_commit_oid, created_at
             ) VALUES (?, ?, ?, ?, 'author_initial', 1, 0, 'running', 'author', ?, 'authoring', 'may_mutate', 'fresh', 'author-initial', 'commit', ?, '2026-03-12T00:00:00Z')",
        )
        .bind(&job_id)
        .bind(&project_id)
        .bind(&item_id)
        .bind(&revision_id)
        .bind(&workspace_id)
        .bind(&base_commit_oid)
        .execute(&db.pool)
        .await
        .expect("insert job");

        let app = build_router(db.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/api/projects/{project_id}/items/{item_id}/jobs/{job_id}/cancel"
                    ))
                    .method("POST")
                    .body(Body::empty())
                    .expect("build request"),
            )
            .await
            .expect("route response");

        assert_eq!(response.status(), StatusCode::OK);
        let job_state: (String, String) =
            sqlx::query_as("SELECT status, outcome_class FROM jobs WHERE id = ?")
                .bind(&job_id)
                .fetch_one(&db.pool)
                .await
                .expect("job state");
        assert_eq!(job_state.0, "cancelled");
        assert_eq!(job_state.1, "cancelled");
        let workspace_state: (String, Option<String>) =
            sqlx::query_as("SELECT status, current_job_id FROM workspaces WHERE id = ?")
                .bind(&workspace_id)
                .fetch_one(&db.pool)
                .await
                .expect("workspace state");
        assert_eq!(workspace_state.0, "ready");
        assert_eq!(workspace_state.1, None);
    }

    #[tokio::test]
    async fn reset_workspace_route_restores_authoring_workspace_head() {
        let repo = temp_git_repo();
        let base_commit_oid = git_output(&repo, &["rev-parse", "HEAD"]);
        let workspace_path =
            std::env::temp_dir().join(format!("ingot-http-api-workspace-{}", Uuid::now_v7()));
        git(
            &repo,
            &[
                "update-ref",
                "refs/ingot/workspaces/wrk_reset_test",
                &base_commit_oid,
            ],
        );
        git(
            &repo,
            &[
                "worktree",
                "add",
                "--detach",
                workspace_path.to_str().expect("workspace path"),
                "refs/ingot/workspaces/wrk_reset_test",
            ],
        );
        write_file(&workspace_path.join("tracked.txt"), "changed");

        let db_path = std::env::temp_dir().join(format!("ingot-http-api-db-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let project_id = "prj_00000000000000000000000000000044".to_string();
        let workspace_id = "wrk_00000000000000000000000000000044".to_string();

        sqlx::query(
            "INSERT INTO projects (id, name, path, default_branch, color, created_at, updated_at)
             VALUES (?, 'Test', ?, 'main', '#000', '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(&project_id)
        .bind(repo.display().to_string())
        .execute(&db.pool)
        .await
        .expect("insert project");

        sqlx::query(
            "INSERT INTO workspaces (
                id, project_id, kind, strategy, path, created_for_revision_id, parent_workspace_id,
                target_ref, workspace_ref, base_commit_oid, head_commit_oid, retention_policy,
                status, current_job_id, created_at, updated_at
             ) VALUES (?, ?, 'authoring', 'worktree', ?, NULL, NULL, 'refs/heads/main', 'refs/ingot/workspaces/wrk_reset_test', ?, ?, 'persistent', 'ready', NULL, '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(&workspace_id)
        .bind(&project_id)
        .bind(workspace_path.display().to_string())
        .bind(&base_commit_oid)
        .bind(&base_commit_oid)
        .execute(&db.pool)
        .await
        .expect("insert workspace");

        let app = build_router(db.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/api/projects/{project_id}/workspaces/{workspace_id}/reset"
                    ))
                    .method("POST")
                    .body(Body::empty())
                    .expect("build request"),
            )
            .await
            .expect("route response");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            git_output(&workspace_path, &["rev-parse", "HEAD"]),
            base_commit_oid
        );
        assert_eq!(
            std::fs::read_to_string(workspace_path.join("tracked.txt")).expect("tracked file"),
            "initial"
        );
    }

    #[tokio::test]
    async fn remove_workspace_route_deletes_abandoned_workspace_ref_and_path() {
        let repo = temp_git_repo();
        let head_commit_oid = git_output(&repo, &["rev-parse", "HEAD"]);
        let workspace_path = std::env::temp_dir().join(format!(
            "ingot-http-api-remove-workspace-{}",
            Uuid::now_v7()
        ));
        git(
            &repo,
            &[
                "update-ref",
                "refs/ingot/workspaces/wrk_remove_test",
                &head_commit_oid,
            ],
        );
        git(
            &repo,
            &[
                "worktree",
                "add",
                "--detach",
                workspace_path.to_str().expect("workspace path"),
                "refs/ingot/workspaces/wrk_remove_test",
            ],
        );

        let db_path = std::env::temp_dir().join(format!("ingot-http-api-db-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let project_id = "prj_00000000000000000000000000000043".to_string();
        let workspace_id = "wrk_00000000000000000000000000000043".to_string();

        sqlx::query(
            "INSERT INTO projects (id, name, path, default_branch, color, created_at, updated_at)
             VALUES (?, 'Test', ?, 'main', '#000', '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(&project_id)
        .bind(repo.display().to_string())
        .execute(&db.pool)
        .await
        .expect("insert project");

        sqlx::query(
            "INSERT INTO workspaces (
                id, project_id, kind, strategy, path, created_for_revision_id, parent_workspace_id,
                target_ref, workspace_ref, base_commit_oid, head_commit_oid, retention_policy,
                status, current_job_id, created_at, updated_at
             ) VALUES (?, ?, 'review', 'worktree', ?, NULL, NULL, NULL, 'refs/ingot/workspaces/wrk_remove_test', ?, ?, 'ephemeral', 'abandoned', NULL, '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(&workspace_id)
        .bind(&project_id)
        .bind(workspace_path.display().to_string())
        .bind(&head_commit_oid)
        .bind(&head_commit_oid)
        .execute(&db.pool)
        .await
        .expect("insert workspace");

        let app = build_router(db.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/api/projects/{project_id}/workspaces/{workspace_id}/remove"
                    ))
                    .method("POST")
                    .body(Body::empty())
                    .expect("build request"),
            )
            .await
            .expect("route response");

        assert_eq!(response.status(), StatusCode::OK);
        assert!(!workspace_path.exists());
        let ref_exists = Command::new("git")
            .args([
                "rev-parse",
                "--verify",
                "--quiet",
                "refs/ingot/workspaces/wrk_remove_test",
            ])
            .current_dir(&repo)
            .status()
            .expect("check ref");
        assert!(!ref_exists.success());
    }

    #[tokio::test]
    async fn start_route_marks_job_running_and_sets_lease_fields() {
        let (repo, db, project_id, item_id, seeded_job_id) = seeded_route_test_app().await;
        let start_job_id = "job_00000000000000000000000000000064".to_string();
        let workspace_id = "wrk_00000000000000000000000000000064".to_string();
        let head_commit_oid = git_output(&repo, &["rev-parse", "HEAD"]);
        sqlx::query("DELETE FROM jobs WHERE id = ?")
            .bind(&seeded_job_id)
            .execute(&db.pool)
            .await
            .expect("delete seeded job");

        sqlx::query(
            "INSERT INTO workspaces (
                id, project_id, kind, strategy, path, created_for_revision_id, parent_workspace_id,
                target_ref, workspace_ref, base_commit_oid, head_commit_oid, retention_policy,
                status, current_job_id, created_at, updated_at
             ) VALUES (?, ?, 'authoring', 'worktree', ?, ?, NULL, 'refs/heads/main', 'refs/ingot/workspaces/wrk_start', ?, ?, 'persistent', 'ready', NULL, '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(&workspace_id)
        .bind(&project_id)
        .bind(repo.join("start-workspace").display().to_string())
        .bind("rev_00000000000000000000000000000000")
        .bind(&head_commit_oid)
        .bind(&head_commit_oid)
        .execute(&db.pool)
        .await
        .expect("insert workspace");

        sqlx::query(
            "INSERT INTO jobs (
                id, project_id, item_id, item_revision_id, step_id, semantic_attempt_no, retry_no,
                status, phase_kind, workspace_id, workspace_kind, execution_permission, context_policy,
                phase_template_slug, output_artifact_kind, input_head_commit_oid, created_at
             ) VALUES (?, ?, ?, 'rev_00000000000000000000000000000000', 'author_initial', 1, 0, 'assigned', 'author', ?, 'authoring', 'may_mutate', 'fresh', 'author-initial', 'commit', ?, '2026-03-12T00:00:00Z')",
        )
        .bind(&start_job_id)
        .bind(&project_id)
        .bind(&item_id)
        .bind(&workspace_id)
        .bind(&head_commit_oid)
        .execute(&db.pool)
        .await
        .expect("insert assigned job");

        let app = build_router(db.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/jobs/{start_job_id}/start"))
                    .method("POST")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "lease_owner_id": "ingotd:test",
                            "process_pid": 1234,
                            "lease_duration_seconds": 60
                        })
                        .to_string(),
                    ))
                    .expect("build request"),
            )
            .await
            .expect("route response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(json["status"].as_str(), Some("running"));
        assert_eq!(json["lease_owner_id"].as_str(), Some("ingotd:test"));
        assert_eq!(json["process_pid"].as_u64(), Some(1234));
        assert!(json["started_at"].as_str().is_some());
        assert!(json["heartbeat_at"].as_str().is_some());
    }

    #[tokio::test]
    async fn heartbeat_route_refreshes_running_job_lease() {
        let (_repo, db, project_id, item_id, seeded_job_id) = seeded_route_test_app().await;
        let running_job_id = "job_00000000000000000000000000000063".to_string();
        sqlx::query("DELETE FROM jobs WHERE id = ?")
            .bind(&seeded_job_id)
            .execute(&db.pool)
            .await
            .expect("delete seeded job");

        sqlx::query(
            "INSERT INTO jobs (
                id, project_id, item_id, item_revision_id, step_id, semantic_attempt_no, retry_no,
                status, phase_kind, workspace_kind, execution_permission, context_policy,
                phase_template_slug, output_artifact_kind, lease_owner_id, heartbeat_at, lease_expires_at,
                created_at, started_at
             ) VALUES (?, ?, ?, 'rev_00000000000000000000000000000000', 'author_initial', 1, 0, 'running', 'author', 'authoring', 'may_mutate', 'fresh', 'author-initial', 'commit', 'ingotd:test', '2026-03-12T00:00:00Z', '2026-03-12T00:01:00Z', '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(&running_job_id)
        .bind(&project_id)
        .bind(&item_id)
        .execute(&db.pool)
        .await
        .expect("insert running job");

        let app = build_router(db.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/jobs/{running_job_id}/heartbeat"))
                    .method("POST")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "lease_owner_id": "ingotd:test",
                            "lease_duration_seconds": 120
                        })
                        .to_string(),
                    ))
                    .expect("build request"),
            )
            .await
            .expect("route response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(json["status"].as_str(), Some("running"));
        assert_eq!(json["lease_owner_id"].as_str(), Some("ingotd:test"));
        assert!(json["heartbeat_at"].as_str().is_some());
    }

    #[tokio::test]
    async fn complete_route_rejects_stale_prepared_convergence_after_target_moves() {
        let repo = temp_git_repo();
        let initial_target = git_output(&repo, &["rev-parse", "HEAD"]);
        let db_path = std::env::temp_dir().join(format!("ingot-http-api-db-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");

        let project_id = "prj_00000000000000000000000000000001".to_string();
        let item_id = "itm_00000000000000000000000000000001".to_string();
        let revision_id = "rev_00000000000000000000000000000001".to_string();
        let job_id = "job_00000000000000000000000000000001".to_string();
        let workspace_id = "wrk_00000000000000000000000000000001".to_string();
        let convergence_id = "conv_00000000000000000000000000000001".to_string();

        sqlx::query(
            "INSERT INTO projects (id, name, path, default_branch, color, created_at, updated_at)
             VALUES (?, 'Test', ?, 'main', '#000', '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(&project_id)
        .bind(repo.display().to_string())
        .execute(&db.pool)
        .await
        .expect("insert project");

        sqlx::query(
            "INSERT INTO items (
                id, project_id, classification, workflow_version, lifecycle_state, parking_state,
                approval_state, escalation_state, current_revision_id, origin_kind, origin_finding_id,
                priority, labels, created_at, updated_at
             ) VALUES (?, ?, 'change', 'delivery:v1', 'open', 'active', 'not_requested', 'none', ?, 'manual', NULL, 'major', '[]', '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(&item_id)
        .bind(&project_id)
        .bind(&revision_id)
        .execute(&db.pool)
        .await
        .expect("insert item");

        sqlx::query(
            "INSERT INTO item_revisions (
                id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
                approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid,
                seed_target_commit_oid, supersedes_revision_id, created_at
             ) VALUES (?, ?, 1, 'Title', 'Desc', 'AC', 'refs/heads/main', 'required', '{}', '{}', ?, ?, NULL, '2026-03-12T00:00:00Z')",
        )
        .bind(&revision_id)
        .bind(&item_id)
        .bind(&initial_target)
        .bind(&initial_target)
        .execute(&db.pool)
        .await
        .expect("insert revision");

        sqlx::query(
            "INSERT INTO workspaces (
                id, project_id, kind, strategy, path, created_for_revision_id, retention_policy,
                status, created_at, updated_at
             ) VALUES (?, ?, 'integration', 'worktree', ?, ?, 'ephemeral', 'ready', '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(&workspace_id)
        .bind(&project_id)
        .bind(repo.display().to_string())
        .bind(&revision_id)
        .execute(&db.pool)
        .await
        .expect("insert workspace");

        sqlx::query(
            "INSERT INTO jobs (
                id, project_id, item_id, item_revision_id, step_id, semantic_attempt_no, retry_no,
                status, phase_kind, workspace_kind, execution_permission, context_policy,
                phase_template_slug, output_artifact_kind, input_base_commit_oid, input_head_commit_oid, created_at
             ) VALUES (?, ?, ?, ?, 'validate_integrated', 1, 0, 'running', 'validate', 'integration', 'must_not_mutate', 'resume_context', 'validate-integrated', 'validation_report', ?, ?, '2026-03-12T00:00:00Z')",
        )
        .bind(&job_id)
        .bind(&project_id)
        .bind(&item_id)
        .bind(&revision_id)
        .bind(&initial_target)
        .bind(&initial_target)
        .execute(&db.pool)
        .await
        .expect("insert job");

        sqlx::query(
            "INSERT INTO convergences (
                id, project_id, item_id, item_revision_id, source_workspace_id, integration_workspace_id,
                source_head_commit_oid, target_ref, strategy, status, input_target_commit_oid,
                prepared_commit_oid, final_target_commit_oid, conflict_summary, created_at, completed_at
             ) VALUES (?, ?, ?, ?, ?, NULL, ?, 'refs/heads/main', 'rebase_then_fast_forward', 'prepared', ?, ?, NULL, NULL, '2026-03-12T00:00:00Z', NULL)",
        )
        .bind(&convergence_id)
        .bind(&project_id)
        .bind(&item_id)
        .bind(&revision_id)
        .bind(&workspace_id)
        .bind(&initial_target)
        .bind(&initial_target)
        .bind(&initial_target)
        .bind(&initial_target)
        .execute(&db.pool)
        .await
        .expect("insert convergence");

        write_file(&repo.join("tracked.txt"), "next");
        git(&repo, &["add", "tracked.txt"]);
        git(&repo, &["commit", "-m", "next"]);

        let app = build_router(db.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/jobs/{job_id}/complete"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "outcome_class": "clean",
                            "result_schema_version": "validation_report:v1",
                            "result_payload": {
                                "outcome": "clean",
                                "summary": "ok",
                                "checks": [{
                                    "name": "lint",
                                    "status": "pass",
                                    "summary": "ok"
                                }],
                                "findings": []
                            }
                        })
                        .to_string(),
                    ))
                    .expect("build request"),
            )
            .await
            .expect("complete route response");

        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("error json");

        assert_eq!(
            json["error"]["code"].as_str(),
            Some("prepared_convergence_stale")
        );

        let item_approval_state: String =
            sqlx::query_scalar("SELECT approval_state FROM items WHERE id = ?")
                .bind(&item_id)
                .fetch_one(&db.pool)
                .await
                .expect("item approval state");
        let job_status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id = ?")
            .bind(&job_id)
            .fetch_one(&db.pool)
            .await
            .expect("job status");

        assert_eq!(item_approval_state, "not_requested");
        assert_eq!(job_status, "running");
    }

    #[tokio::test]
    async fn complete_route_clears_item_escalation_after_successful_retry() {
        let repo = temp_git_repo();
        let head_commit = git_output(&repo, &["rev-parse", "HEAD"]);
        let db_path = std::env::temp_dir().join(format!("ingot-http-api-db-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");

        let project_id = "prj_00000000000000000000000000000071".to_string();
        let item_id = "itm_00000000000000000000000000000071".to_string();
        let revision_id = "rev_00000000000000000000000000000071".to_string();
        let failed_job_id = "job_00000000000000000000000000000071".to_string();
        let retry_job_id = "job_00000000000000000000000000000072".to_string();

        sqlx::query(
            "INSERT INTO projects (id, name, path, default_branch, color, created_at, updated_at)
             VALUES (?, 'Test', ?, 'main', '#000', '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(&project_id)
        .bind(repo.display().to_string())
        .execute(&db.pool)
        .await
        .expect("insert project");

        sqlx::query(
            "INSERT INTO items (
                id, project_id, classification, workflow_version, lifecycle_state, parking_state,
                approval_state, escalation_state, escalation_reason, current_revision_id, origin_kind, origin_finding_id,
                priority, labels, created_at, updated_at
             ) VALUES (?, ?, 'change', 'delivery:v1', 'open', 'active', 'not_requested', 'operator_required', 'step_failed', ?, 'manual', NULL, 'major', '[]', '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(&item_id)
        .bind(&project_id)
        .bind(&revision_id)
        .execute(&db.pool)
        .await
        .expect("insert item");

        sqlx::query(
            "INSERT INTO item_revisions (
                id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
                approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid,
                seed_target_commit_oid, supersedes_revision_id, created_at
             ) VALUES (?, ?, 1, 'Title', 'Desc', 'AC', 'refs/heads/main', 'required', '{}', '{}', ?, ?, NULL, '2026-03-12T00:00:00Z')",
        )
        .bind(&revision_id)
        .bind(&item_id)
        .bind(&head_commit)
        .bind(&head_commit)
        .execute(&db.pool)
        .await
        .expect("insert revision");

        sqlx::query(
            "INSERT INTO jobs (
                id, project_id, item_id, item_revision_id, step_id, semantic_attempt_no, retry_no,
                status, outcome_class, phase_kind, workspace_kind, execution_permission, context_policy,
                phase_template_slug, output_artifact_kind, input_base_commit_oid, input_head_commit_oid,
                error_code, created_at, started_at, ended_at
             ) VALUES (?, ?, ?, ?, 'validate_candidate_initial', 1, 0, 'failed', 'terminal_failure', 'validate', 'authoring', 'must_not_mutate', 'resume_context', 'validate-candidate', 'validation_report', ?, ?, 'step_failed', '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z', '2026-03-12T00:01:00Z')",
        )
        .bind(&failed_job_id)
        .bind(&project_id)
        .bind(&item_id)
        .bind(&revision_id)
        .bind(&head_commit)
        .bind(&head_commit)
        .execute(&db.pool)
        .await
        .expect("insert failed job");

        sqlx::query(
            "INSERT INTO jobs (
                id, project_id, item_id, item_revision_id, step_id, semantic_attempt_no, retry_no,
                supersedes_job_id, status, phase_kind, workspace_kind, execution_permission, context_policy,
                phase_template_slug, output_artifact_kind, input_base_commit_oid, input_head_commit_oid,
                created_at, started_at
             ) VALUES (?, ?, ?, ?, 'validate_candidate_initial', 1, 1, ?, 'running', 'validate', 'authoring', 'must_not_mutate', 'resume_context', 'validate-candidate', 'validation_report', ?, ?, '2026-03-12T00:02:00Z', '2026-03-12T00:02:00Z')",
        )
        .bind(&retry_job_id)
        .bind(&project_id)
        .bind(&item_id)
        .bind(&revision_id)
        .bind(&failed_job_id)
        .bind(&head_commit)
        .bind(&head_commit)
        .execute(&db.pool)
        .await
        .expect("insert retry job");

        let app = build_router(db.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/jobs/{retry_job_id}/complete"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "outcome_class": "clean",
                            "result_schema_version": "validation_report:v1",
                            "result_payload": {
                                "outcome": "clean",
                                "summary": "ok",
                                "checks": [{
                                    "name": "lint",
                                    "status": "pass",
                                    "summary": "ok"
                                }],
                                "findings": [],
                                "extensions": null
                            }
                        })
                        .to_string(),
                    ))
                    .expect("build request"),
            )
            .await
            .expect("complete route response");

        assert_eq!(response.status(), StatusCode::OK);

        let item_row: (String, Option<String>) =
            sqlx::query_as("SELECT escalation_state, escalation_reason FROM items WHERE id = ?")
                .bind(&item_id)
                .fetch_one(&db.pool)
                .await
                .expect("load item escalation");
        assert_eq!(item_row.0, "none");
        assert_eq!(item_row.1, None);

        let activity = db
            .list_activity_by_project(ProjectId::from_str(&project_id).expect("project id"), 20, 0)
            .await
            .expect("list activity");
        assert!(activity.iter().any(|entry| {
            entry.event_type == ActivityEventType::ItemEscalationCleared
                && entry.entity_id == item_id
        }));
    }

    fn temp_git_repo() -> PathBuf {
        let path = std::env::temp_dir().join(format!("ingot-http-api-{}", Uuid::now_v7()));
        fs::create_dir_all(&path).expect("create temp repo dir");
        git(&path, &["init"]);
        git(&path, &["branch", "-M", "main"]);
        git(&path, &["config", "user.name", "Ingot Test"]);
        git(&path, &["config", "user.email", "ingot@example.com"]);
        write_file(&path.join("tracked.txt"), "initial");
        git(&path, &["add", "tracked.txt"]);
        git(&path, &["commit", "-m", "initial"]);
        path
    }

    fn fake_codex_probe_script() -> PathBuf {
        let path = std::env::temp_dir().join(format!("ingot-fake-codex-{}.sh", Uuid::now_v7()));
        fs::write(
            &path,
            r#"#!/bin/sh
if [ "$1" = "exec" ] && [ "$2" = "--help" ]; then
  cat <<'EOF'
Usage: codex exec [OPTIONS] [PROMPT] [COMMAND]
  -s, --sandbox <SANDBOX_MODE>
  -C, --cd <DIR>
      --output-schema <FILE>
      --json
  -o, --output-last-message <FILE>
EOF
  exit 0
fi
echo "unexpected arguments: $@" >&2
exit 1
"#,
        )
        .expect("write fake codex");
        let mut permissions = fs::metadata(&path)
            .expect("fake codex metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).expect("chmod fake codex");
        path
    }

    fn git(path: &PathBuf, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .expect("run git");

        assert!(status.success(), "git {:?} failed", args);
    }

    fn git_output(path: &PathBuf, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("run git output");

        assert!(output.status.success(), "git {:?} failed", args);
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn write_file(path: &PathBuf, contents: &str) {
        fs::write(path, contents).expect("write file");
    }

    async fn seeded_route_test_app() -> (PathBuf, Database, String, String, String) {
        let repo = temp_git_repo();
        let db_path = std::env::temp_dir().join(format!("ingot-http-api-db-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");

        let project_id = "prj_00000000000000000000000000000000".to_string();
        let item_id = "itm_00000000000000000000000000000000".to_string();
        let revision_id = "rev_00000000000000000000000000000000".to_string();
        let job_id = "job_00000000000000000000000000000000".to_string();

        sqlx::query(
            "INSERT INTO projects (id, name, path, default_branch, color, created_at, updated_at)
             VALUES (?, 'Test', ?, 'main', '#000', '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(&project_id)
        .bind(repo.display().to_string())
        .execute(&db.pool)
        .await
        .expect("insert project");

        sqlx::query(
            "INSERT INTO items (
                id, project_id, classification, workflow_version, lifecycle_state, parking_state,
                approval_state, escalation_state, current_revision_id, origin_kind, origin_finding_id,
                priority, labels, created_at, updated_at
             ) VALUES (?, ?, 'change', 'delivery:v1', 'open', 'active', 'not_requested', 'none', ?, 'manual', NULL, 'major', '[]', '2026-03-12T00:00:00Z', '2026-03-12T00:00:00Z')",
        )
        .bind(&item_id)
        .bind(&project_id)
        .bind(&revision_id)
        .execute(&db.pool)
        .await
        .expect("insert item");

        sqlx::query(
            "INSERT INTO item_revisions (
                id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
                approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid,
                seed_target_commit_oid, supersedes_revision_id, created_at
             ) VALUES (?, ?, 1, 'Title', 'Desc', 'AC', 'refs/heads/main', 'required', '{}', '{}', 'base', 'target', NULL, '2026-03-12T00:00:00Z')",
        )
        .bind(&revision_id)
        .bind(&item_id)
        .execute(&db.pool)
        .await
        .expect("insert revision");

        sqlx::query(
            "INSERT INTO jobs (
                id, project_id, item_id, item_revision_id, step_id, semantic_attempt_no, retry_no,
                status, phase_kind, workspace_kind, execution_permission, context_policy,
                phase_template_slug, output_artifact_kind, input_base_commit_oid, input_head_commit_oid, created_at
             ) VALUES (?, ?, ?, ?, 'validate_candidate_initial', 1, 0, 'running', 'validate', 'authoring', 'must_not_mutate', 'resume_context', 'validate-candidate', 'validation_report', 'base', 'head', '2026-03-12T00:00:00Z')",
        )
        .bind(&job_id)
        .bind(&project_id)
        .bind(&item_id)
        .bind(&revision_id)
        .execute(&db.pool)
        .await
        .expect("insert job");

        (repo, db, project_id, item_id, job_id)
    }

    fn test_project(path: PathBuf) -> Project {
        Project {
            id: ProjectId::from_uuid(Uuid::nil()),
            name: "Test".into(),
            path: path.display().to_string(),
            default_branch: "main".into(),
            color: "#000000".into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn test_prepared_convergence() -> Convergence {
        Convergence {
            id: ConvergenceId::from_uuid(Uuid::now_v7()),
            project_id: ProjectId::from_uuid(Uuid::nil()),
            item_id: ItemId::from_uuid(Uuid::nil()),
            item_revision_id: ItemRevisionId::from_uuid(Uuid::nil()),
            source_workspace_id: WorkspaceId::from_uuid(Uuid::now_v7()),
            integration_workspace_id: Some(WorkspaceId::from_uuid(Uuid::now_v7())),
            source_head_commit_oid: "head".into(),
            target_ref: "refs/heads/main".into(),
            strategy: ConvergenceStrategy::RebaseThenFastForward,
            status: ConvergenceStatus::Prepared,
            input_target_commit_oid: Some("base".into()),
            prepared_commit_oid: Some("prepared".into()),
            final_target_commit_oid: None,
            target_head_valid: Some(true),
            conflict_summary: None,
            created_at: Utc::now(),
            completed_at: None,
        }
    }

    fn test_finding() -> Finding {
        Finding {
            id: FindingId::new(),
            project_id: ProjectId::from_uuid(Uuid::nil()),
            source_item_id: ItemId::from_uuid(Uuid::nil()),
            source_item_revision_id: ItemRevisionId::from_uuid(Uuid::nil()),
            source_job_id: JobId::from_uuid(Uuid::nil()),
            source_step_id: "investigate_item".into(),
            source_report_schema_version: "finding_report:v1".into(),
            source_finding_key: "finding-1".into(),
            source_subject_kind: FindingSubjectKind::Candidate,
            source_subject_base_commit_oid: None,
            source_subject_head_commit_oid: "head".into(),
            code: "BUG001".into(),
            severity: FindingSeverity::High,
            summary: "summary".into(),
            paths: vec!["src/lib.rs".into()],
            evidence: serde_json::json!(["evidence"]),
            triage_state: FindingTriageState::Untriaged,
            linked_item_id: None,
            triage_note: None,
            created_at: Utc::now(),
            triaged_at: None,
        }
    }
}
