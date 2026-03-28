use std::path::Path as FsPath;

use ingot_domain::branch_name::BranchName;
use ingot_domain::ports::{ConflictKind, RepositoryError};
use ingot_domain::workspace::{Workspace, WorkspaceStatus};
use ingot_git::commands::{
    GitCommandError, check_ref_format, current_branch_name, resolve_ref_oid,
};
use ingot_usecases::item::normalize_target_ref;
use ingot_usecases::{CompleteJobError, UseCaseError};
use ingot_workspace::WorkspaceError;

use crate::error::ApiError;

use super::normalize::normalize_branch_name;

pub(crate) fn workspace_to_api_error(error: WorkspaceError) -> ApiError {
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

pub(crate) fn ensure_workspace_not_busy(workspace: &Workspace) -> Result<(), ApiError> {
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

pub(crate) fn git_to_internal(error: GitCommandError) -> ApiError {
    ApiError::from(UseCaseError::Internal(error.to_string()))
}

pub(crate) fn api_to_usecase_error(error: ApiError) -> UseCaseError {
    match error {
        ApiError::UseCase(error) => error,
        ApiError::BadRequest { message, .. }
        | ApiError::Conflict { message, .. }
        | ApiError::NotFound { message, .. }
        | ApiError::Validation { message }
        | ApiError::Internal { message } => UseCaseError::Internal(message),
    }
}

pub(crate) fn repo_to_job_completion(error: RepositoryError) -> ApiError {
    match error {
        RepositoryError::Conflict(ConflictKind::JobNotActive) => UseCaseError::JobNotActive.into(),
        other => repo_to_internal(other),
    }
}

#[allow(dead_code)]
pub(crate) fn repo_to_job_expiration(error: RepositoryError) -> ApiError {
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

pub(crate) fn repo_to_item(error: RepositoryError) -> ApiError {
    match error {
        RepositoryError::NotFound => UseCaseError::ItemNotFound.into(),
        other => ApiError::from(UseCaseError::Repository(other)),
    }
}

pub(crate) fn repo_to_project(error: RepositoryError) -> ApiError {
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

pub(crate) fn repo_to_agent(error: RepositoryError) -> ApiError {
    match error {
        RepositoryError::NotFound => ApiError::NotFound {
            code: "agent_not_found",
            message: "Agent not found".into(),
        },
        other => ApiError::from(UseCaseError::Repository(other)),
    }
}

pub(crate) fn repo_to_agent_mutation(error: RepositoryError) -> ApiError {
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

pub(crate) fn repo_to_finding(error: RepositoryError) -> ApiError {
    match error {
        RepositoryError::NotFound => UseCaseError::FindingNotFound.into(),
        other => ApiError::from(UseCaseError::Repository(other)),
    }
}

pub(crate) fn complete_job_error_to_api_error(error: CompleteJobError) -> ApiError {
    match error {
        CompleteJobError::BadRequest { code, message } => ApiError::BadRequest { code, message },
        CompleteJobError::UseCase(error) => error.into(),
    }
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

#[cfg(test)]
mod tests {
    use ingot_domain::ports::{ConflictKind, RepositoryError};
    use ingot_usecases::UseCaseError;

    use super::{repo_to_job_expiration, repo_to_project};
    use crate::error::ApiError;

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
}
