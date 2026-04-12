use chrono::Utc;
use ingot_domain::activity::{Activity, ActivityEventType, ActivitySubject};
use ingot_domain::finding::{Finding, FindingSeverity, FindingTriageState};
use ingot_domain::ids::{ActivityId, JobId};
use ingot_domain::item::{ApprovalState, Item};
use ingot_domain::ports::{
    ActivityRepository, FindingRepository, ItemRepository, RevisionRepository,
};
use ingot_domain::project::{AutoTriageDecision, AutoTriagePolicy, Project};
use ingot_domain::revision::ItemRevision;
use ingot_domain::step_id::StepId;
use tracing::info;

use crate::UseCaseError;

use super::triage::{
    BacklogFindingOverrides, TriageFindingInput, backlog_finding_with_promotion,
    promotion_overrides_for_finding, triage_finding,
};

#[derive(Debug)]
pub struct AutoTriagedFinding {
    pub finding: Finding,
    pub backlog: Option<(Item, ItemRevision)>,
    pub launch_backlog_item: bool,
}

pub fn auto_triage_findings(
    findings: &[Finding],
    policy: &AutoTriagePolicy,
    source_item: &Item,
    source_revision: &ItemRevision,
    existing_items: &[Item],
) -> Result<Vec<AutoTriagedFinding>, UseCaseError> {
    let mut results = Vec::new();
    let mut last_sort_key = existing_items
        .iter()
        .max_by_key(|item| &item.sort_key)
        .map(|item| item.sort_key.clone());

    for finding in findings.iter().filter(|f| f.triage.is_unresolved()) {
        let decision = policy.decision_for(finding.severity);
        match decision {
            AutoTriageDecision::FixNow => {
                if finding.investigation.is_some() {
                    let sort_key = crate::item::next_sort_key_after(last_sort_key.as_deref());
                    let severity_label = match finding.severity {
                        FindingSeverity::Critical => "critical",
                        FindingSeverity::High => "high",
                        FindingSeverity::Medium => "medium",
                        FindingSeverity::Low => "low",
                    };
                    let promotion_overrides = promotion_overrides_for_finding(finding, &[]);
                    let (linked_item, linked_revision, triaged) = backlog_finding_with_promotion(
                        finding,
                        source_item,
                        source_revision,
                        BacklogFindingOverrides::default(),
                        sort_key.clone(),
                        Some(format!("auto-triaged: {severity_label} severity")),
                        promotion_overrides,
                    )?;
                    last_sort_key = Some(sort_key);
                    results.push(AutoTriagedFinding {
                        finding: triaged,
                        backlog: Some((linked_item, linked_revision)),
                        launch_backlog_item: true,
                    });
                } else {
                    let triaged = triage_finding(
                        finding,
                        TriageFindingInput {
                            triage_state: FindingTriageState::FixNow,
                            triage_note: None,
                            linked_item_id: None,
                        },
                    )?;
                    results.push(AutoTriagedFinding {
                        finding: triaged,
                        backlog: None,
                        launch_backlog_item: false,
                    });
                }
            }
            AutoTriageDecision::Backlog => {
                let sort_key = crate::item::next_sort_key_after(last_sort_key.as_deref());
                let severity_label = match finding.severity {
                    FindingSeverity::Critical => "critical",
                    FindingSeverity::High => "high",
                    FindingSeverity::Medium => "medium",
                    FindingSeverity::Low => "low",
                };
                let promotion_overrides = promotion_overrides_for_finding(finding, &[]);
                let (linked_item, linked_revision, triaged) = backlog_finding_with_promotion(
                    finding,
                    source_item,
                    source_revision,
                    BacklogFindingOverrides::default(),
                    sort_key.clone(),
                    Some(format!("auto-triaged: {severity_label} severity")),
                    promotion_overrides,
                )?;
                last_sort_key = Some(sort_key);
                results.push(AutoTriagedFinding {
                    finding: triaged,
                    backlog: Some((linked_item, linked_revision)),
                    launch_backlog_item: false,
                });
            }
            AutoTriageDecision::Skip => {}
        }
    }

    Ok(results)
}

#[allow(clippy::too_many_arguments)]
pub async fn execute_auto_triage<F, I, R, A>(
    finding_repo: &F,
    item_repo: &I,
    revision_repo: &R,
    activity_repo: &A,
    project: &Project,
    item: &Item,
    job_id: JobId,
    step_id: StepId,
    policy: &AutoTriagePolicy,
) -> Result<Vec<AutoTriagedFinding>, UseCaseError>
where
    F: FindingRepository,
    I: ItemRepository,
    R: RevisionRepository,
    A: ActivityRepository,
{
    let all_findings = finding_repo.list_by_item(item.id).await?;
    let job_findings: Vec<_> = all_findings
        .into_iter()
        .filter(|f| f.source_job_id == job_id && f.triage.is_unresolved())
        .collect();

    if job_findings.is_empty() {
        return Ok(vec![]);
    }

    let revision = revision_repo.get(item.current_revision_id).await?;
    let existing_items = item_repo.list_by_project(item.project_id).await?;

    let results = auto_triage_findings(&job_findings, policy, item, &revision, &existing_items)?;

    for result in &results {
        if let Some((ref linked_item, ref linked_revision)) = result.backlog {
            finding_repo
                .link_backlog(&result.finding, linked_item, linked_revision, None)
                .await?;
        } else {
            finding_repo.triage(&result.finding).await?;
        }

        activity_repo
            .append(&Activity {
                id: ActivityId::new(),
                project_id: project.id,
                event_type: ActivityEventType::FindingTriaged,
                subject: ActivitySubject::Finding(result.finding.id),
                payload: serde_json::json!({
                    "item_id": item.id,
                    "origin": "auto_triage",
                    "triage_state": result.finding.triage.state(),
                    "linked_item_id": result.finding.triage.linked_item_id(),
                }),
                created_at: Utc::now(),
            })
            .await?;
    }

    if step_id == StepId::ValidateIntegrated && item.current_revision_id == revision.id {
        let updated_findings = finding_repo.list_by_item(item.id).await?;
        let job_findings_after: Vec<_> = updated_findings
            .iter()
            .filter(|f| f.source_job_id == job_id && f.source_item_revision_id == revision.id)
            .collect();

        let all_resolved_non_blocking = !job_findings_after.is_empty()
            && job_findings_after.iter().all(|f| {
                !f.triage.is_unresolved() && f.triage.state() != FindingTriageState::FixNow
            });

        if all_resolved_non_blocking {
            let mut current_item = item_repo.get(item.id).await?;
            let next_approval_state = crate::item::pending_approval_state(revision.approval_policy);
            if current_item.approval_state != next_approval_state {
                current_item.approval_state = next_approval_state;
                current_item.updated_at = Utc::now();
                item_repo.update(&current_item).await?;

                if next_approval_state == ApprovalState::Pending {
                    activity_repo
                        .append(&Activity {
                            id: ActivityId::new(),
                            project_id: project.id,
                            event_type: ActivityEventType::ApprovalRequested,
                            subject: ActivitySubject::Item(item.id),
                            payload: serde_json::json!({ "source": "auto_triage" }),
                            created_at: Utc::now(),
                        })
                        .await?;
                }
            }
        }
    }

    info!(
        item_id = %item.id,
        job_id = %job_id,
        triaged_count = results.len(),
        "auto-triaged findings"
    );

    Ok(results)
}
