use chrono::Utc;
use ingot_domain::finding::{FindingSubjectKind, FindingTriage, FindingTriageState};
use ingot_domain::ids::{ItemId, ItemRevisionId, JobId, ProjectId};
use ingot_domain::job::{Job, JobInput, JobStatus, OutcomeClass, OutputArtifactKind, PhaseKind};
use ingot_domain::step_id::StepId;
use ingot_test_support::fixtures::{
    ConvergenceBuilder, FindingBuilder, JobBuilder, nil_item, nil_revision,
};
use uuid::Uuid;

use crate::UseCaseError;

use super::{
    BacklogFindingOverrides, TriageFindingInput, auto_triage_findings, backlog_finding,
    execute_auto_triage, extract_findings, parse_revision_context_summary, triage_finding,
};

#[test]
fn extraction_marks_integrated_validation_findings_as_integrated() {
    let item = nil_item();
    let mut job = test_job();
    job.step_id = StepId::ValidateIntegrated;
    job.phase_kind = PhaseKind::Validate;
    job.job_input = JobInput::integrated_subject("base".into(), "head".into());
    job.state = ingot_domain::job::JobState::Completed {
        assignment: None,
        started_at: None,
        outcome_class: OutcomeClass::Findings,
        ended_at: chrono::Utc::now(),
        output_commit_oid: None,
        result_schema_version: Some("validation_report:v1".into()),
        result_payload: Some(serde_json::json!({
            "outcome": "findings",
            "summary": "Found issues",
            "checks": [],
            "findings": [{
                "finding_key": "f-1",
                "code": "VAL001",
                "severity": "high",
                "summary": "Integrated issue",
                "paths": ["src/lib.rs"],
                "evidence": ["broken"]
            }]
        })),
    };

    let extracted = extract_findings(&item, &job, &[]).unwrap();

    assert_eq!(extracted.outcome_class, OutcomeClass::Findings);
    assert_eq!(extracted.findings.len(), 1);
    assert_eq!(
        extracted.findings[0].source_subject_kind,
        FindingSubjectKind::Integrated
    );
}

#[test]
fn backlog_links_item_and_finding() {
    let item = nil_item();
    let revision = nil_revision();
    let finding = test_finding();

    let (linked_item, linked_revision, triaged_finding) = backlog_finding(
        &finding,
        &item,
        &revision,
        BacklogFindingOverrides::default(),
        "80".to_string(),
        None,
    )
    .unwrap();

    assert!(linked_item.origin.is_promoted_finding());
    assert_eq!(linked_item.origin.finding_id(), Some(finding.id));
    assert_eq!(linked_revision.item_id, linked_item.id);
    assert_eq!(
        triaged_finding.triage.linked_item_id(),
        Some(linked_item.id)
    );
    assert_eq!(triaged_finding.triage.state(), FindingTriageState::Backlog);
}

#[test]
fn dismissed_invalid_requires_reason() {
    let finding = test_finding();
    assert!(
        triage_finding(
            &finding,
            TriageFindingInput {
                triage_state: FindingTriageState::DismissedInvalid,
                triage_note: Some("".into()),
                linked_item_id: None,
            },
        )
        .is_err()
    );
}

#[test]
fn triage_allows_revising_a_previous_nonblocking_decision() {
    let mut finding = test_finding();
    finding.triage = FindingTriage::WontFix {
        triage_note: "accepted".into(),
        triaged_at: Utc::now(),
    };

    let retriaged = triage_finding(
        &finding,
        TriageFindingInput {
            triage_state: FindingTriageState::FixNow,
            triage_note: None,
            linked_item_id: None,
        },
    )
    .expect("retriage from wont_fix to fix_now");

    assert_eq!(retriaged.triage.state(), FindingTriageState::FixNow);
    assert_eq!(retriaged.triage.triage_note(), None);
    assert_eq!(retriaged.triage.linked_item_id(), None);
}

#[test]
fn revision_context_summary_uses_row_updated_at() {
    let context = ingot_domain::revision_context::RevisionContext {
        item_revision_id: ItemRevisionId::from_uuid(Uuid::nil()),
        schema_version: "revision_context:v1".into(),
        payload: ingot_domain::revision_context::RevisionContextPayload {
            authoring_head_commit_oid: None,
            changed_paths: vec!["src/lib.rs".into()],
            latest_validation: None,
            latest_review: None,
            accepted_result_refs: vec![],
            operator_notes_excerpt: Some("note".into()),
        },
        updated_from_job_id: None,
        updated_at: Utc::now(),
    };

    let summary = parse_revision_context_summary(Some(&context)).expect("summary");

    assert_eq!(summary.updated_at, context.updated_at);
    assert_eq!(summary.changed_paths, vec!["src/lib.rs".to_string()]);
    assert_eq!(summary.operator_notes_excerpt.as_deref(), Some("note"));
}

