use ingot_domain::commit_oid::CommitOid;
use ingot_domain::convergence::{Convergence, ConvergenceStatus};
use ingot_domain::finding::Finding;
use ingot_domain::ids::JobId;
use ingot_domain::item::{Item, ParkingState};
use ingot_domain::job::{Job, JobInput, JobStatus, OutcomeClass, OutputArtifactKind};
use ingot_domain::revision::ItemRevision;
use ingot_domain::step_id::StepId;
use ingot_workflow::{Evaluation, Evaluator, step};

use crate::UseCaseError;

#[derive(Debug, Clone)]
pub struct DispatchJobCommand {
    pub step_id: Option<StepId>,
}

pub fn dispatch_job(
    item: &Item,
    revision: &ItemRevision,
    jobs: &[Job],
    findings: &[Finding],
    convergences: &[Convergence],
    command: DispatchJobCommand,
) -> Result<Job, UseCaseError> {
    ensure_item_dispatchable(item)?;
    ensure_no_active_execution(item.current_revision_id, jobs, convergences)?;

    let evaluation = Evaluator::new().evaluate(item, revision, jobs, findings, convergences);
    let step_id = select_dispatch_step(&evaluation, command.step_id)?;
    let contract = step::find_step(step_id);

    if !contract.is_dispatchable_job() {
        return Err(UseCaseError::IllegalStepDispatch(format!(
            "Step is not dispatchable: {step_id}"
        )));
    }

    let template_slug = template_slug_for_step(revision, step_id, contract.default_template_slug);
    let job_input = job_input_for_step(step_id, revision, jobs, convergences);
    let semantic_attempt_no = next_semantic_attempt_no(jobs, item.current_revision_id, step_id);

    Ok(Job {
        id: JobId::new(),
        project_id: item.project_id,
        item_id: item.id,
        item_revision_id: item.current_revision_id,
        step_id,
        semantic_attempt_no,
        retry_no: 0,
        supersedes_job_id: None,
        phase_kind: contract.phase_kind,
        workspace_kind: contract.workspace_kind,
        execution_permission: contract.execution_permission,
        context_policy: contract.context_policy,
        phase_template_slug: template_slug,
        job_input,
        output_artifact_kind: contract.output_artifact_kind,
        created_at: chrono::Utc::now(),
        state: ingot_domain::job::JobState::Queued,
    })
}

pub fn retry_job(
    item: &Item,
    revision: &ItemRevision,
    jobs: &[Job],
    findings: &[Finding],
    convergences: &[Convergence],
    previous_job: &Job,
) -> Result<Job, UseCaseError> {
    ensure_item_dispatchable(item)?;

    if previous_job.item_revision_id != item.current_revision_id {
        return Err(UseCaseError::IllegalStepDispatch(
            "Cannot retry a job from a superseded revision".into(),
        ));
    }

    ensure_no_active_execution(item.current_revision_id, jobs, convergences)?;

    if !previous_job.state.is_terminal()
        || matches!(
            previous_job.state.outcome_class(),
            Some(OutcomeClass::Clean | OutcomeClass::Findings)
        )
    {
        return Err(UseCaseError::IllegalStepDispatch(
            "Only terminal non-success jobs can be retried".into(),
        ));
    }

    let evaluation = Evaluator::new().evaluate(item, revision, jobs, findings, convergences);
    let contract = step::find_step(previous_job.step_id);

    if contract.execution_permission == ingot_domain::job::ExecutionPermission::DaemonOnly {
        return Err(UseCaseError::IllegalStepDispatch(
            "Daemon-executed jobs cannot be retried manually".into(),
        ));
    }

    let closure_position_allows_retry = evaluation.current_step_id == Some(previous_job.step_id);
    let report_only_retry = evaluation
        .auxiliary_dispatchable_step_ids
        .iter()
        .any(|step_id| step_id == &previous_job.step_id);

    if !closure_position_allows_retry && !report_only_retry {
        return Err(UseCaseError::IllegalStepDispatch(format!(
            "Step is not retryable in the current state: {}",
            previous_job.step_id
        )));
    }

    let template_slug = template_slug_for_step(
        revision,
        previous_job.step_id,
        contract.default_template_slug,
    );
    let job_input = job_input_for_step(previous_job.step_id, revision, jobs, convergences);
    let retry_no = jobs
        .iter()
        .filter(|job| job.item_revision_id == item.current_revision_id)
        .filter(|job| job.step_id == previous_job.step_id)
        .filter(|job| job.semantic_attempt_no == previous_job.semantic_attempt_no)
        .map(|job| job.retry_no)
        .max()
        .unwrap_or(previous_job.retry_no)
        + 1;

    Ok(Job {
        id: JobId::new(),
        project_id: item.project_id,
        item_id: item.id,
        item_revision_id: item.current_revision_id,
        step_id: previous_job.step_id,
        semantic_attempt_no: previous_job.semantic_attempt_no,
        retry_no,
        supersedes_job_id: Some(previous_job.id),
        phase_kind: contract.phase_kind,
        workspace_kind: contract.workspace_kind,
        execution_permission: contract.execution_permission,
        context_policy: contract.context_policy,
        phase_template_slug: template_slug,
        job_input,
        output_artifact_kind: contract.output_artifact_kind,
        created_at: chrono::Utc::now(),
        state: ingot_domain::job::JobState::Queued,
    })
}

