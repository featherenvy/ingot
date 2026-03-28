use axum::routing::{get, post};
use axum::{Json, Router};

use super::AppState;
use super::support::config::load_effective_config;
use crate::error::ApiError;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/config", get(get_global_config))
        .route("/api/demo-catalog", get(crate::demo::get_demo_catalog))
        .route("/api/demo-project", post(crate::demo::create_demo_project))
}

pub(super) async fn health() -> &'static str {
    "ok"
}

pub(super) async fn get_global_config() -> Result<Json<ingot_config::IngotConfig>, ApiError> {
    Ok(Json(load_effective_config(None)?))
}
