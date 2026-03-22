use std::fs;
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use chrono::Utc;
use ingot_domain::project::{AgentRouting, AutoTriageDecision, AutoTriagePolicy, ExecutionMode};
use ingot_git::project_repo::{ensure_mirror, project_repo_paths};
use tower::ServiceExt;

mod common;
use common::*;
use ingot_usecases::{DispatchNotify, ProjectLocks};

#[tokio::test]
async fn create_project_route_registers_repo_and_exposes_project_config() {
    let repo = temp_git_repo("ingot-http-api");
    let db = migrated_test_db("ingot-http-api-db").await;

    fs::create_dir_all(repo.join(".ingot")).expect("create config dir");
    write_file(
        &repo.join(".ingot/config.yml"),
        "defaults:\n  candidate_rework_budget: 7\n  integration_rework_budget: 9\n  approval_policy: not_required\n  overflow_strategy: truncate\n",
    );

    let app = test_router(db.clone());
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/projects")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "path": repo.display().to_string(),
                        "color": "#123abc"
                    })
                    .to_string(),
                ))
                .expect("build request"),
        )
        .await
        .expect("create project response");

    assert_eq!(response.status(), StatusCode::CREATED);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("project json");
    let project_id = json["id"].as_str().expect("project id");

    assert_eq!(json["default_branch"].as_str(), Some("main"));
    assert_eq!(json["color"].as_str(), Some("#123abc"));
    assert_eq!(
        json["name"].as_str(),
        repo.file_name().and_then(|name| name.to_str())
    );

    let config_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/projects/{project_id}/config"))
                .body(Body::empty())
                .expect("build config request"),
        )
        .await
        .expect("project config response");

    assert_eq!(config_response.status(), StatusCode::OK);
    let config_body = to_bytes(config_response.into_body(), usize::MAX)
        .await
        .expect("read config body");
    let config_json: serde_json::Value = serde_json::from_slice(&config_body).expect("config json");

    assert_eq!(
        config_json["defaults"]["approval_policy"].as_str(),
        Some("not_required")
    );
    assert_eq!(
        config_json["defaults"]["candidate_rework_budget"].as_u64(),
        Some(7)
    );

    let list_response = app
        .oneshot(
            Request::builder()
                .uri("/api/projects")
                .body(Body::empty())
                .expect("build list request"),
        )
        .await
        .expect("list projects response");

    assert_eq!(list_response.status(), StatusCode::OK);
    let list_body = to_bytes(list_response.into_body(), usize::MAX)
        .await
        .expect("read list body");
    let list_json: serde_json::Value = serde_json::from_slice(&list_body).expect("list json");
    assert_eq!(list_json.as_array().map(Vec::len), Some(1));
}

#[tokio::test]
async fn project_activity_route_lists_recorded_activity() {
    let repo = temp_git_repo("ingot-http-api");
    let db = migrated_test_db("ingot-http-api-db").await;
    let app = test_router(db.clone());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/projects")
                .method("POST")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "name": "Test",
                        "path": repo.display().to_string()
                    })
                    .to_string(),
                ))
                .expect("build project request"),
        )
        .await
        .expect("project route response");
    let project_body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("project body");
    let project_json: serde_json::Value =
        serde_json::from_slice(&project_body).expect("project json");
    let project_id = project_json["id"].as_str().expect("project id");

    let item_response = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/projects/{project_id}/items"))
                .method("POST")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "title": "Title",
                        "description": "Desc",
                        "acceptance_criteria": "AC"
                    })
                    .to_string(),
                ))
                .expect("build item request"),
        )
        .await
        .expect("item route response");
    assert_eq!(item_response.status(), StatusCode::CREATED);

    let activity_response = test_router(db.clone())
        .oneshot(
            Request::builder()
                .uri(format!("/api/projects/{project_id}/activity"))
                .method("GET")
                .body(Body::empty())
                .expect("build activity request"),
        )
        .await
        .expect("activity route response");

    assert_eq!(activity_response.status(), StatusCode::OK);
    let activity_body = to_bytes(activity_response.into_body(), usize::MAX)
        .await
        .expect("activity body");
    let activity_json: serde_json::Value =
        serde_json::from_slice(&activity_body).expect("activity json");
    assert_eq!(activity_json.as_array().map(Vec::len), Some(1));
    assert_eq!(
        activity_json[0]["event_type"].as_str(),
        Some("item_created")
    );
}

