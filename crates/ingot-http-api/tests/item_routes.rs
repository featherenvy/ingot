use std::fs;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use chrono::Utc;
use ingot_domain::git_operation::{GitOperation, GitOperationStatus, OperationPayload};
use ingot_domain::ids::{GitOperationId, ProjectId, WorkspaceId};
use ingot_domain::job::{
    ContextPolicy, ExecutionPermission, JobStatus, OutcomeClass, OutputArtifactKind, PhaseKind,
};
use ingot_domain::workspace::WorkspaceKind;
use tower::ServiceExt;

mod common;
use common::*;

#[tokio::test]
async fn create_item_route_uses_project_config_defaults_when_policy_is_omitted() {
    let repo = temp_git_repo("ingot-http-api");
    let db = migrated_test_db("ingot-http-api-db").await;
    let app = test_router(db.clone());

    fs::create_dir_all(repo.join(".ingot")).expect("create config dir");
    write_file(
        &repo.join(".ingot/config.yml"),
        "defaults:\n  candidate_rework_budget: 7\n  integration_rework_budget: 9\n  approval_policy: not_required\n  overflow_strategy: truncate\n",
    );

    let create_project_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/projects")
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
        .expect("project response");
    let project_body = to_bytes(create_project_response.into_body(), usize::MAX)
        .await
        .expect("read project body");
    let project_json: serde_json::Value =
        serde_json::from_slice(&project_body).expect("project json");
    let project_id = project_json["id"].as_str().expect("project id");

    let item_response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/projects/{project_id}/items"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "title": "Config-backed item",
                        "description": "Load defaults from project config",
                        "acceptance_criteria": "The revision freezes config defaults"
                    })
                    .to_string(),
                ))
                .expect("build item request"),
        )
        .await
        .expect("item response");

    assert_eq!(item_response.status(), StatusCode::CREATED);
    let item_body = to_bytes(item_response.into_body(), usize::MAX)
        .await
        .expect("read item body");
    let item_json: serde_json::Value = serde_json::from_slice(&item_body).expect("item json");

    assert_eq!(
        item_json["current_revision"]["approval_policy"].as_str(),
        Some("not_required")
    );
    assert_eq!(
        item_json["item"]["approval_state"].as_str(),
        Some("not_required")
    );
    assert_eq!(
        item_json["current_revision"]["policy_snapshot"]["candidate_rework_budget"].as_u64(),
        Some(7)
    );
    assert_eq!(
        item_json["current_revision"]["policy_snapshot"]["integration_rework_budget"].as_u64(),
        Some(9)
    );
}

#[tokio::test]
async fn create_item_route_derives_initial_revision_with_null_seed_commit() {
    let repo = temp_git_repo("ingot-http-api");
    let seed_head = git_output(&repo, &["rev-parse", "HEAD"]);
    let db = migrated_test_db("ingot-http-api-db").await;

    let project_id = "prj_00000000000000000000000000000021".to_string();
    sqlx::query(
        "INSERT INTO projects (id, name, path, default_branch, color, created_at, updated_at)
         VALUES (?, 'Test', ?, 'main', '#000', ?, ?)",
    )
    .bind(project_id.as_str())
    .bind(repo.display().to_string())
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert project");

    let app = test_router(db.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/projects/{project_id}/items"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "title": "Implement feature",
                        "description": "Add the MVP path",
                        "acceptance_criteria": "The route creates an item"
                    })
                    .to_string(),
                ))
                .expect("build create request"),
        )
        .await
        .expect("create item response");

    assert_eq!(response.status(), StatusCode::CREATED);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("detail json");

    assert_eq!(
        json["current_revision"]["target_ref"].as_str(),
        Some("refs/heads/main")
    );
    assert_eq!(
        json["current_revision"]["seed_commit_oid"],
        serde_json::Value::Null
    );
    assert_eq!(
        json["current_revision"]["seed_target_commit_oid"].as_str(),
        Some(seed_head.as_str())
    );
    assert_eq!(
        json["evaluation"]["dispatchable_step_id"].as_str(),
        Some("author_initial")
    );

    let revision_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM item_revisions")
        .fetch_one(&db.pool)
        .await
        .expect("revision count");
    assert_eq!(revision_count, 1);
}

#[tokio::test]
async fn create_item_route_rejects_non_branch_target_ref() {
    let repo = temp_git_repo("ingot-http-api");
    let db = migrated_test_db("ingot-http-api-db").await;

    let project_id = "prj_00000000000000000000000000000022".to_string();
    sqlx::query(
        "INSERT INTO projects (id, name, path, default_branch, color, created_at, updated_at)
         VALUES (?, 'Test', ?, 'main', '#000', ?, ?)",
    )
    .bind(project_id.as_str())
    .bind(repo.display().to_string())
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert project");

    let response = test_router(db.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/projects/{project_id}/items"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "title": "Invalid target",
                        "description": "Reject non-branch refs",
                        "acceptance_criteria": "route returns invalid_target_ref",
                        "target_ref": "refs/tags/v1"
                    })
                    .to_string(),
                ))
                .expect("build request"),
        )
        .await
        .expect("item response");

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(json["error"]["code"].as_str(), Some("invalid_target_ref"));
}

#[tokio::test]
async fn create_item_route_rejects_git_invalid_branch_name() {
    let repo = temp_git_repo("ingot-http-api");
    let db = migrated_test_db("ingot-http-api-db").await;

    let project_id = "prj_00000000000000000000000000000023".to_string();
    sqlx::query(
        "INSERT INTO projects (id, name, path, default_branch, color, created_at, updated_at)
         VALUES (?, 'Test', ?, 'main', '#000', ?, ?)",
    )
    .bind(project_id.as_str())
    .bind(repo.display().to_string())
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert project");

    let response = test_router(db.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/projects/{project_id}/items"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "title": "Invalid branch",
                        "description": "Reject git-invalid branch names",
                        "acceptance_criteria": "route returns invalid_target_ref",
                        "target_ref": "foo..bar"
                    })
                    .to_string(),
                ))
                .expect("build request"),
        )
        .await
        .expect("item response");

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(json["error"]["code"].as_str(), Some("invalid_target_ref"));
}

