
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use ingot_domain::job::{ContextPolicy, ExecutionPermission, JobStatus, OutcomeClass, OutputArtifactKind, PhaseKind};
use ingot_domain::workspace::WorkspaceKind;
use ingot_store_sqlite::Database;
use tower::ServiceExt;
use uuid::Uuid;

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

    let db_path = std::env::temp_dir().join(format!("ingot-http-api-db-{}.db", Uuid::now_v7()));
    let db = Database::connect(&db_path).await.expect("connect db");
    db.migrate().await.expect("migrate db");
    let project_id = "prj_00000000000000000000000000000099".to_string();
    let item_id = "itm_00000000000000000000000000000099".to_string();
    let revision_id = "rev_00000000000000000000000000000099".to_string();
    let author_job_id = "job_00000000000000000000000000000098".to_string();
    let validate_job_id = "job_00000000000000000000000000000097".to_string();
    let workspace_id = "wrk_00000000000000000000000000000099".to_string();

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
         ) VALUES (?, ?, 1, 'Title', 'Desc', 'AC', 'refs/heads/main', 'required', '{\"workflow_version\":\"delivery:v1\",\"approval_policy\":\"required\",\"candidate_rework_budget\":7,\"integration_rework_budget\":8}', '{\"author_initial\":\"author-initial\"}', ?, ?, NULL, ?)",
    )
    .bind(&revision_id)
    .bind(&item_id)
    .bind(&base_commit_oid)
    .bind(&base_commit_oid)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert revision");

    sqlx::query(
        "INSERT INTO workspaces (
            id, project_id, kind, strategy, path, created_for_revision_id, parent_workspace_id,
            target_ref, workspace_ref, base_commit_oid, head_commit_oid, retention_policy,
            status, current_job_id, created_at, updated_at
         ) VALUES (?, ?, 'authoring', 'worktree', ?, ?, NULL, 'refs/heads/main', 'refs/ingot/workspaces/wrk_source', ?, ?, 'persistent', 'ready', NULL, ?, ?)",
    )
    .bind(&workspace_id)
    .bind(&project_id)
    .bind(repo.join("source-workspace").display().to_string())
    .bind(&revision_id)
    .bind(&base_commit_oid)
    .bind(&source_commit_oid)
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert source workspace");

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
            result_payload: Some(serde_json::json!({
                "outcome": "clean",
                "summary": "validation clean",
                "checks": [],
                "findings": []
            })),
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
    let queue_state: (String,) = sqlx::query_as(
        "SELECT status FROM convergence_queue_entries WHERE item_revision_id = ?",
    )
    .bind(&revision_id)
    .fetch_one(&db.pool)
    .await
    .expect("queue state");
    assert_eq!(queue_state.0, "head");
}

