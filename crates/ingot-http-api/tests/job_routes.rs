use std::path::PathBuf;
use std::str::FromStr;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use ingot_agent_runtime::{DispatcherConfig, JobDispatcher};
use ingot_domain::activity::ActivityEventType;
use ingot_domain::convergence::ConvergenceStatus;
use ingot_domain::ids::ProjectId;
use ingot_domain::item::{ApprovalState, EscalationReason};
use ingot_domain::job::{
    ContextPolicy, ExecutionPermission, JobInput, JobStatus, OutcomeClass, OutputArtifactKind,
    PhaseKind,
};
use ingot_domain::test_support::{
    ConvergenceBuilder, ItemBuilder, JobBuilder, ProjectBuilder, RevisionBuilder, WorkspaceBuilder,
};
use ingot_domain::workspace::{RetentionPolicy, WorkspaceKind, WorkspaceStatus};
use ingot_test_support::reports::clean_review_report;
use tower::ServiceExt;

mod common;
use common::*;

#[tokio::test]
async fn logs_route_returns_normalized_output_and_raw_route_returns_raw_artifacts() {
    let (_repo, db, _project_id, _item_id, job_id) = seeded_route_test_app().await;
    let state_root = ingot_test_support::env::temp_state_root("ingot-http-api-job-logs-state");
    let logs_dir = PathBuf::from(&state_root).join("logs").join(&job_id);
    tokio::fs::create_dir_all(&logs_dir)
        .await
        .expect("create logs dir");
    tokio::fs::write(logs_dir.join("prompt.txt"), "review the candidate")
        .await
        .expect("write prompt");
    tokio::fs::write(
        logs_dir.join("stdout.log"),
        "{\"type\":\"item.completed\"}\n",
    )
    .await
    .expect("write stdout");
    tokio::fs::write(logs_dir.join("stderr.log"), "warn\n")
        .await
        .expect("write stderr");
    tokio::fs::write(
        logs_dir.join("output.jsonl"),
        concat!(
            "{\"sequence\":1,\"channel\":\"primary\",\"kind\":\"text\",\"status\":null,",
            "\"title\":null,\"text\":\"hello\",\"data\":null}\n"
        ),
    )
    .await
    .expect("write output jsonl");
    tokio::fs::write(
        logs_dir.join("result.json"),
        serde_json::to_vec_pretty(&serde_json::json!({"outcome":"clean","summary":"ok"}))
            .expect("result json"),
    )
    .await
    .expect("write result");

    let app = ingot_http_api::build_router_with_project_locks_and_state_root(
        db.clone(),
        ingot_usecases::ProjectLocks::default(),
        PathBuf::from(&state_root),
        ingot_usecases::DispatchNotify::default(),
    );

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/jobs/{job_id}/logs"))
                .body(Body::empty())
                .expect("build logs request"),
        )
        .await
        .expect("logs route response");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read logs body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("logs json");
    assert_eq!(json["prompt"].as_str(), Some("review the candidate"));
    assert_eq!(
        json["output"]["schema_version"].as_str(),
        Some("agent_output:v1")
    );
    assert_eq!(
        json["output"]["segments"][0]["text"].as_str(),
        Some("hello")
    );
    assert_eq!(json["result"]["payload"]["summary"].as_str(), Some("ok"));

    let raw_response = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/jobs/{job_id}/logs/raw"))
                .body(Body::empty())
                .expect("build raw logs request"),
        )
        .await
        .expect("raw logs route response");

    assert_eq!(raw_response.status(), StatusCode::OK);
    let raw_body = to_bytes(raw_response.into_body(), usize::MAX)
        .await
        .expect("read raw body");
    let raw_json: serde_json::Value = serde_json::from_slice(&raw_body).expect("raw logs json");
    assert_eq!(
        raw_json["stdout"].as_str(),
        Some("{\"type\":\"item.completed\"}\n")
    );
    assert_eq!(raw_json["stderr"].as_str(), Some("warn\n"));
    assert_eq!(raw_json["result"]["summary"].as_str(), Some("ok"));
}

#[tokio::test]
async fn logs_route_falls_back_to_raw_artifacts_when_normalized_output_is_missing() {
    let (_repo, db, _project_id, _item_id, job_id) = seeded_route_test_app().await;
    let state_root =
        ingot_test_support::env::temp_state_root("ingot-http-api-job-logs-raw-fallback-state");
    let logs_dir = PathBuf::from(&state_root).join("logs").join(&job_id);
    tokio::fs::create_dir_all(&logs_dir)
        .await
        .expect("create logs dir");
    tokio::fs::write(logs_dir.join("stdout.log"), "legacy stdout\n")
        .await
        .expect("write stdout");
    tokio::fs::write(logs_dir.join("stderr.log"), "legacy stderr\n")
        .await
        .expect("write stderr");

    let app = ingot_http_api::build_router_with_project_locks_and_state_root(
        db.clone(),
        ingot_usecases::ProjectLocks::default(),
        PathBuf::from(&state_root),
        ingot_usecases::DispatchNotify::default(),
    );

    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/jobs/{job_id}/logs"))
                .body(Body::empty())
                .expect("build logs request"),
        )
        .await
        .expect("logs route response");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read logs body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("logs json");

    assert_eq!(json["output"]["segments"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        json["output"]["segments"][0],
        serde_json::json!({
            "sequence": 1,
            "channel": "primary",
            "kind": "raw_fallback",
            "status": null,
            "title": "stdout",
            "text": "legacy stdout\n",
            "data": {
                "source_artifact": "stdout.log"
            }
        })
    );
    assert_eq!(
        json["output"]["segments"][1],
        serde_json::json!({
            "sequence": 2,
            "channel": "diagnostic",
            "kind": "raw_fallback",
            "status": null,
            "title": "stderr",
            "text": "legacy stderr\n",
            "data": {
                "source_artifact": "stderr.log"
            }
        })
    );
}

