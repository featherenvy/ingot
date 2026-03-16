mod agents;
mod convergence;
mod dispatch;
mod findings;
mod harness;
mod items;
mod jobs;
mod projects;
pub(super) mod support;
#[cfg(test)]
mod test_helpers;
pub(super) mod types;
mod workspaces;

use items::RevisionLaneTeardown;
pub(crate) use items::{append_activity, load_effective_config};
use support::*;
pub(crate) use support::{
    ensure_git_valid_target_ref, git_to_internal, parse_config_approval_policy, repo_to_internal,
    repo_to_project_mutation, resolve_default_branch,
};

use std::path::Path as FsPath;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use chrono::Utc;
use ingot_agent_adapters::registry::{default_agent_capabilities, probe_and_apply};
use ingot_config::IngotConfig;
use ingot_config::loader::load_config;
use ingot_domain::activity::{Activity, ActivityEventType};
use ingot_domain::agent::{Agent, AgentStatus};

use ingot_domain::convergence::Convergence;
use ingot_domain::convergence_queue::{ConvergenceQueueEntry, ConvergenceQueueEntryStatus};
use ingot_domain::finding::{Finding, FindingTriageState};
use ingot_domain::git_operation::{GitEntityType, GitOperation, GitOperationStatus, OperationKind};
use ingot_domain::ids::{AgentId, FindingId, ItemId, JobId, ProjectId, WorkspaceId};
use ingot_domain::item::{
    ApprovalState, Classification, DoneReason, Escalation, EscalationReason, Item, Lifecycle,
    Priority, ResolutionSource,
};
use ingot_domain::job::{Job, JobAssignment, JobStatus, OutcomeClass};
use ingot_domain::ports::{ProjectMutationLockPort, RepositoryError};
use ingot_domain::project::Project;
use ingot_domain::revision::{ApprovalPolicy, ItemRevision};
use ingot_domain::workspace::{Workspace, WorkspaceKind, WorkspaceStatus};
use ingot_git::GitJobCompletionPort;
use ingot_git::commands::{
    delete_ref, git, is_commit_reachable_from_any_ref, resolve_ref_oid, update_ref,
};
use ingot_git::commit::{
    ConvergenceCommitTrailers, abort_cherry_pick, cherry_pick_no_commit, commit_message,
    create_daemon_convergence_commit, list_commits_oldest_first, working_tree_has_changes,
};
use ingot_git::diff::changed_paths_between;
use ingot_git::project_repo::{CheckoutSyncStatus, checkout_sync_status, project_repo_paths};
use ingot_store_sqlite::{Database, StartJobExecutionParams};
use ingot_usecases::convergence::{
    ConvergenceCommandPort, ConvergenceService, ConvergenceSystemActionPort,
};
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
    CompleteJobCommand, CompleteJobService, ProjectLocks, UseCaseError, rebuild_revision_context,
};
use ingot_workflow::{AllowedAction, Evaluation, Evaluator, PhaseStatus, RecommendedAction, step};
use ingot_workspace::{
    ensure_authoring_workspace_state, provision_integration_workspace, provision_review_workspace,
    remove_workspace,
};
use tracing::warn;

use crate::error::ApiError;

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) db: Database,
    complete_job_service: CompleteJobService<Database, GitJobCompletionPort, ProjectLocks>,
    pub(crate) project_locks: ProjectLocks,
    state_root: PathBuf,
}

/// Build the Axum router with all API routes.
pub fn build_router(db: Database) -> Router {
    build_router_with_project_locks_and_state_root(
        db,
        ProjectLocks::default(),
        default_state_root(),
    )
}

pub fn build_router_with_project_locks(db: Database, project_locks: ProjectLocks) -> Router {
    build_router_with_project_locks_and_state_root(db, project_locks, default_state_root())
}

