use std::fmt::Display;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::Method;
use axum::middleware;
use axum::response::Response;
use ingot_config::paths::default_state_root;
use ingot_domain::ids::ItemId;
use ingot_domain::project::Project;
use ingot_domain::revision::ItemRevision;
use ingot_git::GitJobCompletionPort;
use ingot_git::project_repo::project_repo_paths_for_project;
use ingot_store_sqlite::Database;
use ingot_usecases::{CompleteJobService, DispatchNotify, ProjectLocks, UiEventBus};

use crate::error::ApiError;

use super::infra_ports::HttpInfraAdapter;
use super::items::{self, RevisionLaneTeardown};
use super::jobs;
use super::support::errors::{repo_to_internal, repo_to_item};
use super::{agents, convergence, core, dispatch, findings, harness, projects, workspaces};

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) db: Database,
    pub(crate) complete_job_service:
        CompleteJobService<Database, GitJobCompletionPort, ProjectLocks>,
    pub(crate) project_locks: ProjectLocks,
    pub(crate) dispatch_notify: DispatchNotify,
    pub(crate) ui_events: UiEventBus,
    pub(crate) state_root: PathBuf,
}

impl AppState {
    pub(super) fn infra(&self) -> HttpInfraAdapter {
        HttpInfraAdapter::new(self)
    }

    pub(super) fn job_logs_dir(&self, job_id: impl Display) -> PathBuf {
        ingot_config::paths::job_logs_dir(self.state_root.as_path(), job_id)
    }
}

/// Build the Axum router with all API routes.
pub fn build_router(db: Database) -> Router {
    build_router_with_project_locks_and_state_root_and_events(
        db,
        ProjectLocks::default(),
        default_state_root(),
        DispatchNotify::default(),
        UiEventBus::default(),
    )
}

pub fn build_router_with_project_locks(db: Database, project_locks: ProjectLocks) -> Router {
    build_router_with_project_locks_and_state_root_and_events(
        db,
        project_locks,
        default_state_root(),
        DispatchNotify::default(),
        UiEventBus::default(),
    )
}

pub fn build_router_with_project_locks_and_state_root(
    db: Database,
    project_locks: ProjectLocks,
    state_root: PathBuf,
    dispatch_notify: DispatchNotify,
) -> Router {
    build_router_with_project_locks_and_state_root_and_events(
        db,
        project_locks,
        state_root,
        dispatch_notify,
        UiEventBus::default(),
    )
}

pub fn build_router_with_project_locks_and_state_root_and_events(
    db: Database,
    project_locks: ProjectLocks,
    state_root: PathBuf,
    dispatch_notify: DispatchNotify,
    ui_events: UiEventBus,
) -> Router {
    let repo_path_resolver_root = state_root.clone();
    let state = AppState {
        db: db.clone(),
        complete_job_service: CompleteJobService::with_repo_path_resolver(
            db,
            GitJobCompletionPort,
            project_locks.clone(),
            Arc::new(move |project: &Project| {
                project_repo_paths_for_project(repo_path_resolver_root.as_path(), project)
                    .mirror_git_dir
            }),
        ),
        project_locks,
        dispatch_notify,
        ui_events,
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
        .merge(super::ws::routes())
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
/// Over-notification is harmless because the dispatcher drains until idle.
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

pub(crate) async fn teardown_revision_lane_state(
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

    let infra = state.infra();
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
