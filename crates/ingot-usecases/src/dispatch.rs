use std::future::Future;

use chrono::Utc;
use ingot_domain::activity::{Activity, ActivityEventType, ActivitySubject};
use ingot_domain::commit_oid::CommitOid;
use ingot_domain::convergence::Convergence;
use ingot_domain::finding::Finding;
use ingot_domain::git_operation::{
    GitOperation, GitOperationEntityRef, GitOperationStatus, OperationPayload,
};
use ingot_domain::git_ref::GitRef;
use ingot_domain::ids::{ActivityId, GitOperationId, ItemRevisionId, JobId, ProjectId};
use ingot_domain::item::{EscalationReason, Item};
use ingot_domain::job::{
    ExecutionPermission, Job, JobInput, JobStatus, OutcomeClass, OutputArtifactKind,
};
use ingot_domain::ports::{
    ActivityRepository, FindingRepository, GitOperationRepository, JobRepository,
    WorkspaceRepository,
};
use ingot_domain::project::Project;
use ingot_domain::revision::ItemRevision;
use ingot_domain::step_id::StepId;
use ingot_domain::workspace::{Workspace, WorkspaceKind};
use ingot_workflow::{ClosureRelevance, Evaluator, step};

use crate::UseCaseError;
use crate::git_operation_journal::{create_planned, mark_applied};
use crate::job::{DispatchJobCommand, dispatch_job};

pub trait DispatchInfraPort: Send + Sync {
    fn resolve_ref_oid(
        &self,
        project_id: ProjectId,
        ref_name: &GitRef,
    ) -> impl Future<Output = Result<Option<CommitOid>, UseCaseError>> + Send;

    fn update_ref(
        &self,
        project_id: ProjectId,
        ref_name: &GitRef,
        commit_oid: &CommitOid,
    ) -> impl Future<Output = Result<(), UseCaseError>> + Send;

    fn delete_ref(
        &self,
        project_id: ProjectId,
        ref_name: &GitRef,
    ) -> impl Future<Output = Result<(), UseCaseError>> + Send;

    fn remove_workspace_files(
        &self,
        project_id: ProjectId,
        workspace: &Workspace,
    ) -> impl Future<Output = Result<(), UseCaseError>> + Send;
}

