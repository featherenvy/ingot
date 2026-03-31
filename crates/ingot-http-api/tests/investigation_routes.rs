use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use ingot_domain::finding::FindingSubjectKind;
use ingot_domain::item::{Classification, WorkflowVersion};
use ingot_domain::job::{
    ContextPolicy, ExecutionPermission, JobStatus, OutcomeClass, OutputArtifactKind, PhaseKind,
};
use ingot_domain::revision::AuthoringBaseSeed;
use ingot_domain::workspace::WorkspaceKind;
use tower::ServiceExt;

mod common;
use common::*;

#[tokio::test]
async fn create_investigation_item() {
    let repo = temp_git_repo("investigation");
    let db = migrated_test_db("investigation-create").await;

    let project_id = "prj_aa000000000000000000000000000011";
    persist_test_project(&db, &repo, project_id).await;

    let app = test_router(db.clone());

    // Create investigation item.
    let item_response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/projects/{project_id}/items"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "title": "Find helper duplication",
                        "description": "Investigate each crate for duplicate helpers",
                        "acceptance_criteria": "Produce findings",
                        "classification": "investigation",
                        "target_ref": "refs/heads/main"
                    })
                    .to_string(),
                ))
                .expect("build item request"),
        )
        .await
        .expect("item response");

    let status = item_response.status();
    let item_body = to_bytes(item_response.into_body(), usize::MAX)
        .await
        .expect("read item body");
    if status != StatusCode::CREATED {
        let body_str = String::from_utf8_lossy(&item_body);
        panic!("expected 201, got {status}: {body_str}");
    }
    let json: serde_json::Value = serde_json::from_slice(&item_body).expect("item json");

    assert_eq!(
        json["item"]["classification"].as_str(),
        Some("investigation"),
        "classification should be investigation"
    );
    assert_eq!(
        json["item"]["workflow_version"].as_str(),
        Some("investigation:v1"),
        "workflow_version should be investigation:v1"
    );
    // The evaluator's `finish()` overrides board_status to WORKING when
    // dispatchable_step_id is present (i.e., a job is ready to dispatch).
    assert_eq!(
        json["evaluation"]["board_status"].as_str(),
        Some("WORKING"),
        "new investigation item with dispatchable step should be in WORKING"
    );
    assert_eq!(
        json["evaluation"]["phase_status"].as_str(),
        Some("new"),
        "phase_status should be new"
    );
    assert_eq!(
        json["evaluation"]["dispatchable_step_id"].as_str(),
        Some("investigate_project"),
        "dispatchable_step_id should be investigate_project"
    );
}

