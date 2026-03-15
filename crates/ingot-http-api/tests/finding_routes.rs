use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use ingot_domain::job::{ContextPolicy, ExecutionPermission, JobStatus, OutcomeClass, OutputArtifactKind, PhaseKind};
use ingot_domain::workspace::WorkspaceKind;
use ingot_store_sqlite::Database;
use tower::ServiceExt;
use uuid::Uuid;

mod common;
use common::*;

#[tokio::test]
async fn triaging_final_integrated_finding_enters_pending_approval() {
    let repo = temp_git_repo("ingot-http-api");
    let head = git_output(&repo, &["rev-parse", "HEAD"]);
    let db_path =
        std::env::temp_dir().join(format!("ingot-http-api-triage-{}.db", Uuid::now_v7()));
    let db = Database::connect(&db_path).await.expect("connect db");
    db.migrate().await.expect("migrate db");

    let project_id = "prj_11111111111111111111111111111111";
    let item_id = "itm_11111111111111111111111111111111";
    let revision_id = "rev_11111111111111111111111111111111";
    let job_id = "job_11111111111111111111111111111111";
    let convergence_id = "conv_11111111111111111111111111111111";
    let workspace_id = "wrk_11111111111111111111111111111111";
    let finding_id = "fnd_11111111111111111111111111111111";

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
    insert_test_job_row(
        &db,
        TestJobInsert {
            id: job_id,
            project_id,
            item_id,
            item_revision_id: revision_id,
            step_id: "validate_integrated",
            status: JobStatus::Completed,
            outcome_class: Some(OutcomeClass::Findings),
            phase_kind: PhaseKind::Validate,
            workspace_kind: WorkspaceKind::Integration,
            execution_permission: ExecutionPermission::MustNotMutate,
            context_policy: ContextPolicy::ResumeContext,
            phase_template_slug: "validate-integrated",
            output_artifact_kind: OutputArtifactKind::ValidationReport,
            job_input: TestJobInput::IntegratedSubject(&head, &head),
            created_at: "2026-03-12T00:00:00Z",
            ended_at: Some("2026-03-12T00:01:00Z"),
            ..TestJobInsert::new(
                job_id,
                project_id,
                item_id,
                revision_id,
                "validate_integrated",
            )
        },
    )
    .await;
    sqlx::query(
        "INSERT INTO workspaces (
            id, project_id, kind, strategy, path, created_for_revision_id, parent_workspace_id,
            target_ref, workspace_ref, base_commit_oid, head_commit_oid, retention_policy,
            status, current_job_id, created_at, updated_at
         ) VALUES (?, ?, 'authoring', 'worktree', ?, ?, NULL, 'refs/heads/main', NULL, ?, ?, 'ephemeral', 'ready', NULL, ?, ?)",
    )
    .bind(workspace_id)
    .bind(project_id)
    .bind(repo.join("workspace").display().to_string())
    .bind(revision_id)
    .bind(&head)
    .bind(&head)
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert workspace");
    sqlx::query(
        "INSERT INTO convergences (
            id, project_id, item_id, item_revision_id, source_workspace_id, integration_workspace_id,
            source_head_commit_oid, target_ref, strategy, status, input_target_commit_oid,
            prepared_commit_oid, final_target_commit_oid, conflict_summary, created_at, completed_at
         ) VALUES (?, ?, ?, ?, ?, NULL, ?, 'refs/heads/main', 'rebase_then_fast_forward', 'prepared', ?, ?, NULL, NULL, ?, NULL)",
    )
    .bind(convergence_id)
    .bind(project_id)
    .bind(item_id)
    .bind(revision_id)
    .bind(workspace_id)
    .bind(&head)
    .bind(&head)
    .bind(&head)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert convergence");
    sqlx::query(
        "INSERT INTO findings (
            id, project_id, source_item_id, source_item_revision_id, source_job_id, source_step_id,
            source_report_schema_version, source_finding_key, source_subject_kind,
            source_subject_base_commit_oid, source_subject_head_commit_oid, code, severity, summary,
            paths, evidence, triage_state, linked_item_id, triage_note, created_at, triaged_at
         ) VALUES (?, ?, ?, ?, ?, 'validate_integrated', 'validation_report:v1', 'finding-1', 'integrated', ?, ?, 'BUG001', 'high', 'summary', '[]', '[]', 'untriaged', NULL, NULL, ?, NULL)",
    )
    .bind(finding_id)
    .bind(project_id)
    .bind(item_id)
    .bind(revision_id)
    .bind(job_id)
    .bind(&head)
    .bind(&head)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert finding");

    let response = test_router(db.clone())
        .oneshot(
            Request::builder()
                .uri(format!("/api/findings/{finding_id}/triage"))
                .method("POST")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "triage_state": "wont_fix",
                        "triage_note": "accepted risk"
                    })
                    .to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("triage request");

    assert_eq!(response.status(), StatusCode::OK);
    let approval_state: String =
        sqlx::query_scalar("SELECT approval_state FROM items WHERE id = ?")
            .bind(item_id)
            .fetch_one(&db.pool)
            .await
            .expect("load approval state");
    assert_eq!(approval_state, "pending");
    let queued_review_jobs: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM jobs WHERE item_id = ? AND phase_kind = 'review' AND status = 'queued'",
    )
    .bind(item_id)
    .fetch_one(&db.pool)
    .await
    .expect("count queued review jobs");
    assert_eq!(queued_review_jobs, 0);
}