#[tokio::test]
async fn defer_and_resume_routes_toggle_parking_state() {
    let repo = temp_git_repo("ingot-http-api");
    let db = migrated_test_db("ingot-http-api-db").await;
    let project_id = "prj_00000000000000000000000000000055".to_string();
    let item_id = "itm_00000000000000000000000000000055".to_string();
    let revision_id = "rev_00000000000000000000000000000055".to_string();
    let head = git_output(&repo, &["rev-parse", "HEAD"]);

    sqlx::query(
        "INSERT INTO projects (id, name, path, default_branch, color, created_at, updated_at)
         VALUES (?, 'Test', ?, 'main', '#000', ?, ?)",
    )
    .bind(project_id.as_str())
    .bind(repo.display().to_string())
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert project");
    sqlx::query(
        "INSERT INTO items (
            id, project_id, classification, workflow_version, lifecycle_state, parking_state,
            approval_state, escalation_state, current_revision_id, origin_kind, origin_finding_id,
            priority, labels, created_at, updated_at
         ) VALUES (?, ?, 'change', 'delivery:v1', 'open', 'active', 'not_requested', 'none', ?, 'manual', NULL, 'major', '[]', ?, ?)",
    )
    .bind(item_id.as_str())
    .bind(project_id.as_str())
    .bind(revision_id.as_str())
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert item");
    sqlx::query(
        "INSERT INTO item_revisions (
            id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
            approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid,
            seed_target_commit_oid, supersedes_revision_id, created_at
         ) VALUES (?, ?, 1, 'Title', 'Desc', 'AC', 'refs/heads/main', 'required', '{\"workflow_version\":\"delivery:v1\",\"approval_policy\":\"required\",\"candidate_rework_budget\":7,\"integration_rework_budget\":8}', '{\"author_initial\":\"author-initial\"}', ?, ?, NULL, ?)",
    )
    .bind(revision_id.as_str())
    .bind(item_id.as_str())
    .bind(&head)
    .bind(&head)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert revision");

    let app = test_router(db.clone());
    let defer_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/projects/{project_id}/items/{item_id}/defer"))
                .method("POST")
                .body(Body::empty())
                .expect("build defer request"),
        )
        .await
        .expect("defer route response");
    assert_eq!(defer_response.status(), StatusCode::OK);

    let resume_response = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/projects/{project_id}/items/{item_id}/resume"))
                .method("POST")
                .body(Body::empty())
                .expect("build resume request"),
        )
        .await
        .expect("resume route response");
    assert_eq!(resume_response.status(), StatusCode::OK);
    let body = to_bytes(resume_response.into_body(), usize::MAX)
        .await
        .expect("resume body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("resume json");
    assert_eq!(json["item"]["parking_state"].as_str(), Some("active"));
}

#[tokio::test]
async fn resume_route_auto_dispatches_projected_review_job() {
    let repo = temp_git_repo("ingot-http-api");
    let seed_head = git_output(&repo, &["rev-parse", "HEAD"]);
    write_file(&repo.join("tracked.txt"), "authored change");
    git(&repo, &["add", "tracked.txt"]);
    git(&repo, &["commit", "-m", "authored change"]);
    let authored_head = git_output(&repo, &["rev-parse", "HEAD"]);

    let db = migrated_test_db("ingot-http-api-db").await;
    let project_id = "prj_00000000000000000000000000000058".to_string();
    let item_id = "itm_00000000000000000000000000000058".to_string();
    let revision_id = "rev_00000000000000000000000000000058".to_string();
    let author_job_id = "job_00000000000000000000000000000058".to_string();

    sqlx::query(
        "INSERT INTO projects (id, name, path, default_branch, color, created_at, updated_at)
         VALUES (?, 'Test', ?, 'main', '#000', ?, ?)",
    )
    .bind(project_id.as_str())
    .bind(repo.display().to_string())
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert project");
    sqlx::query(
        "INSERT INTO items (
            id, project_id, classification, workflow_version, lifecycle_state, parking_state,
            approval_state, escalation_state, current_revision_id, origin_kind, origin_finding_id,
            priority, labels, created_at, updated_at
         ) VALUES (?, ?, 'change', 'delivery:v1', 'open', 'deferred', 'not_requested', 'none', ?, 'manual', NULL, 'major', '[]', ?, ?)",
    )
    .bind(item_id.as_str())
    .bind(project_id.as_str())
    .bind(revision_id.as_str())
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert item");
    sqlx::query(
        "INSERT INTO item_revisions (
            id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
            approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid,
            seed_target_commit_oid, supersedes_revision_id, created_at
         ) VALUES (?, ?, 1, 'Title', 'Desc', 'AC', 'refs/heads/main', 'required', '{}', '{}', ?, ?, NULL, ?)",
    )
    .bind(revision_id.as_str())
    .bind(item_id.as_str())
    .bind(&seed_head)
    .bind(&seed_head)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert revision");
    insert_test_job_row(
        &db,
        TestJobInsert {
            id: author_job_id.as_str(),
            project_id: project_id.as_str(),
            item_id: item_id.as_str(),
            item_revision_id: revision_id.as_str(),
            step_id: "author_initial",
            status: JobStatus::Completed,
            outcome_class: Some(OutcomeClass::Clean),
            phase_kind: PhaseKind::Author,
            workspace_kind: WorkspaceKind::Authoring,
            execution_permission: ExecutionPermission::MayMutate,
            context_policy: ContextPolicy::Fresh,
            phase_template_slug: "author-initial",
            output_artifact_kind: OutputArtifactKind::Commit,
            job_input: TestJobInput::AuthoringHead(&seed_head),
            output_commit_oid: Some(&authored_head),
            created_at: "2026-03-12T00:00:00Z",
            started_at: Some("2026-03-12T00:00:00Z"),
            ended_at: Some("2026-03-12T00:01:00Z"),
            ..TestJobInsert::new(
                author_job_id.as_str(),
                project_id.as_str(),
                item_id.as_str(),
                revision_id.as_str(),
                "author_initial",
            )
        },
    )
    .await;

    let response = test_router(db.clone())
        .oneshot(
            Request::builder()
                .uri(format!("/api/projects/{project_id}/items/{item_id}/resume"))
                .method("POST")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("resume route response");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("resume body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("resume json");
    assert_eq!(json["item"]["parking_state"].as_str(), Some("active"));

    let queued_review: (String, String, String) = sqlx::query_as(
        "SELECT step_id, input_base_commit_oid, input_head_commit_oid
         FROM jobs
         WHERE item_id = ? AND step_id = 'review_incremental_initial' AND status = 'queued'",
    )
    .bind(&item_id)
    .fetch_one(&db.pool)
    .await
    .expect("queued review job");
    assert_eq!(queued_review.0, "review_incremental_initial");
    assert_eq!(queued_review.1, seed_head);
    assert_eq!(queued_review.2, authored_head);
}

#[tokio::test]
async fn resume_route_returns_success_when_projected_review_auto_dispatch_cannot_bind_subject() {
    let repo = temp_git_repo("ingot-http-api");
    let db = migrated_test_db("ingot-http-api-db").await;
    let project_id = "prj_00000000000000000000000000000063".to_string();
    let item_id = "itm_00000000000000000000000000000063".to_string();
    let revision_id = "rev_00000000000000000000000000000063".to_string();
    let author_job_id = "job_00000000000000000000000000000063".to_string();

    sqlx::query(
        "INSERT INTO projects (id, name, path, default_branch, color, created_at, updated_at)
         VALUES (?, 'Test', ?, 'main', '#000', ?, ?)",
    )
    .bind(project_id.as_str())
    .bind(repo.display().to_string())
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert project");
    sqlx::query(
        "INSERT INTO items (
            id, project_id, classification, workflow_version, lifecycle_state, parking_state,
            approval_state, escalation_state, current_revision_id, origin_kind, origin_finding_id,
            priority, labels, created_at, updated_at
         ) VALUES (?, ?, 'change', 'delivery:v1', 'open', 'deferred', 'not_requested', 'none', ?, 'manual', NULL, 'major', '[]', ?, ?)",
    )
    .bind(item_id.as_str())
    .bind(project_id.as_str())
    .bind(revision_id.as_str())
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert item");
    sqlx::query(
        "INSERT INTO item_revisions (
            id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
            approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid,
            seed_target_commit_oid, supersedes_revision_id, created_at
         ) VALUES (?, ?, 1, 'Title', 'Desc', 'AC', 'refs/heads/main', 'required', '{}', '{}', NULL, 'target-head', NULL, ?)",
    )
    .bind(revision_id.as_str())
    .bind(item_id.as_str())
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert revision");
    insert_test_job_row(
        &db,
        TestJobInsert {
            id: author_job_id.as_str(),
            project_id: project_id.as_str(),
            item_id: item_id.as_str(),
            item_revision_id: revision_id.as_str(),
            step_id: "author_initial",
            status: JobStatus::Completed,
            outcome_class: Some(OutcomeClass::Clean),
            phase_kind: PhaseKind::Author,
            workspace_kind: WorkspaceKind::Authoring,
            execution_permission: ExecutionPermission::MayMutate,
            context_policy: ContextPolicy::Fresh,
            phase_template_slug: "author-initial",
            output_artifact_kind: OutputArtifactKind::Commit,
            job_input: TestJobInput::AuthoringHead("missing-head"),
            created_at: "2026-03-12T00:00:00Z",
            started_at: Some("2026-03-12T00:00:00Z"),
            ended_at: Some("2026-03-12T00:01:00Z"),
            ..TestJobInsert::new(
                author_job_id.as_str(),
                project_id.as_str(),
                item_id.as_str(),
                revision_id.as_str(),
                "author_initial",
            )
        },
    )
    .await;

    let response = test_router(db.clone())
        .oneshot(
            Request::builder()
                .uri(format!("/api/projects/{project_id}/items/{item_id}/resume"))
                .method("POST")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("resume route response");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("resume body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("resume json");
    assert_eq!(json["item"]["parking_state"].as_str(), Some("active"));

    let queued_review_jobs: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM jobs
         WHERE item_id = ? AND step_id = 'review_incremental_initial' AND status = 'queued'",
    )
    .bind(&item_id)
    .fetch_one(&db.pool)
    .await
    .expect("count queued review jobs");
    assert_eq!(queued_review_jobs, 0);

    let parsed_project_id = parse_id::<ProjectId>(&project_id);
    let parsed_item_id = parse_id(&item_id);
    let parsed_revision_id = parse_id(&revision_id);

    let project = db.get_project(parsed_project_id).await.expect("project");
    let item = db.get_item(parsed_item_id).await.expect("item");
    let revision = db.get_revision(parsed_revision_id).await.expect("revision");
    let jobs = db.list_jobs_by_item(parsed_item_id).await.expect("jobs");
    let findings = db
        .list_findings_by_item(parsed_item_id)
        .await
        .expect("findings");

    let result = ingot_usecases::dispatch::auto_dispatch_review(
        &db,
        &db,
        &db,
        &project,
        &item,
        &revision,
        &jobs,
        &findings,
        &[],
    )
    .await;
    assert!(matches!(
        result,
        Err(ingot_usecases::UseCaseError::Internal(message))
            if message.contains("incomplete candidate subject")
    ));
}

