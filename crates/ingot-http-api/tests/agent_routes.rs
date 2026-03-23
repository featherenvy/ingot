use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use tower::ServiceExt;

mod common;
use common::*;

#[tokio::test]
async fn create_agent_route_probes_cli_and_lists_agents() {
    let db = migrated_test_db("ingot-http-api-db").await;
    let app = test_router(db.clone());
    let fake_codex = fake_codex_probe_script();

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/agents")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "name": "Codex CLI",
                        "adapter_kind": "codex",
                        "provider": "openai",
                        "model": "gpt-5-codex",
                        "cli_path": fake_codex.display().to_string()
                    })
                    .to_string(),
                ))
                .expect("build create request"),
        )
        .await
        .expect("create agent response");

    assert_eq!(response.status(), StatusCode::CREATED);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("agent json");
    assert_eq!(json["status"].as_str(), Some("available"));
    assert_eq!(json["slug"].as_str(), Some("codex-cli"));
    assert!(
        json["health_check"]
            .as_str()
            .is_some_and(|value| value.contains("codex exec help ok"))
    );

    let list_response = app
        .oneshot(
            Request::builder()
                .uri("/api/agents")
                .body(Body::empty())
                .expect("build list request"),
        )
        .await
        .expect("list agents response");
    assert_eq!(list_response.status(), StatusCode::OK);
    let list_body = to_bytes(list_response.into_body(), usize::MAX)
        .await
        .expect("read list body");
    let list_json: serde_json::Value = serde_json::from_slice(&list_body).expect("list json");
    assert_eq!(list_json.as_array().map(Vec::len), Some(1));
}

#[tokio::test]
async fn update_reprobe_and_delete_agent_routes_mutate_bootstrap_state() {
    let db = migrated_test_db("ingot-http-api-db").await;
    let app = test_router(db.clone());
    let fake_codex = fake_codex_probe_script();

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/agents")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "name": "Codex CLI",
                        "adapter_kind": "codex",
                        "provider": "openai",
                        "model": "gpt-5-codex",
                        "cli_path": fake_codex.display().to_string()
                    })
                    .to_string(),
                ))
                .expect("build create request"),
        )
        .await
        .expect("create agent response");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_json: serde_json::Value = serde_json::from_slice(&create_body).expect("create json");
    let agent_id = create_json["id"].as_str().expect("agent id");

    let update_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/agents/{agent_id}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "slug": "codex-primary",
                        "model": "gpt-5"
                    })
                    .to_string(),
                ))
                .expect("build update request"),
        )
        .await
        .expect("update agent response");

    assert_eq!(update_response.status(), StatusCode::OK);
    let update_body = to_bytes(update_response.into_body(), usize::MAX)
        .await
        .expect("read update body");
    let update_json: serde_json::Value = serde_json::from_slice(&update_body).expect("update json");
    assert_eq!(update_json["slug"].as_str(), Some("codex-primary"));
    assert_eq!(update_json["model"].as_str(), Some("gpt-5"));

    sqlx::query("UPDATE agents SET cli_path = '/definitely/missing/ingot-cli' WHERE id = ?")
        .bind(agent_id)
        .execute(db.raw_pool())
        .await
        .expect("update cli path");

    let reprobe_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/agents/{agent_id}/reprobe"))
                .body(Body::empty())
                .expect("build reprobe request"),
        )
        .await
        .expect("reprobe response");

    assert_eq!(reprobe_response.status(), StatusCode::OK);
    let reprobe_body = to_bytes(reprobe_response.into_body(), usize::MAX)
        .await
        .expect("read reprobe body");
    let reprobe_json: serde_json::Value =
        serde_json::from_slice(&reprobe_body).expect("reprobe json");
    assert_eq!(reprobe_json["status"].as_str(), Some("unavailable"));

    let delete_response = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/agents/{agent_id}"))
                .body(Body::empty())
                .expect("build delete request"),
        )
        .await
        .expect("delete response");

    assert_eq!(delete_response.status(), StatusCode::NO_CONTENT);

    let agents: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agents")
        .fetch_one(db.raw_pool())
        .await
        .expect("agent count");
    assert_eq!(agents, 0);
}
