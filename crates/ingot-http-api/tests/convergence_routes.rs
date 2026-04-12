use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use ingot_domain::item::{ApprovalState, EscalationReason};
use ingot_domain::job::{
    ContextPolicy, ExecutionPermission, JobStatus, OutcomeClass, OutputArtifactKind, PhaseKind,
};
use ingot_domain::revision::{ApprovalPolicy, AuthoringBaseSeed};
use ingot_domain::workspace::WorkspaceKind;
use ingot_domain::workspace::{RetentionPolicy, WorkspaceStatus};
use tower::ServiceExt;

mod common;
use common::*;

#[tokio::test]
async fn prepare_convergence_route_queues_lane_head_for_async_prepare() {
    let repo = temp_git_repo("ingot-http-api");
    let base_commit_oid = git_output(&repo, &["rev-parse", "HEAD"]);
    write_file(&repo.join("tracked.txt"), "candidate change");
    git(&repo, &["add", "tracked.txt"]);
    git(&repo, &["commit", "-m", "candidate commit"]);
    let source_commit_oid = git_output(&repo, &["rev-parse", "HEAD"]);
    git(
        &repo,
        &[
            "update-ref",
            "refs/ingot/workspaces/wrk_source",
            &source_commit_oid,
        ],
    );
    git(&repo, &["reset", "--hard", &base_commit_oid]);

    let db = migrated_test_db("ingot-http-api-db").await;
    let project_id = "prj_00000000000000000000000000000099".to_string();
    let item_id = "itm_00000000000000000000000000000099".to_string();
    let revision_id = "rev_00000000000000000000000000000099".to_string();
    let author_job_id = "job_00000000000000000000000000000098".to_string();
    let validate_job_id = "job_00000000000000000000000000000097".to_string();
    let workspace_id = "wrk_00000000000000000000000000000099".to_string();

    persist_test_change(
        &db,
        &repo,
        &project_id,
        &item_id,
        &revision_id,
        |item| item,
        |revision| {
            revision
                .approval_policy(ApprovalPolicy::Required)
                .seed(AuthoringBaseSeed::Explicit {
                    seed_commit_oid: base_commit_oid.clone().into(),
                    seed_target_commit_oid: base_commit_oid.clone().into(),
                })
                .template_map_snapshot(serde_json::json!({"author_initial":"author-initial"}))
        },
    )
    .await;
    let revision_policy_snapshot = serde_json::json!({
        "workflow_version": "delivery:v1",
        "approval_policy": "required",
        "candidate_rework_budget": 7,
        "integration_rework_budget": 8
    });
    sqlx::query("UPDATE item_revisions SET policy_snapshot = ? WHERE id = ?")
        .bind(revision_policy_snapshot.to_string())
        .bind(&revision_id)
        .execute(db.raw_pool())
        .await
        .expect("update revision policy snapshot");

    persist_test_workspace(
        &db,
        &project_id,
        WorkspaceKind::Authoring,
        &workspace_id,
        |workspace| {
            workspace
                .created_for_revision_id(parse_id(&revision_id))
                .path(repo.join("source-workspace").display().to_string())
                .workspace_ref("refs/ingot/workspaces/wrk_source")
                .base_commit_oid(&base_commit_oid)
                .head_commit_oid(&source_commit_oid)
                .retention_policy(RetentionPolicy::Persistent)
                .status(WorkspaceStatus::Ready)
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
            workspace_id: Some(&workspace_id),
            workspace_kind: WorkspaceKind::Authoring,
            execution_permission: ExecutionPermission::MayMutate,
            context_policy: ContextPolicy::Fresh,
            phase_template_slug: "author-initial",
            output_artifact_kind: OutputArtifactKind::Commit,
            job_input: TestJobInput::None,
            output_commit_oid: Some(&source_commit_oid),
            created_at: "2026-03-12T00:00:00Z",
            ended_at: Some("2026-03-12T00:05:00Z"),
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
            id: &validate_job_id,
            project_id: &project_id,
            item_id: &item_id,
            item_revision_id: &revision_id,
            step_id: "validate_candidate_initial",
            status: JobStatus::Completed,
            outcome_class: Some(OutcomeClass::Clean),
            phase_kind: PhaseKind::Validate,
            workspace_kind: WorkspaceKind::Authoring,
            execution_permission: ExecutionPermission::MustNotMutate,
            context_policy: ContextPolicy::ResumeContext,
            phase_template_slug: "validate-candidate",
            output_artifact_kind: OutputArtifactKind::ValidationReport,
            job_input: TestJobInput::CandidateSubject(&base_commit_oid, &source_commit_oid),
            result_schema_version: Some("validation_report:v1"),
            result_payload: Some(clean_validation_report("validation clean")),
            created_at: "2026-03-12T00:06:00Z",
            ended_at: Some("2026-03-12T00:07:00Z"),
            ..TestJobInsert::new(
                &validate_job_id,
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
                    "/api/projects/{project_id}/items/{item_id}/convergence/prepare"
                ))
                .method("POST")
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("route response");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json body");

    assert_eq!(json["queue"]["state"].as_str(), Some("head"));
    assert_eq!(json["queue"]["position"].as_i64(), Some(1));
    assert_eq!(json["convergences"].as_array().map(Vec::len), Some(0));
    let queue_state: (String,) =
        sqlx::query_as("SELECT status FROM convergence_queue_entries WHERE item_revision_id = ?")
            .bind(&revision_id)
            .fetch_one(db.raw_pool())
            .await
            .expect("queue state");
    assert_eq!(queue_state.0, "head");
}

#[tokio::test]
async fn approve_route_atomically_finalizes_convergence_and_closes_item() {
    let repo = temp_git_repo("ingot-http-api");
    let base_commit_oid = git_output(&repo, &["rev-parse", "HEAD"]);
    write_file(&repo.join("tracked.txt"), "prepared change");
    git(&repo, &["add", "tracked.txt"]);
    git(&repo, &["commit", "-m", "prepared commit"]);
    let prepared_commit_oid = git_output(&repo, &["rev-parse", "HEAD"]);
    git(
        &repo,
        &[
            "update-ref",
            "refs/ingot/workspaces/wrk_integration",
            &prepared_commit_oid,
        ],
    );
    git(&repo, &["reset", "--hard", &base_commit_oid]);

    let db = migrated_test_db("ingot-http-api-db").await;
    let project_id = "prj_00000000000000000000000000000088".to_string();
    let item_id = "itm_00000000000000000000000000000088".to_string();
    let revision_id = "rev_00000000000000000000000000000088".to_string();
    let workspace_id = "wrk_00000000000000000000000000000088".to_string();
    let convergence_id = "conv_00000000000000000000000000000088".to_string();

    persist_test_change(
        &db,
        &repo,
        &project_id,
        &item_id,
        &revision_id,
        |item| {
            item.approval_state(ApprovalState::Pending)
                .escalated(EscalationReason::ManualDecisionRequired)
        },
        |revision| {
            revision
                .approval_policy(ApprovalPolicy::Required)
                .seed(AuthoringBaseSeed::Explicit {
                    seed_commit_oid: base_commit_oid.clone().into(),
                    seed_target_commit_oid: base_commit_oid.clone().into(),
                })
                .template_map_snapshot(serde_json::json!({"author_initial":"author-initial"}))
        },
    )
    .await;
    let revision_policy_snapshot = serde_json::json!({
        "workflow_version": "delivery:v1",
        "approval_policy": "required",
        "candidate_rework_budget": 7,
        "integration_rework_budget": 8
    });
    sqlx::query("UPDATE item_revisions SET policy_snapshot = ? WHERE id = ?")
        .bind(revision_policy_snapshot.to_string())
        .bind(&revision_id)
        .execute(db.raw_pool())
        .await
        .expect("update revision policy snapshot");

    persist_test_workspace(
        &db,
        &project_id,
        WorkspaceKind::Integration,
        &workspace_id,
        |workspace| {
            workspace
                .created_for_revision_id(parse_id(&revision_id))
                .path(repo.join("integration-workspace").display().to_string())
                .workspace_ref("refs/ingot/workspaces/wrk_integration")
                .base_commit_oid(&base_commit_oid)
                .head_commit_oid(&prepared_commit_oid)
                .retention_policy(RetentionPolicy::Persistent)
                .status(WorkspaceStatus::Ready)
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
                .source_head_commit_oid(&prepared_commit_oid)
                .input_target_commit_oid(&base_commit_oid)
                .prepared_commit_oid(&prepared_commit_oid)
        },
    )
    .await;
    sqlx::query(
        "INSERT INTO convergence_queue_entries (
            id, project_id, item_id, item_revision_id, target_ref, status, head_acquired_at,
            created_at, updated_at, released_at
        ) VALUES (?, ?, ?, ?, 'refs/heads/main', 'head', ?, ?, ?, NULL)",
    )
    .bind("cqe_00000000000000000000000000000063")
    .bind(&project_id)
    .bind(&item_id)
    .bind(&revision_id)
    .bind(TS)
    .bind(TS)
    .bind(TS)
    .execute(db.raw_pool())
    .await
    .expect("insert queue entry");

    let app = test_router(db.clone());
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/projects/{project_id}/items/{item_id}/approval/approve"
                ))
                .method("POST")
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("route response");

    assert_eq!(response.status(), StatusCode::OK);

    let item_state: (String, String, Option<String>) = sqlx::query_as(
        "SELECT lifecycle_state, approval_state, resolution_source FROM items WHERE id = ?",
    )
    .bind(&item_id)
    .fetch_one(db.raw_pool())
    .await
    .expect("item state");
    assert_eq!(item_state.0, "done");
    assert_eq!(item_state.1, "approved");
    assert_eq!(item_state.2, Some("approval_command".into()));

    let convergence_status: (String,) =
        sqlx::query_as("SELECT status FROM convergences WHERE id = ?")
            .bind(&convergence_id)
            .fetch_one(db.raw_pool())
            .await
            .expect("convergence status");
    assert_eq!(convergence_status.0, "finalized");

    let queue_state: (String,) =
        sqlx::query_as("SELECT status FROM convergence_queue_entries WHERE item_revision_id = ?")
            .bind(&revision_id)
            .fetch_one(db.raw_pool())
            .await
            .expect("queue state");
    assert_eq!(queue_state.0, "released");
    assert_eq!(
        git_output(&repo, &["rev-parse", "HEAD"]),
        prepared_commit_oid
    );
    assert_eq!(
        git_output(&repo, &["rev-parse", "refs/heads/main"]),
        prepared_commit_oid
    );
    assert_eq!(
        std::fs::read_to_string(repo.join("tracked.txt")).expect("read checkout file"),
        "prepared change"
    );
}