#[tokio::test]
async fn defer_route_cancels_lane_head_and_clears_granted() {
    let repo = temp_git_repo("ingot-http-api");
    let db = migrated_test_db("ingot-http-api-db").await;
    let project_id = "prj_00000000000000000000000000000056".to_string();
    let item_id = "itm_00000000000000000000000000000056".to_string();
    let revision_id = "rev_00000000000000000000000000000056".to_string();
    let running_job_id = "job_00000000000000000000000000000056".to_string();
    let head = git_output(&repo, &["rev-parse", "HEAD"]);

    sqlx::query(
        "INSERT INTO projects (id, name, path, default_branch, color, created_at, updated_at)
         VALUES (?, 'Test', ?, 'main', '#000', ?, ?)",
    )
    .bind(project_id.as_str())
    .bind(repo.display().to_string())
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert project");
    sqlx::query(
        "INSERT INTO items (
            id, project_id, classification, workflow_version, lifecycle_state, parking_state,
            approval_state, escalation_state, escalation_reason, current_revision_id, origin_kind, origin_finding_id,
            priority, labels, created_at, updated_at
         ) VALUES (?, ?, 'change', 'delivery:v1', 'open', 'active', 'granted', 'operator_required', 'checkout_sync_blocked', ?, 'manual', NULL, 'major', '[]', ?, ?)",
    )
    .bind(item_id.as_str())
    .bind(project_id.as_str())
    .bind(revision_id.as_str())
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert item");
    sqlx::query(
        "INSERT INTO item_revisions (
            id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
            approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid,
            seed_target_commit_oid, supersedes_revision_id, created_at
         ) VALUES (?, ?, 1, 'Title', 'Desc', 'AC', 'refs/heads/main', 'required', '{}', '{}', ?, ?, NULL, ?)",
    )
    .bind(revision_id.as_str())
    .bind(item_id.as_str())
    .bind(&head)
    .bind(&head)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert revision");
    sqlx::query(
        "INSERT INTO workspaces (
            id, project_id, kind, strategy, path, created_for_revision_id, parent_workspace_id,
            target_ref, workspace_ref, base_commit_oid, head_commit_oid, retention_policy,
            status, current_job_id, created_at, updated_at
         ) VALUES ('wrk_00000000000000000000000000000056', ?, 'authoring', 'worktree', ?, ?, NULL, 'refs/heads/main', 'refs/ingot/workspaces/defer-source', ?, ?, 'persistent', 'busy', ?, ?, ?)",
    )
    .bind(project_id.as_str())
    .bind(repo.join("defer-source").display().to_string())
    .bind(revision_id.as_str())
    .bind(&head)
    .bind(&head)
    .bind(running_job_id.as_str())
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert workspace");
    insert_test_job_row(
        &db,
        TestJobInsert {
            id: running_job_id.as_str(),
            project_id: project_id.as_str(),
            item_id: item_id.as_str(),
            item_revision_id: revision_id.as_str(),
            step_id: "author_initial",
            status: JobStatus::Running,
            phase_kind: PhaseKind::Author,
            workspace_id: Some("wrk_00000000000000000000000000000056"),
            workspace_kind: WorkspaceKind::Authoring,
            execution_permission: ExecutionPermission::MayMutate,
            context_policy: ContextPolicy::Fresh,
            phase_template_slug: "author-initial",
            output_artifact_kind: OutputArtifactKind::Commit,
            job_input: TestJobInput::AuthoringHead(&head),
            created_at: "2026-03-12T00:00:00Z",
            ..TestJobInsert::new(
                running_job_id.as_str(),
                project_id.as_str(),
                item_id.as_str(),
                revision_id.as_str(),
                "author_initial",
            )
        },
    )
    .await;
    sqlx::query(
        "INSERT INTO convergence_queue_entries (
            id, project_id, item_id, item_revision_id, target_ref, status, head_acquired_at,
            created_at, updated_at, released_at
         ) VALUES (?, ?, ?, ?, 'refs/heads/main', 'head', ?, ?, ?, NULL)",
    )
    .bind("cqe_00000000000000000000000000000056")
    .bind(&project_id)
    .bind(&item_id)
    .bind(&revision_id)
    .bind(TS)
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert queue entry");

    let response = test_router(db.clone())
        .oneshot(
            Request::builder()
                .uri(format!("/api/projects/{project_id}/items/{item_id}/defer"))
                .method("POST")
                .body(Body::empty())
                .expect("build defer request"),
        )
        .await
        .expect("defer route response");
    assert_eq!(response.status(), StatusCode::OK);

    let item_state: (String, String, String) = sqlx::query_as(
        "SELECT parking_state, approval_state, escalation_state FROM items WHERE id = ?",
    )
    .bind(&item_id)
    .fetch_one(&db.pool)
    .await
    .expect("item state");
    assert_eq!(item_state.0, "deferred");
    assert_eq!(item_state.1, "not_requested");
    assert_eq!(item_state.2, "none");

    let queue_state: (String,) =
        sqlx::query_as("SELECT status FROM convergence_queue_entries WHERE item_revision_id = ?")
            .bind(&revision_id)
            .fetch_one(&db.pool)
            .await
            .expect("queue state");
    assert_eq!(queue_state.0, "cancelled");

    let job_state: (String,) = sqlx::query_as("SELECT status FROM jobs WHERE id = ?")
        .bind(&running_job_id)
        .fetch_one(&db.pool)
        .await
        .expect("job state");
    assert_eq!(job_state.0, "cancelled");

    let workspace_state: (String, Option<String>) =
        sqlx::query_as("SELECT status, current_job_id FROM workspaces WHERE id = ?")
            .bind("wrk_00000000000000000000000000000056")
            .fetch_one(&db.pool)
            .await
            .expect("workspace state");
    assert_eq!(workspace_state.0, "ready");
    assert_eq!(workspace_state.1, None);
}