#[test]
fn validation_reports_require_checks_and_failed_signal_for_findings() {
    let item = nil_item();
    let mut job = test_job();
    job.step_id = StepId::ValidateCandidateInitial;
    job.phase_kind = PhaseKind::Validate;
    job.job_input = JobInput::candidate_subject("base".into(), "head".into());
    job.state = ingot_domain::job::JobState::Completed {
        assignment: None,
        started_at: None,
        outcome_class: OutcomeClass::Findings,
        ended_at: chrono::Utc::now(),
        output_commit_oid: None,
        result_schema_version: Some("validation_report:v1".into()),
        result_payload: Some(serde_json::json!({
            "outcome": "findings",
            "summary": "Found issues",
            "checks": [],
            "findings": []
        })),
    };

    let error = extract_findings(&item, &job, &[]).expect_err("expected protocol violation");
    assert!(matches!(error, UseCaseError::ProtocolViolation(_)));
}

#[test]
fn review_reports_require_overall_risk() {
    let item = nil_item();
    let mut job = test_job();
    job.step_id = StepId::ReviewCandidateInitial;
    job.phase_kind = PhaseKind::Review;
    job.job_input = JobInput::candidate_subject("base".into(), "head".into());
    job.state = ingot_domain::job::JobState::Completed {
        assignment: None,
        started_at: None,
        outcome_class: OutcomeClass::Findings,
        ended_at: chrono::Utc::now(),
        output_commit_oid: None,
        result_schema_version: Some("review_report:v1".into()),
        result_payload: Some(serde_json::json!({
            "outcome": "clean",
            "summary": "Looks good",
            "review_subject": {
                "base_commit_oid": "base",
                "head_commit_oid": "head"
            },
            "findings": []
        })),
    };

    let error = extract_findings(&item, &job, &[]).expect_err("expected protocol violation");
    assert!(matches!(error, UseCaseError::ProtocolViolation(_)));
}

#[test]
fn validation_reports_reject_duplicate_finding_keys() {
    let item = nil_item();
    let mut job = test_job();
    job.step_id = StepId::ValidateCandidateInitial;
    job.phase_kind = PhaseKind::Validate;
    job.job_input = JobInput::candidate_subject("base".into(), "head".into());
    job.state = ingot_domain::job::JobState::Completed {
        assignment: None,
        started_at: None,
        outcome_class: OutcomeClass::Findings,
        ended_at: chrono::Utc::now(),
        output_commit_oid: None,
        result_schema_version: Some("validation_report:v1".into()),
        result_payload: Some(serde_json::json!({
            "outcome": "findings",
            "summary": "Found issues",
            "checks": [{
                "name": "lint",
                "status": "fail",
                "summary": "lint failed"
            }],
            "findings": [
                {
                    "finding_key": "f-1",
                    "code": "VAL001",
                    "severity": "high",
                    "summary": "first",
                    "paths": ["src/lib.rs"],
                    "evidence": ["broken"]
                },
                {
                    "finding_key": "f-1",
                    "code": "VAL002",
                    "severity": "medium",
                    "summary": "second",
                    "paths": ["src/main.rs"],
                    "evidence": ["still broken"]
                }
            ]
        })),
    };

    let error = extract_findings(&item, &job, &[]).expect_err("expected protocol violation");
    assert!(matches!(error, UseCaseError::ProtocolViolation(_)));
}