#[tokio::test]
async fn update_and_delete_project_routes_mutate_registered_project() {
    let repo = temp_git_repo("ingot-http-api");
    let db = migrated_test_db("ingot-http-api-db").await;
    let app = test_router(db.clone());

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/projects")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "name": "Original",
                        "path": repo.display().to_string()
                    })
                    .to_string(),
                ))
                .expect("build create request"),
        )
        .await
        .expect("create project response");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_json: serde_json::Value = serde_json::from_slice(&create_body).expect("create json");
    let project_id = create_json["id"].as_str().expect("project id");

    let update_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/projects/{project_id}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "name": "Renamed",
                        "color": "#abcdef"
                    })
                    .to_string(),
                ))
                .expect("build update request"),
        )
        .await
        .expect("update project response");

    assert_eq!(update_response.status(), StatusCode::OK);
    let update_body = to_bytes(update_response.into_body(), usize::MAX)
        .await
        .expect("read update body");
    let update_json: serde_json::Value = serde_json::from_slice(&update_body).expect("update json");
    assert_eq!(update_json["name"].as_str(), Some("Renamed"));
    assert_eq!(update_json["color"].as_str(), Some("#abcdef"));

    let delete_response = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/projects/{project_id}"))
                .body(Body::empty())
                .expect("build delete request"),
        )
        .await
        .expect("delete project response");

    assert_eq!(delete_response.status(), StatusCode::NO_CONTENT);

    let projects: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM projects")
        .fetch_one(&db.pool)
        .await
        .expect("project count");
    assert_eq!(projects, 0);
}

#[tokio::test]
async fn update_project_route_waits_for_project_mutation_lock() {
    let repo = temp_git_repo("ingot-http-api");
    let db = migrated_test_db("ingot-http-api-db").await;
    let project_id = "prj_000000000000000000000000000000a1";
    let project = test_project_builder(&repo, project_id)
        .execution_mode(ExecutionMode::Autopilot)
        .build()
        .persist(&db)
        .await
        .expect("persist project");

    let state_root =
        std::env::temp_dir().join(format!("ingot-http-api-state-{}", uuid::Uuid::now_v7()));
    let paths = project_repo_paths(state_root.as_path(), project.id, &repo);
    ensure_mirror(&paths).await.expect("ensure mirror");

    let locks = ProjectLocks::default();
    let app = ingot_http_api::build_router_with_project_locks_and_state_root(
        db.clone(),
        locks.clone(),
        state_root,
        DispatchNotify::default(),
    );

    let guard = locks.acquire(project.id).await;
    let response_task = tokio::spawn(
        app.clone().oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/projects/{project_id}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "execution_mode": "manual"
                    })
                    .to_string(),
                ))
                .expect("build update request"),
        ),
    );

    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !response_task.is_finished(),
        "update route should wait for the project mutation lock"
    );

    drop(guard);

    let response = response_task
        .await
        .expect("update task join")
        .expect("update response");
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn update_project_route_allows_clearing_auto_triage_policy_with_null() {
    let repo = temp_git_repo("ingot-http-api");
    let db = migrated_test_db("ingot-http-api-db").await;
    let project_id = "prj_000000000000000000000000000000a3";
    let project = test_project_builder(&repo, project_id)
        .execution_mode(ExecutionMode::Autopilot)
        .auto_triage_policy(AutoTriagePolicy {
            critical: AutoTriageDecision::FixNow,
            high: AutoTriageDecision::Backlog,
            medium: AutoTriageDecision::Backlog,
            low: AutoTriageDecision::Skip,
        })
        .build()
        .persist(&db)
        .await
        .expect("persist project");

    let app = test_router(db.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/projects/{project_id}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "auto_triage_policy": null
                    })
                    .to_string(),
                ))
                .expect("build update request"),
        )
        .await
        .expect("update response");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read update body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("update json");
    assert!(json["auto_triage_policy"].is_null());

    let stored = db
        .get_project(project.id)
        .await
        .expect("reload stored project");
    assert!(
        stored.auto_triage_policy.is_none(),
        "explicit null should clear the saved auto-triage policy"
    );
}