#[tokio::test]
async fn defer_route_refreshes_revision_context_summary_after_cancelling_jobs() {
    let repo = temp_git_repo("ingot-http-api");
    let db = migrated_test_db("ingot-http-api-db").await;
    let project_id = "prj_00000000000000000000000000000062";
    let item_id = "itm_00000000000000000000000000000062";
    let revision_id = "rev_00000000000000000000000000000062";
    let running_job_id = "job_00000000000000000000000000000062";
    let workspace_id = "wrk_00000000000000000000000000000062";
    let head = git_output(&repo, &["rev-parse", "HEAD"]);
    let stale_revision_context = serde_json::json!({
        "authoring_head_commit_oid": "stale-head",
        "changed_paths": ["src/lib.rs"],
        "latest_validation": serde_json::Value::Null,
        "latest_review": {
            "job_id": running_job_id,
            "schema_version": "review_report:v1",
            "outcome": "clean",
            "summary": "stale summary"
        },
        "accepted_result_refs": [{
            "job_id": running_job_id,
            "step_id": "review_candidate_initial",
            "schema_version": "review_report:v1",
            "outcome": "clean",
            "summary": "stale summary"
        }],
        "operator_notes_excerpt": serde_json::Value::Null,
    });

    sqlx::query(
        "INSERT INTO projects (id, name, path, default_branch, color, created_at, updated_at)
         VALUES (?, 'Test', ?, 'main', '#000', ?, ?)",
    )
    .bind(project_id)
    .bind(repo.display().to_string())
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert project");
    sqlx::query(
        "INSERT INTO items (
            id, project_id, classification, workflow_version, lifecycle_state, parking_state,
            approval_state, escalation_state, current_revision_id, origin_kind, origin_finding_id,
            priority, labels, created_at, updated_at
         ) VALUES (?, ?, 'change', 'delivery:v1', 'open', 'active', 'not_requested', 'none', ?, 'manual', NULL, 'major', '[]', ?, ?)",
    )
    .bind(item_id)
    .bind(project_id)
    .bind(revision_id)
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert item");
    sqlx::query(
        "INSERT INTO item_revisions (
            id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
            approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid,
            seed_target_commit_oid, supersedes_revision_id, created_at
         ) VALUES (?, ?, 1, 'Title', 'Desc', 'AC', 'refs/heads/main', 'required', '{}', '{}', ?, ?, NULL, ?)",
    )
    .bind(revision_id)
    .bind(item_id)
    .bind(&head)
    .bind(&head)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert revision");
    sqlx::query(
        "INSERT INTO workspaces (
            id, project_id, kind, strategy, path, created_for_revision_id, parent_workspace_id,
            target_ref, workspace_ref, base_commit_oid, head_commit_oid, retention_policy,
            status, current_job_id, created_at, updated_at
         ) VALUES (?, ?, 'authoring', 'worktree', ?, ?, NULL, 'refs/heads/main', 'refs/ingot/workspaces/defer-refresh', ?, ?, 'persistent', 'busy', ?, ?, ?)",
    )
    .bind(workspace_id)
    .bind(project_id)
    .bind(repo.join("defer-refresh").display().to_string())
    .bind(revision_id)
    .bind(&head)
    .bind(&head)
    .bind(running_job_id)
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert workspace");
    insert_test_job_row(
        &db,
        TestJobInsert {
            id: running_job_id,
            project_id,
            item_id,
            item_revision_id: revision_id,
            step_id: "review_candidate_initial",
            status: JobStatus::Running,
            phase_kind: PhaseKind::Review,
            workspace_id: Some(workspace_id),
            workspace_kind: WorkspaceKind::Review,
            execution_permission: ExecutionPermission::MustNotMutate,
            context_policy: ContextPolicy::ResumeContext,
            phase_template_slug: "review-candidate",
            output_artifact_kind: OutputArtifactKind::ReviewReport,
            job_input: TestJobInput::CandidateSubject(&head, &head),
            created_at: "2026-03-12T00:00:00Z",
            ..TestJobInsert::new(
                running_job_id,
                project_id,
                item_id,
                revision_id,
                "review_candidate_initial",
            )
        },
    )
    .await;
    sqlx::query(
        "INSERT INTO revision_contexts (
            item_revision_id, schema_version, payload, updated_from_job_id, updated_at
         ) VALUES (?, 'revision_context:v1', ?, ?, ?)",
    )
    .bind(revision_id)
    .bind(stale_revision_context.to_string())
    .bind(running_job_id)
    .bind(Utc::now())
    .execute(&db.pool)
    .await
    .expect("insert stale revision context");

    let response = test_router(db.clone())
        .oneshot(
            Request::builder()
                .uri(format!("/api/projects/{project_id}/items/{item_id}/defer"))
                .method("POST")
                .body(Body::empty())
                .expect("build defer request"),
        )
        .await
        .expect("defer route response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");

    assert_eq!(
        json["revision_context_summary"]["latest_review"],
        serde_json::Value::Null
    );
    assert_eq!(
        json["revision_context_summary"]["accepted_result_refs"],
        serde_json::json!([])
    );
    assert_eq!(
        json["revision_context_summary"]["changed_paths"],
        serde_json::json!([])
    );
}