fn select_dispatch_step(
    evaluation: &Evaluation,
    requested_step_id: Option<StepId>,
) -> Result<StepId, UseCaseError> {
    if let Some(requested_step_id) = requested_step_id {
        if evaluation.dispatchable_step_id == Some(requested_step_id)
            || evaluation
                .auxiliary_dispatchable_step_ids
                .contains(&requested_step_id)
        {
            return Ok(requested_step_id);
        }

        return Err(UseCaseError::IllegalStepDispatch(format!(
            "Step is not dispatchable in the current state: {requested_step_id}"
        )));
    }

    evaluation.dispatchable_step_id.ok_or_else(|| {
        UseCaseError::IllegalStepDispatch(
            "No closure-relevant step is dispatchable in the current state".into(),
        )
    })
}

fn template_slug_for_step(
    revision: &ItemRevision,
    step_id: StepId,
    default_template_slug: Option<&'static str>,
) -> String {
    revision
        .template_map_snapshot
        .get(step_id.as_str())
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned)
        .or_else(|| default_template_slug.map(ToOwned::to_owned))
        .unwrap_or_else(|| step_id.to_string())
}

fn job_input_for_step(
    step_id: StepId,
    revision: &ItemRevision,
    jobs: &[Job],
    convergences: &[Convergence],
) -> JobInput {
    let seed_head = revision.seed.seed_commit_oid().map(ToOwned::to_owned);
    let current_head = current_authoring_head(jobs, revision);
    let previous_head = previous_authoring_head(jobs, revision);
    let prepared_convergence = selected_prepared_convergence(revision.id, convergences);

    match step_id {
        StepId::AuthorInitial => seed_head
            .map(JobInput::authoring_head)
            .unwrap_or(JobInput::None),
        StepId::RepairCandidate | StepId::RepairAfterIntegration => current_head
            .map(JobInput::authoring_head)
            .unwrap_or(JobInput::None),
        StepId::ReviewIncrementalInitial => job_input_from_range(seed_head, current_head, false),
        StepId::ReviewIncrementalRepair | StepId::ReviewIncrementalAfterIntegrationRepair => {
            job_input_from_range(previous_head, current_head, false)
        }
        StepId::ReviewCandidateInitial
        | StepId::ReviewCandidateRepair
        | StepId::ValidateCandidateInitial
        | StepId::ValidateCandidateRepair
        | StepId::ReviewAfterIntegrationRepair
        | StepId::ValidateAfterIntegrationRepair => {
            job_input_from_range(seed_head, current_head, false)
        }
        StepId::InvestigateItem => prepared_convergence
            .map(|convergence| job_input_from_prepared_convergence(convergence, false))
            .unwrap_or_else(|| job_input_from_range(seed_head, current_head, false)),
        StepId::ValidateIntegrated => prepared_convergence
            .map(|convergence| job_input_from_prepared_convergence(convergence, true))
            .unwrap_or(JobInput::None),
        _ => JobInput::None,
    }
}

