use std::collections::HashSet;

use chrono::Utc;
use ingot_domain::convergence::{Convergence, ConvergenceStatus};
use ingot_domain::finding::{Finding, FindingSeverity, FindingSubjectKind, FindingTriageState};
use ingot_domain::ids::{FindingId, ItemId, ItemRevisionId};
use ingot_domain::item::{
    ApprovalState, Classification, Escalation, Item, Lifecycle, Origin, ParkingState,
};
use ingot_domain::job::{Job, OutcomeClass};
use ingot_domain::revision::{ApprovalPolicy, ItemRevision};
use serde::Deserialize;

use crate::UseCaseError;

#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct BacklogFindingOverrides {
    pub target_ref: Option<String>,
    pub approval_policy: Option<ApprovalPolicy>,
}

#[derive(Debug, Clone)]
pub struct TriageFindingInput {
    pub triage_state: FindingTriageState,
    pub triage_note: Option<String>,
    pub linked_item_id: Option<ItemId>,
}

#[derive(Debug, Deserialize)]
struct FindingV1 {
    finding_key: String,
    code: String,
    severity: FindingSeverity,
    summary: String,
    paths: Vec<String>,
    evidence: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ValidationReportV1 {
    outcome: String,
    summary: String,
    checks: Vec<ValidationCheckV1>,
    findings: Vec<FindingV1>,
}

#[derive(Debug, Deserialize)]
struct ReviewSubjectV1 {
    base_commit_oid: String,
    head_commit_oid: String,
}

#[derive(Debug, Deserialize)]
struct ReviewReportV1 {
    outcome: String,
    summary: String,
    review_subject: ReviewSubjectV1,
    overall_risk: ReviewOverallRisk,
    findings: Vec<FindingV1>,
}

#[derive(Debug, Deserialize)]
struct FindingReportV1 {
    outcome: String,
    summary: String,
    findings: Vec<FindingV1>,
}

#[derive(Debug, Deserialize)]
struct RevisionContextPayload {
    changed_paths: Vec<String>,
    latest_validation: Option<ingot_domain::revision_context::RevisionContextResultSummary>,
    latest_review: Option<ingot_domain::revision_context::RevisionContextResultSummary>,
    accepted_result_refs: Vec<ingot_domain::revision_context::RevisionContextAcceptedResultRef>,
    operator_notes_excerpt: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct ValidationCheckV1 {
    name: String,
    #[allow(dead_code)]
    status: ValidationCheckStatus,
    summary: String,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ValidationCheckStatus {
    Pass,
    Fail,
    Skip,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ReviewOverallRisk {
    Low,
    Medium,
    High,
}

#[derive(Debug)]
pub struct ExtractedFindings {
    pub outcome_class: OutcomeClass,
    pub findings: Vec<Finding>,
}

pub fn extract_findings(
    item: &Item,
    job: &Job,
    convergences: &[Convergence],
) -> Result<ExtractedFindings, UseCaseError> {
    let Some(schema_version) = job.state.result_schema_version() else {
        return Ok(ExtractedFindings {
            outcome_class: OutcomeClass::Clean,
            findings: vec![],
        });
    };
    let Some(result_payload) = job.state.result_payload() else {
        return Ok(ExtractedFindings {
            outcome_class: OutcomeClass::Clean,
            findings: vec![],
        });
    };

    let (report_outcome, report_findings) = match schema_version {
        "validation_report:v1" => {
            let report: ValidationReportV1 = serde_json::from_value(result_payload.clone())
                .map_err(|err| UseCaseError::ProtocolViolation(err.to_string()))?;
            let outcome_class = validate_validation_report(
                &report.outcome,
                &report.findings,
                &report.checks,
                &report.summary,
            )?;
            (outcome_class, report.findings)
        }
        "review_report:v1" => {
            let report: ReviewReportV1 = serde_json::from_value(result_payload.clone())
                .map_err(|err| UseCaseError::ProtocolViolation(err.to_string()))?;
            let outcome_class =
                validate_review_report(&report.outcome, &report.findings, &report.summary)?;

            if job.job_input.base_commit_oid()
                != Some(report.review_subject.base_commit_oid.as_str())
                || job.job_input.head_commit_oid()
                    != Some(report.review_subject.head_commit_oid.as_str())
            {
                return Err(UseCaseError::ProtocolViolation(
                    "review subject does not match job input commits".into(),
                ));
            }

            let _ = report.overall_risk;
            (outcome_class, report.findings)
        }
        "finding_report:v1" => {
            let report: FindingReportV1 = serde_json::from_value(result_payload.clone())
                .map_err(|err| UseCaseError::ProtocolViolation(err.to_string()))?;
            let outcome_class =
                validate_finding_report(&report.outcome, &report.findings, &report.summary)?;
            (outcome_class, report.findings)
        }
        _ => {
            return Ok(ExtractedFindings {
                outcome_class: OutcomeClass::Clean,
                findings: vec![],
            });
        }
    };

    ensure_unique_finding_keys(&report_findings)?;

    let source_subject_kind = classify_subject(job, convergences);
    let created_at = Utc::now();
    let source_subject_base_commit_oid = match source_subject_kind {
        FindingSubjectKind::Integrated | FindingSubjectKind::Candidate => {
            job.job_input.base_commit_oid().map(ToOwned::to_owned)
        }
    };
    let source_subject_head_commit_oid = job
        .job_input
        .head_commit_oid()
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            UseCaseError::ProtocolViolation("finding extraction requires job_input head".into())
        })?;

    let findings = report_findings
        .into_iter()
        .map(|finding| {
            Ok(Finding {
                id: FindingId::new(),
                project_id: item.project_id,
                source_item_id: item.id,
                source_item_revision_id: job.item_revision_id,
                source_job_id: job.id,
                source_step_id: job.step_id.clone(),
                source_report_schema_version: schema_version.into(),
                source_finding_key: finding.finding_key,
                source_subject_kind,
                source_subject_base_commit_oid: source_subject_base_commit_oid.clone(),
                source_subject_head_commit_oid: source_subject_head_commit_oid.clone(),
                code: finding.code,
                severity: finding.severity,
                summary: finding.summary,
                paths: finding.paths,
                evidence: serde_json::json!(finding.evidence),
                triage_state: FindingTriageState::Untriaged,
                linked_item_id: None,
                triage_note: None,
                created_at,
                triaged_at: None,
            })
        })
        .collect::<Result<Vec<_>, UseCaseError>>()?;

    Ok(ExtractedFindings {
        outcome_class: report_outcome,
        findings,
    })
}

fn ensure_unique_finding_keys(findings: &[FindingV1]) -> Result<(), UseCaseError> {
    let mut seen_keys = HashSet::with_capacity(findings.len());

    for finding in findings {
        if !seen_keys.insert(finding.finding_key.as_str()) {
            return Err(UseCaseError::ProtocolViolation(format!(
                "duplicate finding_key in report: {}",
                finding.finding_key
            )));
        }
    }

    Ok(())
}

pub fn triage_finding(
    finding: &Finding,
    input: TriageFindingInput,
) -> Result<Finding, UseCaseError> {
    if input.triage_state == FindingTriageState::Untriaged {
        return Err(UseCaseError::InvalidFindingTriage(
            "triage_state must resolve the finding".into(),
        ));
    }

    let triage_note = normalize_note(input.triage_note);
    let mut triaged = finding.clone();
    triaged.triage_state = input.triage_state;
    triaged.triaged_at = Some(Utc::now());

    match input.triage_state {
        FindingTriageState::FixNow => {
            ensure_note_absent(&triage_note, "fix_now")?;
            ensure_link_absent(input.linked_item_id, "fix_now")?;
            triaged.linked_item_id = None;
            triaged.triage_note = None;
        }
        FindingTriageState::WontFix => {
            ensure_note_present(&triage_note, "wont_fix")?;
            ensure_link_absent(input.linked_item_id, "wont_fix")?;
            triaged.linked_item_id = None;
            triaged.triage_note = triage_note;
        }
        FindingTriageState::Backlog => {
            let linked_item_id = input.linked_item_id.ok_or_else(|| {
                UseCaseError::InvalidFindingTriage(
                    "backlog triage requires a linked_item_id".into(),
                )
            })?;
            triaged.linked_item_id = Some(linked_item_id);
            triaged.triage_note = triage_note;
        }
        FindingTriageState::Duplicate => {
            let linked_item_id = input.linked_item_id.ok_or_else(|| {
                UseCaseError::InvalidFindingTriage(
                    "duplicate triage requires a linked_item_id".into(),
                )
            })?;
            triaged.linked_item_id = Some(linked_item_id);
            triaged.triage_note = triage_note;
        }
        FindingTriageState::DismissedInvalid => {
            ensure_note_present(&triage_note, "dismissed_invalid")?;
            ensure_link_absent(input.linked_item_id, "dismissed_invalid")?;
            triaged.linked_item_id = None;
            triaged.triage_note = triage_note;
        }
        FindingTriageState::NeedsInvestigation => {
            ensure_note_present(&triage_note, "needs_investigation")?;
            ensure_link_absent(input.linked_item_id, "needs_investigation")?;
            triaged.linked_item_id = None;
            triaged.triage_note = triage_note;
        }
        FindingTriageState::Untriaged => unreachable!("handled above"),
    }

    Ok(triaged)
}

pub fn backlog_finding(
    finding: &Finding,
    source_item: &Item,
    source_revision: &ItemRevision,
    overrides: BacklogFindingOverrides,
    triage_note: Option<String>,
) -> Result<(Item, ItemRevision, Finding), UseCaseError> {
    if !finding.triage_state.is_unresolved() {
        return Err(UseCaseError::FindingNotTriageable);
    }

    if finding.source_subject_head_commit_oid.trim().is_empty()
        || (finding.source_subject_kind == FindingSubjectKind::Integrated
            && finding
                .source_subject_base_commit_oid
                .as_deref()
                .is_none_or(str::is_empty))
    {
        return Err(UseCaseError::FindingSubjectUnreachable);
    }

    let item_id = ItemId::new();
    let revision_id = ItemRevisionId::new();
    let approval_policy = overrides
        .approval_policy
        .unwrap_or(source_revision.approval_policy);
    let created_at = Utc::now();
    let triage_note = normalize_note(triage_note);
    let evidence_lines = finding
        .evidence
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str())
                .map(|value| format!("- {value}"))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let linked_item = Item {
        id: item_id,
        project_id: source_item.project_id,
        classification: Classification::Bug,
        workflow_version: source_item.workflow_version.clone(),
        lifecycle: Lifecycle::Open,
        parking_state: ParkingState::Active,
        approval_state: match approval_policy {
            ApprovalPolicy::Required => ApprovalState::NotRequested,
            ApprovalPolicy::NotRequired => ApprovalState::NotRequired,
        },
        escalation: Escalation::None,
        current_revision_id: revision_id,
        origin: Origin::PromotedFinding {
            finding_id: finding.id,
        },
        priority: source_item.priority,
        labels: vec![],
        operator_notes: None,
        created_at,
        updated_at: created_at,
    };

