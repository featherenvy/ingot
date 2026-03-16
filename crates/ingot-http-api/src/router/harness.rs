use std::path::Path;

use axum::Json;
use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use ingot_domain::harness::HarnessProfile;

use super::AppState;
use super::support::{parse_id, repo_to_project};
use crate::error::ApiError;

pub(super) async fn get_harness_profile(
    State(state): State<AppState>,
    AxumPath(project_id): AxumPath<String>,
) -> Result<Json<HarnessProfile>, ApiError> {
    let project_id = parse_id(&project_id, "project")?;
    let project = state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;

    Ok(Json(read_harness_profile(Path::new(&project.path))?))
}

pub(super) async fn put_harness_profile(
    State(state): State<AppState>,
    AxumPath(project_id): AxumPath<String>,
    body: String,
) -> Result<impl IntoResponse, ApiError> {
    let project_id = parse_id(&project_id, "project")?;
    let project = state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;

    let profile = parse_harness_profile(&body)?;

    let toml_path = harness_toml_path(Path::new(&project.path));
    let ingot_dir = toml_path
        .parent()
        .expect("harness.toml should have a parent directory");
    std::fs::create_dir_all(ingot_dir).map_err(|error| {
        ApiError::internal(format!("failed to create .ingot directory: {error}"))
    })?;
    std::fs::write(&toml_path, &body)
        .map_err(|error| ApiError::internal(format!("failed to write harness.toml: {error}")))?;

    Ok((StatusCode::OK, Json(profile)))
}

fn read_harness_profile(project_path: &Path) -> Result<HarnessProfile, ApiError> {
    let toml_path = harness_toml_path(project_path);
    match std::fs::read_to_string(&toml_path) {
        Ok(content) => parse_harness_profile(&content),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(HarnessProfile::default()),
        Err(error) => Err(ApiError::internal(format!(
            "failed to read harness.toml: {error}"
        ))),
    }
}

fn parse_harness_profile(content: &str) -> Result<HarnessProfile, ApiError> {
    HarnessProfile::from_toml(content)
        .map_err(|error| ApiError::validation(format!("invalid harness profile: {error}")))
}

fn harness_toml_path(project_path: &Path) -> std::path::PathBuf {
    project_path.join(".ingot").join("harness.toml")
}
