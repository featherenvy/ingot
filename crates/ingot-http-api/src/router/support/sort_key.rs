use ingot_usecases::item::next_sort_key;

use crate::error::ApiError;
use crate::router::AppState;

use super::errors::repo_to_internal;

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