#[test]
fn review_reports_reject_duplicate_finding_keys() {
    let item = nil_item();
    let mut job = test_job();
    job.step_id = StepId::ReviewCandidateInitial;
    job.phase_kind = PhaseKind::Review;
    job.job_input = JobInput::candidate_subject("base".into(), "head".into());
    job.state = ingot_domain::job::JobState::Completed {
        assignment: None,
        started_at: None,
        outcome_class: OutcomeClass::Findings,
        ended_at: chrono::Utc::now(),
        output_commit_oid: None,
        result_schema_version: Some("review_report:v1".into()),
        result_payload: Some(serde_json::json!({
            "outcome": "findings",
            "summary": "Found issues",
            "review_subject": {
                "base_commit_oid": "base",
                "head_commit_oid": "head"
            },
            "overall_risk": "high",
            "findings": [
                {
                    "finding_key": "f-1",
                    "code": "REV001",
                    "severity": "high",
                    "summary": "first",
                    "paths": ["src/lib.rs"],
                    "evidence": ["broken"]
                },
                {
                    "finding_key": "f-1",
                    "code": "REV002",
                    "severity": "medium",
                    "summary": "second",
                    "paths": ["src/main.rs"],
                    "evidence": ["still broken"]
                }
            ]
        })),
    };

    let error = extract_findings(&item, &job, &[]).expect_err("expected protocol violation");
    assert!(matches!(error, UseCaseError::ProtocolViolation(_)));
}

#[test]
fn finding_reports_reject_duplicate_finding_keys() {
    let item = nil_item();
    let mut job = test_job();
    job.step_id = StepId::InvestigateItem;
    job.phase_kind = PhaseKind::Investigate;
    job.job_input = JobInput::candidate_subject("base".into(), "head".into());
    job.state = ingot_domain::job::JobState::Completed {
        assignment: None,
        started_at: None,
        outcome_class: OutcomeClass::Findings,
        ended_at: chrono::Utc::now(),
        output_commit_oid: None,
        result_schema_version: Some("finding_report:v1".into()),
        result_payload: Some(serde_json::json!({
            "outcome": "findings",
            "summary": "Found issues",
            "findings": [
                {
                    "finding_key": "f-1",
                    "code": "BUG001",
                    "severity": "high",
                    "summary": "first",
                    "paths": ["src/lib.rs"],
                    "evidence": ["broken"]
                },
                {
                    "finding_key": "f-1",
                    "code": "BUG002",
                    "severity": "medium",
                    "summary": "second",
                    "paths": ["src/main.rs"],
                    "evidence": ["still broken"]
                }
            ]
        })),
    };

    let error = extract_findings(&item, &job, &[]).expect_err("expected protocol violation");
    assert!(matches!(error, UseCaseError::ProtocolViolation(_)));
}

fn test_job() -> Job {
    let nil = Uuid::nil();
    JobBuilder::new(
        ProjectId::from_uuid(nil),
        ItemId::from_uuid(nil),
        ItemRevisionId::from_uuid(nil),
        "investigate_item",
    )
    .id(JobId::from_uuid(nil))
    .status(JobStatus::Completed)
    .outcome_class(OutcomeClass::Findings)
    .phase_kind(PhaseKind::Investigate)
    .workspace_kind(ingot_domain::workspace::WorkspaceKind::Review)
    .execution_permission(ingot_domain::job::ExecutionPermission::MustNotMutate)
    .phase_template_slug("investigate-item")
    .job_input(JobInput::candidate_subject("base".into(), "head".into()))
    .output_artifact_kind(OutputArtifactKind::FindingReport)
    .ended_at(Utc::now())
    .build()
}

fn test_finding() -> ingot_domain::finding::Finding {
    FindingBuilder::new(
        ProjectId::from_uuid(Uuid::nil()),
        ItemId::from_uuid(Uuid::nil()),
        ItemRevisionId::from_uuid(Uuid::nil()),
        JobId::from_uuid(Uuid::nil()),
    )
    .source_step_id("investigate_item")
    .summary("Summary")
    .evidence(serde_json::json!(["broken"]))
    .build()
}

#[allow(dead_code)]
fn _test_convergence() -> ingot_domain::convergence::Convergence {
    ConvergenceBuilder::new(
        ProjectId::from_uuid(Uuid::nil()),
        ItemId::from_uuid(Uuid::nil()),
        ItemRevisionId::from_uuid(Uuid::nil()),
    )
    .prepared_commit_oid("head")
    .target_head_valid(true)
    .build()
}

fn test_finding_with_severity(
    severity: ingot_domain::finding::FindingSeverity,
) -> ingot_domain::finding::Finding {
    FindingBuilder::new(
        ProjectId::from_uuid(Uuid::nil()),
        ItemId::from_uuid(Uuid::nil()),
        ItemRevisionId::from_uuid(Uuid::nil()),
        JobId::from_uuid(Uuid::nil()),
    )
    .source_step_id("investigate_item")
    .severity(severity)
    .summary("Summary")
    .evidence(serde_json::json!(["broken"]))
    .build()
}

