use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use ingot_usecases::UseCaseError;

#[derive(Debug)]
pub enum ApiError {
    UseCase(UseCaseError),
    BadRequest { code: &'static str, message: String },
    Conflict { code: &'static str, message: String },
    NotFound { code: &'static str, message: String },
    Validation { message: String },
    Internal { message: String },
}

impl ApiError {
    pub fn invalid_id(entity: impl AsRef<str>, value: &str) -> Self {
        let entity = entity.as_ref();
        Self::BadRequest {
            code: "invalid_id",
            message: format!("Invalid {entity} id: {value}"),
        }
    }

    pub fn validation(message: impl Into<String>) -> Self {
        Self::Validation {
            message: message.into(),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal {
            message: message.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            ApiError::UseCase(use_case_error) => match use_case_error {
                UseCaseError::ProjectNotFound => (
                    StatusCode::NOT_FOUND,
                    "project_not_found",
                    "Project not found".into(),
                ),
                UseCaseError::ItemNotFound => (
                    StatusCode::NOT_FOUND,
                    "item_not_found",
                    "Item not found".into(),
                ),
                UseCaseError::ItemNotOpen => (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "item_not_open",
                    "Item is not open".into(),
                ),
                UseCaseError::ItemNotIdle => (
                    StatusCode::CONFLICT,
                    "item_not_idle",
                    "Item is not idle".into(),
                ),
                UseCaseError::ApprovalNotPending => (
                    StatusCode::CONFLICT,
                    "approval_not_pending",
                    "Approval is not pending".into(),
                ),
                UseCaseError::ConvergenceNotPreparable => (
                    StatusCode::CONFLICT,
                    "convergence_not_preparable",
                    "Convergence cannot be prepared in the current item state".into(),
                ),
                UseCaseError::ConvergenceNotQueued => (
                    StatusCode::CONFLICT,
                    "convergence_not_queued",
                    "A lane head is required before approval can be granted".into(),
                ),
                UseCaseError::ConvergenceNotLaneHead => (
                    StatusCode::CONFLICT,
                    "convergence_not_lane_head",
                    "Only the target-ref lane head can be approved".into(),
                ),
                UseCaseError::JobNotActive => (
                    StatusCode::CONFLICT,
                    "job_not_active",
                    "Job is not active".into(),
                ),
                UseCaseError::FindingNotFound => (
                    StatusCode::NOT_FOUND,
                    "finding_not_found",
                    "Finding not found".into(),
                ),
                UseCaseError::FindingNotTriageable => (
                    StatusCode::CONFLICT,
                    "finding_not_triageable",
                    "Finding is not triageable".into(),
                ),
                UseCaseError::FindingSubjectUnreachable => (
                    StatusCode::CONFLICT,
                    "finding_subject_unreachable",
                    "Finding subject is unreachable".into(),
                ),
                UseCaseError::InvalidFindingTriage(message) => (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "invalid_finding_triage",
                    message,
                ),
                UseCaseError::IllegalStepDispatch(message) => (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "illegal_step_dispatch",
                    message,
                ),
                UseCaseError::ActiveJobExists => (
                    StatusCode::CONFLICT,
                    "active_job_exists",
                    "Active job exists".into(),
                ),
                UseCaseError::ActiveConvergenceExists => (
                    StatusCode::CONFLICT,
                    "active_convergence_exists",
                    "Active convergence exists".into(),
                ),
                UseCaseError::CompletedItemCannotReopen => (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "completed_item_cannot_reopen",
                    "Completed items cannot be reopened".into(),
                ),
                UseCaseError::InvalidTargetRef(target_ref) => (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "invalid_target_ref",
                    format!("Target ref must be a branch under refs/heads/*: {target_ref}"),
                ),
                UseCaseError::TargetRefUnresolved(target_ref) => (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "target_ref_unresolved",
                    format!("Target ref could not be resolved: {target_ref}"),
                ),
                UseCaseError::RevisionSeedUnreachable(seed_name) => (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "revision_seed_unreachable",
                    format!("Revision seed is not reachable: {seed_name}"),
                ),
                UseCaseError::LinkedItemNotFound => (
                    StatusCode::NOT_FOUND,
                    "linked_item_not_found",
                    "Linked item not found".into(),
                ),
                UseCaseError::LinkedItemProjectMismatch => (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "linked_item_project_mismatch",
                    "Linked item must belong to the same project".into(),
                ),
                UseCaseError::PreparedConvergenceMissing => (
                    StatusCode::CONFLICT,
                    "prepared_convergence_missing",
                    "No prepared convergence exists".into(),
                ),
                UseCaseError::PreparedConvergenceStale => (
                    StatusCode::CONFLICT,
                    "prepared_convergence_stale",
                    "Prepared convergence is stale".into(),
                ),
                UseCaseError::ProtocolViolation(message) => (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "protocol_violation",
                    message,
                ),
                UseCaseError::Repository(_) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "Internal error".into(),
                ),
                UseCaseError::Internal(message) => {
                    (StatusCode::INTERNAL_SERVER_ERROR, "internal_error", message)
                }
            },
            ApiError::Conflict { code, message } => (StatusCode::CONFLICT, code, message),
            ApiError::NotFound { code, message } => (StatusCode::NOT_FOUND, code, message),
            ApiError::BadRequest { code, message } => (StatusCode::BAD_REQUEST, code, message),
            ApiError::Validation { message } => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "validation_error",
                message,
            ),
            ApiError::Internal { message } => {
                (StatusCode::INTERNAL_SERVER_ERROR, "internal_error", message)
            }
        };

        let body = json!({
            "error": {
                "code": code,
                "message": message,
            }
        });

        (status, axum::Json(body)).into_response()
    }
}

impl From<UseCaseError> for ApiError {
    fn from(error: UseCaseError) -> Self {
        Self::UseCase(error)
    }
}
