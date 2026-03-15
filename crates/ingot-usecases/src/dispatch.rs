use chrono::Utc;
use ingot_domain::activity::{Activity, ActivityEventType};
use ingot_domain::convergence::Convergence;
use ingot_domain::finding::Finding;
use ingot_domain::ids::ActivityId;
use ingot_domain::item::{EscalationReason, Item};
use ingot_domain::job::{Job, JobInput, JobStatus, OutcomeClass, OutputArtifactKind};
use ingot_domain::ports::{ActivityRepository, JobRepository, WorkspaceRepository};
use ingot_domain::project::Project;
use ingot_domain::revision::ItemRevision;
use ingot_domain::workspace::Workspace;
use ingot_workflow::{ClosureRelevance, Evaluator, step};

use crate::UseCaseError;
use crate::job::{DispatchJobCommand, dispatch_job};

/// Returns true for steps that need candidate subjects filled from workspace history.
pub fn should_fill_candidate_subject_from_workspace(step_id: &str) -> bool {
    matches!(
        step_id,
        step::REVIEW_INCREMENTAL_INITIAL
            | step::REVIEW_CANDIDATE_INITIAL
            | step::REVIEW_CANDIDATE_REPAIR
            | step::VALIDATE_CANDIDATE_INITIAL
            | step::VALIDATE_CANDIDATE_REPAIR
            | step::REVIEW_AFTER_INTEGRATION_REPAIR
            | step::VALIDATE_AFTER_INTEGRATION_REPAIR
            | step::INVESTIGATE_ITEM
    )
}

/// Returns the most recent output commit OID for a revision from completed authoring jobs,
/// falling back to the revision's seed commit OID.
pub fn current_authoring_head_for_revision(
    jobs: &[Job],
    revision: &ItemRevision,
) -> Option<String> {
    jobs.iter()
        .filter(|job| job.item_revision_id == revision.id)
        .filter(|job| job.status == JobStatus::Completed)
        .filter(|job| job.output_artifact_kind == OutputArtifactKind::Commit)
        .filter_map(|job| {
            job.output_commit_oid
                .as_ref()
                .map(|commit_oid| ((job.ended_at, job.created_at), commit_oid.clone()))
        })
        .max_by_key(|(sort_key, _)| *sort_key)
        .map(|(_, commit_oid)| commit_oid)
        .or_else(|| revision.seed_commit_oid.clone())
}

/// Returns the effective authoring head for a revision, considering both completed jobs
/// and the authoring workspace's head commit.
pub fn current_authoring_head_for_revision_with_workspace(
    revision: &ItemRevision,
    jobs: &[Job],
    workspace: Option<&Workspace>,
) -> Option<String> {
    if let Some(commit_oid) = current_authoring_head_for_revision(jobs, revision) {
        return Some(commit_oid);
    }

    workspace.and_then(|ws| ws.head_commit_oid.clone())
}

/// Returns the effective authoring base commit OID for a revision, using the seed commit
/// if available, otherwise the authoring workspace's base commit.
pub fn effective_authoring_base_commit_oid(
    revision: &ItemRevision,
    workspace: Option<&Workspace>,
) -> Option<String> {
    if let Some(seed_commit_oid) = revision.seed_commit_oid.clone() {
        return Some(seed_commit_oid);
    }

    workspace.and_then(|ws| ws.base_commit_oid.clone())
}

/// Returns true if the job's step is closure-relevant (i.e., failures on it should escalate).
pub fn is_closure_relevant_job(job: &Job) -> bool {
    matches!(
        step::find_step(&job.step_id).map(|step| step.closure_relevance),
        Some(ClosureRelevance::ClosureRelevant)
    )
}

/// Returns the escalation reason for a job failure, if applicable.
pub fn failure_escalation_reason(
    job: &Job,
    outcome_class: OutcomeClass,
) -> Option<EscalationReason> {
    if !is_closure_relevant_job(job) {
        return None;
    }

    match outcome_class {
        OutcomeClass::TerminalFailure => Some(EscalationReason::StepFailed),
        OutcomeClass::ProtocolViolation => Some(EscalationReason::ProtocolViolation),
        OutcomeClass::Clean
        | OutcomeClass::Findings
        | OutcomeClass::TransientFailure
        | OutcomeClass::Cancelled => None,
    }
}

/// Maps an outcome class to the terminal job status for failure endpoints.
/// Returns None for outcome classes that are not valid failures (Clean, Findings).
pub fn failure_status(outcome_class: OutcomeClass) -> Option<JobStatus> {
    match outcome_class {
        OutcomeClass::TransientFailure
        | OutcomeClass::TerminalFailure
        | OutcomeClass::ProtocolViolation => Some(JobStatus::Failed),
        OutcomeClass::Cancelled => Some(JobStatus::Cancelled),
        OutcomeClass::Clean | OutcomeClass::Findings => None,
    }
}

/// Returns true if we should clear an item's escalation after a successful retry.
pub fn should_clear_item_escalation_on_success(item: &Item, job: &Job) -> bool {
    item.escalation.is_escalated() && job.retry_no > 0 && is_closure_relevant_job(job)
}

