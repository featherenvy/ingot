use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use ingot_domain::finding::{FindingSubjectKind, FindingTriageState};
use ingot_domain::item::{Classification, Origin};
use ingot_domain::job::{
    ContextPolicy, ExecutionPermission, JobStatus, OutcomeClass, OutputArtifactKind, PhaseKind,
};
use ingot_domain::revision::AuthoringBaseSeed;
use ingot_domain::workspace::RetentionPolicy;
use ingot_domain::workspace::WorkspaceKind;
use tower::ServiceExt;

mod common;
use common::*;

#[tokio::test]
async fn triaging_final_integrated_finding_enters_pending_approval() {
    let repo = temp_git_repo("ingot-http-api");
    let head = git_output(&repo, &["rev-parse", "HEAD"]);
    let db = migrated_test_db("ingot-http-api-triage").await;

    let project_id = "prj_11111111111111111111111111111111";
    let item_id = "itm_11111111111111111111111111111111";
    let revision_id = "rev_11111111111111111111111111111111";
    let job_id = "job_11111111111111111111111111111111";
    let convergence_id = "conv_11111111111111111111111111111111";
    let source_workspace_id = "wrk_11111111111111111111111111111111";
    let integration_workspace_id = "wrk_11111111111111111111111111111112";
    let finding_id = "fnd_11111111111111111111111111111111";

    test_project_builder(&repo, project_id)
        .name("Test")
        .build()
        .persist(&db)
        .await
        .expect("insert project");
    let item = test_item_builder(project_id, revision_id, item_id).build();
    let revision = test_revision_builder(item_id, revision_id)
        .seed(AuthoringBaseSeed::Explicit {
            seed_commit_oid: head.clone().into(),
            seed_target_commit_oid: head.clone().into(),
        })
        .build();
    (item, revision).persist(&db).await.expect("insert item");
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
    test_workspace_builder(project_id, WorkspaceKind::Authoring, source_workspace_id)
        .created_for_revision_id(parse_id(revision_id))
        .path(repo.join("workspace").display().to_string())
        .retention_policy(RetentionPolicy::Ephemeral)
        .base_commit_oid(&head)
        .head_commit_oid(&head)
        .build()
        .persist(&db)
        .await
        .expect("insert workspace");
    test_workspace_builder(
        project_id,
        WorkspaceKind::Integration,
        integration_workspace_id,
    )
    .created_for_revision_id(parse_id(revision_id))
    .path(repo.join("integration-workspace").display().to_string())
    .retention_policy(RetentionPolicy::Ephemeral)
    .base_commit_oid(&head)
    .head_commit_oid(&head)
    .build()
    .persist(&db)
    .await
    .expect("insert integration workspace");
    test_convergence_builder(project_id, item_id, revision_id, convergence_id)
        .source_workspace_id(parse_id(source_workspace_id))
        .integration_workspace_id(parse_id(integration_workspace_id))
        .source_head_commit_oid(&head)
        .input_target_commit_oid(&head)
        .prepared_commit_oid(&head)
        .build()
        .persist(&db)
        .await
        .expect("insert convergence");
    test_finding_builder(project_id, item_id, revision_id, job_id, finding_id)
        .source_step_id("validate_integrated")
        .source_report_schema_version("validation_report:v1")
        .source_finding_key("finding-1")
        .source_subject_kind(FindingSubjectKind::Integrated)
        .source_subject_base_commit_oid(Some(&head))
        .source_subject_head_commit_oid(&head)
        .paths(vec![])
        .evidence(serde_json::json!([]))
        .build()
        .persist(&db)
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
            .fetch_one(db.raw_pool())
            .await
            .expect("load approval state");
    assert_eq!(approval_state, "pending");
    let queued_review_jobs: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM jobs WHERE item_id = ? AND phase_kind = 'review' AND status = 'queued'",
    )
    .bind(item_id)
    .fetch_one(db.raw_pool())
    .await
    .expect("count queued review jobs");
    assert_eq!(queued_review_jobs, 0);
}

