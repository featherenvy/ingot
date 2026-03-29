use super::deps::*;
use super::support::{
    errors::{ensure_workspace_not_busy, repo_to_internal, repo_to_project},
    path::ApiPath,
};
use super::types::*;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
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
}

pub(super) async fn reset_workspace_route(
    State(state): State<AppState>,
    ApiPath(ProjectWorkspacePathParams {
        project_id,
        workspace_id,
    }): ApiPath<ProjectWorkspacePathParams>,
) -> Result<Json<Workspace>, ApiError> {
    state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;
    let _guard = state
        .project_locks
        .acquire_project_mutation(project_id)
        .await;
    let workspace = load_available_workspace(&state, project_id, workspace_id).await?;
    let infra = state.infra();
    let workspace = ingot_usecases::workspace::reset_workspace(
        &state.db, &state.db, &state.db, &infra, project_id, &workspace,
    )
    .await?;

    Ok(Json(workspace))
}

pub(super) async fn abandon_workspace_route(
    State(state): State<AppState>,
    ApiPath(ProjectWorkspacePathParams {
        project_id,
        workspace_id,
    }): ApiPath<ProjectWorkspacePathParams>,
) -> Result<Json<Workspace>, ApiError> {
    state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;
    let _guard = state
        .project_locks
        .acquire_project_mutation(project_id)
        .await;
    let workspace = load_available_workspace(&state, project_id, workspace_id).await?;
    let workspace = ingot_usecases::workspace::abandon_workspace(&state.db, &workspace).await?;
    Ok(Json(workspace))
}

pub(super) async fn remove_workspace_route(
    State(state): State<AppState>,
    ApiPath(ProjectWorkspacePathParams {
        project_id,
        workspace_id,
    }): ApiPath<ProjectWorkspacePathParams>,
) -> Result<Json<Workspace>, ApiError> {
    state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;
    let _guard = state
        .project_locks
        .acquire_project_mutation(project_id)
        .await;
    let workspace = load_available_workspace(&state, project_id, workspace_id).await?;
    let infra = state.infra();
    let workspace = ingot_usecases::workspace::remove_workspace_full(
        &state.db, &state.db, &state.db, &infra, project_id, &workspace,
    )
    .await?;

    Ok(Json(workspace))
}

async fn load_available_workspace(
    state: &AppState,
    project_id: ProjectId,
    workspace_id: WorkspaceId,
) -> Result<Workspace, ApiError> {
    let workspace = state
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
    Ok(workspace)
}