#[tokio::test]
async fn logs_route_does_not_synthesize_raw_fallback_when_output_artifact_exists_but_is_empty() {
    let (_repo, db, _project_id, _item_id, job_id) = seeded_route_test_app().await;
    let state_root =
        ingot_test_support::env::temp_state_root("ingot-http-api-job-logs-empty-output-state");
    let logs_dir = PathBuf::from(&state_root).join("logs").join(&job_id);
    tokio::fs::create_dir_all(&logs_dir)
        .await
        .expect("create logs dir");
    tokio::fs::write(logs_dir.join("stdout.log"), "legacy stdout\n")
        .await
        .expect("write stdout");
    tokio::fs::write(logs_dir.join("stderr.log"), "legacy stderr\n")
        .await
        .expect("write stderr");
    tokio::fs::write(logs_dir.join("output.jsonl"), "")
        .await
        .expect("write empty output jsonl");

    let app = ingot_http_api::build_router_with_project_locks_and_state_root(
        db.clone(),
        ingot_usecases::ProjectLocks::default(),
        PathBuf::from(&state_root),
        ingot_usecases::DispatchNotify::default(),
    );

    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/jobs/{job_id}/logs"))
                .body(Body::empty())
                .expect("build logs request"),
        )
        .await
        .expect("logs route response");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read logs body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("logs json");

    assert_eq!(json["output"]["schema_version"], "agent_output:v1");
    assert_eq!(json["output"]["segments"], serde_json::json!([]));
}

#[tokio::test]
async fn fail_route_persists_escalation_and_item_detail_projection() {
    let (_repo, db, project_id, item_id, job_id) = seeded_route_test_app().await;
    let app = test_router(db.clone());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/jobs/{job_id}/fail"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "outcome_class": "terminal_failure",
                        "error_code": "worker_failed",
                        "error_message": "boom"
                    })
                    .to_string(),
                ))
                .expect("build request"),
        )
        .await
        .expect("fail route response");

    assert_eq!(response.status(), StatusCode::OK);

    let detail_response = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/projects/{project_id}/items/{item_id}"))
                .body(Body::empty())
                .expect("build detail request"),
        )
        .await
        .expect("detail route response");

    assert_eq!(detail_response.status(), StatusCode::OK);
    let body = to_bytes(detail_response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("detail json");

    assert_eq!(
        json["item"]["escalation_state"].as_str(),
        Some("operator_required")
    );
    assert_eq!(
        json["item"]["escalation_reason"].as_str(),
        Some("step_failed")
    );
    assert_eq!(
        json["evaluation"]["phase_status"].as_str(),
        Some("escalated")
    );
    assert_eq!(json["jobs"][0]["status"].as_str(), Some("failed"));
    assert_eq!(
        json["jobs"][0]["outcome_class"].as_str(),
        Some("terminal_failure")
    );
}

#[tokio::test]
async fn expire_route_persists_terminal_job_without_auto_redispatch() {
    let (_repo, db, project_id, item_id, job_id) = seeded_route_test_app().await;
    let app = test_router(db.clone());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/jobs/{job_id}/expire"))
                .body(Body::empty())
                .expect("build expire request"),
        )
        .await
        .expect("expire route response");

    assert_eq!(response.status(), StatusCode::OK);

    let detail_response = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/projects/{project_id}/items/{item_id}"))
                .body(Body::empty())
                .expect("build detail request"),
        )
        .await
        .expect("detail route response");

    assert_eq!(detail_response.status(), StatusCode::OK);
    let body = to_bytes(detail_response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("detail json");

    assert_eq!(json["item"]["escalation_state"].as_str(), Some("none"));
    assert!(json["evaluation"]["dispatchable_step_id"].is_null());
    assert_eq!(
        json["evaluation"]["next_recommended_action"].as_str(),
        Some("none")
    );
    assert_eq!(
        json["evaluation"]["current_step_id"].as_str(),
        Some("validate_candidate_initial")
    );
    assert_eq!(json["jobs"][0]["status"].as_str(), Some("expired"));
    assert_eq!(
        json["jobs"][0]["outcome_class"].as_str(),
        Some("transient_failure")
    );
}

