mod common;

use ingot_domain::commit_oid::CommitOid;
use ingot_domain::convergence::{Convergence, ConvergenceStatus};
use ingot_domain::finding::{
    EstimatedScope, InvestigationFindingMetadata, InvestigationPromotion, InvestigationScope,
};
use ingot_domain::ids::{ItemId, ItemRevisionId, ProjectId, WorkspaceId};
use ingot_domain::item::{ApprovalState, Classification, Item, Origin};
use ingot_domain::job::{
    ContextPolicy, ExecutionPermission, Job, JobInput, JobStatus, OutcomeClass, OutputArtifactKind,
    PhaseKind,
};
use ingot_domain::ports::{
    ConflictKind, JobCompletionMutation, PreparedConvergenceGuard, RepositoryError,
};
use ingot_domain::project::Project;
use ingot_domain::revision::ItemRevision;
use ingot_domain::test_support::{
    ConvergenceBuilder, FindingBuilder, ItemBuilder, JobBuilder, ProjectBuilder, RevisionBuilder,
    WorkspaceBuilder, default_timestamp, parse_timestamp,
};
use ingot_domain::workspace::{RetentionPolicy, Workspace, WorkspaceKind};
use ingot_store_sqlite::Database;
use ingot_test_support::reports::clean_validation_report;
use ingot_test_support::sqlite::PersistFixture;

async fn persist_project(db: &Database) -> Project {
    ProjectBuilder::new("/tmp/test")
        .name("Test")
        .build()
        .persist(db)
        .await
        .expect("create project")
}

async fn persist_item_with_revision(
    db: &Database,
    project_id: ProjectId,
    target_ref: &str,
) -> (Item, ItemRevision) {
    let item_id = ItemId::new();
    let mut revision = RevisionBuilder::new(item_id)
        .seed_commit_oid(Some("abc"))
        .seed_target_commit_oid(Some("def"))
        .build();
    revision.target_ref = target_ref.into();

    let item = ItemBuilder::new(project_id, revision.id)
        .id(item_id)
        .build();
    (item, revision)
        .persist(db)
        .await
        .expect("create item with revision")
}

async fn persist_promoted_finding_item(
    db: &Database,
    project_id: ProjectId,
    finding_id: ingot_domain::ids::FindingId,
) -> (Item, ItemRevision) {
    let item_id = ItemId::new();
    let revision = RevisionBuilder::new(item_id)
        .seed_commit_oid(Some("head"))
        .seed_target_commit_oid(Some("def"))
        .build();

    let mut item = ItemBuilder::new(project_id, revision.id)
        .id(item_id)
        .build();
    item.classification = Classification::Bug;
    item.origin = Origin::PromotedFinding { finding_id };

    (item, revision)
        .persist(db)
        .await
        .expect("create promoted item")
}

async fn persist_investigate_job(
    db: &Database,
    project_id: ProjectId,
    item_id: ItemId,
    revision_id: ItemRevisionId,
    status: JobStatus,
) -> Job {
    JobBuilder::new(project_id, item_id, revision_id, "investigate_item")
        .status(status)
        .phase_kind(PhaseKind::Investigate)
        .workspace_kind(WorkspaceKind::Review)
        .execution_permission(ExecutionPermission::MustNotMutate)
        .context_policy(ContextPolicy::Fresh)
        .phase_template_slug("investigate-item")
        .output_artifact_kind(OutputArtifactKind::FindingReport)
        .build()
        .persist(db)
        .await
        .expect("create investigate job")
}

async fn persist_integration_workspace(
    db: &Database,
    project_id: ProjectId,
    revision_id: ItemRevisionId,
) -> Workspace {
    let mut workspace = WorkspaceBuilder::new(project_id, WorkspaceKind::Integration)
        .created_for_revision_id(revision_id)
        .path("/tmp/workspace")
        .build();
    workspace.retention_policy = RetentionPolicy::Ephemeral;
    workspace.target_ref = None;
    workspace.workspace_ref = None;

    workspace
        .persist(db)
        .await
        .expect("create integration workspace")
}

