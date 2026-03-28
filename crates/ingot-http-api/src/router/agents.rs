use std::path::PathBuf;

use super::deps::*;
use super::support::{
    errors::{repo_to_agent, repo_to_agent_mutation, repo_to_internal},
    normalize::{normalize_agent_slug, normalize_non_empty},
    path::ApiPath,
};
use super::types::*;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/agents", get(list_agents).post(create_agent))
        .route(
            "/api/agents/{agent_id}",
            put(update_agent).delete(delete_agent),
        )
        .route("/api/agents/{agent_id}/reprobe", post(reprobe_agent))
}

pub(super) async fn list_agents(
    State(state): State<AppState>,
) -> Result<Json<Vec<Agent>>, ApiError> {
    let agents = state.db.list_agents().await.map_err(repo_to_internal)?;
    Ok(Json(agents))
}

pub(super) async fn create_agent(
    State(state): State<AppState>,
    Json(request): Json<CreateAgentRequest>,
) -> Result<(StatusCode, Json<Agent>), ApiError> {
    let mut agent = Agent {
        id: AgentId::new(),
        slug: normalize_agent_slug(request.slug.as_deref(), &request.name)?,
        name: normalize_non_empty("agent name", &request.name)?,
        adapter_kind: request.adapter_kind,
        provider: request.provider,
        model: request.model,
        cli_path: PathBuf::from(normalize_non_empty("cli path", &request.cli_path)?),
        capabilities: request
            .capabilities
            .unwrap_or_else(|| default_agent_capabilities(request.adapter_kind)),
        health_check: None,
        status: AgentStatus::Probing,
    };
    probe_and_apply(&mut agent).await;

    state
        .db
        .create_agent(&agent)
        .await
        .map_err(repo_to_agent_mutation)?;

    Ok((StatusCode::CREATED, Json(agent)))
}

pub(super) async fn update_agent(
    State(state): State<AppState>,
    ApiPath(AgentPathParams { agent_id }): ApiPath<AgentPathParams>,
    Json(request): Json<UpdateAgentRequest>,
) -> Result<Json<Agent>, ApiError> {
    let existing = state.db.get_agent(agent_id).await.map_err(repo_to_agent)?;
    let existing_name = existing.name.clone();
    let existing_slug = existing.slug.clone();
    let existing_provider = existing.provider;
    let existing_model = existing.model.clone();
    let existing_cli_path = existing.cli_path.clone();
    let existing_capabilities = existing.capabilities.clone();
    let existing_health_check = existing.health_check.clone();
    let adapter_kind = request.adapter_kind.unwrap_or(existing.adapter_kind);
    let name = match request.name.as_deref() {
        Some(name) => normalize_non_empty("agent name", name)?,
        None => existing_name,
    };
    let mut agent = Agent {
        id: existing.id,
        slug: match request.slug.as_deref() {
            Some(slug) => normalize_agent_slug(Some(slug), &name)?,
            None => existing_slug,
        },
        name,
        adapter_kind,
        provider: request.provider.unwrap_or(existing_provider),
        model: request.model.unwrap_or(existing_model),
        cli_path: match request.cli_path.as_deref() {
            Some(cli_path) => PathBuf::from(normalize_non_empty("cli path", cli_path)?),
            None => existing_cli_path,
        },
        capabilities: request.capabilities.unwrap_or(existing_capabilities),
        health_check: existing_health_check,
        status: AgentStatus::Probing,
    };
    probe_and_apply(&mut agent).await;

    state
        .db
        .update_agent(&agent)
        .await
        .map_err(repo_to_agent_mutation)?;

    Ok(Json(agent))
}

pub(super) async fn delete_agent(
    State(state): State<AppState>,
    ApiPath(AgentPathParams { agent_id }): ApiPath<AgentPathParams>,
) -> Result<StatusCode, ApiError> {
    state
        .db
        .delete_agent(agent_id)
        .await
        .map_err(repo_to_agent_mutation)?;

    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn reprobe_agent(
    State(state): State<AppState>,
    ApiPath(AgentPathParams { agent_id }): ApiPath<AgentPathParams>,
) -> Result<Json<Agent>, ApiError> {
    let mut agent = state.db.get_agent(agent_id).await.map_err(repo_to_agent)?;
    probe_and_apply(&mut agent).await;

    state
        .db
        .update_agent(&agent)
        .await
        .map_err(repo_to_agent_mutation)?;

    Ok(Json(agent))
}