#[tokio::test]
async fn approve_route_succeeds_when_target_ref_is_already_at_the_prepared_commit() {
    let repo = temp_git_repo("ingot-http-api");
    let base_commit_oid = git_output(&repo, &["rev-parse", "HEAD"]);
    write_file(&repo.join("tracked.txt"), "prepared change");
    git(&repo, &["add", "tracked.txt"]);
    git(&repo, &["commit", "-m", "prepared commit"]);
    let prepared_commit_oid = git_output(&repo, &["rev-parse", "HEAD"]);
    git(
        &repo,
        &[
            "update-ref",
            "refs/ingot/workspaces/wrk_integration",
            &prepared_commit_oid,
        ],
    );
    git(&repo, &["reset", "--hard", &base_commit_oid]);

    let db = migrated_test_db("ingot-http-api-db").await;
    let project_id = "prj_00000000000000000000000000000077".to_string();
    let item_id = "itm_00000000000000000000000000000077".to_string();
    let revision_id = "rev_00000000000000000000000000000077".to_string();
    let workspace_id = "wrk_00000000000000000000000000000077".to_string();
    let convergence_id = "conv_00000000000000000000000000000077".to_string();

    persist_test_change(
        &db,
        &repo,
        &project_id,
        &item_id,
        &revision_id,
        |item| {
            item.approval_state(ApprovalState::Pending)
                .escalated(EscalationReason::ManualDecisionRequired)
        },
        |revision| {
            revision
                .approval_policy(ApprovalPolicy::Required)
                .seed(AuthoringBaseSeed::Explicit {
                    seed_commit_oid: base_commit_oid.clone().into(),
                    seed_target_commit_oid: base_commit_oid.clone().into(),
                })
                .template_map_snapshot(serde_json::json!({"author_initial":"author-initial"}))
        },
    )
    .await;
    let revision_policy_snapshot = serde_json::json!({
        "workflow_version": "delivery:v1",
        "approval_policy": "required",
        "candidate_rework_budget": 7,
        "integration_rework_budget": 8
    });
    sqlx::query("UPDATE item_revisions SET policy_snapshot = ? WHERE id = ?")
        .bind(revision_policy_snapshot.to_string())
        .bind(&revision_id)
        .execute(db.raw_pool())
        .await
        .expect("update revision policy snapshot");

    persist_test_workspace(
        &db,
        &project_id,
        WorkspaceKind::Integration,
        &workspace_id,
        |workspace| {
            workspace
                .created_for_revision_id(parse_id(&revision_id))
                .path(repo.join("integration-workspace").display().to_string())
                .workspace_ref("refs/ingot/workspaces/wrk_integration")
                .base_commit_oid(&base_commit_oid)
                .head_commit_oid(&prepared_commit_oid)
                .retention_policy(RetentionPolicy::Persistent)
                .status(WorkspaceStatus::Ready)
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
                .source_head_commit_oid(&prepared_commit_oid)
                .input_target_commit_oid(&base_commit_oid)
                .prepared_commit_oid(&prepared_commit_oid)
        },
    )
    .await;
    sqlx::query(
        "INSERT INTO convergence_queue_entries (
            id, project_id, item_id, item_revision_id, target_ref, status, head_acquired_at,
            created_at, updated_at, released_at
        ) VALUES (?, ?, ?, ?, 'refs/heads/main', 'head', ?, ?, ?, NULL)",
    )
    .bind("cqe_00000000000000000000000000000077")
    .bind(&project_id)
    .bind(&item_id)
    .bind(&revision_id)
    .bind(TS)
    .bind(TS)
    .bind(TS)
    .execute(db.raw_pool())
    .await
    .expect("insert queue entry");

    git(&repo, &["reset", "--hard", &prepared_commit_oid]);

    let app = test_router(db.clone());
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/projects/{project_id}/items/{item_id}/approval/approve"
                ))
                .method("POST")
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("route response");

    assert_eq!(response.status(), StatusCode::OK);

    let item_state: (String, String, Option<String>) = sqlx::query_as(
        "SELECT lifecycle_state, approval_state, resolution_source FROM items WHERE id = ?",
    )
    .bind(&item_id)
    .fetch_one(db.raw_pool())
    .await
    .expect("item state");
    assert_eq!(item_state.0, "done");
    assert_eq!(item_state.1, "approved");
    assert_eq!(item_state.2, Some("approval_command".into()));

    let convergence_status: (String,) =
        sqlx::query_as("SELECT status FROM convergences WHERE id = ?")
            .bind(&convergence_id)
            .fetch_one(db.raw_pool())
            .await
            .expect("convergence status");
    assert_eq!(convergence_status.0, "finalized");

    let queue_state: (String,) =
        sqlx::query_as("SELECT status FROM convergence_queue_entries WHERE item_revision_id = ?")
            .bind(&revision_id)
            .fetch_one(db.raw_pool())
            .await
            .expect("queue state");
    assert_eq!(queue_state.0, "released");
}