async fn persist_validate_job(
    db: &Database,
    project_id: ProjectId,
    item_id: ItemId,
    revision_id: ItemRevisionId,
    expected_target_head_oid: &str,
    prepared_head_oid: &str,
) -> Job {
    JobBuilder::new(project_id, item_id, revision_id, "validate_integrated")
        .status(JobStatus::Running)
        .phase_kind(PhaseKind::Validate)
        .workspace_kind(WorkspaceKind::Integration)
        .execution_permission(ExecutionPermission::MustNotMutate)
        .context_policy(ContextPolicy::ResumeContext)
        .phase_template_slug("validate-integrated")
        .output_artifact_kind(OutputArtifactKind::ValidationReport)
        .job_input(JobInput::integrated_subject(
            expected_target_head_oid.into(),
            prepared_head_oid.into(),
        ))
        .build()
        .persist(db)
        .await
        .expect("create validate job")
}

#[allow(clippy::too_many_arguments)]
async fn persist_prepared_convergence(
    db: &Database,
    project_id: ProjectId,
    item_id: ItemId,
    revision_id: ItemRevisionId,
    source_workspace_id: WorkspaceId,
    integration_workspace_id: WorkspaceId,
    expected_target_head_oid: &str,
    prepared_head_oid: &str,
) -> Convergence {
    ConvergenceBuilder::new(project_id, item_id, revision_id)
        .source_workspace_id(source_workspace_id)
        .integration_workspace_id(integration_workspace_id)
        .source_head_commit_oid(prepared_head_oid)
        .status(ConvergenceStatus::Prepared)
        .input_target_commit_oid(expected_target_head_oid)
        .prepared_commit_oid(prepared_head_oid)
        .build()
        .persist(db)
        .await
        .expect("create convergence")
}

#[tokio::test]
async fn migrate_supports_finding_promotion_relationships() {
    let (db, path) = common::migrated_test_db_with_path("ingot-store").await;
    let raw_pool = common::raw_sqlite_pool(&path).await;
    let project = persist_project(&db).await;
    let (source_item, source_revision) =
        persist_item_with_revision(&db, project.id, "refs/heads/main").await;
    let job = persist_investigate_job(
        &db,
        project.id,
        source_item.id,
        source_revision.id,
        JobStatus::Completed,
    )
    .await;

    let mut finding = FindingBuilder::new(project.id, source_item.id, source_revision.id, job.id)
        .source_step_id("investigate_item")
        .source_report_schema_version("finding_report:v1")
        .source_finding_key("finding-1")
        .source_subject_base_commit_oid(None::<String>)
        .source_subject_head_commit_oid("head")
        .summary("Summary")
        .paths(Vec::new())
        .evidence(serde_json::json!([]))
        .build()
        .persist(&db)
        .await
        .expect("create finding");

    let (promoted_item, _) = persist_promoted_finding_item(&db, project.id, finding.id).await;

    finding.triage = ingot_domain::finding::FindingTriage::Backlog {
        linked_item_id: promoted_item.id,
        triage_note: None,
        triaged_at: default_timestamp(),
    };
    db.triage_finding(&finding).await.expect("promote finding");

    let fk_violations: Vec<(String, String, String, i64)> =
        sqlx::query_as("PRAGMA foreign_key_check")
            .fetch_all(&raw_pool)
            .await
            .expect("foreign key check");
    let completed = db
        .load_completed_job_completion(job.id)
        .await
        .expect("load completed job completion")
        .expect("completed job completion should exist");

    assert!(
        fk_violations.is_empty(),
        "foreign key violations: {fk_violations:?}"
    );
    assert_eq!(completed.job.state.status(), JobStatus::Completed);
    assert_eq!(completed.finding_count, 1);
}

#[tokio::test]
async fn finding_round_trip_preserves_investigation_metadata() {
    let db = common::migrated_test_db("ingot-store-investigation-finding").await;
    let project = persist_project(&db).await;
    let (item, revision) = persist_item_with_revision(&db, project.id, "refs/heads/main").await;
    let job =
        persist_investigate_job(&db, project.id, item.id, revision.id, JobStatus::Completed).await;

    let finding = FindingBuilder::new(project.id, item.id, revision.id, job.id)
        .source_step_id("investigate_item")
        .source_report_schema_version("investigation_report:v1")
        .investigation(InvestigationFindingMetadata {
            scope: InvestigationScope {
                description: "Scanned all crates for duplicate helpers".into(),
                paths_examined: vec!["crates/".into()],
                methodology: "AST comparison".into(),
            },
            promotion: InvestigationPromotion {
                title: "Extract shared temp_git_repo helper".into(),
                description: "Move the helper into shared test support".into(),
                acceptance_criteria: "Only one helper remains".into(),
                classification: Classification::Change,
                estimated_scope: EstimatedScope::Small,
            },
            group_key: Some("helper-dedup".into()),
        })
        .build();

    db.create_finding(&finding).await.expect("create finding");

    let reloaded = db.get_finding(finding.id).await.expect("reload finding");
    let metadata = reloaded
        .investigation
        .expect("investigation metadata should persist");

    assert_eq!(metadata.scope.methodology, "AST comparison");
    assert_eq!(metadata.scope.paths_examined, vec!["crates/".to_string()]);
    assert_eq!(metadata.group_key.as_deref(), Some("helper-dedup"));
    assert_eq!(metadata.promotion.classification, Classification::Change);
    assert_eq!(metadata.promotion.estimated_scope, EstimatedScope::Small);
}