/// Auto-dispatch a closure-relevant review job if the evaluator recommends one.
///
/// Requires pre-hydrated convergences (with `target_head_valid` set) and pre-loaded entity state.
/// Fills candidate subject from workspace/job history. Creates and persists the job.
///
/// Returns `Some(job)` if a review was dispatched, `None` if not dispatchable.
/// Does NOT handle workspace provisioning or investigation refs — callers do that.
#[allow(clippy::too_many_arguments)]
pub async fn auto_dispatch_review<J, W, A>(
    job_repo: &J,
    workspace_repo: &W,
    activity_repo: &A,
    project: &Project,
    item: &Item,
    revision: &ItemRevision,
    jobs: &[Job],
    findings: &[Finding],
    convergences: &[Convergence],
) -> Result<Option<Job>, UseCaseError>
where
    J: JobRepository,
    W: WorkspaceRepository,
    A: ActivityRepository,
{
    let evaluation = Evaluator::new().evaluate(item, revision, jobs, findings, convergences);
    let Some(step_id) = evaluation.dispatchable_step_id.as_deref() else {
        return Ok(None);
    };

    if !step::is_closure_relevant_review_step(step_id) {
        return Ok(None);
    }

    let mut job = dispatch_job(
        item,
        revision,
        jobs,
        findings,
        convergences,
        DispatchJobCommand {
            step_id: Some(step_id.to_string()),
        },
    )?;

    // Fill candidate subject from workspace history if needed
    if should_fill_candidate_subject_from_workspace(&job.step_id) {
        let authoring_workspace = workspace_repo
            .find_authoring_for_revision(revision.id)
            .await?;
        let mut base = job.job_input.base_commit_oid().map(ToOwned::to_owned);
        let mut head = job.job_input.head_commit_oid().map(ToOwned::to_owned);
        if base.is_none() {
            base = effective_authoring_base_commit_oid(revision, authoring_workspace.as_ref());
        }
        if head.is_none() {
            head = current_authoring_head_for_revision_with_workspace(
                revision,
                jobs,
                authoring_workspace.as_ref(),
            );
        }
        match (base, head) {
            (Some(base), Some(head)) => {
                job.job_input = JobInput::candidate_subject(base, head);
            }
            _ => {
                return Err(UseCaseError::Internal(format!(
                    "incomplete candidate subject for auto-dispatched review {}",
                    job.step_id
                )));
            }
        }
    }

    job_repo.create(&job).await?;
    activity_repo
        .append(&Activity {
            id: ActivityId::new(),
            project_id: project.id,
            event_type: ActivityEventType::JobDispatched,
            entity_type: "job".into(),
            entity_id: job.id.to_string(),
            payload: serde_json::json!({ "item_id": item.id, "step_id": job.step_id }),
            created_at: Utc::now(),
        })
        .await?;

    Ok(Some(job))
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use ingot_domain::ids::{ItemId, ItemRevisionId, ProjectId};
    use ingot_domain::job::OutputArtifactKind;
    use ingot_test_support::fixtures::{JobBuilder, RevisionBuilder};
    use uuid::Uuid;

    use super::*;

    #[test]
    fn authoring_head_from_latest_completed_commit_job() {
        let item_id = ItemId::from_uuid(Uuid::nil());
        let revision_id = ItemRevisionId::from_uuid(Uuid::nil());
        let project_id = ProjectId::from_uuid(Uuid::nil());
        let now = Utc::now();
        let revision = RevisionBuilder::new(item_id)
            .id(revision_id)
            .explicit_seed("seed")
            .created_at(now)
            .build();
        let job = JobBuilder::new(project_id, item_id, revision_id, "author_initial")
            .status(ingot_domain::job::JobStatus::Completed)
            .outcome_class(ingot_domain::job::OutcomeClass::Clean)
            .output_artifact_kind(OutputArtifactKind::Commit)
            .output_commit_oid("abc123")
            .created_at(now)
            .started_at(now)
            .ended_at(now)
            .build();

        assert_eq!(
            current_authoring_head_for_revision(&[job], &revision),
            Some("abc123".into())
        );
    }

    #[test]
    fn authoring_head_falls_back_to_seed_commit() {
        let item_id = ItemId::from_uuid(Uuid::nil());
        let revision_id = ItemRevisionId::from_uuid(Uuid::nil());
        let now = Utc::now();
        let revision = RevisionBuilder::new(item_id)
            .id(revision_id)
            .explicit_seed("seed")
            .created_at(now)
            .build();

        assert_eq!(
            current_authoring_head_for_revision(&[], &revision),
            Some("seed".into())
        );
    }

    #[test]
    fn should_fill_is_true_for_review_steps() {
        assert!(should_fill_candidate_subject_from_workspace(
            "review_incremental_initial"
        ));
        assert!(should_fill_candidate_subject_from_workspace(
            "investigate_item"
        ));
    }

    #[test]
    fn should_fill_is_false_for_authoring_steps() {
        assert!(!should_fill_candidate_subject_from_workspace(
            "author_initial"
        ));
    }
}