#[tokio::test]
async fn retry_route_requeues_terminal_non_success_job_on_current_revision() {
    let repo = temp_git_repo("ingot-http-api");
    let base_commit_oid = git_output(&repo, &["rev-parse", "HEAD"]);

    let db = migrated_test_db("ingot-http-api-db").await;
    let project_id = "prj_00000000000000000000000000000066".to_string();
    let item_id = "itm_00000000000000000000000000000066".to_string();
    let revision_id = "rev_00000000000000000000000000000066".to_string();
    let job_id = "job_00000000000000000000000000000066".to_string();

    persist_test_change(
        &db,
        &repo,
        &project_id,
        &item_id,
        &revision_id,
        |item| item.escalated(EscalationReason::StepFailed),
        |revision| revision.explicit_seed(&base_commit_oid),
    )
    .await;

    insert_test_job_row(
        &db,
        TestJobInsert {
            id: &job_id,
            project_id: &project_id,
            item_id: &item_id,
            item_revision_id: &revision_id,
            step_id: "review_candidate_initial",
            status: JobStatus::Failed,
            outcome_class: Some(OutcomeClass::TerminalFailure),
            phase_kind: PhaseKind::Review,
            workspace_kind: WorkspaceKind::Review,
            execution_permission: ExecutionPermission::MustNotMutate,
            context_policy: ContextPolicy::Fresh,
            phase_template_slug: "review-candidate",
            output_artifact_kind: OutputArtifactKind::ReviewReport,
            job_input: TestJobInput::CandidateSubject(&base_commit_oid, &base_commit_oid),
            error_code: Some("step_failed"),
            created_at: "2026-03-12T00:00:00Z",
            ended_at: Some("2026-03-12T00:05:00Z"),
            ..TestJobInsert::new(
                &job_id,
                &project_id,
                &item_id,
                &revision_id,
                "review_candidate_initial",
            )
        },
    )
    .await;

    let app = test_router(db.clone());
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/projects/{project_id}/items/{item_id}/jobs/{job_id}/retry"
                ))
                .method("POST")
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("route response");

    assert_eq!(response.status(), StatusCode::CREATED);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(json["step_id"].as_str(), Some("review_candidate_initial"));
    assert_eq!(json["semantic_attempt_no"].as_u64(), Some(1));
    assert_eq!(json["retry_no"].as_u64(), Some(1));
    assert_eq!(json["supersedes_job_id"].as_str(), Some(job_id.as_str()));
    assert_eq!(json["status"].as_str(), Some("queued"));
    assert!(json["workspace_id"].is_null());
}

#[tokio::test]
async fn retry_route_requeues_authoring_job_without_persisting_assigned_state() {
    let repo = temp_git_repo("ingot-http-api");
    let seed_commit_oid = git_output(&repo, &["rev-parse", "HEAD"]);

    let db = migrated_test_db("ingot-http-api-db").await;
    let project_id = "prj_00000000000000000000000000000068".to_string();
    let item_id = "itm_00000000000000000000000000000068".to_string();
    let revision_id = "rev_00000000000000000000000000000068".to_string();
    let failed_job_id = "job_00000000000000000000000000000068".to_string();

    persist_test_change(
        &db,
        &repo,
        &project_id,
        &item_id,
        &revision_id,
        |item| item.escalated(EscalationReason::StepFailed),
        |revision| revision.explicit_seed(&seed_commit_oid),
    )
    .await;

    insert_test_job_row(
        &db,
        TestJobInsert {
            id: &failed_job_id,
            project_id: &project_id,
            item_id: &item_id,
            item_revision_id: &revision_id,
            step_id: "author_initial",
            status: JobStatus::Failed,
            outcome_class: Some(OutcomeClass::TerminalFailure),
            phase_kind: PhaseKind::Author,
            workspace_kind: WorkspaceKind::Authoring,
            execution_permission: ExecutionPermission::MayMutate,
            context_policy: ContextPolicy::Fresh,
            phase_template_slug: "author-initial",
            output_artifact_kind: OutputArtifactKind::Commit,
            job_input: TestJobInput::AuthoringHead(&seed_commit_oid),
            error_code: Some("step_failed"),
            created_at: "2026-03-12T00:00:00Z",
            ended_at: Some("2026-03-12T00:05:00Z"),
            ..TestJobInsert::new(
                &failed_job_id,
                &project_id,
                &item_id,
                &revision_id,
                "author_initial",
            )
        },
    )
    .await;

    let app = test_router(db.clone());
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/projects/{project_id}/items/{item_id}/jobs/{failed_job_id}/retry"
                ))
                .method("POST")
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("route response");

    assert_eq!(response.status(), StatusCode::CREATED);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    let retried_job_id = json["id"].as_str().expect("retried job id");

    assert_eq!(json["step_id"].as_str(), Some("author_initial"));
    assert_eq!(json["status"].as_str(), Some("queued"));
    assert!(json["workspace_id"].is_null());
    assert_eq!(
        json["supersedes_job_id"].as_str(),
        Some(failed_job_id.as_str())
    );

    let retried_job: (String, Option<String>) =
        sqlx::query_as("SELECT status, workspace_id FROM jobs WHERE id = ?")
            .bind(retried_job_id)
            .fetch_one(db.raw_pool())
            .await
            .expect("retried job row");
    assert_eq!(retried_job.0, "queued");
    assert_eq!(retried_job.1, None);

    let detail_response = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/projects/{project_id}/items/{item_id}"))
                .body(Body::empty())
                .expect("build detail request"),
        )
        .await
        .expect("detail route response");
    assert_eq!(detail_response.status(), StatusCode::OK);
    let detail_body = to_bytes(detail_response.into_body(), usize::MAX)
        .await
        .expect("read detail body");
    let detail_json: serde_json::Value = serde_json::from_slice(&detail_body).expect("detail json");
    assert_eq!(detail_json["workspaces"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        detail_json["workspaces"][0]["kind"].as_str(),
        Some("authoring")
    );
    assert_eq!(
        detail_json["workspaces"][0]["status"].as_str(),
        Some("ready")
    );
}

