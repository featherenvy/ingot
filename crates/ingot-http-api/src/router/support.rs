use std::path::Path as FsPath;
use std::path::PathBuf;
use std::str::FromStr;

use ingot_config::IngotConfig;
use ingot_domain::git_operation::OperationKind;
use ingot_domain::ports::RepositoryError;
use ingot_domain::project::Project;
use ingot_domain::revision::ApprovalPolicy;
use ingot_domain::workspace::{Workspace, WorkspaceStatus};
use ingot_git::commands::{check_ref_format, current_branch_name, resolve_ref_oid};
use ingot_git::project_repo::{ensure_mirror, project_repo_paths};
use ingot_usecases::item::normalize_target_ref;
use ingot_usecases::{CompleteJobError, UseCaseError};
use ingot_workspace::WorkspaceError;

use crate::error::ApiError;

use super::AppState;

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

pub(super) fn global_config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".ingot").join("config.yml")
}

pub(super) fn logs_root(state_root: &FsPath) -> PathBuf {
    state_root.join("logs")
}

pub(super) fn project_config_path(project: &Project) -> PathBuf {
    FsPath::new(&project.path).join(".ingot").join("config.yml")
}

pub(super) fn project_paths(
    state: &AppState,
    project: &Project,
) -> ingot_git::project_repo::ProjectRepoPaths {
    project_repo_paths(
        state.state_root.as_path(),
        project.id,
        FsPath::new(&project.path),
    )
}

pub(super) async fn refresh_project_mirror(
    state: &AppState,
    project: &Project,
) -> Result<ingot_git::project_repo::ProjectRepoPaths, ApiError> {
    let paths = project_paths(state, project);
    let has_unresolved_finalize = state
        .db
        .list_unresolved_git_operations()
        .await
        .map_err(repo_to_internal)?
        .into_iter()
        .any(|operation| {
            operation.project_id == project.id
                && operation.operation_kind == OperationKind::FinalizeTargetRef
        });
    if !(has_unresolved_finalize && paths.mirror_git_dir.exists()) {
        ensure_mirror(&paths).await.map_err(git_to_internal)?;
    }
    Ok(paths)
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
) -> Result<String, ApiError> {
    let branch = if let Some(branch) = requested_branch {
        normalize_branch_name(branch)?
    } else {
        current_branch_name(repo_path)
            .await
            .map_err(|error| ApiError::BadRequest {
                code: "invalid_project_repo",
                message: error.to_string(),
            })?
    };

    let target_ref = normalize_target_ref(&branch)?;
    ensure_git_valid_target_ref(&target_ref).await?;
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

pub(crate) fn parse_config_approval_policy(
    config: &IngotConfig,
) -> Result<ApprovalPolicy, ApiError> {
    match config.defaults.approval_policy.as_str() {
        "required" => Ok(ApprovalPolicy::Required),
        "not_required" => Ok(ApprovalPolicy::NotRequired),
        other => Err(ApiError::BadRequest {
            code: "config_invalid",
            message: format!("Unsupported approval policy in config: {other}"),
        }),
    }
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

pub(super) fn normalize_branch_name(branch: &str) -> Result<String, ApiError> {
    let branch = normalize_non_empty("default branch", branch)?;
    Ok(branch
        .strip_prefix("refs/heads/")
        .unwrap_or(branch.as_str())
        .to_string())
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

pub(super) fn parse_id<T>(value: &str, entity: &'static str) -> Result<T, ApiError>
where
    T: FromStr,
{
    value
        .parse()
        .map_err(|_| ApiError::invalid_id(entity, value))
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
        RepositoryError::Conflict(message) if message == "job_not_active" => {
            UseCaseError::JobNotActive.into()
        }
        other => repo_to_internal(other),
    }
}

pub(super) fn repo_to_job_failure(error: RepositoryError) -> ApiError {
    match error {
        RepositoryError::Conflict(message) if message == "job_revision_stale" => {
            UseCaseError::ProtocolViolation(
                "job failure does not match the current item revision".into(),
            )
            .into()
        }
        other => repo_to_job_completion(other),
    }
}

#[allow(dead_code)]
pub(super) fn repo_to_job_expiration(error: RepositoryError) -> ApiError {
    match error {
        RepositoryError::Conflict(message) if message == "job_revision_stale" => {
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
        RepositoryError::Conflict(message) if message.contains("projects.path") => {
            ApiError::Conflict {
                code: "project_path_conflict",
                message: "A project is already registered for that path".into(),
            }
        }
        RepositoryError::Conflict(message) if message.contains("FOREIGN KEY") => {
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
        RepositoryError::Conflict(message) if message.contains("agents.slug") => {
            ApiError::Conflict {
                code: "agent_slug_conflict",
                message: "An agent with that slug already exists".into(),
            }
        }
        RepositoryError::Conflict(message) if message.contains("FOREIGN KEY") => {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_not_found_maps_to_project_error() {
        let error = repo_to_project(RepositoryError::NotFound);

        assert!(matches!(
            error,
            ApiError::UseCase(UseCaseError::ProjectNotFound)
        ));
    }

    #[test]
    fn failure_revision_stale_maps_to_protocol_violation() {
        let error = repo_to_job_failure(RepositoryError::Conflict("job_revision_stale".into()));

        assert!(matches!(
            error,
            ApiError::UseCase(UseCaseError::ProtocolViolation(message))
                if message == "job failure does not match the current item revision"
        ));
    }

    #[test]
    fn expiration_revision_stale_maps_to_protocol_violation() {
        let error = repo_to_job_expiration(RepositoryError::Conflict("job_revision_stale".into()));

        assert!(matches!(
            error,
            ApiError::UseCase(UseCaseError::ProtocolViolation(message))
                if message == "job expiration does not match the current item revision"
        ));
    }
}
