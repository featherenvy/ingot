use std::path::Path as FsPath;
use std::path::PathBuf;

use axum::extract::path::ErrorKind as PathErrorKind;
use axum::extract::rejection::{FailedToDeserializePathParams, PathRejection};
use axum::extract::{FromRequestParts, Path, RawPathParams};
use axum::http::request::Parts;
use ingot_config::IngotConfig;
use ingot_config::loader::load_config;
use ingot_domain::activity::{Activity, ActivityEventType, ActivitySubject};
use ingot_domain::branch_name::BranchName;
use ingot_domain::ids::{ActivityId, AgentId, FindingId, ItemId, JobId, ProjectId, WorkspaceId};
use ingot_domain::ports::{ConflictKind, RepositoryError};
use ingot_domain::project::Project;
use ingot_domain::workspace::{Workspace, WorkspaceStatus};
use ingot_git::commands::{check_ref_format, current_branch_name, resolve_ref_oid};
use ingot_git::project_repo::project_repo_paths;
use ingot_usecases::item::{next_sort_key, normalize_target_ref};
use ingot_usecases::{CompleteJobError, UseCaseError};
use ingot_workspace::WorkspaceError;
use serde::de::DeserializeOwned;

use crate::error::ApiError;

use super::AppState;

// ---------------------------------------------------------------------------
// Path extractors and helpers
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub(super) struct ApiPath<T>(pub(super) T);

impl<T, S> FromRequestParts<S> for ApiPath<T>
where
    T: DeserializeOwned + Send,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let raw_path_params = RawPathParams::from_request_parts(parts, state).await.ok();
        let Path(params) =
            Path::<T>::from_request_parts(parts, state)
                .await
                .map_err(|rejection| {
                    path_rejection_to_api_error(rejection, raw_path_params.as_ref())
                })?;
        Ok(Self(params))
    }
}

fn path_rejection_to_api_error(
    rejection: PathRejection,
    raw_path_params: Option<&RawPathParams>,
) -> ApiError {
    match rejection {
        PathRejection::FailedToDeserializePathParams(error) => {
            failed_path_params_to_api_error(error, raw_path_params)
        }
        PathRejection::MissingPathParams(_) => {
            ApiError::internal("missing path parameters for matched route")
        }
        _ => ApiError::internal("unexpected path extraction failure"),
    }
}

fn failed_path_params_to_api_error(
    error: FailedToDeserializePathParams,
    raw_path_params: Option<&RawPathParams>,
) -> ApiError {
    let body_text = error.body_text();

    match error.into_kind() {
        PathErrorKind::ParseErrorAtKey { key, value, .. }
        | PathErrorKind::DeserializeError { key, value, .. } => {
            ApiError::invalid_id(path_param_entity_name(&key), &value)
        }
        PathErrorKind::InvalidUtf8InPathParam { key } => ApiError::BadRequest {
            code: "invalid_id",
            message: format!(
                "Invalid {} id: path parameter was not valid UTF-8",
                path_param_entity_name(&key)
            ),
        },
        PathErrorKind::ParseErrorAtIndex { value, .. }
        | PathErrorKind::ParseError { value, .. } => ApiError::invalid_id("resource", &value),
        PathErrorKind::WrongNumberOfParameters { .. } | PathErrorKind::UnsupportedType { .. } => {
            ApiError::internal(body_text)
        }
        PathErrorKind::Message(_) => raw_path_params
            .and_then(invalid_id_from_raw_path_params)
            .unwrap_or_else(|| ApiError::internal(body_text)),
        _ => ApiError::internal(body_text),
    }
}

pub(super) fn path_param_entity_name(key: &str) -> &str {
    key.strip_suffix("_id").unwrap_or(key)
}

