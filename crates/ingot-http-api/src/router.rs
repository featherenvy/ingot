use axum::{Router, routing::get};

/// Build the Axum router with all API routes.
pub fn build_router() -> Router {
    Router::new().route("/api/health", get(health))
    // Project and agent registry
    // .route("/api/projects", ...)
    // .route("/api/agents", ...)
    // Config and definitions
    // .route("/api/config", ...)
    // .route("/api/workflows", ...)
    // .route("/api/reload", ...)
    // Item endpoints (project-scoped)
    // .route("/api/projects/:project_id/items", ...)
    // Job endpoints
    // .route("/api/projects/:project_id/jobs", ...)
    // Workspace and convergence endpoints
    // .route("/api/projects/:project_id/workspaces", ...)
    // Activity
    // .route("/api/activity", ...)
    // WebSocket
    // .route("/api/ws", ...)
}

async fn health() -> &'static str {
    "ok"
}