#[tokio::test]
async fn approve_route_reuses_existing_finalize_op_when_checkout_is_blocked() {
    let repo = temp_git_repo("ingot-http-api");
    let base_commit_oid = git_output(&repo, &["rev-parse", "HEAD"]);
    write_file(&repo.join("tracked.txt"), "prepared change");
    git(&repo, &["add", "tracked.txt"]);
    git(&repo, &["commit", "-m", "prepared commit"]);
    let prepared_commit_oid = git_output(&repo, &["rev-parse", "HEAD"]);
    git(
        &repo,
        &[
            "update-ref",
            "refs/ingot/workspaces/wrk_integration_blocked",
            &prepared_commit_oid,
        ],
    );
    git(&repo, &["reset", "--hard", &base_commit_oid]);
    write_file(&repo.join("tracked.txt"), "dirty checkout");

    let db = migrated_test_db("ingot-http-api-db").await;
    let project_id = "prj_00000000000000000000000000000076".to_string();
    let item_id = "itm_00000000000000000000000000000076".to_string();
    let revision_id = "rev_00000000000000000000000000000076".to_string();
    let workspace_id = "wrk_00000000000000000000000000000076".to_string();
    let convergence_id = "conv_00000000000000000000000000000076".to_string();

    persist_test_change(
        &db,
        &repo,
        &project_id,
        &item_id,
        &revision_id,
        |item| {
            item.approval_state(ApprovalState::Pending)
                .escalated(EscalationReason::ManualDecisionRequired)
        },
        |revision| {
            revision
                .approval_policy(ApprovalPolicy::Required)
                .seed(AuthoringBaseSeed::Explicit {
                    seed_commit_oid: base_commit_oid.clone().into(),
                    seed_target_commit_oid: base_commit_oid.clone().into(),
                })
                .template_map_snapshot(serde_json::json!({"author_initial":"author-initial"}))
        },
    )
    .await;
    let revision_policy_snapshot = serde_json::json!({
        "workflow_version": "delivery:v1",
        "approval_policy": "required",
        "candidate_rework_budget": 7,
        "integration_rework_budget": 8
    });
    sqlx::query("UPDATE item_revisions SET policy_snapshot = ? WHERE id = ?")
        .bind(revision_policy_snapshot.to_string())
        .bind(&revision_id)
        .execute(db.raw_pool())
        .await
        .expect("update revision policy snapshot");

    persist_test_workspace(
        &db,
        &project_id,
        WorkspaceKind::Integration,
        &workspace_id,
        |workspace| {
            workspace
                .created_for_revision_id(parse_id(&revision_id))
                .path(
                    repo.join("integration-workspace-blocked")
                        .display()
                        .to_string(),
                )
                .workspace_ref("refs/ingot/workspaces/wrk_integration_blocked")
                .base_commit_oid(&base_commit_oid)
                .head_commit_oid(&prepared_commit_oid)
                .retention_policy(RetentionPolicy::Persistent)
                .status(WorkspaceStatus::Ready)
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
                .source_head_commit_oid(&prepared_commit_oid)
                .input_target_commit_oid(&base_commit_oid)
                .prepared_commit_oid(&prepared_commit_oid)
        },
    )
    .await;
    sqlx::query(
        "INSERT INTO convergence_queue_entries (
            id, project_id, item_id, item_revision_id, target_ref, status, head_acquired_at,
            created_at, updated_at, released_at
        ) VALUES (?, ?, ?, ?, 'refs/heads/main', 'head', ?, ?, ?, NULL)",
    )
    .bind("cqe_00000000000000000000000000000076")
    .bind(&project_id)
    .bind(&item_id)
    .bind(&revision_id)
    .bind(TS)
    .bind(TS)
    .bind(TS)
    .execute(db.raw_pool())
    .await
    .expect("insert queue entry");

    let app = test_router(db.clone());
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/projects/{project_id}/items/{item_id}/approval/approve"
                ))
                .method("POST")
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("route response");
    assert_eq!(response.status(), StatusCode::OK);

    let repeat_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/projects/{project_id}/items/{item_id}/approval/approve"
                ))
                .method("POST")
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("repeat route response");
    assert_eq!(repeat_response.status(), StatusCode::CONFLICT);

    let item_state: (String, String, String, Option<String>) = sqlx::query_as(
        "SELECT lifecycle_state, approval_state, escalation_state, escalation_reason FROM items WHERE id = ?",
    )
    .bind(&item_id)
    .fetch_one(db.raw_pool())
    .await
    .expect("item state");
    assert_eq!(item_state.0, "done");
    assert_eq!(item_state.1, "approved");
    assert_eq!(item_state.2, "none");
    assert_eq!(item_state.3, None);

    let convergence_status: (String,) =
        sqlx::query_as("SELECT status FROM convergences WHERE id = ?")
            .bind(&convergence_id)
            .fetch_one(db.raw_pool())
            .await
            .expect("convergence status");
    assert_eq!(convergence_status.0, "finalized");

    let queue_state: (String,) =
        sqlx::query_as("SELECT status FROM convergence_queue_entries WHERE item_revision_id = ?")
            .bind(&revision_id)
            .fetch_one(db.raw_pool())
            .await
            .expect("queue state");
    assert_eq!(queue_state.0, "released");

    let git_ops: Vec<(String, String)> = sqlx::query_as(
        "SELECT id, status FROM git_operations
         WHERE operation_kind = 'finalize_target_ref' AND entity_id = ?
         ORDER BY created_at ASC",
    )
    .bind(&convergence_id)
    .fetch_all(db.raw_pool())
    .await
    .expect("git operations");
    assert_eq!(git_ops.len(), 1);
    assert_eq!(git_ops[0].1, "applied");

    assert_eq!(
        git_output(&repo, &["rev-parse", "refs/heads/main"]),
        base_commit_oid
    );
}

