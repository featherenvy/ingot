use axum::extract::path::ErrorKind as PathErrorKind;
use axum::extract::rejection::{FailedToDeserializePathParams, PathRejection};
use axum::extract::{FromRequestParts, Path, RawPathParams};
use axum::http::request::Parts;
use ingot_domain::ids::{AgentId, FindingId, ItemId, JobId, ProjectId, WorkspaceId};
use serde::de::DeserializeOwned;

use crate::error::ApiError;

#[derive(Debug)]
pub(crate) struct ApiPath<T>(pub(crate) T);

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

fn path_param_entity_name(key: &str) -> &str {
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

#[cfg(test)]
mod tests {
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use axum::{Json, Router};
    use ingot_domain::ids::{ItemId, ProjectId};
    use serde::Deserialize;
    use tower::ServiceExt;

    use super::ApiPath;
    use crate::error::ApiError;

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