#[tokio::test]
async fn revise_route_creates_superseding_revision() {
    let repo = temp_git_repo("ingot-http-api");
    let db = migrated_test_db("ingot-http-api-db").await;
    let project_id = "prj_00000000000000000000000000000054".to_string();
    let item_id = "itm_00000000000000000000000000000054".to_string();
    let revision_id = "rev_00000000000000000000000000000054".to_string();
    let head = git_output(&repo, &["rev-parse", "HEAD"]);

    sqlx::query(
        "INSERT INTO projects (id, name, path, default_branch, color, created_at, updated_at)
         VALUES (?, 'Test', ?, 'main', '#000', ?, ?)",
    )
    .bind(&project_id)
    .bind(repo.display().to_string())
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert project");
    sqlx::query(
        "INSERT INTO items (
            id, project_id, classification, workflow_version, lifecycle_state, parking_state,
            approval_state, escalation_state, escalation_reason, current_revision_id, origin_kind, origin_finding_id,
            priority, labels, created_at, updated_at
         ) VALUES (?, ?, 'change', 'delivery:v1', 'open', 'active', 'not_requested', 'operator_required', 'step_failed', ?, 'manual', NULL, 'major', '[]', ?, ?)",
    )
    .bind(&item_id)
    .bind(&project_id)
    .bind(&revision_id)
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert item");
    sqlx::query(
        "INSERT INTO item_revisions (
            id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
            approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid,
            seed_target_commit_oid, supersedes_revision_id, created_at
         ) VALUES (?, ?, 1, 'Title', 'Desc', 'AC', 'refs/heads/main', 'required', '{\"workflow_version\":\"delivery:v1\",\"approval_policy\":\"required\",\"candidate_rework_budget\":3,\"integration_rework_budget\":4}', '{\"author_initial\":\"author-initial\"}', ?, ?, NULL, ?)",
    )
    .bind(&revision_id)
    .bind(&item_id)
    .bind(&head)
    .bind(&head)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert revision");

    let app = test_router(db.clone());
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/projects/{project_id}/items/{item_id}/revise"))
                .method("POST")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "title": "Revised Title",
                        "approval_policy": "not_required"
                    })
                    .to_string(),
                ))
                .expect("build revise request"),
        )
        .await
        .expect("revise route response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("revise body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("revise json");
    assert_eq!(
        json["current_revision"]["title"].as_str(),
        Some("Revised Title")
    );
    assert_eq!(
        json["current_revision"]["approval_policy"].as_str(),
        Some("not_required")
    );
    assert_eq!(
        json["current_revision"]["policy_snapshot"]["approval_policy"].as_str(),
        Some("not_required")
    );
    assert_eq!(
        json["current_revision"]["policy_snapshot"]["candidate_rework_budget"].as_u64(),
        Some(3)
    );
    assert_eq!(
        json["current_revision"]["supersedes_revision_id"].as_str(),
        Some(revision_id.as_str())
    );
    assert_eq!(json["item"]["escalation_state"].as_str(), Some("none"));
    assert_eq!(
        json["item"]["approval_state"].as_str(),
        Some("not_required")
    );

    let revision_policy_snapshot: String = sqlx::query_scalar(
        "SELECT policy_snapshot FROM item_revisions WHERE item_id = ? AND revision_no = 2",
    )
    .bind(&item_id)
    .fetch_one(&db.pool)
    .await
    .expect("load revised policy snapshot");
    let revision_policy_snapshot: serde_json::Value =
        serde_json::from_str(&revision_policy_snapshot).expect("revised policy snapshot json");
    assert_eq!(
        revision_policy_snapshot["approval_policy"].as_str(),
        Some("not_required")
    );
    assert_eq!(
        revision_policy_snapshot["candidate_rework_budget"].as_u64(),
        Some(3)
    );
}

