use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use ingot_usecases::UseCaseError;

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, message) = match &self.0 {
            UseCaseError::ItemNotFound => {
                (StatusCode::NOT_FOUND, "item_not_found", "Item not found")
            }
            UseCaseError::ItemNotOpen => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "item_not_open",
                "Item is not open",
            ),
            UseCaseError::ItemNotIdle => {
                (StatusCode::CONFLICT, "item_not_idle", "Item is not idle")
            }
            UseCaseError::ApprovalNotPending => (
                StatusCode::CONFLICT,
                "approval_not_pending",
                "Approval is not pending",
            ),
            UseCaseError::IllegalStepDispatch(msg) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "illegal_step_dispatch",
                msg.as_str(),
            ),
            UseCaseError::ActiveJobExists => (
                StatusCode::CONFLICT,
                "active_job_exists",
                "Active job exists",
            ),
            UseCaseError::ActiveConvergenceExists => (
                StatusCode::CONFLICT,
                "active_convergence_exists",
                "Active convergence exists",
            ),
            UseCaseError::CompletedItemCannotReopen => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "completed_item_cannot_reopen",
                "Completed items cannot be reopened",
            ),
            UseCaseError::PreparedConvergenceMissing => (
                StatusCode::CONFLICT,
                "prepared_convergence_missing",
                "No prepared convergence exists",
            ),
            UseCaseError::PreparedConvergenceStale => (
                StatusCode::CONFLICT,
                "prepared_convergence_stale",
                "Prepared convergence is stale",
            ),
            UseCaseError::Repository(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "Internal error",
            ),
            UseCaseError::Internal(msg) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                msg.as_str(),
            ),
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

pub struct ApiError(pub UseCaseError);

impl From<UseCaseError> for ApiError {
    fn from(e: UseCaseError) -> Self {
        Self(e)
    }
}