#[tokio::test]
async fn backlog_triage_rejects_self_linked_item() {
    let repo = temp_git_repo("ingot-http-api");
    let head = git_output(&repo, &["rev-parse", "HEAD"]);
    let db_path =
        std::env::temp_dir().join(format!("ingot-http-api-backlog-{}.db", Uuid::now_v7()));
    let db = Database::connect(&db_path).await.expect("connect db");
    db.migrate().await.expect("migrate db");

    let project_id = "prj_22222222222222222222222222222222";
    let item_id = "itm_22222222222222222222222222222222";
    let revision_id = "rev_22222222222222222222222222222222";
    let finding_id = "fnd_22222222222222222222222222222222";
    let job_id = "job_22222222222222222222222222222222";

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
    insert_test_job_row(
        &db,
        TestJobInsert {
            id: job_id,
            project_id,
            item_id,
            item_revision_id: revision_id,
            step_id: "review_candidate_initial",
            status: JobStatus::Completed,
            outcome_class: Some(OutcomeClass::Findings),
            phase_kind: PhaseKind::Review,
            workspace_kind: WorkspaceKind::Review,
            execution_permission: ExecutionPermission::MustNotMutate,
            context_policy: ContextPolicy::Fresh,
            phase_template_slug: "review-candidate",
            output_artifact_kind: OutputArtifactKind::ReviewReport,
            job_input: TestJobInput::CandidateSubject(&head, &head),
            created_at: "2026-03-12T00:00:00Z",
            ended_at: Some("2026-03-12T00:01:00Z"),
            ..TestJobInsert::new(
                job_id,
                project_id,
                item_id,
                revision_id,
                "review_candidate_initial",
            )
        },
    )
    .await;
    sqlx::query(
        "INSERT INTO findings (
            id, project_id, source_item_id, source_item_revision_id, source_job_id, source_step_id,
            source_report_schema_version, source_finding_key, source_subject_kind,
            source_subject_base_commit_oid, source_subject_head_commit_oid, code, severity, summary,
            paths, evidence, triage_state, linked_item_id, triage_note, created_at, triaged_at
         ) VALUES (?, ?, ?, ?, ?, 'review_candidate_initial', 'review_report:v1', 'finding-1', 'candidate', ?, ?, 'BUG001', 'high', 'summary', '[]', '[]', 'untriaged', NULL, NULL, ?, NULL)",
    )
    .bind(finding_id)
    .bind(project_id)
    .bind(item_id)
    .bind(revision_id)
    .bind(job_id)
    .bind(&head)
    .bind(&head)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert finding");

    let response = test_router(db.clone())
        .oneshot(
            Request::builder()
                .uri(format!("/api/findings/{finding_id}/triage"))
                .method("POST")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "triage_state": "backlog",
                        "linked_item_id": item_id
                    })
                    .to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("triage request");

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn retriaging_backlog_created_item_clears_origin_backlink() {
    let repo = temp_git_repo("ingot-http-api");
    let head = git_output(&repo, &["rev-parse", "HEAD"]);
    let db_path =
        std::env::temp_dir().join(format!("ingot-http-api-retriage-{}.db", Uuid::now_v7()));
    let db = Database::connect(&db_path).await.expect("connect db");
    db.migrate().await.expect("migrate db");

    let project_id = "prj_33333333333333333333333333333333";
    let item_id = "itm_33333333333333333333333333333333";
    let revision_id = "rev_33333333333333333333333333333333";
    let finding_id = "fnd_33333333333333333333333333333333";
    let job_id = "job_33333333333333333333333333333333";
    let linked_item_id = "itm_44444444444444444444444444444444";
    let linked_revision_id = "rev_44444444444444444444444444444444";

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
    insert_test_job_row(
        &db,
        TestJobInsert {
            id: job_id,
            project_id,
            item_id,
            item_revision_id: revision_id,
            step_id: "review_candidate_initial",
            status: JobStatus::Completed,
            outcome_class: Some(OutcomeClass::Findings),
            phase_kind: PhaseKind::Review,
            workspace_kind: WorkspaceKind::Review,
            execution_permission: ExecutionPermission::MustNotMutate,
            context_policy: ContextPolicy::Fresh,
            phase_template_slug: "review-candidate",
            output_artifact_kind: OutputArtifactKind::ReviewReport,
            job_input: TestJobInput::CandidateSubject(&head, &head),
            created_at: "2026-03-12T00:00:00Z",
            ended_at: Some("2026-03-12T00:01:00Z"),
            ..TestJobInsert::new(
                job_id,
                project_id,
                item_id,
                revision_id,
                "review_candidate_initial",
            )
        },
    )
    .await;
    sqlx::query(
        "INSERT INTO findings (
            id, project_id, source_item_id, source_item_revision_id, source_job_id, source_step_id,
            source_report_schema_version, source_finding_key, source_subject_kind,
            source_subject_base_commit_oid, source_subject_head_commit_oid, code, severity, summary,
            paths, evidence, triage_state, linked_item_id, triage_note, created_at, triaged_at
         ) VALUES (?, ?, ?, ?, ?, 'review_candidate_initial', 'review_report:v1', 'finding-1', 'candidate', ?, ?, 'BUG001', 'high', 'summary', '[]', '[]', 'untriaged', NULL, NULL, ?, NULL)",
    )
    .bind(finding_id)
    .bind(project_id)
    .bind(item_id)
    .bind(revision_id)
    .bind(job_id)
    .bind(&head)
    .bind(&head)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert finding");
    sqlx::query(
        "INSERT INTO items (
            id, project_id, classification, workflow_version, lifecycle_state, parking_state,
            approval_state, escalation_state, current_revision_id, origin_kind, origin_finding_id,
            priority, labels, created_at, updated_at
         ) VALUES (?, ?, 'bug', 'delivery:v1', 'open', 'active', 'not_requested', 'none', ?, 'promoted_finding', ?, 'major', '[]', ?, ?)",
    )
    .bind(linked_item_id)
    .bind(project_id)
    .bind(linked_revision_id)
    .bind(finding_id)
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert linked item");
    sqlx::query(
        "INSERT INTO item_revisions (
            id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
            approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid,
            seed_target_commit_oid, supersedes_revision_id, created_at
         ) VALUES (?, ?, 1, 'Bug', 'Desc', 'AC', 'refs/heads/main', 'required', '{}', '{}', ?, ?, NULL, ?)",
    )
    .bind(linked_revision_id)
    .bind(linked_item_id)
    .bind(&head)
    .bind(&head)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert linked revision");
    sqlx::query(
        "UPDATE findings
         SET triage_state = 'backlog', linked_item_id = ?, triaged_at = '2026-03-12T00:01:00Z'
         WHERE id = ?",
    )
    .bind(linked_item_id)
    .bind(finding_id)
    .execute(&db.pool)
    .await
    .expect("mark finding backlog");

    let response = test_router(db.clone())
        .oneshot(
            Request::builder()
                .uri(format!("/api/findings/{finding_id}/triage"))
                .method("POST")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "triage_state": "fix_now"
                    })
                    .to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("triage request");

    assert_eq!(response.status(), StatusCode::OK);
    let origin_kind: String = sqlx::query_scalar("SELECT origin_kind FROM items WHERE id = ?")
        .bind(linked_item_id)
        .fetch_one(&db.pool)
        .await
        .expect("load origin kind");
    let origin_finding_id: Option<String> =
        sqlx::query_scalar("SELECT origin_finding_id FROM items WHERE id = ?")
            .bind(linked_item_id)
            .fetch_one(&db.pool)
            .await
            .expect("load origin finding id");
    assert_eq!(origin_kind, "manual");
    assert_eq!(origin_finding_id, None);
}