#[tokio::test]
async fn revise_route_cancels_current_lane_state() {
    let repo = temp_git_repo("ingot-http-api");
    let head = git_output(&repo, &["rev-parse", "HEAD"]);
    let db = migrated_test_db("ingot-http-api-db").await;
    let project_id = "prj_00000000000000000000000000000057".to_string();
    let item_id = "itm_00000000000000000000000000000057".to_string();
    let revision_id = "rev_00000000000000000000000000000057".to_string();
    let running_job_id = "job_00000000000000000000000000000057".to_string();
    let convergence_id = "conv_00000000000000000000000000000057".to_string();

    sqlx::query(
        "INSERT INTO projects (id, name, path, default_branch, color, created_at, updated_at)
         VALUES (?, 'Test', ?, 'main', '#000', ?, ?)",
    )
    .bind(&project_id)
    .bind(repo.display().to_string())
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert project");
    sqlx::query(
        "INSERT INTO items (
            id, project_id, classification, workflow_version, lifecycle_state, parking_state,
            approval_state, escalation_state, current_revision_id, origin_kind, origin_finding_id,
            priority, labels, created_at, updated_at
         ) VALUES (?, ?, 'change', 'delivery:v1', 'open', 'active', 'not_requested', 'none', ?, 'manual', NULL, 'major', '[]', ?, ?)",
    )
    .bind(&item_id)
    .bind(&project_id)
    .bind(&revision_id)
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert item");
    sqlx::query(
        "INSERT INTO item_revisions (
            id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
            approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid,
            seed_target_commit_oid, supersedes_revision_id, created_at
         ) VALUES (?, ?, 1, 'Title', 'Desc', 'AC', 'refs/heads/main', 'required', '{}', '{}', ?, ?, NULL, ?)",
    )
    .bind(&revision_id)
    .bind(&item_id)
    .bind(&head)
    .bind(&head)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert revision");
    sqlx::query(
        "INSERT INTO workspaces (
            id, project_id, kind, strategy, path, created_for_revision_id, parent_workspace_id,
            target_ref, workspace_ref, base_commit_oid, head_commit_oid, retention_policy,
            status, current_job_id, created_at, updated_at
         ) VALUES ('wrk_00000000000000000000000000000057', ?, 'authoring', 'worktree', ?, ?, NULL, 'refs/heads/main', 'refs/ingot/workspaces/revise-source', ?, ?, 'persistent', 'busy', ?, ?, ?)",
    )
    .bind(&project_id)
    .bind(repo.join("revise-source").display().to_string())
    .bind(&revision_id)
    .bind(&head)
    .bind(&head)
    .bind(&running_job_id)
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert source workspace");
    insert_test_job_row(
        &db,
        TestJobInsert {
            id: running_job_id.as_str(),
            project_id: project_id.as_str(),
            item_id: item_id.as_str(),
            item_revision_id: revision_id.as_str(),
            step_id: "author_initial",
            status: JobStatus::Running,
            phase_kind: PhaseKind::Author,
            workspace_id: Some("wrk_00000000000000000000000000000057"),
            workspace_kind: WorkspaceKind::Authoring,
            execution_permission: ExecutionPermission::MayMutate,
            context_policy: ContextPolicy::Fresh,
            phase_template_slug: "author-initial",
            output_artifact_kind: OutputArtifactKind::Commit,
            job_input: TestJobInput::AuthoringHead(&head),
            created_at: "2026-03-12T00:00:00Z",
            ..TestJobInsert::new(
                running_job_id.as_str(),
                project_id.as_str(),
                item_id.as_str(),
                revision_id.as_str(),
                "author_initial",
            )
        },
    )
    .await;
    sqlx::query(
        "INSERT INTO workspaces (
            id, project_id, kind, strategy, path, created_for_revision_id, parent_workspace_id,
            target_ref, workspace_ref, base_commit_oid, head_commit_oid, retention_policy,
            status, current_job_id, created_at, updated_at
         ) VALUES ('wrk_00000000000000000000000000000157', ?, 'integration', 'worktree', ?, ?, NULL, 'refs/heads/main', 'refs/ingot/workspaces/revise-integration', ?, ?, 'ephemeral', 'ready', NULL, ?, ?)",
    )
    .bind(&project_id)
    .bind(repo.join("revise-integration").display().to_string())
    .bind(&revision_id)
    .bind(&head)
    .bind(&head)
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert integration workspace");
    sqlx::query(
        "INSERT INTO convergences (
            id, project_id, item_id, item_revision_id, source_workspace_id, integration_workspace_id,
            source_head_commit_oid, target_ref, strategy, status, input_target_commit_oid,
            prepared_commit_oid, final_target_commit_oid, conflict_summary, created_at, completed_at
         ) VALUES (?, ?, ?, ?, 'wrk_00000000000000000000000000000057', 'wrk_00000000000000000000000000000157', ?, 'refs/heads/main', 'rebase_then_fast_forward', 'prepared', ?, ?, NULL, NULL, ?, NULL)",
    )
    .bind(&convergence_id)
    .bind(&project_id)
    .bind(&item_id)
    .bind(&revision_id)
    .bind(&head)
    .bind(&head)
    .bind(&head)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert convergence");
    sqlx::query(
        "INSERT INTO convergence_queue_entries (
            id, project_id, item_id, item_revision_id, target_ref, status, head_acquired_at,
            created_at, updated_at, released_at
         ) VALUES (?, ?, ?, ?, 'refs/heads/main', 'head', ?, ?, ?, NULL)",
    )
    .bind("cqe_00000000000000000000000000000057")
    .bind(&project_id)
    .bind(&item_id)
    .bind(&revision_id)
    .bind(TS)
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert queue entry");
    db.create_git_operation(&GitOperation {
        id: GitOperationId::new(),
        project_id: parse_id::<ProjectId>(&project_id),
        entity_id: convergence_id.clone(),
        payload: OperationPayload::PrepareConvergenceCommit {
            workspace_id: "wrk_00000000000000000000000000000057"
                .parse::<WorkspaceId>()
                .unwrap(),
            ref_name: Some("refs/ingot/workspaces/revise-source".into()),
            expected_old_oid: head.clone(),
            new_oid: Some(head.clone()),
            commit_oid: Some(head.clone()),
            replay_metadata: None,
        },
        status: GitOperationStatus::Applied,
        created_at: Utc::now(),
        completed_at: None,
    })
    .await
    .expect("insert prepare op");
    db.create_git_operation(&GitOperation {
        id: GitOperationId::new(),
        project_id: parse_id::<ProjectId>(&project_id),
        entity_id: convergence_id.clone(),
        payload: OperationPayload::FinalizeTargetRef {
            workspace_id: None,
            ref_name: "refs/heads/main".into(),
            expected_old_oid: head.clone(),
            new_oid: head.clone(),
            commit_oid: Some(head.clone()),
        },
        status: GitOperationStatus::Applied,
        created_at: Utc::now(),
        completed_at: None,
    })
    .await
    .expect("insert finalize op");

    let response = test_router(db.clone())
        .oneshot(
            Request::builder()
                .uri(format!("/api/projects/{project_id}/items/{item_id}/revise"))
                .method("POST")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{\"title\":\"Revised\"}"))
                .expect("build revise request"),
        )
        .await
        .expect("revise route response");
    assert_eq!(response.status(), StatusCode::OK);

    let queue_state: (String,) =
        sqlx::query_as("SELECT status FROM convergence_queue_entries WHERE item_revision_id = ?")
            .bind(&revision_id)
            .fetch_one(&db.pool)
            .await
            .expect("queue state");
    assert_eq!(queue_state.0, "cancelled");

    let convergence_state: (String,) =
        sqlx::query_as("SELECT status FROM convergences WHERE id = ?")
            .bind(&convergence_id)
            .fetch_one(&db.pool)
            .await
            .expect("convergence state");
    assert_eq!(convergence_state.0, "cancelled");

    let job_state: (String,) = sqlx::query_as("SELECT status FROM jobs WHERE id = ?")
        .bind(&running_job_id)
        .fetch_one(&db.pool)
        .await
        .expect("job state");
    assert_eq!(job_state.0, "cancelled");

    let workspace_state: (String, Option<String>) =
        sqlx::query_as("SELECT status, current_job_id FROM workspaces WHERE id = ?")
            .bind("wrk_00000000000000000000000000000057")
            .fetch_one(&db.pool)
            .await
            .expect("workspace state");
    assert_eq!(workspace_state.0, "ready");
    assert_eq!(workspace_state.1, None);

    let op_states: Vec<(String, String)> = sqlx::query_as(
        "SELECT operation_kind, status FROM git_operations WHERE entity_id = ? ORDER BY operation_kind ASC",
    )
    .bind(&convergence_id)
    .fetch_all(&db.pool)
    .await
    .expect("operation states");
    assert!(
        op_states
            .iter()
            .any(|(kind, status)| { kind == "finalize_target_ref" && status == "failed" })
    );
    assert!(
        op_states
            .iter()
            .all(|(_, status)| { status == "failed" || status == "reconciled" })
    );
}