    let linked_revision = ItemRevision {
        id: revision_id,
        item_id,
        revision_no: 1,
        title: finding.summary.clone(),
        description: if evidence_lines.is_empty() {
            finding.summary.clone()
        } else {
            format!(
                "{}\n\nEvidence:\n{}",
                finding.summary,
                evidence_lines.join("\n")
            )
        },
        acceptance_criteria: format!(
            "Resolve finding {} and validate that it no longer reproduces.",
            finding.code
        ),
        target_ref: overrides
            .target_ref
            .unwrap_or_else(|| source_revision.target_ref.clone()),
        approval_policy,
        policy_snapshot: source_revision.policy_snapshot.clone(),
        template_map_snapshot: source_revision.template_map_snapshot.clone(),
        seed_commit_oid: Some(finding.source_subject_head_commit_oid.clone()),
        seed_target_commit_oid: match finding.source_subject_kind {
            FindingSubjectKind::Integrated => finding.source_subject_base_commit_oid.clone(),
            FindingSubjectKind::Candidate => source_revision.seed_target_commit_oid.clone(),
        },
        supersedes_revision_id: None,
        created_at,
    };

    let triaged_finding = triage_finding(
        finding,
        TriageFindingInput {
            triage_state: FindingTriageState::Backlog,
            triage_note,
            linked_item_id: Some(item_id),
        },
    )?;