pub fn build_router_with_project_locks_and_state_root(
    db: Database,
    project_locks: ProjectLocks,
    state_root: PathBuf,
) -> Router {
    let repo_path_resolver_root = state_root.clone();
    let state = AppState {
        db: db.clone(),
        complete_job_service: CompleteJobService::with_repo_path_resolver(
            db,
            GitJobCompletionPort,
            project_locks.clone(),
            Arc::new(move |project: &Project| {
                project_repo_paths(
                    repo_path_resolver_root.as_path(),
                    project.id,
                    FsPath::new(&project.path),
                )
                .mirror_git_dir
            }),
        ),
        project_locks,
        state_root,
    };

    Router::new()
        .route("/api/health", get(health))
        .route("/api/config", get(get_global_config))
        // Project routes
        .route(
            "/api/projects",
            get(projects::list_projects).post(projects::create_project),
        )
        .route("/api/demo-project", post(crate::demo::create_demo_project))
        .route(
            "/api/projects/{project_id}/activity",
            get(projects::list_project_activity),
        )
        .route(
            "/api/projects/{project_id}/workspaces",
            get(projects::list_project_workspaces),
        )
        .route(
            "/api/projects/{project_id}",
            put(projects::update_project).delete(projects::delete_project),
        )
        .route(
            "/api/projects/{project_id}/config",
            get(projects::get_project_config),
        )
        .route(
            "/api/projects/{project_id}/harness",
            get(harness::get_harness_profile).put(harness::put_harness_profile),
        )
        .route(
            "/api/projects/{project_id}/jobs",
            get(projects::list_project_jobs),
        )
        // Workspace routes
        .route(
            "/api/projects/{project_id}/workspaces/{workspace_id}/reset",
            post(workspaces::reset_workspace_route),
        )
        .route(
            "/api/projects/{project_id}/workspaces/{workspace_id}/abandon",
            post(workspaces::abandon_workspace_route),
        )
        .route(
            "/api/projects/{project_id}/workspaces/{workspace_id}/remove",
            post(workspaces::remove_workspace_route),
        )
        // Agent routes
        .route(
            "/api/agents",
            get(agents::list_agents).post(agents::create_agent),
        )
        .route(
            "/api/agents/{agent_id}",
            put(agents::update_agent).delete(agents::delete_agent),
        )
        .route(
            "/api/agents/{agent_id}/reprobe",
            post(agents::reprobe_agent),
        )
        // Item routes
        .route(
            "/api/projects/{project_id}/items",
            get(items::list_items).post(items::create_item),
        )
        .route(
            "/api/projects/{project_id}/items/{item_id}",
            get(items::get_item).patch(items::update_item),
        )
        .route(
            "/api/projects/{project_id}/items/{item_id}/revise",
            post(items::revise_item),
        )
        .route(
            "/api/projects/{project_id}/items/{item_id}/defer",
            post(items::defer_item),
        )
        .route(
            "/api/projects/{project_id}/items/{item_id}/resume",
            post(items::resume_item),
        )
        .route(
            "/api/projects/{project_id}/items/{item_id}/dismiss",
            post(items::dismiss_item),
        )
        .route(
            "/api/projects/{project_id}/items/{item_id}/invalidate",
            post(items::invalidate_item),
        )
        .route(
            "/api/projects/{project_id}/items/{item_id}/reopen",
            post(items::reopen_item),
        )
        .route(
            "/api/projects/{project_id}/items/{item_id}/findings",
            get(items::list_item_findings),
        )
        // Job routes
        .route(
            "/api/projects/{project_id}/items/{item_id}/jobs",
            post(dispatch::dispatch_item_job),
        )
        .route(
            "/api/projects/{project_id}/items/{item_id}/jobs/{job_id}/retry",
            post(dispatch::retry_item_job),
        )
        .route(
            "/api/projects/{project_id}/items/{item_id}/jobs/{job_id}/cancel",
            post(jobs::cancel_item_job),
        )
        .route("/api/jobs/{job_id}/assign", post(jobs::assign_job))
        .route("/api/jobs/{job_id}/start", post(jobs::start_job))
        .route("/api/jobs/{job_id}/heartbeat", post(jobs::heartbeat_job))
        .route("/api/jobs/{job_id}/logs", get(jobs::get_job_logs))
        .route("/api/jobs/{job_id}/complete", post(jobs::complete_job))
        .route("/api/jobs/{job_id}/fail", post(jobs::fail_job))
        .route("/api/jobs/{job_id}/expire", post(jobs::expire_job))
        // Finding routes
        .route("/api/findings/{finding_id}", get(findings::get_finding))
        .route(
            "/api/findings/{finding_id}/triage",
            post(findings::triage_item_finding),
        )
        .route(
            "/api/findings/{finding_id}/promote",
            post(findings::promote_item_from_finding),
        )
        .route(
            "/api/findings/{finding_id}/dismiss",
            post(findings::dismiss_item_finding),
        )
        // Convergence routes
        .route(
            "/api/projects/{project_id}/items/{item_id}/convergence/prepare",
            post(convergence::prepare_item_convergence),
        )
        .route(
            "/api/projects/{project_id}/items/{item_id}/approval/approve",
            post(convergence::approve_item),
        )
        .route(
            "/api/projects/{project_id}/items/{item_id}/approval/reject",
            post(convergence::reject_item_approval),
        )
        .with_state(state)
}