#[tokio::test]
async fn complete_job_rejects_duplicate_terminal_updates() {
    let db = common::migrated_test_db("ingot-store").await;
    let project = persist_project(&db).await;
    let (item, revision) = persist_item_with_revision(&db, project.id, "refs/heads/main").await;
    let job =
        persist_investigate_job(&db, project.id, item.id, revision.id, JobStatus::Running).await;

    db.apply_job_completion(JobCompletionMutation {
        job_id: job.id,
        item_id: item.id,
        expected_item_revision_id: revision.id,
        outcome_class: OutcomeClass::Clean,
        clear_item_escalation: false,
        result_schema_version: Some("finding_report:v1".into()),
        result_payload: Some(serde_json::json!({
            "outcome": "clean",
            "summary": "ok",
            "findings": []
        })),
        output_commit_oid: None,
        findings: vec![],
        prepared_convergence_guard: None,
    })
    .await
    .expect("first completion succeeds");

    let error = db
        .apply_job_completion(JobCompletionMutation {
            job_id: job.id,
            item_id: item.id,
            expected_item_revision_id: revision.id,
            outcome_class: OutcomeClass::Clean,
            clear_item_escalation: false,
            result_schema_version: Some("finding_report:v1".into()),
            result_payload: Some(serde_json::json!({
                "outcome": "clean",
                "summary": "ok",
                "findings": []
            })),
            output_commit_oid: None,
            findings: vec![],
            prepared_convergence_guard: None,
        })
        .await
        .expect_err("duplicate completion should fail");

    assert!(matches!(error, RepositoryError::Conflict(_)));
}

#[tokio::test]
async fn complete_job_rolls_back_when_prepared_convergence_becomes_stale() {
    let db = common::migrated_test_db("ingot-store").await;
    let project = persist_project(&db).await;
    let (item, revision) = persist_item_with_revision(&db, project.id, "refs/heads/main").await;
    let workspace = persist_integration_workspace(&db, project.id, revision.id).await;
    let job = persist_validate_job(
        &db,
        project.id,
        item.id,
        revision.id,
        "expected-target",
        "prepared-head",
    )
    .await;
    let convergence = persist_prepared_convergence(
        &db,
        project.id,
        item.id,
        revision.id,
        workspace.id,
        workspace.id,
        "expected-target",
        "prepared-head",
    )
    .await;

    let error = db
        .apply_job_completion(JobCompletionMutation {
            job_id: job.id,
            item_id: item.id,
            expected_item_revision_id: revision.id,
            outcome_class: OutcomeClass::Clean,
            clear_item_escalation: false,
            result_schema_version: Some("validation_report:v1".into()),
            result_payload: Some(clean_validation_report("ok")),
            output_commit_oid: None,
            findings: vec![],
            prepared_convergence_guard: Some(PreparedConvergenceGuard {
                convergence_id: convergence.id,
                item_revision_id: revision.id,
                target_ref: "refs/heads/main".into(),
                expected_target_head_oid: CommitOid::new("moved-target"),
                next_approval_state: Some(ApprovalState::Pending),
            }),
        })
        .await
        .expect_err("stale prepared convergence should fail");

    assert!(matches!(
        error,
        RepositoryError::Conflict(ConflictKind::PreparedConvergenceStale)
    ));

    let persisted_job = db.get_job(job.id).await.expect("load job after rollback");
    let persisted_item = db
        .get_item(item.id)
        .await
        .expect("load item after rollback");

    assert_eq!(persisted_job.state.status(), JobStatus::Running);
    assert_eq!(persisted_item.approval_state, ApprovalState::NotRequested);
}