fn invalid_id_from_raw_path_params(raw_path_params: &RawPathParams) -> Option<ApiError> {
    for (key, value) in raw_path_params {
        let invalid = match key {
            "agent_id" => value.parse::<AgentId>().is_err(),
            "finding_id" => value.parse::<FindingId>().is_err(),
            "item_id" => value.parse::<ItemId>().is_err(),
            "job_id" => value.parse::<JobId>().is_err(),
            "project_id" => value.parse::<ProjectId>().is_err(),
            "workspace_id" => value.parse::<WorkspaceId>().is_err(),
            _ => false,
        };

        if invalid {
            return Some(ApiError::invalid_id(path_param_entity_name(key), value));
        }
    }

    None
}

pub(super) fn global_config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".ingot").join("config.yml")
}

pub(super) fn logs_root(state_root: &FsPath) -> PathBuf {
    state_root.join("logs")
}

pub(super) fn project_config_path(project: &Project) -> PathBuf {
    project.path.join(".ingot").join("config.yml")
}

pub(super) fn project_paths(
    state: &AppState,
    project: &Project,
) -> ingot_git::project_repo::ProjectRepoPaths {
    project_repo_paths(state.state_root.as_path(), project.id, &project.path)
}

pub(super) async fn refresh_project_mirror(
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

pub(super) async fn next_project_sort_key(
    state: &AppState,
    project_id: ProjectId,
) -> Result<String, ApiError> {
    let items = state
        .db
        .list_items_by_project(project_id)
        .await
        .map_err(repo_to_internal)?;
    Ok(next_sort_key(&items))
}

pub(crate) async fn ensure_git_valid_target_ref(target_ref: &str) -> Result<(), ApiError> {
    match check_ref_format(target_ref)
        .await
        .map_err(git_to_internal)?
    {
        true => Ok(()),
        false => Err(UseCaseError::InvalidTargetRef(target_ref.into()).into()),
    }
}

pub(crate) async fn resolve_default_branch(
    repo_path: &FsPath,
    requested_branch: Option<&str>,
) -> Result<BranchName, ApiError> {
    let branch = if let Some(branch) = requested_branch {
        normalize_branch_name(branch)?
    } else {
        BranchName::new(current_branch_name(repo_path).await.map_err(|error| {
            ApiError::BadRequest {
                code: "invalid_project_repo",
                message: error.to_string(),
            }
        })?)
    };

    let target_ref = normalize_target_ref(branch.as_str())?;
    ensure_git_valid_target_ref(target_ref.as_str()).await?;
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

// ---------------------------------------------------------------------------
// Normalization helpers
// ---------------------------------------------------------------------------

pub(super) fn canonicalize_repo_path(path: &str) -> Result<PathBuf, ApiError> {
    let path = normalize_non_empty("project path", path)?;
    std::fs::canonicalize(path).map_err(|error| ApiError::BadRequest {
        code: "invalid_project_path",
        message: error.to_string(),
    })
}

pub(super) fn normalize_project_name(
    name: Option<&str>,
    path: &FsPath,
) -> Result<String, ApiError> {
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

pub(super) fn normalize_project_color(color: Option<&str>) -> Result<String, ApiError> {
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

pub(super) fn normalize_branch_name(branch: &str) -> Result<BranchName, ApiError> {
    let branch = normalize_non_empty("default branch", branch)?;
    Ok(BranchName::new(
        branch
            .strip_prefix("refs/heads/")
            .unwrap_or(branch.as_str()),
    ))
}

pub(super) fn normalize_agent_slug(
    slug: Option<&str>,
    fallback_name: &str,
) -> Result<String, ApiError> {
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

pub(super) fn normalize_non_empty(field: &'static str, value: &str) -> Result<String, ApiError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(ApiError::BadRequest {
            code: "invalid_input",
            message: format!("{field} is required"),
        });
    }

    Ok(trimmed.to_string())
}

// ---------------------------------------------------------------------------
// Error mappers
// ---------------------------------------------------------------------------

pub(super) fn workspace_to_api_error(error: WorkspaceError) -> ApiError {
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

pub(super) fn ensure_workspace_not_busy(workspace: &Workspace) -> Result<(), ApiError> {
    if workspace.state.status() == WorkspaceStatus::Busy {
        return Err(ApiError::Conflict {
            code: "workspace_busy",
            message: "Workspace is busy".into(),
        });
    }
    Ok(())
}

pub(crate) fn repo_to_internal(error: RepositoryError) -> ApiError {
    ApiError::from(UseCaseError::Repository(error))
}

pub(crate) fn git_to_internal(error: ingot_git::commands::GitCommandError) -> ApiError {
    ApiError::from(UseCaseError::Internal(error.to_string()))
}

pub(super) fn api_to_usecase_error(error: ApiError) -> UseCaseError {
    match error {
        ApiError::UseCase(error) => error,
        ApiError::BadRequest { message, .. }
        | ApiError::Conflict { message, .. }
        | ApiError::NotFound { message, .. }
        | ApiError::Validation { message }
        | ApiError::Internal { message } => UseCaseError::Internal(message),
    }
}

pub(super) fn repo_to_job_completion(error: RepositoryError) -> ApiError {
    match error {
        RepositoryError::Conflict(ConflictKind::JobNotActive) => UseCaseError::JobNotActive.into(),
        other => repo_to_internal(other),
    }
}

#[allow(dead_code)]
pub(super) fn repo_to_job_expiration(error: RepositoryError) -> ApiError {
    match error {
        RepositoryError::Conflict(ConflictKind::JobRevisionStale) => {
            UseCaseError::ProtocolViolation(
                "job expiration does not match the current item revision".into(),
            )
            .into()
        }
        other => repo_to_job_completion(other),
    }
}

pub(super) fn repo_to_item(error: RepositoryError) -> ApiError {
    match error {
        RepositoryError::NotFound => UseCaseError::ItemNotFound.into(),
        other => ApiError::from(UseCaseError::Repository(other)),
    }
}

pub(super) fn repo_to_project(error: RepositoryError) -> ApiError {
    match error {
        RepositoryError::NotFound => UseCaseError::ProjectNotFound.into(),
        other => ApiError::from(UseCaseError::Repository(other)),
    }
}

pub(crate) fn repo_to_project_mutation(error: RepositoryError) -> ApiError {
    match error {
        RepositoryError::NotFound => UseCaseError::ProjectNotFound.into(),
        RepositoryError::Conflict(ConflictKind::DatabaseConstraint(message))
            if message.contains("projects.path") =>
        {
            ApiError::Conflict {
                code: "project_path_conflict",
                message: "A project is already registered for that path".into(),
            }
        }
        RepositoryError::Conflict(ConflictKind::DatabaseConstraint(message))
            if message.contains("FOREIGN KEY") =>
        {
            ApiError::Conflict {
                code: "project_in_use",
                message: "Project cannot be deleted while related items still exist".into(),
            }
        }
        other => ApiError::from(UseCaseError::Repository(other)),
    }
}

pub(super) fn repo_to_agent(error: RepositoryError) -> ApiError {
    match error {
        RepositoryError::NotFound => ApiError::NotFound {
            code: "agent_not_found",
            message: "Agent not found".into(),
        },
        other => ApiError::from(UseCaseError::Repository(other)),
    }
}

pub(super) fn repo_to_agent_mutation(error: RepositoryError) -> ApiError {
    match error {
        RepositoryError::NotFound => repo_to_agent(RepositoryError::NotFound),
        RepositoryError::Conflict(ConflictKind::DatabaseConstraint(message))
            if message.contains("agents.slug") =>
        {
            ApiError::Conflict {
                code: "agent_slug_conflict",
                message: "An agent with that slug already exists".into(),
            }
        }
        RepositoryError::Conflict(ConflictKind::DatabaseConstraint(message))
            if message.contains("FOREIGN KEY") =>
        {
            ApiError::Conflict {
                code: "agent_in_use",
                message: "Agent cannot be deleted while related jobs still exist".into(),
            }
        }
        other => ApiError::from(UseCaseError::Repository(other)),
    }
}

pub(super) fn repo_to_finding(error: RepositoryError) -> ApiError {
    match error {
        RepositoryError::NotFound => UseCaseError::FindingNotFound.into(),
        other => ApiError::from(UseCaseError::Repository(other)),
    }
}

pub(super) fn complete_job_error_to_api_error(error: CompleteJobError) -> ApiError {
    match error {
        CompleteJobError::BadRequest { code, message } => ApiError::BadRequest { code, message },
        CompleteJobError::UseCase(error) => error.into(),
    }
}

// ---------------------------------------------------------------------------
// Cross-module utilities
// ---------------------------------------------------------------------------

pub(crate) async fn append_activity(
    state: &AppState,
    project_id: ProjectId,
    event_type: ActivityEventType,
    subject: ActivitySubject,
    payload: serde_json::Value,
) -> Result<(), ApiError> {
    state
        .db
        .append_activity(&Activity {
            id: ActivityId::new(),
            project_id,
            event_type,
            subject,
            payload,
            created_at: chrono::Utc::now(),
        })
        .await
        .map_err(repo_to_internal)
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

pub(super) async fn read_optional_text(path: PathBuf) -> Result<Option<String>, ApiError> {
    match tokio::fs::read_to_string(path).await {
        Ok(contents) => Ok(Some(contents)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(ApiError::from(UseCaseError::Internal(error.to_string()))),
    }
}

pub(super) async fn read_optional_json(
    path: PathBuf,
) -> Result<Option<serde_json::Value>, ApiError> {
    let Some(contents) = read_optional_text(path).await? else {
        return Ok(None);
    };

    serde_json::from_str(&contents)
        .map(Some)
        .map_err(|error| ApiError::from(UseCaseError::Internal(error.to_string())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use axum::{Json, Router};
    use ingot_domain::ids::ProjectId;
    use serde::Deserialize;
    use tower::ServiceExt;

    #[test]
    fn project_not_found_maps_to_project_error() {
        let error = repo_to_project(RepositoryError::NotFound);

        assert!(matches!(
            error,
            ApiError::UseCase(UseCaseError::ProjectNotFound)
        ));
    }

    #[test]
    fn expiration_revision_stale_maps_to_protocol_violation() {
        let error =
            repo_to_job_expiration(RepositoryError::Conflict(ConflictKind::JobRevisionStale));

        assert!(matches!(
            error,
            ApiError::UseCase(UseCaseError::ProtocolViolation(message))
                if message == "job expiration does not match the current item revision"
        ));
    }

    #[tokio::test]
    async fn api_path_maps_invalid_typed_ids_to_invalid_id_error() {
        #[derive(Debug, Deserialize)]
        struct ProjectPathParams {
            project_id: ProjectId,
        }

        async fn handler(
            ApiPath(ProjectPathParams { project_id }): ApiPath<ProjectPathParams>,
        ) -> Result<Json<()>, ApiError> {
            let _ = project_id;
            Ok(Json(()))
        }

        let app = Router::new().route("/projects/{project_id}", get(handler));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/projects/not-a-project-id")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body should read");
        let body: serde_json::Value =
            serde_json::from_slice(&body).expect("response should be valid json");
        assert_eq!(body["error"]["code"], "invalid_id");
        assert_eq!(
            body["error"]["message"],
            "Invalid project id: not-a-project-id"
        );
    }

    #[tokio::test]
    async fn api_path_preserves_internal_errors_for_path_shape_mismatches() {
        #[derive(Debug, Deserialize)]
        struct ProjectAndItemPathParams {
            project_id: ProjectId,
            item_id: ItemId,
        }

        async fn handler(
            ApiPath(ProjectAndItemPathParams {
                project_id,
                item_id,
            }): ApiPath<ProjectAndItemPathParams>,
        ) -> Result<Json<()>, ApiError> {
            let _ = (project_id, item_id);
            Ok(Json(()))
        }

        let app = Router::new().route("/projects/{project_id}", get(handler));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/projects/prj_00000000000000000000000000000000")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body should read");
        let body: serde_json::Value =
            serde_json::from_slice(&body).expect("response should be valid json");
        assert_eq!(body["error"]["code"], "internal_error");
    }
}