#[test]
fn auto_triage_maps_severity_to_decisions() {
    use ingot_domain::finding::FindingSeverity;
    use ingot_domain::project::AutoTriagePolicy;

    let item = nil_item();
    let revision = nil_revision();
    let policy = AutoTriagePolicy::default();

    let findings = vec![
        test_finding_with_severity(FindingSeverity::Critical),
        test_finding_with_severity(FindingSeverity::High),
        test_finding_with_severity(FindingSeverity::Medium),
        test_finding_with_severity(FindingSeverity::Low),
    ];

    let results = auto_triage_findings(&findings, &policy, &item, &revision, &[]).unwrap();

    assert_eq!(results.len(), 4);
    assert_eq!(
        results[0].finding.triage.state(),
        FindingTriageState::FixNow
    );
    assert!(results[0].backlog.is_none());
    assert_eq!(
        results[1].finding.triage.state(),
        FindingTriageState::FixNow
    );
    assert!(results[1].backlog.is_none());
    assert_eq!(
        results[2].finding.triage.state(),
        FindingTriageState::FixNow
    );
    assert!(results[2].backlog.is_none());
    assert_eq!(
        results[3].finding.triage.state(),
        FindingTriageState::Backlog
    );
    assert!(results[3].backlog.is_some());
}

#[test]
fn auto_triage_skip_leaves_findings_untriaged() {
    use ingot_domain::finding::FindingSeverity;
    use ingot_domain::project::{AutoTriageDecision, AutoTriagePolicy};

    let item = nil_item();
    let revision = nil_revision();
    let policy = AutoTriagePolicy {
        critical: AutoTriageDecision::Skip,
        high: AutoTriageDecision::Skip,
        medium: AutoTriageDecision::Skip,
        low: AutoTriageDecision::Skip,
    };

    let findings = vec![
        test_finding_with_severity(FindingSeverity::High),
        test_finding_with_severity(FindingSeverity::Low),
    ];

    let results = auto_triage_findings(&findings, &policy, &item, &revision, &[]).unwrap();

    assert!(results.is_empty());
}

#[test]
fn auto_triage_empty_findings() {
    use ingot_domain::project::AutoTriagePolicy;

    let item = nil_item();
    let revision = nil_revision();
    let policy = AutoTriagePolicy::default();

    let results = auto_triage_findings(&[], &policy, &item, &revision, &[]).unwrap();

    assert!(results.is_empty());
}

#[test]
fn auto_triage_mix_fix_now_and_backlog() {
    use ingot_domain::finding::FindingSeverity;
    use ingot_domain::project::{AutoTriageDecision, AutoTriagePolicy};

    let item = nil_item();
    let revision = nil_revision();
    let policy = AutoTriagePolicy {
        critical: AutoTriageDecision::FixNow,
        high: AutoTriageDecision::Backlog,
        medium: AutoTriageDecision::FixNow,
        low: AutoTriageDecision::Backlog,
    };

    let findings = vec![
        test_finding_with_severity(FindingSeverity::Critical),
        test_finding_with_severity(FindingSeverity::High),
        test_finding_with_severity(FindingSeverity::Low),
    ];

    let results = auto_triage_findings(&findings, &policy, &item, &revision, &[]).unwrap();

    assert_eq!(results.len(), 3);
    assert_eq!(
        results[0].finding.triage.state(),
        FindingTriageState::FixNow
    );
    assert!(results[0].backlog.is_none());
    assert_eq!(
        results[1].finding.triage.state(),
        FindingTriageState::Backlog
    );
    assert!(results[1].backlog.is_some());
    assert_eq!(
        results[2].finding.triage.state(),
        FindingTriageState::Backlog
    );
    assert!(results[2].backlog.is_some());

    let (item1, _) = results[1].backlog.as_ref().unwrap();
    let (item2, _) = results[2].backlog.as_ref().unwrap();
    assert!(item2.sort_key > item1.sort_key);
}

