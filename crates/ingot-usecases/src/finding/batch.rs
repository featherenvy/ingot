use ingot_domain::finding::Finding;
use ingot_domain::ids::FindingId;
use ingot_domain::item::Item;
use ingot_domain::job::Job;
use ingot_domain::revision::ItemRevision;

use crate::UseCaseError;

use super::triage::{
    BacklogFindingOverrides, backlog_finding_with_promotion, promotion_overrides_for_finding,
};

#[derive(Debug, Clone)]
pub struct BatchPromoteInput {
    pub finding_ids: Vec<FindingId>,
}

#[derive(Debug)]
pub struct BatchPromoteResult {
    pub finding_id: FindingId,
    pub linked_item: Item,
    pub linked_revision: ItemRevision,
    pub triaged_finding: Finding,
}

#[derive(Debug)]
pub struct BatchPromoteSkipped {
    pub finding_id: FindingId,
    pub reason: String,
}

#[derive(Debug)]
pub struct BatchPromoteOutput {
    pub promoted: Vec<BatchPromoteResult>,
    pub skipped: Vec<BatchPromoteSkipped>,
}

/// Batch-promote findings from an investigation report into delivery items.
///
/// This is a pure function -- the caller must pre-load all data and persist
/// results afterward. For each finding_id in the input:
///
/// 1. Locate the finding in `findings`.
/// 2. If the source report is `investigation_report:v1`, extract promotion
///    metadata from the source job's `result_payload` by matching
///    `source_finding_key`.
/// 3. Call `backlog_finding_with_promotion()` with the extracted overrides.
pub fn batch_promote_findings(
    findings: &[Finding],
    source_item: &Item,
    source_revision: &ItemRevision,
    source_jobs: &[Job],
    input: BatchPromoteInput,
    mut sort_key_fn: impl FnMut() -> String,
) -> Result<BatchPromoteOutput, UseCaseError> {
    let mut promoted = Vec::new();
    let mut skipped = Vec::new();

    for finding_id in &input.finding_ids {
        let Some(finding) = findings.iter().find(|f| f.id == *finding_id) else {
            skipped.push(BatchPromoteSkipped {
                finding_id: *finding_id,
                reason: "finding not found".into(),
            });
            continue;
        };

        if !finding.triage.is_unresolved() {
            skipped.push(BatchPromoteSkipped {
                finding_id: *finding_id,
                reason: "finding already triaged".into(),
            });
            continue;
        }

        let promotion_overrides = promotion_overrides_for_finding(finding, source_jobs);
        let sort_key = sort_key_fn();

        match backlog_finding_with_promotion(
            finding,
            source_item,
            source_revision,
            BacklogFindingOverrides::default(),
            sort_key,
            None,
            promotion_overrides,
        ) {
            Ok((linked_item, linked_revision, triaged_finding)) => {
                promoted.push(BatchPromoteResult {
                    finding_id: *finding_id,
                    linked_item,
                    linked_revision,
                    triaged_finding,
                });
            }
            Err(error) => {
                skipped.push(BatchPromoteSkipped {
                    finding_id: *finding_id,
                    reason: error.to_string(),
                });
            }
        }
    }

    Ok(BatchPromoteOutput { promoted, skipped })
}