#[tokio::test]
async fn revise_route_rejects_non_branch_target_ref() {
    let repo = temp_git_repo("ingot-http-api");
    let db = migrated_test_db("ingot-http-api-db").await;
    let project_id = "prj_00000000000000000000000000000058".to_string();
    let item_id = "itm_00000000000000000000000000000058".to_string();
    let revision_id = "rev_00000000000000000000000000000058".to_string();
    let head = git_output(&repo, &["rev-parse", "HEAD"]);

    sqlx::query(
        "INSERT INTO projects (id, name, path, default_branch, color, created_at, updated_at)
         VALUES (?, 'Test', ?, 'main', '#000', ?, ?)",
    )
    .bind(&project_id)
    .bind(repo.display().to_string())
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert project");
    sqlx::query(
        "INSERT INTO items (
            id, project_id, classification, workflow_version, lifecycle_state, parking_state,
            approval_state, escalation_state, current_revision_id, origin_kind, origin_finding_id,
            priority, labels, created_at, updated_at
         ) VALUES (?, ?, 'change', 'delivery:v1', 'open', 'active', 'not_requested', 'none', ?, 'manual', NULL, 'major', '[]', ?, ?)",
    )
    .bind(&item_id)
    .bind(&project_id)
    .bind(&revision_id)
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert item");
    sqlx::query(
        "INSERT INTO item_revisions (
            id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
            approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid,
            seed_target_commit_oid, supersedes_revision_id, created_at
         ) VALUES (?, ?, 1, 'Title', 'Desc', 'AC', 'refs/heads/main', 'required', '{}', '{}', ?, ?, NULL, ?)",
    )
    .bind(&revision_id)
    .bind(&item_id)
    .bind(&head)
    .bind(&head)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert revision");

    let response = test_router(db.clone())
        .oneshot(
            Request::builder()
                .uri(format!("/api/projects/{project_id}/items/{item_id}/revise"))
                .method("POST")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{\"target_ref\":\"refs/remotes/origin/main\"}"))
                .expect("build revise request"),
        )
        .await
        .expect("revise route response");

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(json["error"]["code"].as_str(), Some("invalid_target_ref"));
}

#[tokio::test]
async fn revise_route_rejects_git_invalid_branch_name() {
    let repo = temp_git_repo("ingot-http-api");
    let db = migrated_test_db("ingot-http-api-db").await;
    let project_id = "prj_00000000000000000000000000000061".to_string();
    let item_id = "itm_00000000000000000000000000000061".to_string();
    let revision_id = "rev_00000000000000000000000000000061".to_string();
    let head = git_output(&repo, &["rev-parse", "HEAD"]);

    sqlx::query(
        "INSERT INTO projects (id, name, path, default_branch, color, created_at, updated_at)
         VALUES (?, 'Test', ?, 'main', '#000', ?, ?)",
    )
    .bind(&project_id)
    .bind(repo.display().to_string())
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert project");
    sqlx::query(
        "INSERT INTO items (
            id, project_id, classification, workflow_version, lifecycle_state, parking_state,
            approval_state, escalation_state, current_revision_id, origin_kind, origin_finding_id,
            priority, labels, created_at, updated_at
         ) VALUES (?, ?, 'change', 'delivery:v1', 'open', 'active', 'not_requested', 'none', ?, 'manual', NULL, 'major', '[]', ?, ?)",
    )
    .bind(&item_id)
    .bind(&project_id)
    .bind(&revision_id)
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert item");
    sqlx::query(
        "INSERT INTO item_revisions (
            id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
            approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid,
            seed_target_commit_oid, supersedes_revision_id, created_at
         ) VALUES (?, ?, 1, 'Title', 'Desc', 'AC', 'refs/heads/main', 'required', '{}', '{}', ?, ?, NULL, ?)",
    )
    .bind(&revision_id)
    .bind(&item_id)
    .bind(&head)
    .bind(&head)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert revision");

    let response = test_router(db.clone())
        .oneshot(
            Request::builder()
                .uri(format!("/api/projects/{project_id}/items/{item_id}/revise"))
                .method("POST")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{\"target_ref\":\"bad@{name}\"}"))
                .expect("build revise request"),
        )
        .await
        .expect("revise route response");

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(json["error"]["code"].as_str(), Some("invalid_target_ref"));
}

#[tokio::test]
async fn dismiss_and_reopen_routes_close_and_reopen_item() {
    let repo = temp_git_repo("ingot-http-api");
    let db = migrated_test_db("ingot-http-api-db").await;
    let project_id = "prj_00000000000000000000000000000053".to_string();
    let item_id = "itm_00000000000000000000000000000053".to_string();
    let revision_id = "rev_00000000000000000000000000000053".to_string();
    let head = git_output(&repo, &["rev-parse", "HEAD"]);

    sqlx::query(
        "INSERT INTO projects (id, name, path, default_branch, color, created_at, updated_at)
         VALUES (?, 'Test', ?, 'main', '#000', ?, ?)",
    )
    .bind(&project_id)
    .bind(repo.display().to_string())
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert project");
    sqlx::query(
        "INSERT INTO items (
            id, project_id, classification, workflow_version, lifecycle_state, parking_state,
            approval_state, escalation_state, current_revision_id, origin_kind, origin_finding_id,
            priority, labels, created_at, updated_at
         ) VALUES (?, ?, 'change', 'delivery:v1', 'open', 'active', 'not_requested', 'none', ?, 'manual', NULL, 'major', '[]', ?, ?)",
    )
    .bind(&item_id)
    .bind(&project_id)
    .bind(&revision_id)
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert item");
    sqlx::query(
        "INSERT INTO item_revisions (
            id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
            approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid,
            seed_target_commit_oid, supersedes_revision_id, created_at
         ) VALUES (?, ?, 1, 'Title', 'Desc', 'AC', 'refs/heads/main', 'required', '{\"workflow_version\":\"delivery:v1\",\"approval_policy\":\"required\",\"candidate_rework_budget\":5,\"integration_rework_budget\":6}', '{\"author_initial\":\"author-initial\"}', ?, ?, NULL, ?)",
    )
    .bind(&revision_id)
    .bind(&item_id)
    .bind(&head)
    .bind(&head)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert revision");

    let app = test_router(db.clone());
    let dismiss_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/projects/{project_id}/items/{item_id}/dismiss"
                ))
                .method("POST")
                .body(Body::empty())
                .expect("build dismiss request"),
        )
        .await
        .expect("dismiss route response");
    assert_eq!(dismiss_response.status(), StatusCode::OK);

    let reopen_response = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/projects/{project_id}/items/{item_id}/reopen"))
                .method("POST")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "approval_policy": "not_required"
                    })
                    .to_string(),
                ))
                .expect("build reopen request"),
        )
        .await
        .expect("reopen route response");
    assert_eq!(reopen_response.status(), StatusCode::OK);
    let body = to_bytes(reopen_response.into_body(), usize::MAX)
        .await
        .expect("reopen body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("reopen json");
    assert_eq!(json["item"]["lifecycle_state"].as_str(), Some("open"));
    assert_eq!(json["item"]["done_reason"], serde_json::Value::Null);
    assert_eq!(
        json["current_revision"]["approval_policy"].as_str(),
        Some("not_required")
    );
    assert_eq!(
        json["current_revision"]["policy_snapshot"]["approval_policy"].as_str(),
        Some("not_required")
    );
    assert_eq!(
        json["current_revision"]["policy_snapshot"]["candidate_rework_budget"].as_u64(),
        Some(5)
    );
    assert_eq!(
        json["current_revision"]["supersedes_revision_id"].as_str(),
        Some(revision_id.as_str())
    );
    assert_eq!(
        json["item"]["approval_state"].as_str(),
        Some("not_required")
    );

    let revision_policy_snapshot: String = sqlx::query_scalar(
        "SELECT policy_snapshot FROM item_revisions WHERE item_id = ? AND revision_no = 2",
    )
    .bind(&item_id)
    .fetch_one(&db.pool)
    .await
    .expect("load reopened policy snapshot");
    let revision_policy_snapshot: serde_json::Value =
        serde_json::from_str(&revision_policy_snapshot).expect("reopened policy snapshot json");
    assert_eq!(
        revision_policy_snapshot["approval_policy"].as_str(),
        Some("not_required")
    );
    assert_eq!(
        revision_policy_snapshot["candidate_rework_budget"].as_u64(),
        Some(5)
    );
}

