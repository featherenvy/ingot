mod agents;
mod convergence;
mod convergence_port;
mod core;
mod dispatch;
mod findings;
mod harness;
mod infra_ports;
mod item_projection;
mod items;
mod jobs;
mod projects;
pub(super) mod support;
#[cfg(test)]
mod test_helpers;
pub(super) mod types;
mod workspaces;

use items::RevisionLaneTeardown;
use support::*;
pub(crate) use support::{append_activity, load_effective_config};
pub(crate) use support::{
    ensure_git_valid_target_ref, git_to_internal, repo_to_internal, repo_to_project_mutation,
    resolve_default_branch,
};

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Query, Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware;
use axum::response::Response;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use chrono::Utc;
use infra_ports::HttpInfraAdapter;
use ingot_agent_adapters::registry::{default_agent_capabilities, probe_and_apply};
#[cfg(not(test))]
use ingot_config::paths::default_state_root as shared_default_state_root;
use ingot_domain::activity::{Activity, ActivityEventType, ActivitySubject};
use ingot_domain::agent::{Agent, AgentStatus};

use ingot_domain::commit_oid::CommitOid;
use ingot_domain::convergence::Convergence;
use ingot_domain::convergence_queue::{ConvergenceQueueEntry, ConvergenceQueueEntryStatus};
use ingot_domain::finding::{Finding, FindingTriageState};
use ingot_domain::git_operation::GitOperation;
use ingot_domain::git_ref::GitRef;
use ingot_domain::ids::{AgentId, FindingId, ItemId, JobId, ProjectId, WorkspaceId};
use ingot_domain::item::{
    ApprovalState, Classification, DoneReason, Escalation, EscalationReason, Item, Lifecycle,
    Priority, ResolutionSource,
};
use ingot_domain::job::{Job, JobStatus, OutcomeClass};
use ingot_domain::ports::{ProjectMutationLockPort, RepositoryError};
use ingot_domain::project::Project;
use ingot_domain::revision::{ApprovalPolicy, AuthoringBaseSeed, ItemRevision};
use ingot_domain::workspace::{Workspace, WorkspaceKind, WorkspaceStatus};
use ingot_git::GitJobCompletionPort;
use ingot_git::project_repo::{CheckoutSyncStatus, project_repo_paths};
use ingot_store_sqlite::Database;
use ingot_usecases::convergence::{
    ConvergenceCommandPort, ConvergenceService, ConvergenceSystemActionPort,
};
use ingot_usecases::finding::{
    BacklogFindingOverrides, TriageFindingInput, backlog_finding, parse_revision_context_summary,
    triage_finding,
};
use ingot_usecases::item::{
    CreateItemInput, approval_state_for_policy, create_manual_item, normalize_target_ref,
};
use ingot_usecases::{
    CompleteJobCommand, CompleteJobService, DispatchNotify, ProjectLocks, UseCaseError,
    rebuild_revision_context,
};
use ingot_workflow::{
    AllowedAction, Evaluation, Evaluator, NamedRecommendedAction, PhaseStatus, RecommendedAction,
    step,
};
use tracing::warn;

use crate::error::ApiError;

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) db: Database,
    complete_job_service: CompleteJobService<Database, GitJobCompletionPort, ProjectLocks>,
    pub(crate) project_locks: ProjectLocks,
    pub(crate) dispatch_notify: DispatchNotify,
    state_root: PathBuf,
}

/// Build the Axum router with all API routes.
pub fn build_router(db: Database) -> Router {
    build_router_with_project_locks_and_state_root(
        db,
        ProjectLocks::default(),
        default_state_root(),
        DispatchNotify::default(),
    )
}

pub fn build_router_with_project_locks(db: Database, project_locks: ProjectLocks) -> Router {
    build_router_with_project_locks_and_state_root(
        db,
        project_locks,
        default_state_root(),
        DispatchNotify::default(),
    )
}

pub fn build_router_with_project_locks_and_state_root(
    db: Database,
    project_locks: ProjectLocks,
    state_root: PathBuf,
    dispatch_notify: DispatchNotify,
) -> Router {
    let repo_path_resolver_root = state_root.clone();
    let state = AppState {
        db: db.clone(),
        complete_job_service: CompleteJobService::with_repo_path_resolver(
            db,
            GitJobCompletionPort,
            project_locks.clone(),
            Arc::new(move |project: &Project| {
                project_repo_paths(repo_path_resolver_root.as_path(), project.id, &project.path)
                    .mirror_git_dir
            }),
        ),
        project_locks,
        dispatch_notify,
        state_root,
    };

    Router::new()
        .merge(core::routes())
        .merge(projects::routes())
        .merge(harness::routes())
        .merge(workspaces::routes())
        .merge(agents::routes())
        .merge(items::routes())
        .merge(dispatch::routes())
        .merge(jobs::routes())
        .merge(findings::routes())
        .merge(convergence::routes())
        .layer(middleware::from_fn_with_state(
            state.clone(),
            dispatch_notify_layer,
        ))
        .with_state(state)
}

/// Wakes the background dispatcher after every successful write request.
///
/// Applied to routes that create dispatchable work. Write methods (POST, PUT,
/// PATCH, DELETE) that return 2xx trigger `dispatch_notify.notify()`.
/// Over-notification is harmless — the dispatcher drains until idle, so
/// spurious wakeups just re-enter the drain loop. This eliminates the class
/// of bugs where a handler forgets to notify.
///
/// Excludes HEAD (auto-served by Axum for GET routes) and GET to avoid waking
/// the dispatcher on read-only requests like health probes.
async fn dispatch_notify_layer(
    State(state): State<AppState>,
    request: Request,
    next: middleware::Next,
) -> Response {
    let should_notify = is_dispatch_write(request.method());
    let notify_reason =
        should_notify.then(|| format!("http {} {}", request.method(), request.uri().path()));
    let response = next.run(request).await;
    if should_notify && response.status().is_success() {
        state.dispatch_notify.notify_with_reason(
            notify_reason.expect("write requests should always have a notify reason"),
        );
    }
    response
}

fn is_dispatch_write(method: &Method) -> bool {
    matches!(
        method,
        &Method::POST | &Method::PUT | &Method::PATCH | &Method::DELETE
    )
}

pub(super) async fn teardown_revision_lane_state(
    state: &AppState,
    project: &Project,
    item_id: ItemId,
    revision: &ItemRevision,
) -> Result<RevisionLaneTeardown, ApiError> {
    let uc_result = ingot_usecases::teardown::teardown_revision_lane(
        &state.db, &state.db, &state.db, &state.db, &state.db, &state.db, project.id, item_id,
        revision,
    )
    .await?;

    let item = state.db.get_item(item_id).await.map_err(repo_to_item)?;
    jobs::refresh_revision_context_for_job_like(state, &item, revision).await?;

    // Perform filesystem cleanup for integration workspaces
    let infra = HttpInfraAdapter::new(state);
    for workspace_id in &uc_result.integration_workspace_ids {
        let workspace = state
            .db
            .get_workspace(*workspace_id)
            .await
            .map_err(repo_to_internal)?;
        if workspace.path.exists() {
            let _ = infra
                .remove_workspace_path(project.id, &workspace.path)
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
    shared_default_state_root()
}

#[cfg(test)]
fn default_state_root() -> PathBuf {
    std::env::temp_dir().join(format!("ingot-http-api-state-{}", uuid::Uuid::now_v7()))
}
