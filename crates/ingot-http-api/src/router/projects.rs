use super::items::load_effective_config;
use super::*;
use super::support::*;
use super::types::*;

pub(super) async fn list_project_activity(
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

pub(super) async fn list_project_workspaces(
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

pub(super) async fn list_projects(State(state): State<AppState>) -> Result<Json<Vec<Project>>, ApiError> {
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
    refresh_project_mirror(&state, &project).await?;

    Ok((StatusCode::CREATED, Json(project)))
}

pub(super) async fn update_project(
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
    refresh_project_mirror(&state, &project).await?;

    Ok(Json(project))
}

pub(super) async fn delete_project(
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

pub(super) async fn get_project_config(
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

pub(super) async fn list_project_jobs(
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

