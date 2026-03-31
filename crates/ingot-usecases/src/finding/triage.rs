use chrono::Utc;
use ingot_agent_protocol::report::{self, InvestigationReportV1};
use ingot_domain::commit_oid::CommitOid;
use ingot_domain::finding::{Finding, FindingSubjectKind, FindingTriageState};
use ingot_domain::git_ref::GitRef;
use ingot_domain::ids::{ItemId, ItemRevisionId};
use ingot_domain::item::{
    Classification, Escalation, Item, Lifecycle, Origin, ParkingState, WorkflowVersion,
};
use ingot_domain::job::Job;
use ingot_domain::revision::{ApprovalPolicy, AuthoringBaseSeed, ItemRevision};

use crate::UseCaseError;
use crate::item::{
    approval_state_for_policy, default_policy_snapshot, default_template_map_snapshot,
};

#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct BacklogFindingOverrides {
    pub target_ref: Option<GitRef>,
    pub approval_policy: Option<ApprovalPolicy>,
}

#[derive(Debug, Clone, Default)]
pub struct PromotionOverrides {
    pub title: Option<String>,
    pub description: Option<String>,
    pub acceptance_criteria: Option<String>,
    pub classification: Option<Classification>,
    pub workflow_version: Option<WorkflowVersion>,
}

#[derive(Debug, Clone)]
pub struct TriageFindingInput {
    pub triage_state: FindingTriageState,
    pub triage_note: Option<String>,
    pub linked_item_id: Option<ItemId>,
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
    let triaged_at = Utc::now();

    match input.triage_state {
        FindingTriageState::FixNow => {
            ensure_note_absent(&triage_note, "fix_now")?;
            ensure_link_absent(input.linked_item_id, "fix_now")?;
        }
        FindingTriageState::WontFix => {
            ensure_link_absent(input.linked_item_id, "wont_fix")?;
        }
        FindingTriageState::DismissedInvalid => {
            ensure_link_absent(input.linked_item_id, "dismissed_invalid")?;
        }
        FindingTriageState::NeedsInvestigation => {
            ensure_link_absent(input.linked_item_id, "needs_investigation")?;
        }
        FindingTriageState::Backlog | FindingTriageState::Duplicate => {}
        FindingTriageState::Untriaged => unreachable!("handled above"),
    }

    let triage = ingot_domain::finding::FindingTriage::try_from_parts(
        input.triage_state,
        input.linked_item_id,
        triage_note,
        Some(triaged_at),
        |state, field| {
            UseCaseError::InvalidFindingTriage(format!(
                "{} triage requires a {field}",
                state.as_str()
            ))
        },
    )?;

    let mut triaged = finding.clone();
    triaged.triage = triage;
    Ok(triaged)
}

pub fn backlog_finding(
    finding: &Finding,
    source_item: &Item,
    source_revision: &ItemRevision,
    overrides: BacklogFindingOverrides,
    sort_key: String,
    triage_note: Option<String>,
) -> Result<(Item, ItemRevision, Finding), UseCaseError> {
    backlog_finding_with_promotion(
        finding,
        source_item,
        source_revision,
        overrides,
        sort_key,
        triage_note,
        None,
    )
}

