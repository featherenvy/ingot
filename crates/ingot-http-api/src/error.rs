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
}

impl ApiError {
    pub fn invalid_id(entity: &'static str, value: &str) -> Self {
        Self::BadRequest {
            code: "invalid_id",
            message: format!("Invalid {entity} id: {value}"),
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
                UseCaseError::FindingNotUntriaged => (
                    StatusCode::CONFLICT,
                    "finding_not_untriaged",
                    "Finding is not untriaged".into(),
                ),
                UseCaseError::FindingSubjectUnreachable => (
                    StatusCode::CONFLICT,
                    "finding_subject_unreachable",
                    "Finding subject is unreachable".into(),
                ),
                UseCaseError::InvalidDismissalReason => (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "invalid_dismissal_reason",
                    "Dismissal reason is required".into(),
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