fn job_input_from_prepared_convergence(convergence: &Convergence, integrated: bool) -> JobInput {
    job_input_from_range(
        convergence
            .state
            .input_target_commit_oid()
            .map(ToOwned::to_owned),
        convergence
            .state
            .prepared_commit_oid()
            .map(ToOwned::to_owned),
        integrated,
    )
}

fn job_input_from_range(
    base_commit_oid: Option<CommitOid>,
    head_commit_oid: Option<CommitOid>,
    integrated: bool,
) -> JobInput {
    match (base_commit_oid, head_commit_oid) {
        (Some(base_commit_oid), Some(head_commit_oid)) => {
            if integrated {
                JobInput::integrated_subject(base_commit_oid, head_commit_oid)
            } else {
                JobInput::candidate_subject(base_commit_oid, head_commit_oid)
            }
        }
        _ => JobInput::None,
    }
}

fn ensure_item_dispatchable(item: &Item) -> Result<(), UseCaseError> {
    if !item.lifecycle.is_open() {
        return Err(UseCaseError::ItemNotOpen);
    }

    if item.parking_state != ParkingState::Active {
        return Err(UseCaseError::ItemNotIdle);
    }

    Ok(())
}

fn ensure_no_active_execution(
    revision_id: ingot_domain::ids::ItemRevisionId,
    jobs: &[Job],
    convergences: &[Convergence],
) -> Result<(), UseCaseError> {
    if jobs
        .iter()
        .any(|job| job.item_revision_id == revision_id && job.state.is_active())
    {
        return Err(UseCaseError::ActiveJobExists);
    }

    if convergences.iter().any(|convergence| {
        convergence.item_revision_id == revision_id
            && matches!(
                convergence.state.status(),
                ConvergenceStatus::Queued | ConvergenceStatus::Running
            )
    }) {
        return Err(UseCaseError::ActiveConvergenceExists);
    }

    Ok(())
}

fn current_authoring_head(jobs: &[Job], revision: &ItemRevision) -> Option<CommitOid> {
    successful_commit_oids(jobs, revision)
        .last()
        .cloned()
        .or_else(|| revision.seed.seed_commit_oid().map(ToOwned::to_owned))
}

fn previous_authoring_head(jobs: &[Job], revision: &ItemRevision) -> Option<CommitOid> {
    let commit_oids = successful_commit_oids(jobs, revision);
    commit_oids
        .iter()
        .rev()
        .nth(1)
        .cloned()
        .or_else(|| revision.seed.seed_commit_oid().map(ToOwned::to_owned))
}

fn successful_commit_oids(jobs: &[Job], revision: &ItemRevision) -> Vec<CommitOid> {
    let mut commit_jobs = jobs
        .iter()
        .filter(|job| job.item_revision_id == revision.id)
        .filter(|job| job.state.status() == JobStatus::Completed)
        .filter(|job| job.output_artifact_kind == OutputArtifactKind::Commit)
        .filter_map(|job| {
            job.state.output_commit_oid().map(|commit_oid| {
                (
                    (job.state.ended_at(), job.created_at),
                    commit_oid.to_owned(),
                )
            })
        })
        .collect::<Vec<_>>();

    commit_jobs.sort_by_key(|(sort_key, _)| *sort_key);
    commit_jobs
        .into_iter()
        .map(|(_, commit_oid)| commit_oid)
        .collect()
}

fn next_semantic_attempt_no(
    jobs: &[Job],
    revision_id: ingot_domain::ids::ItemRevisionId,
    step_id: StepId,
) -> u32 {
    jobs.iter()
        .filter(|job| job.item_revision_id == revision_id && job.step_id == step_id)
        .map(|job| job.semantic_attempt_no)
        .max()
        .unwrap_or(0)
        + 1
}