pub(super) async fn health() -> &'static str {
    "ok"
}

pub(super) async fn get_global_config() -> Result<Json<IngotConfig>, ApiError> {
    Ok(Json(load_effective_config(None)?))
}

pub(super) async fn teardown_revision_lane_state(
    state: &AppState,
    project: &Project,
    item_id: ItemId,
    revision: &ItemRevision,
) -> Result<RevisionLaneTeardown, ApiError> {
    let paths = refresh_project_mirror(state, project).await?;
    let mirror_git_dir = paths.mirror_git_dir.as_path();

    let uc_result = ingot_usecases::teardown::teardown_revision_lane(
        &state.db, &state.db, &state.db, &state.db, &state.db, &state.db, project.id, item_id,
        revision,
    )
    .await?;

    let item = state.db.get_item(item_id).await.map_err(repo_to_item)?;
    jobs::refresh_revision_context_for_job_like(state, &item, revision, mirror_git_dir).await?;

    // Perform filesystem cleanup for integration workspaces
    for workspace_id in &uc_result.integration_workspace_ids {
        let workspace = state
            .db
            .get_workspace(*workspace_id)
            .await
            .map_err(repo_to_internal)?;
        if PathBuf::from(&workspace.path).exists() {
            let _ = ingot_workspace::remove_workspace(mirror_git_dir, FsPath::new(&workspace.path))
                .await;
        }
    }

    // Map to the existing RevisionLaneTeardown type for backward compat with callers
    Ok(RevisionLaneTeardown {
        cancelled_job_ids: uc_result
            .cancelled_job_ids
            .iter()
            .map(ToString::to_string)
            .collect(),
        cancelled_convergence_ids: uc_result
            .cancelled_convergence_ids
            .iter()
            .map(ToString::to_string)
            .collect(),
        cancelled_queue_entry_ids: uc_result
            .cancelled_queue_entry_ids
            .iter()
            .map(ToString::to_string)
            .collect(),
        reconciled_prepare_operation_ids: uc_result
            .reconciled_git_operation_ids
            .iter()
            .map(ToString::to_string)
            .collect(),
        failed_finalize_operation_ids: uc_result
            .failed_git_operation_ids
            .iter()
            .map(ToString::to_string)
            .collect(),
    })
}

#[cfg(not(test))]
pub(super) fn default_state_root() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".ingot")
}

#[cfg(test)]
fn default_state_root() -> PathBuf {
    std::env::temp_dir().join(format!("ingot-http-api-state-{}", uuid::Uuid::now_v7()))
}