#[tokio::test]
async fn execute_auto_triage_transitions_approval_for_validate_integrated() {
    use ingot_domain::ids::ProjectId;
    use ingot_domain::item::ApprovalState;
    use ingot_domain::job::OutputArtifactKind;
    use ingot_domain::ports::{ActivityRepository, FindingRepository, ItemRepository};
    use ingot_domain::project::{AutoTriageDecision, AutoTriagePolicy, ExecutionMode};
    use ingot_domain::revision::ApprovalPolicy;
    use ingot_test_support::fixtures::{ItemBuilder, ProjectBuilder, RevisionBuilder};
    use ingot_test_support::sqlite::migrated_test_db;

    let db = migrated_test_db("ingot-usecases-finding-triage").await;
    let project_id = ProjectId::new();
    let item_id = ItemId::new();
    let revision_id = ItemRevisionId::new();
    let job_id = JobId::new();

    let project = ProjectBuilder::new(
        std::env::temp_dir().join(format!("ingot-finding-triage-{}", uuid::Uuid::now_v7())),
    )
    .id(project_id)
    .execution_mode(ExecutionMode::Autopilot)
    .build();
    let item = ItemBuilder::new(project_id, revision_id)
        .id(item_id)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .approval_policy(ApprovalPolicy::Required)
        .explicit_seed("seed")
        .build();

    db.create_project(&project).await.expect("create project");
    db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let job = JobBuilder::new(project_id, item_id, revision_id, "validate_integrated")
        .id(job_id)
        .status(JobStatus::Completed)
        .outcome_class(OutcomeClass::Findings)
        .output_artifact_kind(OutputArtifactKind::ValidationReport)
        .ended_at(Utc::now())
        .build();
    db.create_job(&job).await.expect("create job");

    let finding = FindingBuilder::new(project_id, item_id, revision_id, job_id)
        .source_step_id("validate_integrated")
        .severity(ingot_domain::finding::FindingSeverity::Low)
        .summary("Minor cosmetic issue")
        .evidence(serde_json::json!(["trivial"]))
        .build();
    db.create_finding(&finding).await.expect("create finding");

    let policy = AutoTriagePolicy {
        critical: AutoTriageDecision::FixNow,
        high: AutoTriageDecision::FixNow,
        medium: AutoTriageDecision::FixNow,
        low: AutoTriageDecision::Backlog,
    };

    execute_auto_triage(
        &db,
        &db,
        &db,
        &db,
        &project,
        &item,
        job_id,
        StepId::ValidateIntegrated,
        &policy,
    )
    .await
    .expect("execute auto triage");

    let findings = FindingRepository::list_by_item(&db, item_id)
        .await
        .expect("list findings");
    let triaged = findings
        .iter()
        .find(|f| f.source_job_id == job_id)
        .expect("find original finding");
    assert_eq!(triaged.triage.state(), FindingTriageState::Backlog);

    let updated_item = ItemRepository::get(&db, item_id)
        .await
        .expect("reload item");
    assert_eq!(
        updated_item.approval_state,
        ApprovalState::Pending,
        "non-blocking Backlog findings on ValidateIntegrated should trigger Pending approval"
    );

    let activities = ActivityRepository::list_by_project(&db, project_id, 100, 0)
        .await
        .expect("list activities");
    assert!(
        activities
            .iter()
            .any(|a| a.event_type == ingot_domain::activity::ActivityEventType::ApprovalRequested),
        "ApprovalRequested activity should be appended"
    );
}

#[tokio::test]
async fn execute_auto_triage_does_not_transition_approval_for_fix_now_findings() {
    use ingot_domain::ids::ProjectId;
    use ingot_domain::item::ApprovalState;
    use ingot_domain::job::OutputArtifactKind;
    use ingot_domain::ports::{FindingRepository, ItemRepository};
    use ingot_domain::project::{AutoTriageDecision, AutoTriagePolicy, ExecutionMode};
    use ingot_domain::revision::ApprovalPolicy;
    use ingot_test_support::fixtures::{ItemBuilder, ProjectBuilder, RevisionBuilder};
    use ingot_test_support::sqlite::migrated_test_db;

    let db = migrated_test_db("ingot-usecases-finding-fixnow").await;
    let project_id = ProjectId::new();
    let item_id = ItemId::new();
    let revision_id = ItemRevisionId::new();
    let job_id = JobId::new();

    let project = ProjectBuilder::new(
        std::env::temp_dir().join(format!("ingot-finding-fixnow-{}", uuid::Uuid::now_v7())),
    )
    .id(project_id)
    .execution_mode(ExecutionMode::Autopilot)
    .build();
    let item = ItemBuilder::new(project_id, revision_id)
        .id(item_id)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .approval_policy(ApprovalPolicy::Required)
        .explicit_seed("seed")
        .build();

    db.create_project(&project).await.expect("create project");
    db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let job = JobBuilder::new(project_id, item_id, revision_id, "validate_integrated")
        .id(job_id)
        .status(JobStatus::Completed)
        .outcome_class(OutcomeClass::Findings)
        .output_artifact_kind(OutputArtifactKind::ValidationReport)
        .ended_at(Utc::now())
        .build();
    db.create_job(&job).await.expect("create job");

    let finding = FindingBuilder::new(project_id, item_id, revision_id, job_id)
        .source_step_id("validate_integrated")
        .severity(ingot_domain::finding::FindingSeverity::High)
        .summary("Critical bug")
        .evidence(serde_json::json!(["broken"]))
        .build();
    db.create_finding(&finding).await.expect("create finding");

    let policy = AutoTriagePolicy {
        critical: AutoTriageDecision::FixNow,
        high: AutoTriageDecision::FixNow,
        medium: AutoTriageDecision::FixNow,
        low: AutoTriageDecision::Backlog,
    };

    execute_auto_triage(
        &db,
        &db,
        &db,
        &db,
        &project,
        &item,
        job_id,
        StepId::ValidateIntegrated,
        &policy,
    )
    .await
    .expect("execute auto triage");

    let findings = FindingRepository::list_by_item(&db, item_id)
        .await
        .expect("list findings");
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].triage.state(), FindingTriageState::FixNow);

    let updated_item = ItemRepository::get(&db, item_id)
        .await
        .expect("reload item");
    assert_eq!(
        updated_item.approval_state,
        ApprovalState::NotRequested,
        "FixNow findings should NOT trigger approval transition"
    );
}