#[tokio::test]
async fn batch_promote_findings() {
    let repo = temp_git_repo("investigation");
    let head = git_output(&repo, &["rev-parse", "HEAD"]);
    let db = migrated_test_db("investigation-batch-promote").await;

    let project_id = "prj_aa000000000000000000000000000001";
    let item_id = "itm_aa000000000000000000000000000001";
    let revision_id = "rev_aa000000000000000000000000000001";
    let job_id = "job_aa000000000000000000000000000001";
    let finding_id_1 = "fnd_aa000000000000000000000000000001";
    let finding_id_2 = "fnd_aa000000000000000000000000000002";

    // Create project.
    persist_test_project(&db, &repo, project_id).await;

    // Create investigation item with correct classification and workflow version.
    let mut item = test_item_builder(project_id, revision_id, item_id).build();
    item.classification = Classification::Investigation;
    item.workflow_version = WorkflowVersion::InvestigationV1;
    let revision = test_revision_builder(item_id, revision_id)
        .seed(AuthoringBaseSeed::Explicit {
            seed_commit_oid: head.clone().into(),
            seed_target_commit_oid: head.clone().into(),
        })
        .build();
    (item, revision)
        .persist(&db)
        .await
        .expect("persist investigation item");

    // Build an investigation_report:v1 payload with promotion metadata.
    let investigation_report = serde_json::json!({
        "outcome": "findings",
        "summary": "Found duplicate helpers across crates",
        "scope": {
            "description": "Scanned all crates for duplicate test helpers",
            "paths_examined": ["crates/"],
            "methodology": "AST comparison"
        },
        "findings": [
            {
                "finding_key": "dup-helper-1",
                "code": "DUP001",
                "severity": "high",
                "summary": "temp_git_repo duplicated in 3 crates",
                "paths": ["crates/a/src/test.rs", "crates/b/src/test.rs"],
                "evidence": ["identical function body"],
                "promotion": {
                    "title": "Extract shared temp_git_repo helper",
                    "description": "Move the duplicated temp_git_repo helper into a shared crate",
                    "acceptance_criteria": "Single definition used by all three crates",
                    "classification": "change",
                    "estimated_scope": "small"
                },
                "group_key": "helper-dedup"
            },
            {
                "finding_key": "dup-helper-2",
                "code": "DUP002",
                "severity": "medium",
                "summary": "migrated_test_db duplicated in 2 crates",
                "paths": ["crates/c/src/test.rs", "crates/d/src/test.rs"],
                "evidence": ["near-identical function body"],
                "promotion": {
                    "title": "Extract shared migrated_test_db helper",
                    "description": "Move the duplicated migrated_test_db helper into a shared crate",
                    "acceptance_criteria": "Single definition used by both crates",
                    "classification": "bug",
                    "estimated_scope": "medium"
                },
                "group_key": "helper-dedup"
            }
        ]
    });

    // Insert a completed investigation job with the report payload.
    insert_test_job_row(
        &db,
        TestJobInsert {
            id: job_id,
            project_id,
            item_id,
            item_revision_id: revision_id,
            step_id: "investigate_project",
            status: JobStatus::Completed,
            outcome_class: Some(OutcomeClass::Findings),
            phase_kind: PhaseKind::Investigate,
            workspace_kind: WorkspaceKind::Review,
            execution_permission: ExecutionPermission::MustNotMutate,
            context_policy: ContextPolicy::Fresh,
            phase_template_slug: "investigate-project",
            output_artifact_kind: OutputArtifactKind::InvestigationReport,
            job_input: TestJobInput::AuthoringHead(&head),
            result_schema_version: Some("investigation_report:v1"),
            result_payload: Some(investigation_report),
            created_at: "2026-03-12T00:00:00Z",
            ended_at: Some("2026-03-12T00:01:00Z"),
            ..TestJobInsert::new(
                job_id,
                project_id,
                item_id,
                revision_id,
                "investigate_project",
            )
        },
    )
    .await;

    // Insert two findings linked to that job.
    persist_test_finding(
        &db,
        project_id,
        item_id,
        revision_id,
        job_id,
        finding_id_1,
        |f| {
            f.source_step_id("investigate_project")
                .source_report_schema_version("investigation_report:v1")
                .source_finding_key("dup-helper-1")
                .source_subject_kind(FindingSubjectKind::Candidate)
                .source_subject_base_commit_oid(Option::<&str>::None)
                .source_subject_head_commit_oid(&head)
                .code("DUP001")
                .summary("temp_git_repo duplicated in 3 crates")
                .paths(vec![
                    "crates/a/src/test.rs".into(),
                    "crates/b/src/test.rs".into(),
                ])
                .evidence(serde_json::json!(["identical function body"]))
        },
    )
    .await;

    persist_test_finding(
        &db,
        project_id,
        item_id,
        revision_id,
        job_id,
        finding_id_2,
        |f| {
            f.source_step_id("investigate_project")
                .source_report_schema_version("investigation_report:v1")
                .source_finding_key("dup-helper-2")
                .source_subject_kind(FindingSubjectKind::Candidate)
                .source_subject_base_commit_oid(Option::<&str>::None)
                .source_subject_head_commit_oid(&head)
                .code("DUP002")
                .summary("migrated_test_db duplicated in 2 crates")
                .paths(vec![
                    "crates/c/src/test.rs".into(),
                    "crates/d/src/test.rs".into(),
                ])
                .evidence(serde_json::json!(["near-identical function body"]))
        },
    )
    .await;

    // Batch promote both findings.
    let response = test_router(db.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/projects/{project_id}/findings/batch-promote"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "finding_ids": [finding_id_1, finding_id_2]
                    })
                    .to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("batch promote response");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");

    // Both findings should be promoted.
    let promoted = json["promoted"].as_array().expect("promoted array");
    assert_eq!(promoted.len(), 2, "both findings should be promoted");
    let skipped = json["skipped"].as_array().expect("skipped array");
    assert!(skipped.is_empty(), "no findings should be skipped");

    // Verify the first promoted item inherits promotion metadata.
    let first = &promoted[0];
    assert_eq!(
        first["item"]["classification"].as_str(),
        Some("change"),
        "first promoted item should inherit classification from promotion"
    );
    assert_eq!(
        first["item"]["workflow_version"].as_str(),
        Some("delivery:v1"),
        "promoted items must use delivery:v1 workflow"
    );
    assert_eq!(
        first["current_revision"]["title"].as_str(),
        Some("Extract shared temp_git_repo helper"),
        "first promoted item title should come from promotion metadata"
    );

    // Verify the second promoted item.
    let second = &promoted[1];
    assert_eq!(
        second["item"]["classification"].as_str(),
        Some("bug"),
        "second promoted item should inherit classification from promotion"
    );
    assert_eq!(
        second["item"]["workflow_version"].as_str(),
        Some("delivery:v1"),
        "promoted items must use delivery:v1 workflow"
    );
    assert_eq!(
        second["current_revision"]["title"].as_str(),
        Some("Extract shared migrated_test_db helper"),
        "second promoted item title should come from promotion metadata"
    );
}