#[tokio::test]
async fn update_project_route_allows_clearing_agent_routing_with_null() {
    let repo = temp_git_repo("ingot-http-api");
    let db = migrated_test_db("ingot-http-api-db").await;
    let project_id = "prj_000000000000000000000000000000a4";
    let project = test_project_builder(&repo, project_id)
        .execution_mode(ExecutionMode::Autopilot)
        .agent_routing(AgentRouting {
            author: Some("claude-code".into()),
            review: Some("codex".into()),
            investigate: None,
        })
        .build()
        .persist(&db)
        .await
        .expect("persist project");

    let app = test_router(db.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/projects/{project_id}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "agent_routing": null
                    })
                    .to_string(),
                ))
                .expect("build update request"),
        )
        .await
        .expect("update response");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read update body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("update json");
    assert!(json["agent_routing"].is_null());

    let stored = db
        .get_project(project.id)
        .await
        .expect("reload stored project");
    assert!(
        stored.agent_routing.is_none(),
        "explicit null should clear the saved agent routing"
    );
}

#[tokio::test]
async fn update_project_route_does_not_overwrite_fields_changed_while_waiting_for_lock() {
    let repo = temp_git_repo("ingot-http-api");
    let db = migrated_test_db("ingot-http-api-db").await;
    let project_id = "prj_000000000000000000000000000000a2";
    let project = test_project_builder(&repo, project_id)
        .name("Original")
        .execution_mode(ExecutionMode::Autopilot)
        .build()
        .persist(&db)
        .await
        .expect("persist project");

    let state_root =
        std::env::temp_dir().join(format!("ingot-http-api-state-{}", uuid::Uuid::now_v7()));
    let paths = project_repo_paths(state_root.as_path(), project.id, &repo);
    ensure_mirror(&paths).await.expect("ensure mirror");

    let locks = ProjectLocks::default();
    let app = ingot_http_api::build_router_with_project_locks_and_state_root(
        db.clone(),
        locks.clone(),
        state_root,
        DispatchNotify::default(),
    );

    let guard = locks.acquire(project.id).await;
    let response_task = tokio::spawn(
        app.clone().oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/projects/{project_id}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "execution_mode": "manual"
                    })
                    .to_string(),
                ))
                .expect("build update request"),
        ),
    );

    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !response_task.is_finished(),
        "update route should still be waiting for the project mutation lock"
    );

    let mut concurrently_updated = db
        .get_project(project.id)
        .await
        .expect("load project for concurrent update");
    concurrently_updated.name = "Renamed while blocked".into();
    concurrently_updated.updated_at = Utc::now();
    db.update_project(&concurrently_updated)
        .await
        .expect("persist concurrent update");

    drop(guard);

    let response = response_task
        .await
        .expect("update task join")
        .expect("update response");
    assert_eq!(response.status(), StatusCode::OK);

    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read update body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("update json");
    assert_eq!(json["execution_mode"].as_str(), Some("manual"));
    assert_eq!(json["name"].as_str(), Some("Renamed while blocked"));

    let stored = db
        .get_project(project.id)
        .await
        .expect("reload stored project");
    assert_eq!(stored.execution_mode, ExecutionMode::Manual);
    assert_eq!(stored.name, "Renamed while blocked");
}