#[tokio::test]
async fn dismiss_route_cancels_lane_state() {
    let repo = temp_git_repo("ingot-http-api");
    let head = git_output(&repo, &["rev-parse", "HEAD"]);
    let db = migrated_test_db("ingot-http-api-db").await;
    let project_id = "prj_00000000000000000000000000000059".to_string();
    let item_id = "itm_00000000000000000000000000000059".to_string();
    let revision_id = "rev_00000000000000000000000000000059".to_string();
    let convergence_id = "conv_00000000000000000000000000000059".to_string();

    sqlx::query(
        "INSERT INTO projects (id, name, path, default_branch, color, created_at, updated_at)
         VALUES (?, 'Test', ?, 'main', '#000', ?, ?)",
    )
    .bind(&project_id)
    .bind(repo.display().to_string())
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert project");
    sqlx::query(
        "INSERT INTO items (
            id, project_id, classification, workflow_version, lifecycle_state, parking_state,
            approval_state, escalation_state, current_revision_id, origin_kind, origin_finding_id,
            priority, labels, created_at, updated_at
         ) VALUES (?, ?, 'change', 'delivery:v1', 'open', 'active', 'granted', 'none', ?, 'manual', NULL, 'major', '[]', ?, ?)",
    )
    .bind(&item_id)
    .bind(&project_id)
    .bind(&revision_id)
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert item");
    sqlx::query(
        "INSERT INTO item_revisions (
            id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
            approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid,
            seed_target_commit_oid, supersedes_revision_id, created_at
         ) VALUES (?, ?, 1, 'Title', 'Desc', 'AC', 'refs/heads/main', 'required', '{}', '{}', ?, ?, NULL, ?)",
    )
    .bind(&revision_id)
    .bind(&item_id)
    .bind(&head)
    .bind(&head)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert revision");
    sqlx::query(
        "INSERT INTO workspaces (
            id, project_id, kind, strategy, path, created_for_revision_id, parent_workspace_id,
            target_ref, workspace_ref, base_commit_oid, head_commit_oid, retention_policy,
            status, current_job_id, created_at, updated_at
         ) VALUES ('wrk_00000000000000000000000000000059', ?, 'authoring', 'worktree', ?, ?, NULL, 'refs/heads/main', 'refs/ingot/workspaces/dismiss-source', ?, ?, 'persistent', 'ready', NULL, ?, ?)",
    )
    .bind(&project_id)
    .bind(repo.join("dismiss-source").display().to_string())
    .bind(&revision_id)
    .bind(&head)
    .bind(&head)
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert source workspace");
    sqlx::query(
        "INSERT INTO workspaces (
            id, project_id, kind, strategy, path, created_for_revision_id, parent_workspace_id,
            target_ref, workspace_ref, base_commit_oid, head_commit_oid, retention_policy,
            status, current_job_id, created_at, updated_at
         ) VALUES ('wrk_00000000000000000000000000000060', ?, 'integration', 'worktree', ?, ?, 'wrk_00000000000000000000000000000059', 'refs/heads/main', 'refs/ingot/workspaces/dismiss-integration', ?, ?, 'persistent', 'ready', NULL, ?, ?)",
    )
    .bind(&project_id)
    .bind(repo.join("dismiss-integration").display().to_string())
    .bind(&revision_id)
    .bind(&head)
    .bind(&head)
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert integration workspace");
    sqlx::query(
        "INSERT INTO convergences (
            id, project_id, item_id, item_revision_id, source_workspace_id, integration_workspace_id,
            source_head_commit_oid, target_ref, strategy, status, input_target_commit_oid,
            prepared_commit_oid, final_target_commit_oid, conflict_summary, created_at, completed_at
         ) VALUES (?, ?, ?, ?, 'wrk_00000000000000000000000000000059', 'wrk_00000000000000000000000000000060', ?, 'refs/heads/main', 'rebase_then_fast_forward', 'running', ?, ?, NULL, NULL, ?, NULL)",
    )
    .bind(&convergence_id)
    .bind(&project_id)
    .bind(&item_id)
    .bind(&revision_id)
    .bind(&head)
    .bind(&head)
    .bind(&head)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert convergence");
    sqlx::query(
        "INSERT INTO convergence_queue_entries (
            id, project_id, item_id, item_revision_id, target_ref, status, head_acquired_at,
            created_at, updated_at, released_at
         ) VALUES (?, ?, ?, ?, 'refs/heads/main', 'head', ?, ?, ?, NULL)",
    )
    .bind("cqe_00000000000000000000000000000059")
    .bind(&project_id)
    .bind(&item_id)
    .bind(&revision_id)
    .bind(TS)
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert queue entry");

    let response = test_router(db.clone())
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/projects/{project_id}/items/{item_id}/dismiss"
                ))
                .method("POST")
                .body(Body::empty())
                .expect("build dismiss request"),
        )
        .await
        .expect("dismiss response");
    assert_eq!(response.status(), StatusCode::OK);

    let queue_state: (String,) =
        sqlx::query_as("SELECT status FROM convergence_queue_entries WHERE item_revision_id = ?")
            .bind(&revision_id)
            .fetch_one(&db.pool)
            .await
            .expect("queue state");
    assert_eq!(queue_state.0, "cancelled");

    let convergence_state: (String,) =
        sqlx::query_as("SELECT status FROM convergences WHERE id = ?")
            .bind(&convergence_id)
            .fetch_one(&db.pool)
            .await
            .expect("convergence state");
    assert_eq!(convergence_state.0, "cancelled");
}