#[tokio::test]
async fn prepare_convergence_route_only_queues_even_when_future_prepare_would_conflict() {
    let repo = temp_git_repo("ingot-http-api");
    let base_commit_oid = git_output(&repo, &["rev-parse", "HEAD"]);
    write_file(&repo.join("tracked.txt"), "source change");
    git(&repo, &["add", "tracked.txt"]);
    git(&repo, &["commit", "-m", "source commit"]);
    let source_commit_oid = git_output(&repo, &["rev-parse", "HEAD"]);
    git(
        &repo,
        &[
            "update-ref",
            "refs/ingot/workspaces/wrk_source_conflict",
            &source_commit_oid,
        ],
    );
    git(&repo, &["reset", "--hard", &base_commit_oid]);
    write_file(&repo.join("tracked.txt"), "target change");
    git(&repo, &["add", "tracked.txt"]);
    git(&repo, &["commit", "-m", "target commit"]);

    let db = migrated_test_db("ingot-http-api-db").await;
    let project_id = "prj_00000000000000000000000000000042".to_string();
    let item_id = "itm_00000000000000000000000000000042".to_string();
    let revision_id = "rev_00000000000000000000000000000042".to_string();
    let workspace_id = "wrk_00000000000000000000000000000042".to_string();
    let author_job_id = "job_00000000000000000000000000000042".to_string();
    let validate_job_id = "job_00000000000000000000000000000041".to_string();

    persist_test_change(
        &db,
        &repo,
        &project_id,
        &item_id,
        &revision_id,
        |item| item,
        |revision| {
            revision
                .approval_policy(ApprovalPolicy::Required)
                .seed(AuthoringBaseSeed::Explicit {
                    seed_commit_oid: base_commit_oid.clone().into(),
                    seed_target_commit_oid: base_commit_oid.clone().into(),
                })
                .template_map_snapshot(serde_json::json!({"author_initial":"author-initial"}))
        },
    )
    .await;
    let revision_policy_snapshot = serde_json::json!({
        "workflow_version": "delivery:v1",
        "approval_policy": "required",
        "candidate_rework_budget": 7,
        "integration_rework_budget": 8
    });
    sqlx::query("UPDATE item_revisions SET policy_snapshot = ? WHERE id = ?")
        .bind(revision_policy_snapshot.to_string())
        .bind(&revision_id)
        .execute(db.raw_pool())
        .await
        .expect("update revision policy snapshot");

    persist_test_workspace(
        &db,
        &project_id,
        WorkspaceKind::Authoring,
        &workspace_id,
        |workspace| {
            workspace
                .created_for_revision_id(parse_id(&revision_id))
                .path(repo.join("source-conflict").display().to_string())
                .workspace_ref("refs/ingot/workspaces/wrk_source_conflict")
                .base_commit_oid(&base_commit_oid)
                .head_commit_oid(&source_commit_oid)
                .retention_policy(RetentionPolicy::Persistent)
                .status(WorkspaceStatus::Ready)
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
            workspace_id: Some(&workspace_id),
            workspace_kind: WorkspaceKind::Authoring,
            execution_permission: ExecutionPermission::MayMutate,
            context_policy: ContextPolicy::Fresh,
            phase_template_slug: "author-initial",
            output_artifact_kind: OutputArtifactKind::Commit,
            job_input: TestJobInput::None,
            output_commit_oid: Some(&source_commit_oid),
            created_at: "2026-03-12T00:00:00Z",
            ended_at: Some("2026-03-12T00:05:00Z"),
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
            id: &validate_job_id,
            project_id: &project_id,
            item_id: &item_id,
            item_revision_id: &revision_id,
            step_id: "validate_candidate_initial",
            status: JobStatus::Completed,
            outcome_class: Some(OutcomeClass::Clean),
            phase_kind: PhaseKind::Validate,
            workspace_kind: WorkspaceKind::Authoring,
            execution_permission: ExecutionPermission::MustNotMutate,
            context_policy: ContextPolicy::ResumeContext,
            phase_template_slug: "validate-candidate",
            output_artifact_kind: OutputArtifactKind::ValidationReport,
            job_input: TestJobInput::CandidateSubject(&base_commit_oid, &source_commit_oid),
            result_schema_version: Some("validation_report:v1"),
            result_payload: Some(clean_validation_report("validation clean")),
            created_at: "2026-03-12T00:06:00Z",
            ended_at: Some("2026-03-12T00:07:00Z"),
            ..TestJobInsert::new(
                &validate_job_id,
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
                    "/api/projects/{project_id}/items/{item_id}/convergence/prepare"
                ))
                .method("POST")
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("route response");

    assert_eq!(response.status(), StatusCode::OK);
    let item_state: (String, Option<String>) =
        sqlx::query_as("SELECT escalation_state, escalation_reason FROM items WHERE id = ?")
            .bind(&item_id)
            .fetch_one(db.raw_pool())
            .await
            .expect("item state");
    assert_eq!(item_state.0, "none");
    assert_eq!(item_state.1, None);
}

#[tokio::test]
async fn reject_approval_route_cancels_prepared_convergence_and_creates_superseding_revision() {
    let repo = temp_git_repo("ingot-http-api");
    let base_commit_oid = git_output(&repo, &["rev-parse", "HEAD"]);
    write_file(&repo.join("tracked.txt"), "candidate change");
    git(&repo, &["add", "tracked.txt"]);
    git(&repo, &["commit", "-m", "candidate"]);
    let candidate_commit_oid = git_output(&repo, &["rev-parse", "HEAD"]);
    git(&repo, &["reset", "--hard", &base_commit_oid]);

    let db = migrated_test_db("ingot-http-api-db").await;
    let project_id = "prj_00000000000000000000000000000077".to_string();
    let item_id = "itm_00000000000000000000000000000077".to_string();
    let revision_id = "rev_00000000000000000000000000000077".to_string();
    let workspace_id = "wrk_00000000000000000000000000000077".to_string();
    let convergence_id = "conv_00000000000000000000000000000077".to_string();
    let author_job_id = "job_00000000000000000000000000000077".to_string();

    persist_test_change(
        &db,
        &repo,
        &project_id,
        &item_id,
        &revision_id,
        |item| {
            item.approval_state(ApprovalState::Pending)
                .escalated(EscalationReason::ManualDecisionRequired)
        },
        |revision| {
            revision
                .approval_policy(ApprovalPolicy::Required)
                .seed(AuthoringBaseSeed::Explicit {
                    seed_commit_oid: base_commit_oid.clone().into(),
                    seed_target_commit_oid: base_commit_oid.clone().into(),
                })
                .template_map_snapshot(serde_json::json!({"author_initial":"author-initial"}))
        },
    )
    .await;
    let revision_policy_snapshot = serde_json::json!({
        "workflow_version": "delivery:v1",
        "approval_policy": "required",
        "candidate_rework_budget": 7,
        "integration_rework_budget": 8
    });
    sqlx::query("UPDATE item_revisions SET policy_snapshot = ? WHERE id = ?")
        .bind(revision_policy_snapshot.to_string())
        .bind(&revision_id)
        .execute(db.raw_pool())
        .await
        .expect("update revision policy snapshot");

    persist_test_workspace(
        &db,
        &project_id,
        WorkspaceKind::Integration,
        &workspace_id,
        |workspace| {
            workspace
                .created_for_revision_id(parse_id(&revision_id))
                .path(repo.join("integration-reject").display().to_string())
                .workspace_ref("refs/ingot/workspaces/wrk_integration_reject")
                .base_commit_oid(&base_commit_oid)
                .head_commit_oid(&candidate_commit_oid)
                .retention_policy(RetentionPolicy::Persistent)
                .status(WorkspaceStatus::Ready)
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
            job_input: TestJobInput::None,
            output_commit_oid: Some(&candidate_commit_oid),
            created_at: "2026-03-12T00:00:00Z",
            ended_at: Some("2026-03-12T00:05:00Z"),
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
                .source_head_commit_oid(&candidate_commit_oid)
                .input_target_commit_oid(&base_commit_oid)
                .prepared_commit_oid(&candidate_commit_oid)
        },
    )
    .await;

    let app = test_router(db.clone());
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/projects/{project_id}/items/{item_id}/approval/reject"
                ))
                .method("POST")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "approval_policy": "not_required"
                    })
                    .to_string(),
                ))
                .expect("build request"),
        )
        .await
        .expect("route response");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(
        json["item"]["approval_state"].as_str(),
        Some("not_required")
    );
    assert_eq!(json["item"]["lifecycle_state"].as_str(), Some("open"));
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
        Some(7)
    );
    assert_ne!(
        json["current_revision"]["id"].as_str(),
        Some(revision_id.as_str())
    );
    assert_eq!(
        json["current_revision"]["supersedes_revision_id"].as_str(),
        Some(revision_id.as_str())
    );
    assert_eq!(
        json["current_revision"]["seed_commit_oid"].as_str(),
        Some(candidate_commit_oid.as_str())
    );

    let revision_policy_snapshot: String = sqlx::query_scalar(
        "SELECT policy_snapshot FROM item_revisions WHERE item_id = ? AND revision_no = 2",
    )
    .bind(&item_id)
    .fetch_one(db.raw_pool())
    .await
    .expect("load rejected policy snapshot");
    let revision_policy_snapshot: serde_json::Value =
        serde_json::from_str(&revision_policy_snapshot).expect("rejected policy snapshot json");
    assert_eq!(
        revision_policy_snapshot["approval_policy"].as_str(),
        Some("not_required")
    );
    assert_eq!(
        revision_policy_snapshot["candidate_rework_budget"].as_u64(),
        Some(7)
    );

    let item_escalation: (String, Option<String>) =
        sqlx::query_as("SELECT escalation_state, escalation_reason FROM items WHERE id = ?")
            .bind(&item_id)
            .fetch_one(db.raw_pool())
            .await
            .expect("item escalation");
    assert_eq!(item_escalation.0, "none");
    assert_eq!(item_escalation.1, None);

    let convergence_status: String =
        sqlx::query_scalar("SELECT status FROM convergences WHERE id = ?")
            .bind(&convergence_id)
            .fetch_one(db.raw_pool())
            .await
            .expect("convergence status");
    assert_eq!(convergence_status, "cancelled");

    let item_state: (String, Option<String>) =
        sqlx::query_as("SELECT escalation_state, escalation_reason FROM items WHERE id = ?")
            .bind(&item_id)
            .fetch_one(db.raw_pool())
            .await
            .expect("item state");
    assert_eq!(item_state.0, "none");
    assert_eq!(item_state.1, None);
}

