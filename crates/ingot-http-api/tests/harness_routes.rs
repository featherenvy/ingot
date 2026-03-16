use std::path::Path;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use tower::ServiceExt;

mod common;
use common::*;

#[tokio::test]
async fn get_harness_route_returns_empty_profile_when_file_is_missing() {
    let repo = temp_git_repo("ingot-http-api");
    let db = migrated_test_db("ingot-http-api-db").await;
    let app = test_router(db.clone());
    let project_id = create_project(app.clone(), &repo).await;

    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/projects/{project_id}/harness"))
                .method("GET")
                .body(Body::empty())
                .expect("build harness request"),
        )
        .await
        .expect("harness route response");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("harness body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("harness json");
    assert_eq!(json["commands"].as_array().map(Vec::len), Some(0));
    assert_eq!(json["skills"]["paths"].as_array().map(Vec::len), Some(0));
}

#[tokio::test]
async fn get_harness_route_rejects_malformed_profile() {
    let repo = temp_git_repo("ingot-http-api");
    let db = migrated_test_db("ingot-http-api-db").await;
    let app = test_router(db.clone());

    std::fs::create_dir_all(repo.join(".ingot")).expect("create .ingot");
    std::fs::write(
        repo.join(".ingot/harness.toml"),
        r#"
[commands.check]
run = "cargo check"
timeout = "bogus"
"#,
    )
    .expect("write malformed harness");

    let project_id = create_project(app.clone(), &repo).await;

    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/projects/{project_id}/harness"))
                .method("GET")
                .body(Body::empty())
                .expect("build harness request"),
        )
        .await
        .expect("harness route response");

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("harness body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("error json");
    assert!(
        json["error"]["message"]
            .as_str()
            .expect("error message")
            .contains("invalid harness profile")
    );
}

async fn create_project(app: axum::Router, repo: &Path) -> String {
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/projects")
                .method("POST")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "path": repo.display().to_string()
                    })
                    .to_string(),
                ))
                .expect("build project request"),
        )
        .await
        .expect("project route response");
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("create body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("project json");
    json["id"].as_str().expect("project id").to_owned()
}