#[tokio::test]
async fn complete_job_rolls_back_when_prepared_convergence_target_ref_changes() {
    let db = common::migrated_test_db("ingot-store").await;
    let project = persist_project(&db).await;
    let (item, revision) = persist_item_with_revision(&db, project.id, "refs/heads/release").await;
    let workspace = persist_integration_workspace(&db, project.id, revision.id).await;
    let job = persist_validate_job(
        &db,
        project.id,
        item.id,
        revision.id,
        "expected-target",
        "prepared-head",
    )
    .await;
    let convergence = persist_prepared_convergence(
        &db,
        project.id,
        item.id,
        revision.id,
        workspace.id,
        workspace.id,
        "expected-target",
        "prepared-head",
    )
    .await;

    let error = db
        .apply_job_completion(JobCompletionMutation {
            job_id: job.id,
            item_id: item.id,
            expected_item_revision_id: revision.id,
            outcome_class: OutcomeClass::Clean,
            clear_item_escalation: false,
            result_schema_version: Some("validation_report:v1".into()),
            result_payload: Some(clean_validation_report("ok")),
            output_commit_oid: None,
            findings: vec![],
            prepared_convergence_guard: Some(PreparedConvergenceGuard {
                convergence_id: convergence.id,
                item_revision_id: revision.id,
                target_ref: "refs/heads/release".into(),
                expected_target_head_oid: CommitOid::new("expected-target"),
                next_approval_state: Some(ApprovalState::Pending),
            }),
        })
        .await
        .expect_err("mismatched target_ref should fail");

    assert!(matches!(
        error,
        RepositoryError::Conflict(ConflictKind::PreparedConvergenceStale)
    ));

    let persisted_job = db.get_job(job.id).await.expect("load job after rollback");
    let persisted_item = db
        .get_item(item.id)
        .await
        .expect("load item after rollback");

    assert_eq!(persisted_job.state.status(), JobStatus::Running);
    assert_eq!(persisted_item.approval_state, ApprovalState::NotRequested);
}

#[tokio::test]
async fn complete_job_rolls_back_when_item_revision_changes_before_commit() {
    let db = common::migrated_test_db("ingot-store").await;
    let project = persist_project(&db).await;
    let item_id = ItemId::new();
    let revision = RevisionBuilder::new(item_id)
        .seed_commit_oid(Some("abc"))
        .seed_target_commit_oid(Some("def"))
        .build();
    let mut next_revision = RevisionBuilder::new(item_id)
        .id(ItemRevisionId::new())
        .revision_no(2)
        .seed_commit_oid(Some("ghi"))
        .seed_target_commit_oid(Some("jkl"))
        .created_at(parse_timestamp("2026-03-13T00:00:00Z"))
        .build();
    next_revision.supersedes_revision_id = Some(revision.id);

    let item = ItemBuilder::new(project.id, next_revision.id)
        .id(item_id)
        .build();
    let (item, revision) = (item, revision)
        .persist(&db)
        .await
        .expect("create item with source revision");
    let next_revision = next_revision
        .persist(&db)
        .await
        .expect("create next revision");

    let job =
        persist_investigate_job(&db, project.id, item.id, revision.id, JobStatus::Running).await;

    let error = db
        .apply_job_completion(JobCompletionMutation {
            job_id: job.id,
            item_id: item.id,
            expected_item_revision_id: revision.id,
            outcome_class: OutcomeClass::Clean,
            clear_item_escalation: false,
            result_schema_version: Some("finding_report:v1".into()),
            result_payload: Some(serde_json::json!({
                "outcome": "clean",
                "summary": "ok",
                "findings": []
            })),
            output_commit_oid: None,
            findings: vec![],
            prepared_convergence_guard: None,
        })
        .await
        .expect_err("revision drift should fail");

    assert!(matches!(
        error,
        RepositoryError::Conflict(ConflictKind::JobRevisionStale)
    ));

    let persisted_job = db.get_job(job.id).await.expect("load job after rollback");
    let persisted_item = db
        .get_item(item.id)
        .await
        .expect("load item after rollback");

    assert_eq!(persisted_job.state.status(), JobStatus::Running);
    assert_eq!(persisted_item.current_revision_id, next_revision.id);
}