#[tokio::test]
async fn retry_route_rejects_daemon_only_validation_job() {
    let repo = temp_git_repo("ingot-http-api");
    let bound_base = git_output(&repo, &["rev-parse", "HEAD"]);

    let db = migrated_test_db("ingot-http-api-db").await;
    let project_id = "prj_00000000000000000000000000000067".to_string();
    let item_id = "itm_00000000000000000000000000000067".to_string();
    let revision_id = "rev_00000000000000000000000000000067".to_string();
    let failed_job_id = "job_00000000000000000000000000000068".to_string();

    persist_test_change(
        &db,
        &repo,
        &project_id,
        &item_id,
        &revision_id,
        |item| item.escalated(EscalationReason::StepFailed),
        |revision| {
            revision
                .seed_commit_oid(None::<String>)
                .seed_target_commit_oid(Some(bound_base.clone()))
        },
    )
    .await;

    insert_test_job_row(
        &db,
        TestJobInsert {
            id: &failed_job_id,
            project_id: &project_id,
            item_id: &item_id,
            item_revision_id: &revision_id,
            step_id: "validate_candidate_initial",
            status: JobStatus::Failed,
            outcome_class: Some(OutcomeClass::TerminalFailure),
            phase_kind: PhaseKind::Validate,
            workspace_kind: WorkspaceKind::Authoring,
            execution_permission: ExecutionPermission::DaemonOnly,
            context_policy: ContextPolicy::None,
            phase_template_slug: "",
            output_artifact_kind: OutputArtifactKind::ValidationReport,
            job_input: TestJobInput::CandidateSubject(&bound_base, &bound_base),
            error_code: Some("step_failed"),
            created_at: "2026-03-12T00:00:00Z",
            ended_at: Some("2026-03-12T00:05:00Z"),
            ..TestJobInsert::new(
                &failed_job_id,
                &project_id,
                &item_id,
                &revision_id,
                "validate_candidate_initial",
            )
        },
    )
    .await;

    let app = test_router(db.clone());
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/projects/{project_id}/items/{item_id}/jobs/{failed_job_id}/retry"
                ))
                .method("POST")
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("route response");

    // Daemon-only validation jobs cannot be retried manually
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn cancel_route_marks_active_job_cancelled_and_clears_workspace_attachment() {
    let repo = temp_git_repo("ingot-http-api");
    let base_commit_oid = git_output(&repo, &["rev-parse", "HEAD"]);

    let db = migrated_test_db("ingot-http-api-db").await;
    let project_id = "prj_00000000000000000000000000000065".to_string();
    let item_id = "itm_00000000000000000000000000000065".to_string();
    let revision_id = "rev_00000000000000000000000000000065".to_string();
    let job_id = "job_00000000000000000000000000000065".to_string();
    let workspace_id = "wrk_00000000000000000000000000000065".to_string();

    persist_test_change(
        &db,
        &repo,
        &project_id,
        &item_id,
        &revision_id,
        |item| item,
        |revision| revision.explicit_seed(&base_commit_oid),
    )
    .await;

    persist_test_workspace(
        &db,
        &project_id,
        WorkspaceKind::Authoring,
        &workspace_id,
        |workspace| {
            workspace
                .created_for_revision_id(parse_id(&revision_id))
                .path(repo.join("cancel-workspace").display().to_string())
                .workspace_ref("refs/ingot/workspaces/wrk_cancel")
                .base_commit_oid(&base_commit_oid)
                .head_commit_oid(&base_commit_oid)
                .retention_policy(RetentionPolicy::Persistent)
                .status(WorkspaceStatus::Busy)
                .current_job_id(parse_id(&job_id))
        },
    )
    .await;

    insert_test_job_row(
        &db,
        TestJobInsert {
            id: &job_id,
            project_id: &project_id,
            item_id: &item_id,
            item_revision_id: &revision_id,
            step_id: "author_initial",
            status: JobStatus::Running,
            phase_kind: PhaseKind::Author,
            workspace_id: Some(&workspace_id),
            workspace_kind: WorkspaceKind::Authoring,
            execution_permission: ExecutionPermission::MayMutate,
            context_policy: ContextPolicy::Fresh,
            phase_template_slug: "author-initial",
            output_artifact_kind: OutputArtifactKind::Commit,
            job_input: TestJobInput::AuthoringHead(&base_commit_oid),
            created_at: "2026-03-12T00:00:00Z",
            ..TestJobInsert::new(
                &job_id,
                &project_id,
                &item_id,
                &revision_id,
                "author_initial",
            )
        },
    )
    .await;

    let app = test_router(db.clone());
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/projects/{project_id}/items/{item_id}/jobs/{job_id}/cancel"
                ))
                .method("POST")
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("route response");

    assert_eq!(response.status(), StatusCode::OK);
    let job_state: (String, String) =
        sqlx::query_as("SELECT status, outcome_class FROM jobs WHERE id = ?")
            .bind(&job_id)
            .fetch_one(db.raw_pool())
            .await
            .expect("job state");
    assert_eq!(job_state.0, "cancelled");
    assert_eq!(job_state.1, "cancelled");
    let workspace_state: (String, Option<String>) =
        sqlx::query_as("SELECT status, current_job_id FROM workspaces WHERE id = ?")
            .bind(&workspace_id)
            .fetch_one(db.raw_pool())
            .await
            .expect("workspace state");
    assert_eq!(workspace_state.0, "ready");
    assert_eq!(workspace_state.1, None);
}

