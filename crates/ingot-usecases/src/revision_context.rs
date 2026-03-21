use chrono::{DateTime, Utc};
use ingot_domain::commit_oid::CommitOid;
use ingot_domain::item::Item;
use ingot_domain::job::{Job, JobStatus, OutputArtifactKind, PhaseKind};
use ingot_domain::revision::ItemRevision;
use ingot_domain::revision_context::{
    RevisionContext, RevisionContextAcceptedResultRef, RevisionContextPayload,
    RevisionContextResultSummary,
};

pub fn rebuild_revision_context(
    item: &Item,
    revision: &ItemRevision,
    jobs: &[Job],
    authoring_head_commit_oid: Option<CommitOid>,
    changed_paths: Vec<String>,
    updated_from_job_id: Option<ingot_domain::ids::JobId>,
    updated_at: DateTime<Utc>,
) -> RevisionContext {
    let revision_jobs = jobs
        .iter()
        .filter(|job| job.item_revision_id == revision.id)
        .cloned()
        .collect::<Vec<_>>();

    RevisionContext {
        item_revision_id: revision.id,
        schema_version: "revision_context:v1".into(),
        payload: RevisionContextPayload {
            authoring_head_commit_oid,
            changed_paths,
            latest_validation: latest_summary(&revision_jobs, PhaseKind::Validate),
            latest_review: latest_summary(&revision_jobs, PhaseKind::Review),
            accepted_result_refs: accepted_result_refs(&revision_jobs),
            operator_notes_excerpt: item.operator_notes.as_deref().map(excerpt_operator_notes),
        },
        updated_from_job_id,
        updated_at,
    }
}

fn latest_summary(jobs: &[Job], phase_kind: PhaseKind) -> Option<RevisionContextResultSummary> {
    structured_result_jobs(jobs)
        .filter(|job| job.phase_kind == phase_kind)
        .max_by_key(|job| (job.state.ended_at(), job.created_at))
        .and_then(summary_from_job)
}

fn accepted_result_refs(jobs: &[Job]) -> Vec<RevisionContextAcceptedResultRef> {
    let mut jobs = structured_result_jobs(jobs).collect::<Vec<_>>();
    jobs.sort_by_key(|job| (job.state.ended_at(), job.created_at));
    jobs.into_iter()
        .filter_map(|job| {
            let outcome = job.state.outcome_class()?;
            let summary = job
                .state
                .result_payload()
                .and_then(|payload| payload.get("summary"))
                .and_then(|value| value.as_str())?;
            let schema_version = job.state.result_schema_version()?;

            Some(RevisionContextAcceptedResultRef {
                job_id: job.id.to_string(),
                step_id: job.step_id,
                schema_version: schema_version.to_owned(),
                outcome: outcome_name(outcome).into(),
                summary: summary.into(),
            })
        })
        .collect()
}

fn summary_from_job(job: &Job) -> Option<RevisionContextResultSummary> {
    Some(RevisionContextResultSummary {
        job_id: job.id.to_string(),
        schema_version: job.state.result_schema_version()?.to_owned(),
        outcome: outcome_name(job.state.outcome_class()?).into(),
        summary: job.state.result_payload()?.get("summary")?.as_str()?.into(),
    })
}

fn structured_result_jobs(jobs: &[Job]) -> impl Iterator<Item = &Job> {
    jobs.iter().filter(|job| {
        job.state.status() == JobStatus::Completed
            && matches!(
                job.output_artifact_kind,
                OutputArtifactKind::ReviewReport
                    | OutputArtifactKind::ValidationReport
                    | OutputArtifactKind::FindingReport
            )
            && job.state.result_schema_version().is_some()
            && job.state.result_payload().is_some()
    })
}

fn outcome_name(outcome: ingot_domain::job::OutcomeClass) -> &'static str {
    match outcome {
        ingot_domain::job::OutcomeClass::Clean => "clean",
        ingot_domain::job::OutcomeClass::Findings => "findings",
        ingot_domain::job::OutcomeClass::TransientFailure => "transient_failure",
        ingot_domain::job::OutcomeClass::TerminalFailure => "terminal_failure",
        ingot_domain::job::OutcomeClass::ProtocolViolation => "protocol_violation",
        ingot_domain::job::OutcomeClass::Cancelled => "cancelled",
    }
}

fn excerpt_operator_notes(notes: &str) -> String {
    const MAX_CHARS: usize = 500;
    notes.chars().take(MAX_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use ingot_domain::ids::{ItemId, ItemRevisionId};
    use ingot_domain::item::{
        ApprovalState, Classification, Escalation, Item, Lifecycle, Origin, ParkingState, Priority,
    };
    use ingot_domain::revision::{ApprovalPolicy, AuthoringBaseSeed, ItemRevision};
    use serde_json::json;
    use uuid::Uuid;

    use super::rebuild_revision_context;

    #[test]
    fn revision_context_keeps_unbound_authoring_head_null() {
        let item_id = ItemId::from_uuid(Uuid::nil());
        let revision_id = ItemRevisionId::from_uuid(Uuid::nil());
        let now = Utc::now();
        let item = Item {
            id: item_id,
            project_id: ingot_domain::ids::ProjectId::from_uuid(Uuid::nil()),
            classification: Classification::Change,
            workflow_version: "delivery:v1".into(),
            lifecycle: Lifecycle::Open,
            parking_state: ParkingState::Active,
            approval_state: ApprovalState::NotRequested,
            escalation: Escalation::None,
            current_revision_id: revision_id,
            origin: Origin::Manual,
            priority: Priority::Major,
            labels: vec![],
            operator_notes: None,
            created_at: now,
            updated_at: now,
        };
        let revision = ItemRevision {
            id: revision_id,
            item_id,
            revision_no: 1,
            title: "Title".into(),
            description: "Desc".into(),
            acceptance_criteria: "AC".into(),
            target_ref: "refs/heads/main".into(),
            approval_policy: ApprovalPolicy::Required,
            policy_snapshot: json!({}),
            template_map_snapshot: json!({}),
            seed: AuthoringBaseSeed::Implicit {
                seed_target_commit_oid: "target".into(),
            },
            supersedes_revision_id: None,
            created_at: now,
        };

        let context = rebuild_revision_context(&item, &revision, &[], None, vec![], None, now);
        assert_eq!(context.payload.authoring_head_commit_oid, None);
    }
}