#[tokio::test]
async fn execute_auto_triage_skips_approval_for_non_validate_integrated() {
    use ingot_domain::ids::ProjectId;
    use ingot_domain::item::ApprovalState;
    use ingot_domain::job::OutputArtifactKind;
    use ingot_domain::ports::{FindingRepository, ItemRepository};
    use ingot_domain::project::{AutoTriageDecision, AutoTriagePolicy, ExecutionMode};
    use ingot_domain::revision::ApprovalPolicy;
    use ingot_test_support::fixtures::{ItemBuilder, ProjectBuilder, RevisionBuilder};
    use ingot_test_support::sqlite::migrated_test_db;

    let db = migrated_test_db("ingot-usecases-finding-guard").await;
    let project_id = ProjectId::new();
    let item_id = ItemId::new();
    let revision_id = ItemRevisionId::new();
    let job_id = JobId::new();

    let project = ProjectBuilder::new(
        std::env::temp_dir().join(format!("ingot-finding-guard-{}", uuid::Uuid::now_v7())),
    )
    .id(project_id)
    .execution_mode(ExecutionMode::Autopilot)
    .build();
    let item = ItemBuilder::new(project_id, revision_id)
        .id(item_id)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .approval_policy(ApprovalPolicy::Required)
        .explicit_seed("seed")
        .build();

    db.create_project(&project).await.expect("create project");
    db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let job = JobBuilder::new(project_id, item_id, revision_id, "investigate_item")
        .id(job_id)
        .status(JobStatus::Completed)
        .outcome_class(OutcomeClass::Findings)
        .output_artifact_kind(OutputArtifactKind::FindingReport)
        .ended_at(Utc::now())
        .build();
    db.create_job(&job).await.expect("create job");

    let finding = FindingBuilder::new(project_id, item_id, revision_id, job_id)
        .source_step_id("investigate_item")
        .severity(ingot_domain::finding::FindingSeverity::Low)
        .summary("Minor issue")
        .evidence(serde_json::json!(["minor"]))
        .build();
    db.create_finding(&finding).await.expect("create finding");

    let policy = AutoTriagePolicy {
        critical: AutoTriageDecision::FixNow,
        high: AutoTriageDecision::FixNow,
        medium: AutoTriageDecision::FixNow,
        low: AutoTriageDecision::Backlog,
    };

    execute_auto_triage(
        &db,
        &db,
        &db,
        &db,
        &project,
        &item,
        job_id,
        StepId::InvestigateItem,
        &policy,
    )
    .await
    .expect("execute auto triage");

    let findings = FindingRepository::list_by_item(&db, item_id)
        .await
        .expect("list findings");
    let triaged = findings
        .iter()
        .find(|f| f.source_job_id == job_id)
        .expect("find original finding");
    assert_eq!(triaged.triage.state(), FindingTriageState::Backlog);

    let updated_item = ItemRepository::get(&db, item_id)
        .await
        .expect("reload item");
    assert_eq!(
        updated_item.approval_state,
        ApprovalState::NotRequested,
        "non-ValidateIntegrated step must not trigger approval transition"
    );
}