#[tokio::test]
async fn complete_route_rejects_stale_prepared_convergence_after_target_moves() {
    let repo = temp_git_repo("ingot-http-api");
    let initial_target = git_output(&repo, &["rev-parse", "HEAD"]);
    let db = migrated_test_db("ingot-http-api-db").await;

    let project_id = "prj_00000000000000000000000000000001".to_string();
    let item_id = "itm_00000000000000000000000000000001".to_string();
    let revision_id = "rev_00000000000000000000000000000001".to_string();
    let job_id = "job_00000000000000000000000000000001".to_string();
    let workspace_id = "wrk_00000000000000000000000000000001".to_string();
    let convergence_id = "conv_00000000000000000000000000000001".to_string();

    persist_test_change(
        &db,
        &repo,
        &project_id,
        &item_id,
        &revision_id,
        |item| item,
        |revision| revision.explicit_seed(&initial_target),
    )
    .await;

    persist_test_workspace(
        &db,
        &project_id,
        WorkspaceKind::Integration,
        &workspace_id,
        |workspace| {
            workspace
                .created_for_revision_id(parse_id(&revision_id))
                .path(repo.display().to_string())
                .retention_policy(RetentionPolicy::Ephemeral)
                .status(WorkspaceStatus::Ready)
                .no_workspace_ref()
        },
    )
    .await;

    insert_test_job_row(
        &db,
        TestJobInsert {
            id: &job_id,
            project_id: &project_id,
            item_id: &item_id,
            item_revision_id: &revision_id,
            step_id: "validate_integrated",
            status: JobStatus::Running,
            phase_kind: PhaseKind::Validate,
            workspace_kind: WorkspaceKind::Integration,
            execution_permission: ExecutionPermission::MustNotMutate,
            context_policy: ContextPolicy::ResumeContext,
            phase_template_slug: "validate-integrated",
            output_artifact_kind: OutputArtifactKind::ValidationReport,
            job_input: TestJobInput::IntegratedSubject(&initial_target, &initial_target),
            created_at: "2026-03-12T00:00:00Z",
            ..TestJobInsert::new(
                &job_id,
                &project_id,
                &item_id,
                &revision_id,
                "validate_integrated",
            )
        },
    )
    .await;

    persist_test_convergence(
        &db,
        &project_id,
        &item_id,
        &revision_id,
        &convergence_id,
        |convergence| {
            convergence
                .source_workspace_id(parse_id(&workspace_id))
                .integration_workspace_id(parse_id(&workspace_id))
                .source_head_commit_oid(&initial_target)
                .input_target_commit_oid(&initial_target)
                .prepared_commit_oid(&initial_target)
        },
    )
    .await;

    write_file(&repo.join("tracked.txt"), "next");
    git(&repo, &["add", "tracked.txt"]);
    git(&repo, &["commit", "-m", "next"]);

    let app = test_router(db.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/jobs/{job_id}/complete"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "outcome_class": "clean",
                        "result_schema_version": "validation_report:v1",
                        "result_payload": clean_validation_report("ok")
                    })
                    .to_string(),
                ))
                .expect("build request"),
        )
        .await
        .expect("complete route response");

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("error json");

    assert_eq!(
        json["error"]["code"].as_str(),
        Some("prepared_convergence_stale")
    );

    let item_approval_state: String =
        sqlx::query_scalar("SELECT approval_state FROM items WHERE id = ?")
            .bind(&item_id)
            .fetch_one(db.raw_pool())
            .await
            .expect("item approval state");
    let job_status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id = ?")
        .bind(&job_id)
        .fetch_one(db.raw_pool())
        .await
        .expect("job status");

    assert_eq!(item_approval_state, "not_requested");
    assert_eq!(job_status, "running");
}

