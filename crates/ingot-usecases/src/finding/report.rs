use std::collections::HashSet;

use chrono::Utc;
use ingot_agent_protocol::report::{
    self, FindingReportV1, FindingV1, InvestigationReportV1, ReviewReportV1, ValidationCheckV1,
    ValidationReportV1,
};
use ingot_domain::convergence::{Convergence, ConvergenceStatus};
use ingot_domain::finding::{Finding, FindingSubjectKind, FindingTriage};
use ingot_domain::ids::FindingId;
use ingot_domain::item::Item;
use ingot_domain::job::{Job, OutcomeClass, PhaseKind};
use ingot_domain::step_id::StepId;

use crate::UseCaseError;

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
        report::VALIDATION_REPORT_V1 => {
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
        report::REVIEW_REPORT_V1 => {
            let report: ReviewReportV1 = serde_json::from_value(result_payload.clone())
                .map_err(|err| UseCaseError::ProtocolViolation(err.to_string()))?;
            let outcome_class =
                validate_review_report(&report.outcome, &report.findings, &report.summary)?;

            if job.job_input.base_commit_oid() != Some(&report.review_subject.base_commit_oid)
                || job.job_input.head_commit_oid() != Some(&report.review_subject.head_commit_oid)
            {
                return Err(UseCaseError::ProtocolViolation(
                    "review subject does not match job input commits".into(),
                ));
            }

            let _ = report.overall_risk;
            (outcome_class, report.findings)
        }
        report::FINDING_REPORT_V1 => {
            let report: FindingReportV1 = serde_json::from_value(result_payload.clone())
                .map_err(|err| UseCaseError::ProtocolViolation(err.to_string()))?;
            let outcome_class =
                validate_finding_report(&report.outcome, &report.findings, &report.summary)?;
            (outcome_class, report.findings)
        }
        report::INVESTIGATION_REPORT_V1 => {
            let report: InvestigationReportV1 = serde_json::from_value(result_payload.clone())
                .map_err(|err| UseCaseError::ProtocolViolation(err.to_string()))?;
            let standard_findings: Vec<FindingV1> = report
                .findings
                .into_iter()
                .map(|f| FindingV1 {
                    finding_key: f.finding_key,
                    code: f.code,
                    severity: f.severity,
                    summary: f.summary,
                    paths: f.paths,
                    evidence: f.evidence,
                })
                .collect();
            let outcome_class =
                validate_finding_report(&report.outcome, &standard_findings, &report.summary)?;
            (outcome_class, standard_findings)
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
                source_step_id: job.step_id,
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
                created_at,
                triage: FindingTriage::Untriaged,
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
    if job.step_id == StepId::ValidateIntegrated {
        return FindingSubjectKind::Integrated;
    }

    if !matches!(job.phase_kind, PhaseKind::Review | PhaseKind::Investigate) {
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
