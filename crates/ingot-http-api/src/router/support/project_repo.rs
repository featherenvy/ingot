use std::path::{Path as FsPath, PathBuf};

use ingot_config::paths::logs_root as shared_logs_root;
use ingot_domain::project::Project;
use ingot_git::project_repo::project_repo_paths;
use ingot_usecases::item::next_sort_key;

use crate::error::ApiError;
use crate::router::AppState;

use super::errors::{git_to_internal, repo_to_internal};

pub(crate) fn logs_root(state_root: &FsPath) -> PathBuf {
    shared_logs_root(state_root)
}

pub(crate) fn project_paths(
    state: &AppState,
    project: &Project,
) -> ingot_git::project_repo::ProjectRepoPaths {
    project_repo_paths(state.state_root.as_path(), project.id, &project.path)
}

pub(crate) async fn refresh_project_mirror(
    state: &AppState,
    project: &Project,
) -> Result<ingot_git::project_repo::ProjectRepoPaths, ApiError> {
    ingot_git::project_repo::refresh_project_mirror(
        &state.db,
        state.state_root.as_path(),
        project.id,
        &project.path,
    )
    .await
    .map_err(|error| match error {
        ingot_git::project_repo::RefreshMirrorError::Repository(error) => repo_to_internal(error),
        ingot_git::project_repo::RefreshMirrorError::Git(error) => git_to_internal(error),
    })
}

pub(crate) async fn next_project_sort_key(
    state: &AppState,
    project_id: ingot_domain::ids::ProjectId,
) -> Result<String, ApiError> {
    let items = state
        .db
        .list_items_by_project(project_id)
        .await
        .map_err(repo_to_internal)?;
    Ok(next_sort_key(&items))
}