#[tokio::test]
async fn complete_route_clears_item_escalation_after_successful_retry() {
    let repo = temp_git_repo("ingot-http-api");
    let head_commit = git_output(&repo, &["rev-parse", "HEAD"]);
    let db = migrated_test_db("ingot-http-api-db").await;

    let project_id = "prj_00000000000000000000000000000071".to_string();
    let item_id = "itm_00000000000000000000000000000071".to_string();
    let revision_id = "rev_00000000000000000000000000000071".to_string();
    let failed_job_id = "job_00000000000000000000000000000071".to_string();
    let retry_job_id = "job_00000000000000000000000000000072".to_string();

    persist_test_change(
        &db,
        &repo,
        &project_id,
        &item_id,
        &revision_id,
        |item| item.escalated(EscalationReason::StepFailed),
        |revision| revision.explicit_seed(&head_commit),
    )
    .await;

    insert_test_job_row(
        &db,
        TestJobInsert {
            id: &failed_job_id,
            project_id: &project_id,
            item_id: &item_id,
            item_revision_id: &revision_id,
            step_id: "validate_candidate_initial",
            status: JobStatus::Failed,
            outcome_class: Some(OutcomeClass::TerminalFailure),
            phase_kind: PhaseKind::Validate,
            workspace_kind: WorkspaceKind::Authoring,
            execution_permission: ExecutionPermission::MustNotMutate,
            context_policy: ContextPolicy::ResumeContext,
            phase_template_slug: "validate-candidate",
            output_artifact_kind: OutputArtifactKind::ValidationReport,
            job_input: TestJobInput::CandidateSubject(&head_commit, &head_commit),
            error_code: Some("step_failed"),
            created_at: "2026-03-12T00:00:00Z",
            started_at: Some("2026-03-12T00:00:00Z"),
            ended_at: Some("2026-03-12T00:01:00Z"),
            ..TestJobInsert::new(
                &failed_job_id,
                &project_id,
                &item_id,
                &revision_id,
                "validate_candidate_initial",
            )
        },
    )
    .await;

    insert_test_job_row(
        &db,
        TestJobInsert {
            id: &retry_job_id,
            project_id: &project_id,
            item_id: &item_id,
            item_revision_id: &revision_id,
            step_id: "validate_candidate_initial",
            retry_no: 1,
            supersedes_job_id: Some(&failed_job_id),
            status: JobStatus::Running,
            phase_kind: PhaseKind::Validate,
            workspace_kind: WorkspaceKind::Authoring,
            execution_permission: ExecutionPermission::MustNotMutate,
            context_policy: ContextPolicy::ResumeContext,
            phase_template_slug: "validate-candidate",
            output_artifact_kind: OutputArtifactKind::ValidationReport,
            job_input: TestJobInput::CandidateSubject(&head_commit, &head_commit),
            created_at: "2026-03-12T00:02:00Z",
            started_at: Some("2026-03-12T00:02:00Z"),
            ..TestJobInsert::new(
                &retry_job_id,
                &project_id,
                &item_id,
                &revision_id,
                "validate_candidate_initial",
            )
        },
    )
    .await;

    let app = test_router(db.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/jobs/{retry_job_id}/complete"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "outcome_class": "clean",
                        "result_schema_version": "validation_report:v1",
                        "result_payload": {
                            "outcome": "clean",
                            "summary": "ok",
                            "checks": [{
                                "name": "lint",
                                "status": "pass",
                                "summary": "ok"
                            }],
                            "findings": [],
                            "extensions": null
                        }
                    })
                    .to_string(),
                ))
                .expect("build request"),
        )
        .await
        .expect("complete route response");

    assert_eq!(response.status(), StatusCode::OK);

    let item_row: (String, Option<String>) =
        sqlx::query_as("SELECT escalation_state, escalation_reason FROM items WHERE id = ?")
            .bind(&item_id)
            .fetch_one(db.raw_pool())
            .await
            .expect("load item escalation");
    assert_eq!(item_row.0, "none");
    assert_eq!(item_row.1, None);

    let activity = db
        .list_activity_by_project(ProjectId::from_str(&project_id).expect("project id"), 20, 0)
        .await
        .expect("list activity");
    assert!(activity.iter().any(|entry| {
        entry.event_type == ActivityEventType::ItemEscalationCleared
            && entry.subject
                == ingot_domain::activity::ActivitySubject::Item(
                    item_id.parse::<ingot_domain::ids::ItemId>().unwrap(),
                )
    }));
}

#[tokio::test]
async fn complete_route_auto_dispatches_candidate_review_after_clean_incremental_review() {
    let repo = temp_git_repo("ingot-http-api");
    let seed_head = git_output(&repo, &["rev-parse", "HEAD"]);
    write_file(&repo.join("tracked.txt"), "candidate change");
    git(&repo, &["add", "tracked.txt"]);
    git(&repo, &["commit", "-m", "candidate change"]);
    let candidate_head = git_output(&repo, &["rev-parse", "HEAD"]);

    let db = migrated_test_db("ingot-http-api-db").await;

    let project_id = "prj_00000000000000000000000000000073".to_string();
    let item_id = "itm_00000000000000000000000000000073".to_string();
    let revision_id = "rev_00000000000000000000000000000073".to_string();
    let author_job_id = "job_00000000000000000000000000000073".to_string();
    let review_job_id = "job_00000000000000000000000000000074".to_string();

    persist_test_change(
        &db,
        &repo,
        &project_id,
        &item_id,
        &revision_id,
        |item| item,
        |revision| revision.explicit_seed(&seed_head),
    )
    .await;
    insert_test_job_row(
        &db,
        TestJobInsert {
            id: &author_job_id,
            project_id: &project_id,
            item_id: &item_id,
            item_revision_id: &revision_id,
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
            output_commit_oid: Some(&candidate_head),
            created_at: "2026-03-12T00:00:00Z",
            started_at: Some("2026-03-12T00:00:00Z"),
            ended_at: Some("2026-03-12T00:01:00Z"),
            ..TestJobInsert::new(
                &author_job_id,
                &project_id,
                &item_id,
                &revision_id,
                "author_initial",
            )
        },
    )
    .await;
    insert_test_job_row(
        &db,
        TestJobInsert {
            id: &review_job_id,
            project_id: &project_id,
            item_id: &item_id,
            item_revision_id: &revision_id,
            step_id: "review_incremental_initial",
            status: JobStatus::Running,
            phase_kind: PhaseKind::Review,
            workspace_kind: WorkspaceKind::Review,
            execution_permission: ExecutionPermission::MustNotMutate,
            context_policy: ContextPolicy::Fresh,
            phase_template_slug: "review-incremental",
            output_artifact_kind: OutputArtifactKind::ReviewReport,
            job_input: TestJobInput::CandidateSubject(&seed_head, &candidate_head),
            created_at: "2026-03-12T00:02:00Z",
            started_at: Some("2026-03-12T00:02:00Z"),
            ..TestJobInsert::new(
                &review_job_id,
                &project_id,
                &item_id,
                &revision_id,
                "review_incremental_initial",
            )
        },
    )
    .await;

    let response = test_router(db.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/jobs/{review_job_id}/complete"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "outcome_class": "clean",
                        "result_schema_version": "review_report:v1",
                        "result_payload": clean_review_report(&seed_head, &candidate_head)
                    })
                    .to_string(),
                ))
                .expect("build request"),
        )
        .await
        .expect("complete route response");

    assert_eq!(response.status(), StatusCode::OK);

    let review_job_status: (String,) = sqlx::query_as("SELECT status FROM jobs WHERE id = ?")
        .bind(&review_job_id)
        .fetch_one(db.raw_pool())
        .await
        .expect("review job status");
    assert_eq!(review_job_status.0, "completed");

    let queued_candidate_review: (String, String, String) = sqlx::query_as(
        "SELECT step_id, input_base_commit_oid, input_head_commit_oid
         FROM jobs
         WHERE item_id = ? AND step_id = 'review_candidate_initial' AND status = 'queued'",
    )
    .bind(&item_id)
    .fetch_one(db.raw_pool())
    .await
    .expect("queued candidate review job");
    assert_eq!(queued_candidate_review.0, "review_candidate_initial");
    assert_eq!(queued_candidate_review.1, seed_head);
    assert_eq!(queued_candidate_review.2, candidate_head);
}