#[tokio::test]
async fn reject_route_allows_pending_with_queue_entry_only() {
    let repo = temp_git_repo("ingot-http-api");
    let head = git_output(&repo, &["rev-parse", "HEAD"]);
    let db = migrated_test_db("ingot-http-api-db").await;
    let project_id = "prj_00000000000000000000000000000060".to_string();
    let item_id = "itm_00000000000000000000000000000060".to_string();
    let revision_id = "rev_00000000000000000000000000000060".to_string();

    persist_test_change(
        &db,
        &repo,
        &project_id,
        &item_id,
        &revision_id,
        |item| item.approval_state(ApprovalState::Pending),
        |revision| revision.explicit_seed(&head),
    )
    .await;
    let convergence_id = "conv_00000000000000000000000000000060".to_string();
    let workspace_id = "wrk_00000000000000000000000000000060".to_string();
    persist_test_workspace(
        &db,
        &project_id,
        WorkspaceKind::Integration,
        &workspace_id,
        |workspace| {
            workspace
                .created_for_revision_id(parse_id(&revision_id))
                .path(repo.join("integration-reject").display().to_string())
                .workspace_ref("refs/ingot/workspaces/wrk_integration_reject")
                .base_commit_oid(&head)
                .head_commit_oid(&head)
                .retention_policy(RetentionPolicy::Persistent)
                .status(WorkspaceStatus::Ready)
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
                .source_head_commit_oid(&head)
                .input_target_commit_oid(&head)
                .prepared_commit_oid(&head)
        },
    )
    .await;
    sqlx::query(
        "INSERT INTO convergence_queue_entries (
            id, project_id, item_id, item_revision_id, target_ref, status, head_acquired_at,
            created_at, updated_at, released_at
         ) VALUES (?, ?, ?, ?, 'refs/heads/main', 'head', ?, ?, ?, NULL)",
    )
    .bind("cqe_00000000000000000000000000000060")
    .bind(&project_id)
    .bind(&item_id)
    .bind(&revision_id)
    .bind(TS)
    .bind(TS)
    .bind(TS)
    .execute(db.raw_pool())
    .await
    .expect("insert queue entry");

    let response = test_router(db.clone())
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/projects/{project_id}/items/{item_id}/approval/reject"
                ))
                .method("POST")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{\"approval_policy\":\"not_required\"}"))
                .expect("build request"),
        )
        .await
        .expect("reject response");

    assert_eq!(response.status(), StatusCode::OK);
    let queue_state: (String,) =
        sqlx::query_as("SELECT status FROM convergence_queue_entries WHERE item_revision_id = ?")
            .bind(&revision_id)
            .fetch_one(db.raw_pool())
            .await
            .expect("queue state");
    assert_eq!(queue_state.0, "cancelled");

    let revision_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM item_revisions WHERE item_id = ?")
            .bind(&item_id)
            .fetch_one(db.raw_pool())
            .await
            .expect("revision count");
    assert_eq!(revision_count, 2);
}
