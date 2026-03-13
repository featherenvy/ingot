use chrono::{DateTime, Utc};
use ingot_domain::item::Item;
use ingot_domain::job::{Job, JobStatus, OutputArtifactKind, PhaseKind};
use ingot_domain::revision::ItemRevision;
use ingot_domain::revision_context::{
    RevisionContext, RevisionContextAcceptedResultRef, RevisionContextResultSummary,
};

pub fn rebuild_revision_context(
    item: &Item,
    revision: &ItemRevision,
    jobs: &[Job],
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
        payload: serde_json::json!({
            "authoring_head_commit_oid": current_authoring_head(&revision_jobs, revision),
            "changed_paths": changed_paths,
            "latest_validation": latest_summary(&revision_jobs, PhaseKind::Validate),
            "latest_review": latest_summary(&revision_jobs, PhaseKind::Review),
            "accepted_result_refs": accepted_result_refs(&revision_jobs),
            "operator_notes_excerpt": item.operator_notes.as_deref().map(excerpt_operator_notes),
        }),
        updated_from_job_id,
        updated_at,
    }
}

fn latest_summary(jobs: &[Job], phase_kind: PhaseKind) -> Option<RevisionContextResultSummary> {
    structured_result_jobs(jobs)
        .filter(|job| job.phase_kind == phase_kind)
        .max_by_key(|job| (job.ended_at, job.created_at))
        .and_then(summary_from_job)
}

fn accepted_result_refs(jobs: &[Job]) -> Vec<RevisionContextAcceptedResultRef> {
    let mut jobs = structured_result_jobs(jobs).collect::<Vec<_>>();
    jobs.sort_by_key(|job| (job.ended_at, job.created_at));
    jobs.into_iter()
        .filter_map(|job| {
            let outcome = job.outcome_class?;
            let summary = job
                .result_payload
                .as_ref()
                .and_then(|payload| payload.get("summary"))
                .and_then(|value| value.as_str())?;
            let schema_version = job.result_schema_version.as_ref()?;

            Some(RevisionContextAcceptedResultRef {
                job_id: job.id.to_string(),
                step_id: job.step_id.clone(),
                schema_version: schema_version.clone(),
                outcome: outcome_name(outcome).into(),
                summary: summary.into(),
            })
        })
        .collect()
}

fn summary_from_job(job: &Job) -> Option<RevisionContextResultSummary> {
    Some(RevisionContextResultSummary {
        job_id: job.id.to_string(),
        schema_version: job.result_schema_version.clone()?,
        outcome: outcome_name(job.outcome_class?).into(),
        summary: job
            .result_payload
            .as_ref()?
            .get("summary")?
            .as_str()?
            .into(),
    })
}

fn structured_result_jobs(jobs: &[Job]) -> impl Iterator<Item = &Job> {
    jobs.iter().filter(|job| {
        job.status == JobStatus::Completed
            && matches!(
                job.output_artifact_kind,
                OutputArtifactKind::ReviewReport
                    | OutputArtifactKind::ValidationReport
                    | OutputArtifactKind::FindingReport
            )
            && job.result_schema_version.is_some()
            && job.result_payload.is_some()
    })
}

fn current_authoring_head(jobs: &[Job], revision: &ItemRevision) -> String {
    jobs.iter()
        .filter(|job| job.status == JobStatus::Completed)
        .filter(|job| job.output_artifact_kind == OutputArtifactKind::Commit)
        .filter_map(|job| {
            job.output_commit_oid
                .as_ref()
                .map(|commit_oid| ((job.ended_at, job.created_at), commit_oid.clone()))
        })
        .max_by_key(|(sort_key, _)| *sort_key)
        .map(|(_, commit_oid)| commit_oid)
        .unwrap_or_else(|| revision.seed_commit_oid.clone())
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