#[tokio::test]
async fn generic_backlog_triage_preserves_investigation_promotion_metadata() {
    let repo = temp_git_repo("investigation");
    let head = git_output(&repo, &["rev-parse", "HEAD"]);
    let db = migrated_test_db("investigation-single-promote").await;

    let project_id = "prj_aa000000000000000000000000000021";
    let item_id = "itm_aa000000000000000000000000000021";
    let revision_id = "rev_aa000000000000000000000000000021";
    let job_id = "job_aa000000000000000000000000000021";
    let finding_id = "fnd_aa000000000000000000000000000021";

    persist_test_project(&db, &repo, project_id).await;

    let mut item = test_item_builder(project_id, revision_id, item_id).build();
    item.classification = Classification::Investigation;
    item.workflow_version = WorkflowVersion::InvestigationV1;
    let revision = test_revision_builder(item_id, revision_id)
        .seed(AuthoringBaseSeed::Explicit {
            seed_commit_oid: head.clone().into(),
            seed_target_commit_oid: head.clone().into(),
        })
        .build();
    (item, revision)
        .persist(&db)
        .await
        .expect("persist investigation item");

    let investigation_report = serde_json::json!({
        "outcome": "findings",
        "summary": "Found duplicate helpers across crates",
        "scope": {
            "description": "Scanned all crates for duplicate test helpers",
            "paths_examined": ["crates/"],
            "methodology": "AST comparison"
        },
        "findings": [
            {
                "finding_key": "dup-helper-1",
                "code": "DUP001",
                "severity": "high",
                "summary": "temp_git_repo duplicated in 3 crates",
                "paths": ["crates/a/src/test.rs", "crates/b/src/test.rs"],
                "evidence": ["identical function body"],
                "promotion": {
                    "title": "Extract shared temp_git_repo helper",
                    "description": "Move the duplicated temp_git_repo helper into a shared crate",
                    "acceptance_criteria": "Single definition used by all three crates",
                    "classification": "change",
                    "estimated_scope": "small"
                },
                "group_key": "helper-dedup"
            }
        ]
    });

    insert_test_job_row(
        &db,
        TestJobInsert {
            id: job_id,
            project_id,
            item_id,
            item_revision_id: revision_id,
            step_id: "investigate_project",
            status: JobStatus::Completed,
            outcome_class: Some(OutcomeClass::Findings),
            phase_kind: PhaseKind::Investigate,
            workspace_kind: WorkspaceKind::Review,
            execution_permission: ExecutionPermission::MustNotMutate,
            context_policy: ContextPolicy::Fresh,
            phase_template_slug: "investigate-project",
            output_artifact_kind: OutputArtifactKind::InvestigationReport,
            job_input: TestJobInput::AuthoringHead(&head),
            result_schema_version: Some("investigation_report:v1"),
            result_payload: Some(investigation_report),
            created_at: "2026-03-12T00:00:00Z",
            ended_at: Some("2026-03-12T00:01:00Z"),
            ..TestJobInsert::new(
                job_id,
                project_id,
                item_id,
                revision_id,
                "investigate_project",
            )
        },
    )
    .await;

    persist_test_finding(
        &db,
        project_id,
        item_id,
        revision_id,
        job_id,
        finding_id,
        |f| {
            f.source_step_id("investigate_project")
                .source_report_schema_version("investigation_report:v1")
                .source_finding_key("dup-helper-1")
                .source_subject_kind(FindingSubjectKind::Candidate)
                .source_subject_base_commit_oid(Option::<&str>::None)
                .source_subject_head_commit_oid(&head)
                .code("DUP001")
                .summary("temp_git_repo duplicated in 3 crates")
                .paths(vec![
                    "crates/a/src/test.rs".into(),
                    "crates/b/src/test.rs".into(),
                ])
                .evidence(serde_json::json!(["identical function body"]))
        },
    )
    .await;

    let response = test_router(db.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/findings/{finding_id}/triage"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "triage_state": "backlog"
                    })
                    .to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("triage response");

    assert_eq!(response.status(), StatusCode::OK);

    let promoted_item: (String, String, String) = sqlx::query_as(
        "SELECT classification, workflow_version, current_revision_id FROM items WHERE origin_finding_id = ?",
    )
    .bind(finding_id)
    .fetch_one(db.raw_pool())
    .await
    .expect("load promoted item");

    assert_eq!(
        promoted_item.0, "change",
        "generic triage should preserve promotion classification"
    );
    assert_eq!(
        promoted_item.1, "delivery:v1",
        "generic triage should promote into the delivery workflow"
    );

    let promoted_revision: (String, String, String) = sqlx::query_as(
        "SELECT title, description, acceptance_criteria FROM item_revisions WHERE id = ?",
    )
    .bind(&promoted_item.2)
    .fetch_one(db.raw_pool())
    .await
    .expect("load promoted revision");

    assert_eq!(
        promoted_revision.0, "Extract shared temp_git_repo helper",
        "generic triage should preserve the promoted title"
    );
    assert_eq!(
        promoted_revision.1, "Move the duplicated temp_git_repo helper into a shared crate",
        "generic triage should preserve the promoted description"
    );
    assert_eq!(
        promoted_revision.2, "Single definition used by all three crates",
        "generic triage should preserve the promoted acceptance criteria"
    );
}