#[tokio::test]
async fn complete_route_recovers_projected_review_after_warning_only_dispatch_failure_on_system_action_tick()
 {
    let repo = temp_git_repo("ingot-http-api");
    let seed_head = git_output(&repo, &["rev-parse", "HEAD"]);
    write_file(&repo.join("tracked.txt"), "candidate change");
    git(&repo, &["add", "tracked.txt"]);
    git(&repo, &["commit", "-m", "candidate change"]);
    let candidate_head = git_output(&repo, &["rev-parse", "HEAD"]);

    let db = migrated_test_db("ingot-http-api-db").await;
    let project_id = "prj_00000000000000000000000000000074".to_string();
    let item_id = "itm_00000000000000000000000000000074".to_string();
    let revision_id = "rev_00000000000000000000000000000074".to_string();
    let author_job_id = "job_00000000000000000000000000000075".to_string();
    let review_job_id = "job_00000000000000000000000000000076".to_string();

    persist_test_change(
        &db,
        &repo,
        &project_id,
        &item_id,
        &revision_id,
        |item| item,
        |revision| {
            revision
                .seed_commit_oid(None::<String>)
                .seed_target_commit_oid(Some("target-head"))
        },
    )
    .await;
    insert_test_job_row(
        &db,
        TestJobInsert {
            id: &author_job_id,
            project_id: &project_id,
            item_id: &item_id,
            item_revision_id: &revision_id,
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
            created_at: "2026-03-12T00:00:00Z",
            started_at: Some("2026-03-12T00:00:00Z"),
            ended_at: Some("2026-03-12T00:01:00Z"),
            ..TestJobInsert::new(
                &author_job_id,
                &project_id,
                &item_id,
                &revision_id,
                "author_initial",
            )
        },
    )
    .await;
    insert_test_job_row(
        &db,
        TestJobInsert {
            id: &review_job_id,
            project_id: &project_id,
            item_id: &item_id,
            item_revision_id: &revision_id,
            step_id: "review_incremental_initial",
            status: JobStatus::Running,
            phase_kind: PhaseKind::Review,
            workspace_kind: WorkspaceKind::Review,
            execution_permission: ExecutionPermission::MustNotMutate,
            context_policy: ContextPolicy::Fresh,
            phase_template_slug: "review-incremental",
            output_artifact_kind: OutputArtifactKind::ReviewReport,
            job_input: TestJobInput::CandidateSubject(&seed_head, &candidate_head),
            created_at: "2026-03-12T00:02:00Z",
            started_at: Some("2026-03-12T00:02:00Z"),
            ..TestJobInsert::new(
                &review_job_id,
                &project_id,
                &item_id,
                &revision_id,
                "review_incremental_initial",
            )
        },
    )
    .await;

    let response = test_router(db.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/jobs/{review_job_id}/complete"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "outcome_class": "clean",
                        "result_schema_version": "review_report:v1",
                        "result_payload": clean_review_report(&seed_head, &candidate_head)
                    })
                    .to_string(),
                ))
                .expect("build request"),
        )
        .await
        .expect("complete route response");
    assert_eq!(response.status(), StatusCode::OK);

    let review_job_status: (String,) = sqlx::query_as("SELECT status FROM jobs WHERE id = ?")
        .bind(&review_job_id)
        .fetch_one(db.raw_pool())
        .await
        .expect("review job status");
    assert_eq!(review_job_status.0, "completed");

    let queued_candidate_reviews: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM jobs
         WHERE item_id = ? AND step_id = 'review_candidate_initial' AND status = 'queued'",
    )
    .bind(&item_id)
    .fetch_one(db.raw_pool())
    .await
    .expect("count queued candidate reviews");
    assert_eq!(queued_candidate_reviews, 0);

    sqlx::query(
        "UPDATE item_revisions
         SET seed_commit_oid = ?, seed_target_commit_oid = ?
         WHERE id = ?",
    )
    .bind(&seed_head)
    .bind(&seed_head)
    .bind(&revision_id)
    .execute(db.raw_pool())
    .await
    .expect("repair revision seed commits");
    sqlx::query("UPDATE jobs SET output_commit_oid = ? WHERE id = ?")
        .bind(&candidate_head)
        .bind(&author_job_id)
        .execute(db.raw_pool())
        .await
        .expect("repair author output commit");

    let convergence_repo = temp_git_repo("ingot-http-api-convergence");
    let base_commit = git_output(&convergence_repo, &["rev-parse", "HEAD"]);
    write_file(&convergence_repo.join("tracked.txt"), "prepared");
    git(&convergence_repo, &["add", "tracked.txt"]);
    git(&convergence_repo, &["commit", "-m", "prepared"]);
    let prepared_commit = git_output(&convergence_repo, &["rev-parse", "HEAD"]);
    git(&convergence_repo, &["reset", "--hard", &base_commit]);
    write_file(&convergence_repo.join("tracked.txt"), "moved target");
    git(&convergence_repo, &["add", "tracked.txt"]);
    git(&convergence_repo, &["commit", "-m", "moved target"]);

    let created_at = parse_timestamp(TS);
    let convergence_project = ProjectBuilder::new(&convergence_repo)
        .created_at(created_at)
        .build();
    db.create_project(&convergence_project)
        .await
        .expect("create convergence project");

    let convergence_item_id = ingot_domain::ids::ItemId::new();
    let convergence_revision_id = ingot_domain::ids::ItemRevisionId::new();
    let convergence_item = ItemBuilder::new(convergence_project.id, convergence_revision_id)
        .id(convergence_item_id)
        .approval_state(ApprovalState::Pending)
        .created_at(created_at)
        .build();
    let convergence_revision = RevisionBuilder::new(convergence_item_id)
        .id(convergence_revision_id)
        .explicit_seed(&base_commit)
        .created_at(created_at)
        .build();
    db.create_item_with_revision(&convergence_item, &convergence_revision)
        .await
        .expect("create convergence item");

    let integration_workspace =
        WorkspaceBuilder::new(convergence_project.id, WorkspaceKind::Integration)
            .created_for_revision_id(convergence_revision.id)
            .base_commit_oid(base_commit.clone())
            .head_commit_oid(prepared_commit.clone())
            .created_at(created_at)
            .build();
    db.create_workspace(&integration_workspace)
        .await
        .expect("create integration workspace");
    let source_workspace = WorkspaceBuilder::new(convergence_project.id, WorkspaceKind::Authoring)
        .created_for_revision_id(convergence_revision.id)
        .base_commit_oid(base_commit.clone())
        .head_commit_oid(prepared_commit.clone())
        .created_at(created_at)
        .build();
    db.create_workspace(&source_workspace)
        .await
        .expect("create source workspace");

    let validate_job = JobBuilder::new(
        convergence_project.id,
        convergence_item.id,
        convergence_revision.id,
        "validate_integrated",
    )
    .status(JobStatus::Completed)
    .outcome_class(OutcomeClass::Clean)
    .phase_kind(PhaseKind::Validate)
    .workspace_id(integration_workspace.id)
    .workspace_kind(WorkspaceKind::Integration)
    .execution_permission(ExecutionPermission::MustNotMutate)
    .context_policy(ContextPolicy::ResumeContext)
    .phase_template_slug("validate-integrated")
    .job_input(JobInput::integrated_subject(
        base_commit.clone().into(),
        prepared_commit.clone().into(),
    ))
    .output_artifact_kind(OutputArtifactKind::ValidationReport)
    .result_schema_version("validation_report:v1")
    .result_payload(clean_validation_report("integrated clean"))
    .created_at(created_at)
    .started_at(created_at)
    .ended_at(created_at)
    .build();
    db.create_job(&validate_job)
        .await
        .expect("create validate job");

    let convergence = ConvergenceBuilder::new(
        convergence_project.id,
        convergence_item.id,
        convergence_revision.id,
    )
    .source_workspace_id(source_workspace.id)
    .integration_workspace_id(integration_workspace.id)
    .source_head_commit_oid(prepared_commit.clone())
    .input_target_commit_oid(base_commit.clone())
    .prepared_commit_oid(prepared_commit.clone())
    .target_head_valid(false)
    .created_at(created_at)
    .build();
    db.create_convergence(&convergence)
        .await
        .expect("create convergence");

    let state_root = ingot_test_support::env::temp_state_root("ingot-http-api-recovery-state");
    let dispatcher = JobDispatcher::new(
        db.clone(),
        ingot_usecases::ProjectLocks::default(),
        DispatcherConfig::new(state_root),
        ingot_usecases::DispatchNotify::default(),
    );

    assert!(dispatcher.tick().await.expect("tick should recover review"));

    let queued_candidate_reviews: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM jobs
         WHERE item_id = ? AND step_id = 'review_candidate_initial' AND status = 'queued'",
    )
    .bind(&item_id)
    .fetch_one(db.raw_pool())
    .await
    .expect("count recovered candidate reviews");
    assert_eq!(queued_candidate_reviews, 1);

    let updated_item = db
        .get_item(convergence_item.id)
        .await
        .expect("updated convergence item");
    assert_eq!(updated_item.approval_state, ApprovalState::NotRequested);
    let updated_convergence = db
        .list_convergences_by_item(convergence_item.id)
        .await
        .expect("list convergences")
        .into_iter()
        .next()
        .expect("updated convergence");
    assert_eq!(
        updated_convergence.state.status(),
        ConvergenceStatus::Failed
    );
}