pub fn backlog_finding_with_promotion(
    finding: &Finding,
    source_item: &Item,
    source_revision: &ItemRevision,
    overrides: BacklogFindingOverrides,
    sort_key: String,
    triage_note: Option<String>,
    promotion: Option<PromotionOverrides>,
) -> Result<(Item, ItemRevision, Finding), UseCaseError> {
    if !finding.triage.is_unresolved() {
        return Err(UseCaseError::FindingNotTriageable);
    }

    if finding
        .source_subject_head_commit_oid
        .as_str()
        .trim()
        .is_empty()
        || (finding.source_subject_kind == FindingSubjectKind::Integrated
            && finding
                .source_subject_base_commit_oid
                .as_ref()
                .map(CommitOid::as_str)
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

    let promotion = promotion.unwrap_or_default();
    let linked_classification = promotion.classification.unwrap_or(Classification::Bug);
    let linked_workflow_version = promotion
        .workflow_version
        .unwrap_or(WorkflowVersion::DeliveryV1);

    // When promoting to a different workflow version than the source, use the
    // default policy/template snapshots for the target workflow.
    let (policy_snapshot, template_map_snapshot) =
        if linked_workflow_version != source_item.workflow_version {
            (
                default_policy_snapshot(approval_policy, 0, 0),
                default_template_map_snapshot(),
            )
        } else {
            (
                source_revision.policy_snapshot.clone(),
                source_revision.template_map_snapshot.clone(),
            )
        };

    let linked_item = Item {
        id: item_id,
        project_id: source_item.project_id,
        classification: linked_classification,
        workflow_version: linked_workflow_version,
        lifecycle: Lifecycle::Open,
        parking_state: ParkingState::Active,
        approval_state: approval_state_for_policy(approval_policy),
        escalation: Escalation::None,
        current_revision_id: revision_id,
        origin: Origin::PromotedFinding {
            finding_id: finding.id,
        },
        priority: source_item.priority,
        labels: vec![],
        operator_notes: None,
        sort_key,
        created_at,
        updated_at: created_at,
    };

    let default_description = if evidence_lines.is_empty() {
        finding.summary.clone()
    } else {
        format!(
            "{}\n\nEvidence:\n{}",
            finding.summary,
            evidence_lines.join("\n")
        )
    };

    let linked_revision = ItemRevision {
        id: revision_id,
        item_id,
        revision_no: 1,
        title: promotion.title.unwrap_or_else(|| finding.summary.clone()),
        description: promotion.description.unwrap_or(default_description),
        acceptance_criteria: promotion.acceptance_criteria.unwrap_or_else(|| {
            format!(
                "Resolve finding {} and validate that it no longer reproduces.",
                finding.code
            )
        }),
        target_ref: overrides
            .target_ref
            .unwrap_or_else(|| source_revision.target_ref.clone()),
        approval_policy,
        policy_snapshot,
        template_map_snapshot,
        seed: AuthoringBaseSeed::Explicit {
            seed_commit_oid: finding.source_subject_head_commit_oid.clone(),
            seed_target_commit_oid: match finding.source_subject_kind {
                FindingSubjectKind::Integrated => finding
                    .source_subject_base_commit_oid
                    .clone()
                    .unwrap_or_else(|| finding.source_subject_head_commit_oid.clone()),
                FindingSubjectKind::Candidate => {
                    source_revision.seed.seed_target_commit_oid().to_owned()
                }
            },
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

/// Extract promotion overrides from the source job's result payload when it is
/// an investigation report and contains a matching finding entry.
pub fn promotion_overrides_for_finding(
    finding: &Finding,
    source_jobs: &[Job],
) -> Option<PromotionOverrides> {
    if let Some(investigation) = finding.investigation.as_ref() {
        return Some(PromotionOverrides {
            title: Some(investigation.promotion.title.clone()),
            description: Some(investigation.promotion.description.clone()),
            acceptance_criteria: Some(investigation.promotion.acceptance_criteria.clone()),
            classification: Some(investigation.promotion.classification),
            workflow_version: Some(WorkflowVersion::DeliveryV1),
        });
    }

    let source_job = source_jobs
        .iter()
        .find(|job| job.id == finding.source_job_id)?;
    let schema_version = source_job.state.result_schema_version()?;

    if schema_version != report::INVESTIGATION_REPORT_V1 {
        return None;
    }

    let result_payload = source_job.state.result_payload()?;
    let report: InvestigationReportV1 = serde_json::from_value(result_payload.clone()).ok()?;

    let investigation_finding = report
        .findings
        .into_iter()
        .find(|candidate| candidate.finding_key == finding.source_finding_key)?;

    let classification = match investigation_finding.promotion.classification {
        report::InvestigationClassification::Change => Classification::Change,
        report::InvestigationClassification::Bug => Classification::Bug,
    };

    Some(PromotionOverrides {
        title: Some(investigation_finding.promotion.title),
        description: Some(investigation_finding.promotion.description),
        acceptance_criteria: Some(investigation_finding.promotion.acceptance_criteria),
        classification: Some(classification),
        workflow_version: Some(WorkflowVersion::DeliveryV1),
    })
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