#[tokio::test]
async fn backlog_triage_rejects_self_linked_item() {
    let repo = temp_git_repo("ingot-http-api");
    let head = git_output(&repo, &["rev-parse", "HEAD"]);
    let db = migrated_test_db("ingot-http-api-backlog").await;

    let project_id = "prj_22222222222222222222222222222222";
    let item_id = "itm_22222222222222222222222222222222";
    let revision_id = "rev_22222222222222222222222222222222";
    let finding_id = "fnd_22222222222222222222222222222222";
    let job_id = "job_22222222222222222222222222222222";

    test_project_builder(&repo, project_id)
        .name("Test")
        .build()
        .persist(&db)
        .await
        .expect("insert project");
    let item = test_item_builder(project_id, revision_id, item_id).build();
    let revision = test_revision_builder(item_id, revision_id)
        .seed(AuthoringBaseSeed::Explicit {
            seed_commit_oid: head.clone().into(),
            seed_target_commit_oid: head.clone().into(),
        })
        .build();
    (item, revision).persist(&db).await.expect("insert item");
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
    test_finding_builder(project_id, item_id, revision_id, job_id, finding_id)
        .source_finding_key("finding-1")
        .source_subject_base_commit_oid(Some(&head))
        .source_subject_head_commit_oid(&head)
        .paths(vec![])
        .evidence(serde_json::json!([]))
        .build()
        .persist(&db)
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
async fn triage_rejects_invalid_linked_item_id_with_invalid_id_error() {
    let db = migrated_test_db("ingot-http-api-triage-invalid-linked-item-id").await;
    let finding_id = "fnd_99999999999999999999999999999999";

    let response = test_router(db)
        .oneshot(
            Request::builder()
                .uri(format!("/api/findings/{finding_id}/triage"))
                .method("POST")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "triage_state": "duplicate",
                        "linked_item_id": "not-an-item-id"
                    })
                    .to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("triage request");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read response body");
    let body: serde_json::Value = serde_json::from_slice(&body).expect("json body");
    assert_eq!(body["error"]["code"], "invalid_id");
    assert_eq!(
        body["error"]["message"],
        "Invalid linked_item id: not-an-item-id"
    );
}

#[tokio::test]
async fn retriaging_backlog_created_item_clears_origin_backlink() {
    let repo = temp_git_repo("ingot-http-api");
    let head = git_output(&repo, &["rev-parse", "HEAD"]);
    let db = migrated_test_db("ingot-http-api-retriage").await;

    let project_id = "prj_33333333333333333333333333333333";
    let item_id = "itm_33333333333333333333333333333333";
    let revision_id = "rev_33333333333333333333333333333333";
    let finding_id = "fnd_33333333333333333333333333333333";
    let job_id = "job_33333333333333333333333333333333";
    let linked_item_id = "itm_44444444444444444444444444444444";
    let linked_revision_id = "rev_44444444444444444444444444444444";

    test_project_builder(&repo, project_id)
        .name("Test")
        .build()
        .persist(&db)
        .await
        .expect("insert project");
    let item = test_item_builder(project_id, revision_id, item_id).build();
    let revision = test_revision_builder(item_id, revision_id)
        .seed(AuthoringBaseSeed::Explicit {
            seed_commit_oid: head.clone().into(),
            seed_target_commit_oid: head.clone().into(),
        })
        .build();
    (item, revision).persist(&db).await.expect("insert item");
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
    let mut linked_item = test_item_builder(project_id, linked_revision_id, linked_item_id).build();
    linked_item.classification = Classification::Bug;
    let linked_revision = test_revision_builder(linked_item_id, linked_revision_id)
        .seed(AuthoringBaseSeed::Explicit {
            seed_commit_oid: head.clone().into(),
            seed_target_commit_oid: head.clone().into(),
        })
        .build();
    let (mut linked_item, _) = (linked_item, linked_revision)
        .persist(&db)
        .await
        .expect("insert linked revision");
    test_finding_builder(project_id, item_id, revision_id, job_id, finding_id)
        .source_finding_key("finding-1")
        .source_subject_base_commit_oid(Some(&head))
        .source_subject_head_commit_oid(&head)
        .paths(vec![])
        .evidence(serde_json::json!([]))
        .triage_state(FindingTriageState::Backlog)
        .linked_item_id(parse_id(linked_item_id))
        .triaged_at(parse_timestamp("2026-03-12T00:01:00Z"))
        .build()
        .persist(&db)
        .await
        .expect("insert finding");
    linked_item.origin = Origin::PromotedFinding {
        finding_id: parse_id(finding_id),
    };
    db.update_item(&linked_item)
        .await
        .expect("mark linked item as promoted");

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
        .fetch_one(db.raw_pool())
        .await
        .expect("load origin kind");
    let origin_finding_id: Option<String> =
        sqlx::query_scalar("SELECT origin_finding_id FROM items WHERE id = ?")
            .bind(linked_item_id)
            .fetch_one(db.raw_pool())
            .await
            .expect("load origin finding id");
    assert_eq!(origin_kind, "manual");
    assert_eq!(origin_finding_id, None);
}