#[tokio::test]
async fn approve_route_grants_lane_head_without_finalizing_synchronously() {
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

    let db_path = std::env::temp_dir().join(format!("ingot-http-api-db-{}.db", Uuid::now_v7()));
    let db = Database::connect(&db_path).await.expect("connect db");
    db.migrate().await.expect("migrate db");
    let project_id = "prj_00000000000000000000000000000088".to_string();
    let item_id = "itm_00000000000000000000000000000088".to_string();
    let revision_id = "rev_00000000000000000000000000000088".to_string();
    let workspace_id = "wrk_00000000000000000000000000000088".to_string();
    let convergence_id = "conv_00000000000000000000000000000088".to_string();

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
         ) VALUES (?, ?, 'change', 'delivery:v1', 'open', 'active', 'pending', 'operator_required', 'manual_decision_required', ?, 'manual', NULL, 'major', '[]', ?, ?)",
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
         ) VALUES (?, ?, 1, 'Title', 'Desc', 'AC', 'refs/heads/main', 'required', '{\"workflow_version\":\"delivery:v1\",\"approval_policy\":\"required\",\"candidate_rework_budget\":7,\"integration_rework_budget\":8}', '{\"author_initial\":\"author-initial\"}', ?, ?, NULL, ?)",
    )
    .bind(&revision_id)
    .bind(&item_id)
    .bind(&base_commit_oid)
    .bind(&base_commit_oid)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert revision");

    sqlx::query(
        "INSERT INTO workspaces (
            id, project_id, kind, strategy, path, created_for_revision_id, parent_workspace_id,
            target_ref, workspace_ref, base_commit_oid, head_commit_oid, retention_policy,
            status, current_job_id, created_at, updated_at
         ) VALUES (?, ?, 'integration', 'worktree', ?, ?, NULL, 'refs/heads/main', 'refs/ingot/workspaces/wrk_integration', ?, ?, 'persistent', 'ready', NULL, ?, ?)",
    )
    .bind(&workspace_id)
    .bind(&project_id)
    .bind(repo.join("integration-workspace").display().to_string())
    .bind(&revision_id)
    .bind(&base_commit_oid)
    .bind(&prepared_commit_oid)
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
         ) VALUES (?, ?, ?, ?, ?, ?, ?, 'refs/heads/main', 'rebase_then_fast_forward', 'prepared', ?, ?, NULL, NULL, ?, NULL)",
    )
    .bind(&convergence_id)
    .bind(&project_id)
    .bind(&item_id)
    .bind(&revision_id)
    .bind(&workspace_id)
    .bind(&workspace_id)
    .bind(&prepared_commit_oid)
    .bind(&base_commit_oid)
    .bind(&prepared_commit_oid)
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
    .bind("cqe_00000000000000000000000000000063")
    .bind(&project_id)
    .bind(&item_id)
    .bind(&revision_id)
    .bind(TS)
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
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
    assert_eq!(
        git_output(&repo, &["rev-parse", "refs/heads/main"]),
        base_commit_oid
    );

    let item_state: (String, String, Option<String>) = sqlx::query_as(
        "SELECT lifecycle_state, approval_state, resolution_source FROM items WHERE id = ?",
    )
    .bind(&item_id)
    .fetch_one(&db.pool)
    .await
    .expect("item state");
    assert_eq!(item_state.0, "open");
    assert_eq!(item_state.1, "granted");
    assert_eq!(item_state.2, None);
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

    let db_path = std::env::temp_dir().join(format!("ingot-http-api-db-{}.db", Uuid::now_v7()));
    let db = Database::connect(&db_path).await.expect("connect db");
    db.migrate().await.expect("migrate db");
    let project_id = "prj_00000000000000000000000000000042".to_string();
    let item_id = "itm_00000000000000000000000000000042".to_string();
    let revision_id = "rev_00000000000000000000000000000042".to_string();
    let workspace_id = "wrk_00000000000000000000000000000042".to_string();
    let author_job_id = "job_00000000000000000000000000000042".to_string();
    let validate_job_id = "job_00000000000000000000000000000041".to_string();

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
         ) VALUES (?, ?, 1, 'Title', 'Desc', 'AC', 'refs/heads/main', 'required', '{\"workflow_version\":\"delivery:v1\",\"approval_policy\":\"required\",\"candidate_rework_budget\":7,\"integration_rework_budget\":8}', '{\"author_initial\":\"author-initial\"}', ?, ?, NULL, ?)",
    )
    .bind(&revision_id)
    .bind(&item_id)
    .bind(&base_commit_oid)
    .bind(&base_commit_oid)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert revision");

    sqlx::query(
        "INSERT INTO workspaces (
            id, project_id, kind, strategy, path, created_for_revision_id, parent_workspace_id,
            target_ref, workspace_ref, base_commit_oid, head_commit_oid, retention_policy,
            status, current_job_id, created_at, updated_at
         ) VALUES (?, ?, 'authoring', 'worktree', ?, ?, NULL, 'refs/heads/main', 'refs/ingot/workspaces/wrk_source_conflict', ?, ?, 'persistent', 'ready', NULL, ?, ?)",
    )
    .bind(&workspace_id)
    .bind(&project_id)
    .bind(repo.join("source-conflict").display().to_string())
    .bind(&revision_id)
    .bind(&base_commit_oid)
    .bind(&source_commit_oid)
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert source workspace");

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
            result_payload: Some(serde_json::json!({
                "outcome": "clean",
                "summary": "validation clean",
                "checks": [],
                "findings": []
            })),
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
            .fetch_one(&db.pool)
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

    let db_path = std::env::temp_dir().join(format!("ingot-http-api-db-{}.db", Uuid::now_v7()));
    let db = Database::connect(&db_path).await.expect("connect db");
    db.migrate().await.expect("migrate db");
    let project_id = "prj_00000000000000000000000000000077".to_string();
    let item_id = "itm_00000000000000000000000000000077".to_string();
    let revision_id = "rev_00000000000000000000000000000077".to_string();
    let workspace_id = "wrk_00000000000000000000000000000077".to_string();
    let convergence_id = "conv_00000000000000000000000000000077".to_string();
    let author_job_id = "job_00000000000000000000000000000077".to_string();

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
         ) VALUES (?, ?, 'change', 'delivery:v1', 'open', 'active', 'pending', 'operator_required', 'manual_decision_required', ?, 'manual', NULL, 'major', '[]', ?, ?)",
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
         ) VALUES (?, ?, 1, 'Title', 'Desc', 'AC', 'refs/heads/main', 'required', '{\"workflow_version\":\"delivery:v1\",\"approval_policy\":\"required\",\"candidate_rework_budget\":7,\"integration_rework_budget\":8}', '{\"author_initial\":\"author-initial\"}', ?, ?, NULL, ?)",
    )
    .bind(&revision_id)
    .bind(&item_id)
    .bind(&base_commit_oid)
    .bind(&base_commit_oid)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert revision");

    sqlx::query(
        "INSERT INTO workspaces (
            id, project_id, kind, strategy, path, created_for_revision_id, parent_workspace_id,
            target_ref, workspace_ref, base_commit_oid, head_commit_oid, retention_policy,
            status, current_job_id, created_at, updated_at
         ) VALUES (?, ?, 'integration', 'worktree', ?, ?, NULL, 'refs/heads/main', 'refs/ingot/workspaces/wrk_integration_reject', ?, ?, 'persistent', 'ready', NULL, ?, ?)",
    )
    .bind(&workspace_id)
    .bind(&project_id)
    .bind(repo.join("integration-reject").display().to_string())
    .bind(&revision_id)
    .bind(&base_commit_oid)
    .bind(&candidate_commit_oid)
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert integration workspace");

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

    sqlx::query(
        "INSERT INTO convergences (
            id, project_id, item_id, item_revision_id, source_workspace_id, integration_workspace_id,
            source_head_commit_oid, target_ref, strategy, status, input_target_commit_oid,
            prepared_commit_oid, final_target_commit_oid, conflict_summary, created_at, completed_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?, 'refs/heads/main', 'rebase_then_fast_forward', 'prepared', ?, ?, NULL, NULL, ?, NULL)",
    )
    .bind(&convergence_id)
    .bind(&project_id)
    .bind(&item_id)
    .bind(&revision_id)
    .bind(&workspace_id)
    .bind(&workspace_id)
    .bind(&candidate_commit_oid)
    .bind(&base_commit_oid)
    .bind(&candidate_commit_oid)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert convergence");

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
    .fetch_one(&db.pool)
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
            .fetch_one(&db.pool)
            .await
            .expect("item escalation");
    assert_eq!(item_escalation.0, "none");
    assert_eq!(item_escalation.1, None);

    let convergence_status: String =
        sqlx::query_scalar("SELECT status FROM convergences WHERE id = ?")
            .bind(&convergence_id)
            .fetch_one(&db.pool)
            .await
            .expect("convergence status");
    assert_eq!(convergence_status, "cancelled");

    let item_state: (String, Option<String>) =
        sqlx::query_as("SELECT escalation_state, escalation_reason FROM items WHERE id = ?")
            .bind(&item_id)
            .fetch_one(&db.pool)
            .await
            .expect("item state");
    assert_eq!(item_state.0, "none");
    assert_eq!(item_state.1, None);
}

#[tokio::test]
async fn reject_route_allows_granted_without_prepared_convergence() {
    let repo = temp_git_repo("ingot-http-api");
    let head = git_output(&repo, &["rev-parse", "HEAD"]);
    let db_path = std::env::temp_dir().join(format!("ingot-http-api-db-{}.db", Uuid::now_v7()));
    let db = Database::connect(&db_path).await.expect("connect db");
    db.migrate().await.expect("migrate db");
    let project_id = "prj_00000000000000000000000000000060".to_string();
    let item_id = "itm_00000000000000000000000000000060".to_string();
    let revision_id = "rev_00000000000000000000000000000060".to_string();

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
    .execute(&db.pool)
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
    let queue_state: (String,) = sqlx::query_as(
        "SELECT status FROM convergence_queue_entries WHERE item_revision_id = ?",
    )
    .bind(&revision_id)
    .fetch_one(&db.pool)
    .await
    .expect("queue state");
    assert_eq!(queue_state.0, "cancelled");

    let revision_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM item_revisions WHERE item_id = ?")
            .bind(&item_id)
            .fetch_one(&db.pool)
            .await
            .expect("revision count");
    assert_eq!(revision_count, 2);
}