pub(crate) fn selected_prepared_convergence(
    revision_id: ingot_domain::ids::ItemRevisionId,
    convergences: &[Convergence],
) -> Option<&Convergence> {
    convergences.iter().find(|convergence| {
        convergence.item_revision_id == revision_id
            && convergence.state.status() == ConvergenceStatus::Prepared
    })
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use ingot_domain::commit_oid::CommitOid;
    use ingot_domain::ids::{ItemId, ItemRevisionId, JobId, ProjectId};
    use ingot_domain::job::{
        ContextPolicy, ExecutionPermission, JobInput, JobState, JobStatus, OutcomeClass,
        OutputArtifactKind, PhaseKind,
    };
    use ingot_domain::test_support::{JobBuilder, nil_item, nil_revision};
    use ingot_domain::workspace::WorkspaceKind;
    use serde_json::json;
    use uuid::Uuid;

    use super::*;

    fn test_job(step_id: &str, output_artifact_kind: OutputArtifactKind) -> Job {
        let nil = Uuid::nil();
        JobBuilder::new(
            ProjectId::from_uuid(nil),
            ItemId::from_uuid(nil),
            ItemRevisionId::from_uuid(nil),
            step_id,
        )
        .id(JobId::from_uuid(nil))
        .status(JobStatus::Running)
        .outcome_class(OutcomeClass::Clean)
        .phase_kind(PhaseKind::Validate)
        .workspace_kind(WorkspaceKind::Integration)
        .execution_permission(ExecutionPermission::MustNotMutate)
        .context_policy(ContextPolicy::ResumeContext)
        .phase_template_slug("validate-integrated")
        .job_input(JobInput::integrated_subject(
            "target".into(),
            "prepared-head".into(),
        ))
        .output_artifact_kind(output_artifact_kind)
        .build()
    }

    #[test]
    fn dispatch_after_repair_commit_reenters_incremental_review_before_candidate_review() {
        let item = nil_item();
        let revision = nil_revision();

        let mut author_initial = test_job("author_initial", OutputArtifactKind::Commit);
        author_initial.phase_kind = PhaseKind::Author;
        author_initial.workspace_kind = WorkspaceKind::Authoring;
        author_initial.execution_permission = ExecutionPermission::MayMutate;
        author_initial.state = JobState::Completed {
            assignment: author_initial.state.assignment().cloned(),
            started_at: author_initial.state.started_at(),
            outcome_class: OutcomeClass::Clean,
            ended_at: Utc::now(),
            output_commit_oid: Some("commit-1".into()),
            result_schema_version: None,
            result_payload: None,
        };

        let mut review_incremental = test_job(
            "review_incremental_initial",
            OutputArtifactKind::ReviewReport,
        );
        review_incremental.id = JobId::from_uuid(Uuid::now_v7());
        review_incremental.phase_kind = PhaseKind::Review;
        review_incremental.workspace_kind = WorkspaceKind::Review;
        review_incremental.execution_permission = ExecutionPermission::MustNotMutate;
        review_incremental.state = JobState::Completed {
            assignment: review_incremental.state.assignment().cloned(),
            started_at: review_incremental.state.started_at(),
            outcome_class: OutcomeClass::Findings,
            ended_at: Utc::now(),
            output_commit_oid: None,
            result_schema_version: Some("review_report:v1".into()),
            result_payload: Some(json!({
                "outcome": "findings",
                "summary": "needs repair",
                "review_subject": {
                    "base_commit_oid": "seed",
                    "head_commit_oid": "commit-1"
                },
                "overall_risk": "medium",
                "findings": [{
                  "finding_key": "f1",
                  "code": "BUG",
                  "severity": "medium",
                  "summary": "repair",
                  "paths": ["src/lib.rs"],
                  "evidence": ["repair"]
                }]
            })),
        };

        let mut repair_candidate = test_job("repair_candidate", OutputArtifactKind::Commit);
        repair_candidate.id = JobId::from_uuid(Uuid::now_v7());
        repair_candidate.phase_kind = PhaseKind::Author;
        repair_candidate.workspace_kind = WorkspaceKind::Authoring;
        repair_candidate.execution_permission = ExecutionPermission::MayMutate;
        repair_candidate.state = JobState::Completed {
            assignment: repair_candidate.state.assignment().cloned(),
            started_at: repair_candidate.state.started_at(),
            outcome_class: OutcomeClass::Clean,
            ended_at: Utc::now(),
            output_commit_oid: Some("commit-2".into()),
            result_schema_version: None,
            result_payload: None,
        };

        let job = dispatch_job(
            &item,
            &revision,
            &[author_initial, review_incremental, repair_candidate],
            &[],
            &[],
            DispatchJobCommand { step_id: None },
        )
        .expect("dispatch after repair");

        assert_eq!(job.step_id, StepId::ReviewIncrementalRepair);
        assert_eq!(
            job.job_input.base_commit_oid().map(CommitOid::as_str),
            Some("commit-1")
        );
        assert_eq!(
            job.job_input.head_commit_oid().map(CommitOid::as_str),
            Some("commit-2")
        );
    }

    #[test]
    fn dispatch_after_clean_incremental_repair_advances_to_candidate_review_then_validation() {
        let item = nil_item();
        let revision = nil_revision();

        let mut repair_candidate = test_job("repair_candidate", OutputArtifactKind::Commit);
        repair_candidate.phase_kind = PhaseKind::Author;
        repair_candidate.workspace_kind = WorkspaceKind::Authoring;
        repair_candidate.execution_permission = ExecutionPermission::MayMutate;
        repair_candidate.state = JobState::Completed {
            assignment: repair_candidate.state.assignment().cloned(),
            started_at: repair_candidate.state.started_at(),
            outcome_class: OutcomeClass::Clean,
            ended_at: Utc::now(),
            output_commit_oid: Some("commit-2".into()),
            result_schema_version: None,
            result_payload: None,
        };

        let mut review_incremental_repair = test_job(
            "review_incremental_repair",
            OutputArtifactKind::ReviewReport,
        );
        review_incremental_repair.id = JobId::from_uuid(Uuid::now_v7());
        review_incremental_repair.phase_kind = PhaseKind::Review;
        review_incremental_repair.workspace_kind = WorkspaceKind::Review;
        review_incremental_repair.execution_permission = ExecutionPermission::MustNotMutate;
        review_incremental_repair.state = JobState::Completed {
            assignment: review_incremental_repair.state.assignment().cloned(),
            started_at: review_incremental_repair.state.started_at(),
            outcome_class: OutcomeClass::Clean,
            ended_at: Utc::now(),
            output_commit_oid: None,
            result_schema_version: Some("review_report:v1".into()),
            result_payload: Some(json!({
                "outcome": "clean",
                "summary": "incremental clean",
                "review_subject": {
                    "base_commit_oid": "seed",
                    "head_commit_oid": "commit-2"
                },
                "overall_risk": "low",
                "findings": []
            })),
        };

        let candidate_review_job = dispatch_job(
            &item,
            &revision,
            &[repair_candidate.clone(), review_incremental_repair.clone()],
            &[],
            &[],
            DispatchJobCommand { step_id: None },
        )
        .expect("dispatch candidate review");
        assert_eq!(candidate_review_job.step_id, StepId::ReviewCandidateRepair);

        let mut review_candidate_repair =
            test_job("review_candidate_repair", OutputArtifactKind::ReviewReport);
        review_candidate_repair.id = JobId::from_uuid(Uuid::now_v7());
        review_candidate_repair.phase_kind = PhaseKind::Review;
        review_candidate_repair.workspace_kind = WorkspaceKind::Review;
        review_candidate_repair.execution_permission = ExecutionPermission::MustNotMutate;
        review_candidate_repair.state = JobState::Completed {
            assignment: review_candidate_repair.state.assignment().cloned(),
            started_at: review_candidate_repair.state.started_at(),
            outcome_class: OutcomeClass::Clean,
            ended_at: Utc::now(),
            output_commit_oid: None,
            result_schema_version: Some("review_report:v1".into()),
            result_payload: Some(json!({
                "outcome": "clean",
                "summary": "candidate clean",
                "review_subject": {
                    "base_commit_oid": "seed",
                    "head_commit_oid": "commit-2"
                },
                "overall_risk": "low",
                "findings": []
            })),
        };

        let validation_job = dispatch_job(
            &item,
            &revision,
            &[
                repair_candidate,
                review_incremental_repair,
                review_candidate_repair,
            ],
            &[],
            &[],
            DispatchJobCommand { step_id: None },
        )
        .expect("dispatch validation");
        assert_eq!(validation_job.step_id, StepId::ValidateCandidateRepair);
        assert_eq!(
            validation_job
                .job_input
                .base_commit_oid()
                .map(CommitOid::as_str),
            Some("seed")
        );
        assert_eq!(
            validation_job
                .job_input
                .head_commit_oid()
                .map(CommitOid::as_str),
            Some("commit-2")
        );
    }
}
