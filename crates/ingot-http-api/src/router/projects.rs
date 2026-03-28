use super::support::load_effective_config;
use super::support::*;
use super::types::*;
use super::*;

pub(super) async fn list_project_activity(
    State(state): State<AppState>,
    ApiPath(ProjectPathParams { project_id }): ApiPath<ProjectPathParams>,
    Query(query): Query<ActivityQuery>,
) -> Result<Json<Vec<Activity>>, ApiError> {
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

pub(super) async fn list_project_workspaces(
    State(state): State<AppState>,
    ApiPath(ProjectPathParams { project_id }): ApiPath<ProjectPathParams>,
) -> Result<Json<Vec<Workspace>>, ApiError> {
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

pub(super) async fn list_projects(
    State(state): State<AppState>,
) -> Result<Json<Vec<Project>>, ApiError> {
    let projects = state.db.list_projects().await.map_err(repo_to_internal)?;
    Ok(Json(projects))
}

pub(super) async fn create_project(
    State(state): State<AppState>,
    Json(request): Json<CreateProjectRequest>,
) -> Result<(StatusCode, Json<Project>), ApiError> {
    let path = canonicalize_repo_path(&request.path)?;
    let default_branch = resolve_default_branch(&path, request.default_branch.as_deref()).await?;
    let now = Utc::now();
    let project = Project {
        id: ProjectId::new(),
        name: normalize_project_name(request.name.as_deref(), &path)?,
        path,
        default_branch,
        color: normalize_project_color(request.color.as_deref())?,
        execution_mode: request.execution_mode.unwrap_or_default(),
        agent_routing: request.agent_routing,
        auto_triage_policy: request.auto_triage_policy,
        created_at: now,
        updated_at: now,
    };

    state
        .db
        .create_project(&project)
        .await
        .map_err(repo_to_project_mutation)?;
    refresh_project_mirror(&state, &project).await?;

    Ok((StatusCode::CREATED, Json(project)))
}

pub(super) async fn update_project(
    State(state): State<AppState>,
    ApiPath(ProjectPathParams { project_id }): ApiPath<ProjectPathParams>,
    Json(request): Json<UpdateProjectRequest>,
) -> Result<Json<Project>, ApiError> {
    let _guard = state
        .project_locks
        .acquire_project_mutation(project_id)
        .await;
    let existing = state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;
    let mut project = existing;
    let path = request
        .path
        .as_deref()
        .map(canonicalize_repo_path)
        .transpose()?
        .unwrap_or_else(|| project.path.clone());
    let default_branch = if request.default_branch.is_some() || request.path.is_some() {
        resolve_default_branch(&path, request.default_branch.as_deref()).await?
    } else {
        project.default_branch.clone()
    };

    project.name = match request.name.as_deref() {
        Some(name) => normalize_non_empty("project name", name)?,
        None => project.name,
    };
    project.path = path;
    project.default_branch = default_branch;
    project.color = match request.color.as_deref() {
        Some(color) => normalize_project_color(Some(color))?,
        None => project.color,
    };
    project.execution_mode = request.execution_mode.unwrap_or(project.execution_mode);
    if let Some(routing) = request.agent_routing {
        project.agent_routing = routing;
    }
    if let Some(policy) = request.auto_triage_policy {
        project.auto_triage_policy = policy;
    }
    project.updated_at = Utc::now();

    state
        .db
        .update_project(&project)
        .await
        .map_err(repo_to_project_mutation)?;
    refresh_project_mirror(&state, &project).await?;

    Ok(Json(project))
}

pub(super) async fn delete_project(
    State(state): State<AppState>,
    ApiPath(ProjectPathParams { project_id }): ApiPath<ProjectPathParams>,
) -> Result<StatusCode, ApiError> {
    state
        .db
        .delete_project(project_id)
        .await
        .map_err(repo_to_project_mutation)?;

    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn get_project_config(
    State(state): State<AppState>,
    ApiPath(ProjectPathParams { project_id }): ApiPath<ProjectPathParams>,
) -> Result<Json<IngotConfig>, ApiError> {
    let project = state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;
    Ok(Json(load_effective_config(Some(&project))?))
}

pub(super) async fn list_project_jobs(
    State(state): State<AppState>,
    ApiPath(ProjectPathParams { project_id }): ApiPath<ProjectPathParams>,
) -> Result<Json<Vec<Job>>, ApiError> {
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