#[must_use]
pub fn investigation_ref_name(job_id: JobId) -> GitRef {
    GitRef::new(format!("refs/ingot/investigations/{job_id}"))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingInvestigationRef {
    pub ref_name: GitRef,
    pub commit_oid: CommitOid,
}

pub async fn plan_and_apply_investigation_ref<GO, A, G>(
    git_op_repo: &GO,
    activity_repo: &A,
    git_port: &G,
    project_id: ProjectId,
    entity: GitOperationEntityRef,
    ref_name: &GitRef,
    commit_oid: &CommitOid,
) -> Result<(), UseCaseError>
where
    GO: GitOperationRepository,
    A: ActivityRepository,
    G: DispatchInfraPort,
{
    let mut operation = GitOperation {
        id: GitOperationId::new(),
        project_id,
        entity,
        payload: OperationPayload::CreateInvestigationRef {
            ref_name: ref_name.clone(),
            new_oid: commit_oid.clone(),
            commit_oid: Some(commit_oid.clone()),
        },
        status: GitOperationStatus::Planned,
        created_at: Utc::now(),
        completed_at: None,
    };
    create_planned(git_op_repo, activity_repo, &operation, project_id).await?;
    git_port
        .update_ref(project_id, ref_name, commit_oid)
        .await?;
    mark_applied(git_op_repo, &mut operation).await?;
    Ok(())
}

pub async fn cleanup_failed_dispatch<W, GO, G>(
    workspace_repo: &W,
    git_op_repo: &GO,
    git_port: &G,
    project_id: ProjectId,
    precreated_workspace: Option<&Workspace>,
    investigation_ref_name: Option<&GitRef>,
) where
    W: WorkspaceRepository,
    GO: GitOperationRepository,
    G: DispatchInfraPort,
{
    if let Some(workspace) = precreated_workspace {
        let _ = git_port.remove_workspace_files(project_id, workspace).await;
        let _ = workspace_repo.delete(workspace.id).await;
    }

    if let Some(ref_name) = investigation_ref_name {
        let _ = git_port.delete_ref(project_id, ref_name).await;
        let _ = git_op_repo
            .delete_investigation_ref_operations(ref_name)
            .await;
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn apply_pending_investigation_ref_or_cleanup<W, J, GO, A, G>(
    workspace_repo: &W,
    job_repo: &J,
    git_op_repo: &GO,
    activity_repo: &A,
    git_port: &G,
    project_id: ProjectId,
    job_id: JobId,
    pending_ref: Option<&PendingInvestigationRef>,
    precreated_workspace: Option<&Workspace>,
) -> Result<(), UseCaseError>
where
    W: WorkspaceRepository,
    J: JobRepository,
    GO: GitOperationRepository,
    A: ActivityRepository,
    G: DispatchInfraPort,
{
    let Some(pending_ref) = pending_ref else {
        return Ok(());
    };
    if let Err(error) = plan_and_apply_investigation_ref(
        git_op_repo,
        activity_repo,
        git_port,
        project_id,
        GitOperationEntityRef::Job(job_id),
        &pending_ref.ref_name,
        &pending_ref.commit_oid,
    )
    .await
    {
        cleanup_failed_dispatch(
            workspace_repo,
            git_op_repo,
            git_port,
            project_id,
            precreated_workspace,
            Some(&pending_ref.ref_name),
        )
        .await;
        let _ = job_repo.delete(job_id).await;
        return Err(error);
    }
    Ok(())
}

pub async fn maybe_cleanup_investigation_ref<F, GO, A, G>(
    finding_repo: &F,
    git_op_repo: &GO,
    activity_repo: &A,
    git_port: &G,
    project_id: ProjectId,
    finding: &Finding,
) -> Result<(), UseCaseError>
where
    F: FindingRepository,
    GO: GitOperationRepository,
    A: ActivityRepository,
    G: DispatchInfraPort,
{
    if finding.source_step_id != step::INVESTIGATE_ITEM
        || finding.source_subject_kind != ingot_domain::finding::FindingSubjectKind::Candidate
    {
        return Ok(());
    }

    let remaining_unresolved = finding_repo
        .list_by_item(finding.source_item_id)
        .await
        .map_err(UseCaseError::Repository)?
        .into_iter()
        .any(|candidate| {
            candidate.source_job_id == finding.source_job_id && candidate.triage.is_unresolved()
        });
    if remaining_unresolved {
        return Ok(());
    }

    let ref_name = investigation_ref_name(finding.source_job_id);
    let existing_oid = git_port.resolve_ref_oid(project_id, &ref_name).await?;
    let Some(existing_oid) = existing_oid else {
        return Ok(());
    };

    let mut operation = GitOperation {
        id: GitOperationId::new(),
        project_id,
        entity: GitOperationEntityRef::Job(finding.source_job_id),
        payload: OperationPayload::RemoveInvestigationRef {
            ref_name: ref_name.clone(),
            expected_old_oid: existing_oid,
        },
        status: GitOperationStatus::Planned,
        created_at: Utc::now(),
        completed_at: None,
    };
    create_planned(git_op_repo, activity_repo, &operation, project_id).await?;
    git_port.delete_ref(project_id, &ref_name).await?;
    mark_applied(git_op_repo, &mut operation).await?;
    Ok(())
}

#[must_use]
pub fn autopilot_dispatch_requires_live_target_head(
    item: &Item,
    revision: &ItemRevision,
    jobs: &[Job],
    findings: &[Finding],
    convergences: &[Convergence],
) -> bool {
    Evaluator::new()
        .evaluate(item, revision, jobs, findings, convergences)
        .dispatchable_step_id
        == Some(step::AUTHOR_INITIAL)
        && revision.seed.seed_commit_oid().is_none()
}

#[must_use]
pub fn should_fill_candidate_subject_from_workspace(step_id: StepId) -> bool {
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

#[must_use]
pub fn current_authoring_head_for_revision(
    jobs: &[Job],
    revision: &ItemRevision,
) -> Option<CommitOid> {
    jobs.iter()
        .filter(|job| job.item_revision_id == revision.id)
        .filter(|job| job.state.status() == JobStatus::Completed)
        .filter(|job| job.output_artifact_kind == OutputArtifactKind::Commit)
        .filter(|job| job.state.output_commit_oid().is_some())
        .max_by_key(|job| (job.state.ended_at(), job.created_at))
        .and_then(|job| job.state.output_commit_oid().cloned())
        .or_else(|| revision.seed.seed_commit_oid().map(ToOwned::to_owned))
}

#[must_use]
pub fn should_rebind_implicit_author_initial_job(
    job: &Job,
    revision: &ItemRevision,
    has_authoring_workspace: bool,
) -> bool {
    job.step_id == step::AUTHOR_INITIAL
        && job.workspace_kind == WorkspaceKind::Authoring
        && job.execution_permission == ExecutionPermission::MayMutate
        && !revision.seed.is_explicit()
        && !has_authoring_workspace
}

#[must_use]
pub fn current_authoring_head_for_revision_with_workspace(
    revision: &ItemRevision,
    jobs: &[Job],
    workspace: Option<&Workspace>,
) -> Option<CommitOid> {
    if let Some(commit_oid) = current_authoring_head_for_revision(jobs, revision) {
        return Some(commit_oid);
    }

    workspace.and_then(|ws| ws.state.head_commit_oid().map(ToOwned::to_owned))
}

#[must_use]
pub fn effective_authoring_base_commit_oid(
    revision: &ItemRevision,
    workspace: Option<&Workspace>,
) -> Option<CommitOid> {
    if let Some(seed_commit_oid) = revision.seed.seed_commit_oid() {
        return Some(seed_commit_oid.to_owned());
    }

    workspace.and_then(|ws| ws.state.base_commit_oid().map(ToOwned::to_owned))
}

fn needs_mutable_authoring_head(job: &Job) -> bool {
    job.workspace_kind == WorkspaceKind::Authoring
        && job.execution_permission == ExecutionPermission::MayMutate
        && job.job_input.head_commit_oid().is_none()
}

fn bind_autopilot_authoring_head_if_needed(
    revision: &ItemRevision,
    jobs: &[Job],
    workspace: Option<&Workspace>,
    author_initial_head_commit_oid: Option<&CommitOid>,
    job: &mut Job,
) -> Result<(), UseCaseError> {
    if !needs_mutable_authoring_head(job) {
        return Ok(());
    }

    let head_commit_oid = match job.step_id {
        step::AUTHOR_INITIAL => author_initial_head_commit_oid.cloned().or_else(|| {
            current_authoring_head_for_revision_with_workspace(revision, jobs, workspace)
        }),
        _ => current_authoring_head_for_revision_with_workspace(revision, jobs, workspace),
    };

    let Some(head_commit_oid) = head_commit_oid else {
        return Err(UseCaseError::Internal(format!(
            "missing authoring head for autopilot-dispatched step {}",
            job.step_id
        )));
    };

    job.job_input = JobInput::authoring_head(head_commit_oid);
    Ok(())
}

fn build_candidate_subject_input(
    step_id: StepId,
    input: &JobInput,
    revision: &ItemRevision,
    jobs: &[Job],
    workspace: Option<&Workspace>,
    context: &str,
) -> Result<JobInput, UseCaseError> {
    let base = input
        .base_commit_oid()
        .map(ToOwned::to_owned)
        .or_else(|| effective_authoring_base_commit_oid(revision, workspace));
    let head = input
        .head_commit_oid()
        .map(ToOwned::to_owned)
        .or_else(|| current_authoring_head_for_revision_with_workspace(revision, jobs, workspace));

    match (base, head) {
        (Some(base), Some(head)) => Ok(JobInput::candidate_subject(base, head)),
        _ => Err(UseCaseError::Internal(format!(
            "incomplete candidate subject for {context} {step_id}"
        ))),
    }
}

async fn append_job_dispatched_activity<A>(
    activity_repo: &A,
    project_id: ingot_domain::ids::ProjectId,
    item_id: ingot_domain::ids::ItemId,
    job: &Job,
    dispatch_origin: &'static str,
) -> Result<(), UseCaseError>
where
    A: ActivityRepository,
{
    activity_repo
        .append(&Activity {
            id: ActivityId::new(),
            project_id,
            event_type: ActivityEventType::JobDispatched,
            subject: ActivitySubject::Job(job.id),
            payload: serde_json::json!({
                "item_id": item_id,
                "step_id": job.step_id,
                "dispatch_origin": dispatch_origin,
            }),
            created_at: Utc::now(),
        })
        .await
        .map_err(UseCaseError::Repository)
}

/// Returns true if the job's step is closure-relevant (i.e., failures on it should escalate).
pub fn is_closure_relevant_job(job: &Job) -> bool {
    step::find_step(job.step_id).closure_relevance == ClosureRelevance::ClosureRelevant
}

/// Select the most-recent terminal job that produced findings on a
/// closure-relevant step for the given revision.
pub fn latest_closure_findings_job(jobs: &[Job], revision_id: ItemRevisionId) -> Option<&Job> {
    jobs.iter()
        .filter(|job| job.item_revision_id == revision_id)
        .filter(|job| job.state.status().is_terminal())
        .filter(|job| job.state.outcome_class() == Some(OutcomeClass::Findings))
        .filter(|job| is_closure_relevant_job(job))
        .max_by_key(|job| (job.state.ended_at(), job.created_at))
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
    let Some(step_id) = evaluation.dispatchable_step_id else {
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
            step_id: Some(step_id),
        },
    )?;

    // Fill candidate subject from workspace history if needed
    if should_fill_candidate_subject_from_workspace(job.step_id) {
        let authoring_workspace = workspace_repo
            .find_authoring_for_revision(revision.id)
            .await?;
        job.job_input = build_candidate_subject_input(
            job.step_id,
            &job.job_input,
            revision,
            jobs,
            authoring_workspace.as_ref(),
            "auto-dispatched review",
        )?;
    }

    job_repo.create(&job).await?;
    append_job_dispatched_activity(activity_repo, project.id, item.id, &job, "system").await?;

    Ok(Some(job))
}

/// Auto-dispatch a closure-relevant validation job if the evaluator recommends one.
///
/// Requires pre-hydrated convergences (with `target_head_valid` set) and pre-loaded entity state.
/// Fills candidate subject from workspace/job history. Creates and persists the job.
///
/// Returns `Some(job)` if a validation step was dispatched, `None` if not dispatchable.
#[allow(clippy::too_many_arguments)]
pub async fn auto_dispatch_validation<J, W, A>(
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
    let Some(step_id) = evaluation.dispatchable_step_id else {
        return Ok(None);
    };

    if !step::is_closure_relevant_validate_step(step_id) {
        return Ok(None);
    }

    let mut job = dispatch_job(
        item,
        revision,
        jobs,
        findings,
        convergences,
        DispatchJobCommand {
            step_id: Some(step_id),
        },
    )?;

    if should_fill_candidate_subject_from_workspace(job.step_id) {
        let authoring_workspace = workspace_repo
            .find_authoring_for_revision(revision.id)
            .await?;
        job.job_input = build_candidate_subject_input(
            job.step_id,
            &job.job_input,
            revision,
            jobs,
            authoring_workspace.as_ref(),
            "auto-dispatched validation",
        )?;
    }

    job_repo.create(&job).await?;
    append_job_dispatched_activity(activity_repo, project.id, item.id, &job, "system").await?;

    Ok(Some(job))
}

/// Auto-dispatch any evaluator-recommended step without the closure-relevance filter.
/// Used when `project.execution_mode == Autopilot`.
///
/// Returns `Some(job)` if dispatched, `None` if no dispatchable step.
/// Human gates (approval, escalation, findings triage) are respected: the evaluator
/// will not set `dispatchable_step_id` when those gates are active.
#[allow(clippy::too_many_arguments)]
pub async fn auto_dispatch_autopilot<J, W, A>(
    job_repo: &J,
    workspace_repo: &W,
    activity_repo: &A,
    project: &Project,
    item: &Item,
    revision: &ItemRevision,
    jobs: &[Job],
    findings: &[Finding],
    convergences: &[Convergence],
    author_initial_head_commit_oid: Option<CommitOid>,
) -> Result<Option<Job>, UseCaseError>
where
    J: JobRepository,
    W: WorkspaceRepository,
    A: ActivityRepository,
{
    let evaluation = Evaluator::new().evaluate(item, revision, jobs, findings, convergences);
    let Some(step_id) = evaluation.dispatchable_step_id else {
        return Ok(None);
    };

    let mut job = dispatch_job(
        item,
        revision,
        jobs,
        findings,
        convergences,
        DispatchJobCommand {
            step_id: Some(step_id),
        },
    )?;

    let needs_authoring_workspace = should_fill_candidate_subject_from_workspace(job.step_id)
        || needs_mutable_authoring_head(&job);
    let authoring_workspace = if needs_authoring_workspace {
        workspace_repo
            .find_authoring_for_revision(revision.id)
            .await?
    } else {
        None
    };

    bind_autopilot_authoring_head_if_needed(
        revision,
        jobs,
        authoring_workspace.as_ref(),
        author_initial_head_commit_oid.as_ref(),
        &mut job,
    )?;

    if should_fill_candidate_subject_from_workspace(job.step_id) {
        job.job_input = build_candidate_subject_input(
            job.step_id,
            &job.job_input,
            revision,
            jobs,
            authoring_workspace.as_ref(),
            "autopilot-dispatched step",
        )?;
    }

    job_repo.create(&job).await?;
    append_job_dispatched_activity(activity_repo, project.id, item.id, &job, "autopilot").await?;

    Ok(Some(job))
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use ingot_domain::ids::{ItemId, ItemRevisionId, ProjectId};
    use ingot_domain::item::ApprovalState;
    use ingot_domain::job::JobInput;
    use ingot_domain::job::OutputArtifactKind;
    use ingot_domain::project::ExecutionMode;
    use ingot_domain::revision::{ApprovalPolicy, AuthoringBaseSeed};
    use ingot_test_support::fixtures::{ItemBuilder, JobBuilder, ProjectBuilder, RevisionBuilder};
    use ingot_test_support::sqlite::migrated_test_db;
    use ingot_workflow::step;
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
            step::REVIEW_INCREMENTAL_INITIAL
        ));
        assert!(should_fill_candidate_subject_from_workspace(
            step::INVESTIGATE_ITEM
        ));
    }

    #[test]
    fn should_fill_is_false_for_authoring_steps() {
        assert!(!should_fill_candidate_subject_from_workspace(
            step::AUTHOR_INITIAL
        ));
    }

    #[test]
    fn implicit_autopilot_author_initial_requires_live_head() {
        let item_id = ItemId::from_uuid(Uuid::nil());
        let revision_id = ItemRevisionId::from_uuid(Uuid::nil());
        let project_id = ProjectId::from_uuid(Uuid::nil());
        let item = ItemBuilder::new(project_id, revision_id)
            .id(item_id)
            .build();
        let revision = RevisionBuilder::new(item_id)
            .id(revision_id)
            .seed_commit_oid(None::<String>)
            .seed_target_commit_oid(Some("target-head".to_string()))
            .build();

        assert!(autopilot_dispatch_requires_live_target_head(
            &item,
            &revision,
            &[],
            &[],
            &[]
        ));
    }

    #[test]
    fn implicit_author_initial_rebind_only_applies_without_workspace() {
        let item_id = ItemId::from_uuid(Uuid::nil());
        let revision_id = ItemRevisionId::from_uuid(Uuid::nil());
        let project_id = ProjectId::from_uuid(Uuid::nil());
        let revision = RevisionBuilder::new(item_id)
            .id(revision_id)
            .seed_commit_oid(None::<String>)
            .seed_target_commit_oid(Some("target-head".to_string()))
            .build();
        let job = JobBuilder::new(project_id, item_id, revision_id, step::AUTHOR_INITIAL)
            .workspace_kind(WorkspaceKind::Authoring)
            .execution_permission(ExecutionPermission::MayMutate)
            .build();

        assert!(should_rebind_implicit_author_initial_job(
            &job, &revision, false
        ));
        assert!(!should_rebind_implicit_author_initial_job(
            &job, &revision, true
        ));
    }

    #[tokio::test]
    async fn autopilot_dispatch_binds_author_initial_from_implicit_target_head() {
        let db = migrated_test_db("ingot-usecases-dispatch").await;
        let project_id = ProjectId::new();
        let item_id = ItemId::new();
        let revision_id = ItemRevisionId::new();

        let project = ProjectBuilder::new(
            std::env::temp_dir().join(format!("ingot-usecases-dispatch-{}", Uuid::now_v7())),
        )
        .id(project_id)
        .execution_mode(ExecutionMode::Autopilot)
        .build();
        let item = ItemBuilder::new(project_id, revision_id)
            .id(item_id)
            .approval_state(ApprovalState::NotRequired)
            .build();
        let revision = RevisionBuilder::new(item_id)
            .id(revision_id)
            .approval_policy(ApprovalPolicy::NotRequired)
            .seed(AuthoringBaseSeed::Implicit {
                seed_target_commit_oid: "target-head".into(),
            })
            .template_map_snapshot(serde_json::json!({"author_initial":"author-initial"}))
            .build();

        db.create_project(&project).await.expect("persist project");
        db.create_item_with_revision(&item, &revision)
            .await
            .expect("persist item");

        let job = auto_dispatch_autopilot(
            &db,
            &db,
            &db,
            &project,
            &item,
            &revision,
            &[],
            &[],
            &[],
            Some("target-head".into()),
        )
        .await
        .expect("autopilot dispatch")
        .expect("author_initial job");

        assert_eq!(job.step_id, step::AUTHOR_INITIAL);
        assert_eq!(
            job.job_input,
            JobInput::authoring_head("target-head".into())
        );
    }

    #[tokio::test]
    async fn autopilot_dispatch_rejects_implicit_author_initial_without_live_head() {
        let db = migrated_test_db("ingot-usecases-dispatch").await;
        let project_id = ProjectId::new();
        let item_id = ItemId::new();
        let revision_id = ItemRevisionId::new();

        let project = ProjectBuilder::new(
            std::env::temp_dir().join(format!("ingot-usecases-dispatch-{}", Uuid::now_v7())),
        )
        .id(project_id)
        .execution_mode(ExecutionMode::Autopilot)
        .build();
        let item = ItemBuilder::new(project_id, revision_id)
            .id(item_id)
            .approval_state(ApprovalState::NotRequired)
            .build();
        let revision = RevisionBuilder::new(item_id)
            .id(revision_id)
            .approval_policy(ApprovalPolicy::NotRequired)
            .seed(AuthoringBaseSeed::Implicit {
                seed_target_commit_oid: "stale-seed-target".into(),
            })
            .template_map_snapshot(serde_json::json!({"author_initial":"author-initial"}))
            .build();

        db.create_project(&project).await.expect("persist project");
        db.create_item_with_revision(&item, &revision)
            .await
            .expect("persist item");

        let error = auto_dispatch_autopilot(
            &db,
            &db,
            &db,
            &project,
            &item,
            &revision,
            &[],
            &[],
            &[],
            None,
        )
        .await
        .expect_err("implicit author_initial requires a live target head");

        assert!(
            error
                .to_string()
                .contains("missing authoring head for autopilot-dispatched step author_initial")
        );
    }
}