    Ok((linked_item, linked_revision, triaged_finding))
}

pub fn parse_revision_context_summary(
    context: Option<&ingot_domain::revision_context::RevisionContext>,
) -> Result<Option<ingot_domain::revision_context::RevisionContextSummary>, UseCaseError> {
    let Some(context) = context else {
        return Ok(None);
    };

    let payload: RevisionContextPayload = serde_json::from_value(context.payload.clone())
        .map_err(|err| UseCaseError::ProtocolViolation(err.to_string()))?;

    Ok(Some(
        ingot_domain::revision_context::RevisionContextSummary {
            updated_at: context.updated_at,
            changed_paths: payload.changed_paths,
            latest_validation: payload.latest_validation,
            latest_review: payload.latest_review,
            accepted_result_refs: payload.accepted_result_refs,
            operator_notes_excerpt: payload.operator_notes_excerpt,
        },
    ))
}

fn validate_validation_report(
    outcome: &str,
    findings: &[FindingV1],
    checks: &[ValidationCheckV1],
    summary: &str,
) -> Result<OutcomeClass, UseCaseError> {
    validate_report_summary(summary)?;

    for check in checks {
        if check.name.trim().is_empty() || check.summary.trim().is_empty() {
            return Err(UseCaseError::ProtocolViolation(
                "validation checks must include name and summary".into(),
            ));
        }
    }

    match outcome {
        "clean" if findings.is_empty() => Ok(OutcomeClass::Clean),
        "clean" => Err(UseCaseError::ProtocolViolation(
            "clean validation reports must not contain findings or failed checks".into(),
        )),
        "findings" if !findings.is_empty() => Ok(OutcomeClass::Findings),
        "findings" => Err(UseCaseError::ProtocolViolation(
            "validation reports with outcome=findings must contain at least one finding".into(),
        )),
        other => Err(UseCaseError::ProtocolViolation(format!(
            "unsupported report outcome {other}"
        ))),
    }
}

fn normalize_note(note: Option<String>) -> Option<String> {
    note.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn ensure_note_present(note: &Option<String>, triage_state: &str) -> Result<(), UseCaseError> {
    if note.is_some() {
        Ok(())
    } else {
        Err(UseCaseError::InvalidFindingTriage(format!(
            "{triage_state} triage requires a triage_note"
        )))
    }
}

fn ensure_note_absent(note: &Option<String>, triage_state: &str) -> Result<(), UseCaseError> {
    if note.is_none() {
        Ok(())
    } else {
        Err(UseCaseError::InvalidFindingTriage(format!(
            "{triage_state} triage does not accept a triage_note"
        )))
    }
}

fn ensure_link_absent(
    linked_item_id: Option<ItemId>,
    triage_state: &str,
) -> Result<(), UseCaseError> {
    if linked_item_id.is_none() {
        Ok(())
    } else {
        Err(UseCaseError::InvalidFindingTriage(format!(
            "{triage_state} triage does not accept a linked_item_id"
        )))
    }
}

fn validate_review_report(
    outcome: &str,
    findings: &[FindingV1],
    summary: &str,
) -> Result<OutcomeClass, UseCaseError> {
    validate_report_summary(summary)?;

    match outcome {
        "clean" if findings.is_empty() => Ok(OutcomeClass::Clean),
        "clean" => Err(UseCaseError::ProtocolViolation(
            "clean review reports must not contain findings".into(),
        )),
        "findings" if !findings.is_empty() => Ok(OutcomeClass::Findings),
        "findings" => Err(UseCaseError::ProtocolViolation(
            "review reports with outcome=findings must contain at least one finding".into(),
        )),
        other => Err(UseCaseError::ProtocolViolation(format!(
            "unsupported report outcome {other}"
        ))),
    }
}

fn validate_finding_report(
    outcome: &str,
    findings: &[FindingV1],
    summary: &str,
) -> Result<OutcomeClass, UseCaseError> {
    validate_report_summary(summary)?;

    match outcome {
        "clean" if findings.is_empty() => Ok(OutcomeClass::Clean),
        "clean" => Err(UseCaseError::ProtocolViolation(
            "clean finding reports must not contain findings".into(),
        )),
        "findings" if !findings.is_empty() => Ok(OutcomeClass::Findings),
        "findings" => Err(UseCaseError::ProtocolViolation(
            "finding reports with outcome=findings must contain at least one finding".into(),
        )),
        other => Err(UseCaseError::ProtocolViolation(format!(
            "unsupported report outcome {other}"
        ))),
    }
}

fn validate_report_summary(summary: &str) -> Result<(), UseCaseError> {
    if summary.trim().is_empty() {
        return Err(UseCaseError::ProtocolViolation(
            "report summary must be present".into(),
        ));
    }

    Ok(())
}

fn classify_subject(job: &Job, convergences: &[Convergence]) -> FindingSubjectKind {
    if job.step_id == "validate_integrated" {
        return FindingSubjectKind::Integrated;
    }

    if !matches!(
        job.phase_kind,
        ingot_domain::job::PhaseKind::Review | ingot_domain::job::PhaseKind::Investigate
    ) {
        return FindingSubjectKind::Candidate;
    }

    let Some(base_commit_oid) = job.job_input.base_commit_oid() else {
        return FindingSubjectKind::Candidate;
    };
    let Some(head_commit_oid) = job.job_input.head_commit_oid() else {
        return FindingSubjectKind::Candidate;
    };

    let matches_integrated_subject = convergences.iter().any(|convergence| {
        matches!(
            convergence.state.status(),
            ConvergenceStatus::Prepared | ConvergenceStatus::Finalized
        ) && convergence.item_revision_id == job.item_revision_id
            && convergence.state.input_target_commit_oid() == Some(base_commit_oid)
            && (convergence.state.prepared_commit_oid() == Some(head_commit_oid)
                || convergence.state.final_target_commit_oid() == Some(head_commit_oid))
    });

    if matches_integrated_subject {
        FindingSubjectKind::Integrated
    } else {
        FindingSubjectKind::Candidate
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use ingot_domain::finding::{FindingSubjectKind, FindingTriageState};
    use ingot_domain::ids::{ItemId, ItemRevisionId, JobId, ProjectId};
    use ingot_domain::job::{
        Job, JobInput, JobStatus, OutcomeClass, OutputArtifactKind, PhaseKind,
    };
    use ingot_test_support::fixtures::{
        ConvergenceBuilder, FindingBuilder, JobBuilder, nil_item, nil_revision,
    };
    use uuid::Uuid;

    use crate::UseCaseError;

    use super::{
        BacklogFindingOverrides, TriageFindingInput, backlog_finding, extract_findings,
        parse_revision_context_summary, triage_finding,
    };

    #[test]
    fn extraction_marks_integrated_validation_findings_as_integrated() {
        let item = nil_item();
        let mut job = test_job();
        job.step_id = "validate_integrated".into();
        job.phase_kind = PhaseKind::Validate;
        job.job_input = JobInput::integrated_subject("base", "head");
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
            None,
        )
        .unwrap();

        assert!(linked_item.origin.is_promoted_finding());
        assert_eq!(linked_item.origin.finding_id(), Some(finding.id));
        assert_eq!(linked_revision.item_id, linked_item.id);
        assert_eq!(triaged_finding.linked_item_id, Some(linked_item.id));
        assert_eq!(triaged_finding.triage_state, FindingTriageState::Backlog);
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
        finding.triage_state = FindingTriageState::WontFix;
        finding.triage_note = Some("accepted".into());
        finding.triaged_at = Some(Utc::now());

        let retriaged = triage_finding(
            &finding,
            TriageFindingInput {
                triage_state: FindingTriageState::FixNow,
                triage_note: None,
                linked_item_id: None,
            },
        )
        .expect("retriage from wont_fix to fix_now");

        assert_eq!(retriaged.triage_state, FindingTriageState::FixNow);
        assert_eq!(retriaged.triage_note, None);
        assert_eq!(retriaged.linked_item_id, None);
    }

    #[test]
    fn revision_context_summary_uses_row_updated_at() {
        let context = ingot_domain::revision_context::RevisionContext {
            item_revision_id: ItemRevisionId::from_uuid(Uuid::nil()),
            schema_version: "revision_context:v1".into(),
            payload: serde_json::json!({
                "changed_paths": ["src/lib.rs"],
                "latest_validation": null,
                "latest_review": null,
                "accepted_result_refs": [],
                "operator_notes_excerpt": "note"
            }),
            updated_from_job_id: None,
            updated_at: Utc::now(),
        };

        let summary = parse_revision_context_summary(Some(&context))
            .unwrap()
            .expect("summary");

        assert_eq!(summary.updated_at, context.updated_at);
        assert_eq!(summary.changed_paths, vec!["src/lib.rs".to_string()]);
        assert_eq!(summary.operator_notes_excerpt.as_deref(), Some("note"));
    }

    #[test]
    fn validation_reports_require_checks_and_failed_signal_for_findings() {
        let item = nil_item();
        let mut job = test_job();
        job.step_id = "validate_candidate_initial".into();
        job.phase_kind = PhaseKind::Validate;
        job.job_input = JobInput::candidate_subject("base", "head");
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
        job.step_id = "review_candidate_initial".into();
        job.phase_kind = PhaseKind::Review;
        job.job_input = JobInput::candidate_subject("base", "head");
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
        job.step_id = "validate_candidate_initial".into();
        job.phase_kind = PhaseKind::Validate;
        job.job_input = JobInput::candidate_subject("base", "head");
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
        job.step_id = "review_candidate_initial".into();
        job.phase_kind = PhaseKind::Review;
        job.job_input = JobInput::candidate_subject("base", "head");
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
        job.step_id = "investigate_item".into();
        job.phase_kind = PhaseKind::Investigate;
        job.job_input = JobInput::candidate_subject("base", "head");
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
        .job_input(JobInput::candidate_subject("base", "head"))
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
}
