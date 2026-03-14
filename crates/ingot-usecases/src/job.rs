use std::path::PathBuf;
use std::sync::Arc;

use crate::UseCaseError;
use crate::finding::extract_findings;
use ingot_domain::convergence::{Convergence, ConvergenceStatus};
use ingot_domain::finding::Finding;
use ingot_domain::ids::JobId;
use ingot_domain::item::{ApprovalState, Item, ParkingState};
use ingot_domain::job::{Job, JobStatus, OutcomeClass, OutputArtifactKind};
use ingot_domain::ports::{
    GitPortError, JobCompletionContext, JobCompletionGitPort, JobCompletionMutation,
    JobCompletionRepository, PreparedConvergenceGuard, ProjectMutationLockPort, RepositoryError,
    TargetRefHoldError,
};
use ingot_domain::project::Project;
use ingot_domain::revision::{ApprovalPolicy, ItemRevision};
use ingot_workflow::step;
use ingot_workflow::{ClosureRelevance, Evaluation, Evaluator};
use serde_json::Value;
use tracing::warn;

#[derive(Debug, Clone)]
pub struct CompleteJobCommand {
    pub job_id: JobId,
    pub outcome_class: OutcomeClass,
    pub result_schema_version: Option<String>,
    pub result_payload: Option<Value>,
    pub output_commit_oid: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompleteJobResult {
    pub finding_count: usize,
}

#[derive(Debug, Clone)]
pub struct DispatchJobCommand {
    pub step_id: Option<String>,
}

#[derive(Debug)]
pub enum CompleteJobError {
    BadRequest { code: &'static str, message: String },
    UseCase(UseCaseError),
}

impl From<UseCaseError> for CompleteJobError {
    fn from(error: UseCaseError) -> Self {
        Self::UseCase(error)
    }
}

#[derive(Debug, Clone)]
struct JobCompletionPlan {
    outcome_class: OutcomeClass,
    result_schema_version: Option<String>,
    result_payload: Option<Value>,
    output_commit_oid: Option<String>,
    findings: Vec<ingot_domain::finding::Finding>,
    prepared_convergence_guard: Option<PreparedConvergenceGuard>,
}

#[derive(Debug, Clone)]
struct NormalizedCompleteJobCommand {
    outcome_class: OutcomeClass,
    result_schema_version: Option<String>,
    result_payload: Option<Value>,
    output_commit_oid: Option<String>,
}

enum LoadedCompletionContext {
    Ready(Box<JobCompletionContext>),
    Retry(CompleteJobResult),
}

#[derive(Clone)]
pub struct CompleteJobService<R, G, L> {
    repository: R,
    git: G,
    project_locks: L,
    repo_path_resolver: Arc<dyn Fn(&Project) -> PathBuf + Send + Sync>,
}

impl<R, G, L> CompleteJobService<R, G, L> {
    pub fn new(repository: R, git: G, project_locks: L) -> Self {
        Self::with_repo_path_resolver(
            repository,
            git,
            project_locks,
            Arc::new(|project: &Project| PathBuf::from(&project.path)),
        )
    }

    pub fn with_repo_path_resolver(
        repository: R,
        git: G,
        project_locks: L,
        repo_path_resolver: Arc<dyn Fn(&Project) -> PathBuf + Send + Sync>,
    ) -> Self {
        Self {
            repository,
            git,
            project_locks,
            repo_path_resolver,
        }
    }
}

impl<R, G, L> CompleteJobService<R, G, L>
where
    R: JobCompletionRepository,
    G: JobCompletionGitPort,
    L: ProjectMutationLockPort,
{
    pub async fn execute(
        &self,
        command: CompleteJobCommand,
    ) -> Result<CompleteJobResult, CompleteJobError> {
        let mut context = match self
            .load_completion_context(command.job_id, &command)
            .await?
        {
            LoadedCompletionContext::Ready(context) => *context,
            LoadedCompletionContext::Retry(result) => return Ok(result),
        };

        let project_lock = if requires_project_serialization(&context.job, command.outcome_class) {
            Some(
                self.project_locks
                    .acquire_project_mutation(context.project.id)
                    .await,
            )
        } else {
            None
        };

        if project_lock.is_some() {
            context = match self
                .load_completion_context(command.job_id, &command)
                .await?
            {
                LoadedCompletionContext::Ready(context) => *context,
                LoadedCompletionContext::Retry(result) => return Ok(result),
            };
        }

        let normalized_command = normalize_completion_command(&context.job, &command)?;
        let plan = self
            .prepare_job_completion(&context, normalized_command)
            .await?;
        let repo_path = (self.repo_path_resolver)(&context.project);
        let ref_hold = if let Some(guard) = plan.prepared_convergence_guard.as_ref() {
            Some(
                self.git
                    .verify_and_hold_target_ref(
                        repo_path.as_path(),
                        &guard.target_ref,
                        &guard.expected_target_head_oid,
                    )
                    .await
                    .map_err(map_target_ref_hold_error)?,
            )
        } else {
            None
        };

        let result = self
            .repository
            .apply_job_completion(JobCompletionMutation {
                job_id: context.job.id,
                item_id: context.item.id,
                expected_item_revision_id: context.job.item_revision_id,
                outcome_class: plan.outcome_class,
                clear_item_escalation: should_clear_item_escalation_on_success(
                    &context.item,
                    &context.job,
                ),
                result_schema_version: plan.result_schema_version,
                result_payload: plan.result_payload,
                output_commit_oid: plan.output_commit_oid,
                findings: plan.findings.clone(),
                prepared_convergence_guard: plan.prepared_convergence_guard.clone(),
            })
            .await
            .map_err(map_completion_apply_error);

        let release_result = if let Some(hold) = ref_hold {
            self.git.release_hold(hold).await.map_err(|error| {
                warn!(
                    ?error,
                    job_id = %context.job.id,
                    "failed to release target ref hold after job completion"
                );
                map_git_port_error(error)
            })
        } else {
            Ok(())
        };

        drop(project_lock);
        result?;
        release_result?;

        Ok(CompleteJobResult {
            finding_count: plan.findings.len(),
        })
    }

    async fn try_completed_job_retry(
        &self,
        job_id: JobId,
        job: &Job,
        command: &CompleteJobCommand,
    ) -> Result<Option<CompleteJobResult>, CompleteJobError> {
        if !completed_job_retry_allowed(job) {
            return Ok(None);
        }

        let Some(completed) = self
            .repository
            .load_completed_job_completion(job_id)
            .await
            .map_err(map_repository_error)?
        else {
            return Ok(None);
        };

        if !completed_job_matches_retry_command(&completed.job, command) {
            return Ok(None);
        }

        Ok(Some(CompleteJobResult {
            finding_count: completed.finding_count,
        }))
    }

    async fn load_completion_context(
        &self,
        job_id: JobId,
        command: &CompleteJobCommand,
    ) -> Result<LoadedCompletionContext, CompleteJobError> {
        let context = self
            .repository
            .load_job_completion_context(job_id)
            .await
            .map_err(map_repository_error)?;
        if let Some(result) = self
            .try_completed_job_retry(job_id, &context.job, command)
            .await?
        {
            return Ok(LoadedCompletionContext::Retry(result));
        }

        validate_completion_context(&context)?;
        Ok(LoadedCompletionContext::Ready(Box::new(context)))
    }

    async fn prepare_job_completion(
        &self,
        context: &JobCompletionContext,
        command: NormalizedCompleteJobCommand,
    ) -> Result<JobCompletionPlan, CompleteJobError> {
        match context.job.output_artifact_kind {
            OutputArtifactKind::Commit => {
                let output_commit_oid = command
                    .output_commit_oid
                    .expect("commit completion should be normalized");

                let commit_is_present = self
                    .git
                    .commit_exists(
                        (self.repo_path_resolver)(&context.project).as_path(),
                        &output_commit_oid,
                    )
                    .await
                    .map_err(map_git_port_error)?;
                if !commit_is_present {
                    return Err(CompleteJobError::BadRequest {
                        code: "missing_output_commit_oid",
                        message:
                            "output_commit_oid does not resolve to a commit in the project repository"
                                .into(),
                    });
                }

                Ok(JobCompletionPlan {
                    outcome_class: command.outcome_class,
                    result_schema_version: None,
                    result_payload: None,
                    output_commit_oid: Some(output_commit_oid),
                    findings: vec![],
                    prepared_convergence_guard: None,
                })
            }
            OutputArtifactKind::ValidationReport
            | OutputArtifactKind::ReviewReport
            | OutputArtifactKind::FindingReport => {
                let result_schema_version = command
                    .result_schema_version
                    .expect("report completion should include schema version");
                let result_payload = command
                    .result_payload
                    .expect("report completion should include payload");

                let mut completed_job = context.job.clone();
                completed_job.outcome_class = Some(command.outcome_class);
                completed_job.result_schema_version = Some(result_schema_version.clone());
                completed_job.result_payload = Some(result_payload.clone());

                let extracted =
                    extract_findings(&context.item, &completed_job, &context.convergences)?;
                if extracted.outcome_class != command.outcome_class {
                    return Err(CompleteJobError::BadRequest {
                        code: "outcome_mismatch",
                        message: format!(
                            "Requested outcome_class={} does not match report outcome {}",
                            outcome_class_name(command.outcome_class),
                            outcome_class_name(extracted.outcome_class)
                        ),
                    });
                }

                let prepared_convergence_guard = prepared_convergence_guard(
                    &context.item,
                    &context.revision,
                    &completed_job,
                    &context.convergences,
                )?;

                Ok(JobCompletionPlan {
                    outcome_class: command.outcome_class,
                    result_schema_version: Some(result_schema_version),
                    result_payload: Some(result_payload),
                    output_commit_oid: None,
                    findings: extracted.findings,
                    prepared_convergence_guard,
                })
            }
            OutputArtifactKind::None => Ok(JobCompletionPlan {
                outcome_class: command.outcome_class,
                result_schema_version: None,
                result_payload: None,
                output_commit_oid: None,
                findings: vec![],
                prepared_convergence_guard: None,
            }),
        }
    }
}

fn normalize_completion_command(
    job: &Job,
    command: &CompleteJobCommand,
) -> Result<NormalizedCompleteJobCommand, CompleteJobError> {
    match job.output_artifact_kind {
        OutputArtifactKind::Commit => {
            if command.outcome_class != OutcomeClass::Clean {
                return Err(CompleteJobError::BadRequest {
                    code: "invalid_outcome_class",
                    message: "Commit-producing jobs may only complete with outcome_class=clean"
                        .into(),
                });
            }

            if command.result_schema_version.is_some() || command.result_payload.is_some() {
                return Err(CompleteJobError::BadRequest {
                    code: "invalid_completion_artifact",
                    message: "Commit-producing jobs must not include structured report payloads"
                        .into(),
                });
            }

            let output_commit_oid = command
                .output_commit_oid
                .clone()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| CompleteJobError::BadRequest {
                    code: "missing_output_commit_oid",
                    message: "Commit-producing jobs must include output_commit_oid".into(),
                })?;

            Ok(NormalizedCompleteJobCommand {
                outcome_class: command.outcome_class,
                result_schema_version: None,
                result_payload: None,
                output_commit_oid: Some(output_commit_oid),
            })
        }
        OutputArtifactKind::ValidationReport
        | OutputArtifactKind::ReviewReport
        | OutputArtifactKind::FindingReport => {
            if command.output_commit_oid.is_some() {
                return Err(CompleteJobError::BadRequest {
                    code: "invalid_completion_artifact",
                    message: "Report-producing jobs must not include output_commit_oid".into(),
                });
            }

            let expected_schema_version = expected_schema_version(job.output_artifact_kind);
            let result_schema_version = command.result_schema_version.clone().ok_or_else(|| {
                CompleteJobError::BadRequest {
                    code: "missing_result_schema_version",
                    message: "Report-producing jobs must include result_schema_version".into(),
                }
            })?;
            let result_payload =
                command
                    .result_payload
                    .clone()
                    .ok_or_else(|| CompleteJobError::BadRequest {
                        code: "missing_result_payload",
                        message: "Report-producing jobs must include result_payload".into(),
                    })?;

            if result_schema_version != expected_schema_version {
                return Err(CompleteJobError::BadRequest {
                    code: "invalid_result_schema_version",
                    message: format!(
                        "Expected result_schema_version={}, got {}",
                        expected_schema_version, result_schema_version
                    ),
                });
            }

            if !matches!(
                command.outcome_class,
                OutcomeClass::Clean | OutcomeClass::Findings
            ) {
                return Err(CompleteJobError::BadRequest {
                    code: "invalid_outcome_class",
                    message:
                        "Report-producing jobs may only complete with outcome_class=clean or findings"
                            .into(),
                });
            }

            Ok(NormalizedCompleteJobCommand {
                outcome_class: command.outcome_class,
                result_schema_version: Some(result_schema_version),
                result_payload: Some(result_payload),
                output_commit_oid: None,
            })
        }
        OutputArtifactKind::None => {
            if command.result_schema_version.is_some()
                || command.result_payload.is_some()
                || command.output_commit_oid.is_some()
            {
                return Err(CompleteJobError::BadRequest {
                    code: "invalid_completion_artifact",
                    message: "Jobs without output artifacts must not include completion artifacts"
                        .into(),
                });
            }

            if command.outcome_class != OutcomeClass::Clean {
                return Err(CompleteJobError::BadRequest {
                    code: "invalid_outcome_class",
                    message: "Artifact-free jobs may only complete with outcome_class=clean".into(),
                });
            }

            Ok(NormalizedCompleteJobCommand {
                outcome_class: command.outcome_class,
                result_schema_version: None,
                result_payload: None,
                output_commit_oid: None,
            })
        }
    }
}

fn completed_job_matches_retry_command(job: &Job, command: &CompleteJobCommand) -> bool {
    job.status == JobStatus::Completed
        && job.outcome_class == Some(command.outcome_class)
        && job.result_schema_version == command.result_schema_version
        && job.result_payload == command.result_payload
        && job.output_commit_oid == command.output_commit_oid
}

fn completed_job_retry_allowed(job: &Job) -> bool {
    job.status == JobStatus::Completed && !completed_job_uses_target_ref_hold(job)
}

fn completed_job_uses_target_ref_hold(job: &Job) -> bool {
    job.step_id == "validate_integrated" && job.outcome_class == Some(OutcomeClass::Clean)
}

fn validate_completion_context(context: &JobCompletionContext) -> Result<(), CompleteJobError> {
    if !context.job.status.is_active() {
        return Err(UseCaseError::JobNotActive.into());
    }

    if context.job.item_revision_id != context.item.current_revision_id {
        return Err(UseCaseError::ProtocolViolation(
            "job completion does not match the current item revision".into(),
        )
        .into());
    }

    Ok(())
}

fn requires_project_serialization(job: &Job, outcome_class: OutcomeClass) -> bool {
    let _ = (job, outcome_class);
    true
}

fn desired_completion_approval_state(
    item: &Item,
    revision: &ItemRevision,
    job: &Job,
) -> Option<ApprovalState> {
    if job.step_id != "validate_integrated" || job.outcome_class != Some(OutcomeClass::Clean) {
        return None;
    }

    let approval_state = match revision.approval_policy {
        ApprovalPolicy::Required => ApprovalState::Pending,
        ApprovalPolicy::NotRequired => ApprovalState::NotRequired,
    };

    if item.approval_state == approval_state {
        None
    } else {
        Some(approval_state)
    }
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
    let step_id = select_dispatch_step(&evaluation, command.step_id.as_deref())?;
    let contract = step::find_step(&step_id)
        .ok_or_else(|| UseCaseError::IllegalStepDispatch(format!("Unknown step: {step_id}")))?;

    if !contract.is_dispatchable_job() {
        return Err(UseCaseError::IllegalStepDispatch(format!(
            "Step is not dispatchable: {step_id}"
        )));
    }

    let template_slug = template_slug_for_step(revision, &step_id, contract.default_template_slug);
    let (input_base_commit_oid, input_head_commit_oid) =
        input_commits_for_step(&step_id, revision, jobs, convergences);
    let semantic_attempt_no = next_semantic_attempt_no(jobs, item.current_revision_id, &step_id);

    Ok(Job {
        id: JobId::new(),
        project_id: item.project_id,
        item_id: item.id,
        item_revision_id: item.current_revision_id,
        step_id,
        semantic_attempt_no,
        retry_no: 0,
        supersedes_job_id: None,
        status: JobStatus::Queued,
        outcome_class: None,
        phase_kind: contract.phase_kind,
        workspace_id: None,
        workspace_kind: contract.workspace_kind,
        execution_permission: contract.execution_permission,
        context_policy: contract.context_policy,
        phase_template_slug: template_slug,
        phase_template_digest: None,
        prompt_snapshot: None,
        input_base_commit_oid,
        input_head_commit_oid,
        output_artifact_kind: contract.output_artifact_kind,
        output_commit_oid: None,
        result_schema_version: None,
        result_payload: None,
        agent_id: None,
        process_pid: None,
        lease_owner_id: None,
        heartbeat_at: None,
        lease_expires_at: None,
        error_code: None,
        error_message: None,
        created_at: chrono::Utc::now(),
        started_at: None,
        ended_at: None,
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

    if !previous_job.status.is_terminal()
        || matches!(
            previous_job.outcome_class,
            Some(OutcomeClass::Clean | OutcomeClass::Findings)
        )
    {
        return Err(UseCaseError::IllegalStepDispatch(
            "Only terminal non-success jobs can be retried".into(),
        ));
    }

    let evaluation = Evaluator::new().evaluate(item, revision, jobs, findings, convergences);
    let contract = step::find_step(&previous_job.step_id).ok_or_else(|| {
        UseCaseError::IllegalStepDispatch(format!("Unknown step: {}", previous_job.step_id))
    })?;
    let closure_position_allows_retry =
        evaluation.current_step_id.as_deref() == Some(previous_job.step_id.as_str());
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
        &previous_job.step_id,
        contract.default_template_slug,
    );
    let (input_base_commit_oid, input_head_commit_oid) =
        input_commits_for_step(&previous_job.step_id, revision, jobs, convergences);
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
        step_id: previous_job.step_id.clone(),
        semantic_attempt_no: previous_job.semantic_attempt_no,
        retry_no,
        supersedes_job_id: Some(previous_job.id),
        status: JobStatus::Queued,
        outcome_class: None,
        phase_kind: contract.phase_kind,
        workspace_id: None,
        workspace_kind: contract.workspace_kind,
        execution_permission: contract.execution_permission,
        context_policy: contract.context_policy,
        phase_template_slug: template_slug,
        phase_template_digest: None,
        prompt_snapshot: None,
        input_base_commit_oid,
        input_head_commit_oid,
        output_artifact_kind: contract.output_artifact_kind,
        output_commit_oid: None,
        result_schema_version: None,
        result_payload: None,
        agent_id: None,
        process_pid: None,
        lease_owner_id: None,
        heartbeat_at: None,
        lease_expires_at: None,
        error_code: None,
        error_message: None,
        created_at: chrono::Utc::now(),
        started_at: None,
        ended_at: None,
    })
}

fn select_dispatch_step(
    evaluation: &Evaluation,
    requested_step_id: Option<&str>,
) -> Result<String, UseCaseError> {
    if let Some(requested_step_id) = requested_step_id {
        if evaluation.dispatchable_step_id.as_deref() == Some(requested_step_id)
            || evaluation
                .auxiliary_dispatchable_step_ids
                .iter()
                .any(|step_id| step_id == requested_step_id)
        {
            return Ok(requested_step_id.to_string());
        }

        return Err(UseCaseError::IllegalStepDispatch(format!(
            "Step is not dispatchable in the current state: {requested_step_id}"
        )));
    }

    evaluation.dispatchable_step_id.clone().ok_or_else(|| {
        UseCaseError::IllegalStepDispatch(
            "No closure-relevant step is dispatchable in the current state".into(),
        )
    })
}

fn template_slug_for_step(
    revision: &ItemRevision,
    step_id: &str,
    default_template_slug: Option<&'static str>,
) -> String {
    revision
        .template_map_snapshot
        .get(step_id)
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned)
        .or_else(|| default_template_slug.map(ToOwned::to_owned))
        .unwrap_or_else(|| step_id.to_string())
}

fn input_commits_for_step(
    step_id: &str,
    revision: &ItemRevision,
    jobs: &[Job],
    convergences: &[Convergence],
) -> (Option<String>, Option<String>) {
    let seed_head = Some(revision.seed_commit_oid.clone());
    let current_head = Some(current_authoring_head(jobs, revision));
    let previous_head = Some(previous_authoring_head(jobs, revision));
    let prepared_convergence = selected_prepared_convergence(revision.id, convergences);

    match step_id {
        step::AUTHOR_INITIAL => (None, seed_head),
        step::REPAIR_CANDIDATE | step::REPAIR_AFTER_INTEGRATION => (None, current_head),
        step::REVIEW_INCREMENTAL_INITIAL => (seed_head, current_head),
        step::REVIEW_INCREMENTAL_REPAIR | step::REVIEW_INCREMENTAL_AFTER_INTEGRATION_REPAIR => {
            (previous_head, current_head)
        }
        step::REVIEW_CANDIDATE_INITIAL
        | step::REVIEW_CANDIDATE_REPAIR
        | step::VALIDATE_CANDIDATE_INITIAL
        | step::VALIDATE_CANDIDATE_REPAIR
        | step::REVIEW_AFTER_INTEGRATION_REPAIR
        | step::VALIDATE_AFTER_INTEGRATION_REPAIR => (seed_head, current_head),
        step::INVESTIGATE_ITEM => {
            if let Some(prepared_convergence) = prepared_convergence {
                (
                    prepared_convergence.input_target_commit_oid.clone(),
                    prepared_convergence.prepared_commit_oid.clone(),
                )
            } else {
                (seed_head, current_head)
            }
        }
        step::VALIDATE_INTEGRATED => prepared_convergence
            .map(|convergence| {
                (
                    convergence.input_target_commit_oid.clone(),
                    convergence.prepared_commit_oid.clone(),
                )
            })
            .unwrap_or((None, None)),
        _ => (None, None),
    }
}

fn ensure_item_dispatchable(item: &Item) -> Result<(), UseCaseError> {
    if item.lifecycle_state != ingot_domain::item::LifecycleState::Open {
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
        .any(|job| job.item_revision_id == revision_id && job.status.is_active())
    {
        return Err(UseCaseError::ActiveJobExists);
    }

    if convergences.iter().any(|convergence| {
        convergence.item_revision_id == revision_id
            && matches!(
                convergence.status,
                ConvergenceStatus::Queued | ConvergenceStatus::Running
            )
    }) {
        return Err(UseCaseError::ActiveConvergenceExists);
    }

    Ok(())
}

fn should_clear_item_escalation_on_success(item: &Item, job: &Job) -> bool {
    item.escalation_state == ingot_domain::item::EscalationState::OperatorRequired
        && job.retry_no > 0
        && matches!(
            step::find_step(&job.step_id).map(|contract| contract.closure_relevance),
            Some(ClosureRelevance::ClosureRelevant)
        )
}

fn current_authoring_head(jobs: &[Job], revision: &ItemRevision) -> String {
    successful_commit_oids(jobs, revision)
        .last()
        .cloned()
        .unwrap_or_else(|| revision.seed_commit_oid.clone())
}

fn previous_authoring_head(jobs: &[Job], revision: &ItemRevision) -> String {
    let commit_oids = successful_commit_oids(jobs, revision);
    commit_oids
        .iter()
        .rev()
        .nth(1)
        .cloned()
        .unwrap_or_else(|| revision.seed_commit_oid.clone())
}

fn successful_commit_oids(jobs: &[Job], revision: &ItemRevision) -> Vec<String> {
    let mut commit_jobs = jobs
        .iter()
        .filter(|job| job.item_revision_id == revision.id)
        .filter(|job| job.status == JobStatus::Completed)
        .filter(|job| job.output_artifact_kind == OutputArtifactKind::Commit)
        .filter_map(|job| {
            job.output_commit_oid
                .as_ref()
                .map(|commit_oid| ((job.ended_at, job.created_at), commit_oid.clone()))
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
    step_id: &str,
) -> u32 {
    jobs.iter()
        .filter(|job| job.item_revision_id == revision_id && job.step_id == step_id)
        .map(|job| job.semantic_attempt_no)
        .max()
        .unwrap_or(0)
        + 1
}

fn selected_prepared_convergence(
    revision_id: ingot_domain::ids::ItemRevisionId,
    convergences: &[Convergence],
) -> Option<&Convergence> {
    convergences.iter().find(|convergence| {
        convergence.item_revision_id == revision_id
            && convergence.status == ConvergenceStatus::Prepared
    })
}

fn prepared_convergence_guard(
    item: &Item,
    revision: &ItemRevision,
    job: &Job,
    convergences: &[Convergence],
) -> Result<Option<PreparedConvergenceGuard>, CompleteJobError> {
    if job.step_id != "validate_integrated" || job.outcome_class != Some(OutcomeClass::Clean) {
        return Ok(None);
    }

    let Some(prepared_convergence) =
        selected_prepared_convergence(job.item_revision_id, convergences)
    else {
        return Err(UseCaseError::PreparedConvergenceMissing.into());
    };

    let Some(expected_target_oid) = prepared_convergence.input_target_commit_oid.as_deref() else {
        return Err(UseCaseError::PreparedConvergenceStale.into());
    };

    Ok(Some(PreparedConvergenceGuard {
        convergence_id: prepared_convergence.id,
        item_revision_id: job.item_revision_id,
        target_ref: prepared_convergence.target_ref.clone(),
        expected_target_head_oid: expected_target_oid.into(),
        next_approval_state: desired_completion_approval_state(item, revision, job),
    }))
}

fn expected_schema_version(output_artifact_kind: OutputArtifactKind) -> &'static str {
    match output_artifact_kind {
        OutputArtifactKind::ValidationReport => "validation_report:v1",
        OutputArtifactKind::ReviewReport => "review_report:v1",
        OutputArtifactKind::FindingReport => "finding_report:v1",
        _ => "",
    }
}

fn outcome_class_name(outcome_class: OutcomeClass) -> &'static str {
    match outcome_class {
        OutcomeClass::Clean => "clean",
        OutcomeClass::Findings => "findings",
        OutcomeClass::Cancelled => "cancelled",
        OutcomeClass::TransientFailure => "transient_failure",
        OutcomeClass::TerminalFailure => "terminal_failure",
        OutcomeClass::ProtocolViolation => "protocol_violation",
    }
}

fn map_repository_error(error: RepositoryError) -> CompleteJobError {
    UseCaseError::Repository(error).into()
}

fn map_completion_apply_error(error: RepositoryError) -> CompleteJobError {
    match error {
        RepositoryError::Conflict(message) if message == "job_not_active" => {
            UseCaseError::JobNotActive.into()
        }
        RepositoryError::Conflict(message) if message == "job_revision_stale" => {
            UseCaseError::ProtocolViolation(
                "job completion does not match the current item revision".into(),
            )
            .into()
        }
        RepositoryError::Conflict(message) if message == "prepared_convergence_missing" => {
            UseCaseError::PreparedConvergenceMissing.into()
        }
        RepositoryError::Conflict(message) if message == "prepared_convergence_stale" => {
            UseCaseError::PreparedConvergenceStale.into()
        }
        other => map_repository_error(other),
    }
}

fn map_git_port_error(error: GitPortError) -> CompleteJobError {
    UseCaseError::Internal(error.to_string()).into()
}

fn map_target_ref_hold_error(error: TargetRefHoldError) -> CompleteJobError {
    match error {
        TargetRefHoldError::Stale => UseCaseError::PreparedConvergenceStale.into(),
        TargetRefHoldError::Internal(message) => UseCaseError::Internal(message).into(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use chrono::Utc;
    use ingot_domain::convergence::{ConvergenceStatus, ConvergenceStrategy};
    use ingot_domain::ids::{ConvergenceId, ItemId, ItemRevisionId, ProjectId, WorkspaceId};
    use ingot_domain::item::{
        Classification, EscalationState, LifecycleState, OriginKind, ParkingState, Priority,
    };
    use ingot_domain::job::{ContextPolicy, ExecutionPermission, JobStatus, PhaseKind};
    use ingot_domain::project::Project;
    use ingot_domain::workspace::WorkspaceKind;
    use serde_json::json;
    use uuid::Uuid;

    use super::*;

    #[tokio::test]
    async fn completion_rejects_schema_mismatch_for_report_jobs() {
        let service = test_service(test_context(test_job(
            "validate_integrated",
            OutputArtifactKind::ValidationReport,
        )));

        let result = service
            .execute(CompleteJobCommand {
                job_id: JobId::from_uuid(Uuid::nil()),
                outcome_class: OutcomeClass::Clean,
                result_schema_version: Some("review_report:v1".into()),
                result_payload: Some(json!({
                    "outcome": "clean",
                    "summary": "ok",
                    "review_subject": {
                        "base_commit_oid": "base",
                        "head_commit_oid": "head"
                    },
                    "overall_risk": "low",
                    "findings": []
                })),
                output_commit_oid: None,
            })
            .await;

        assert!(matches!(
            result,
            Err(CompleteJobError::BadRequest {
                code: "invalid_result_schema_version",
                ..
            })
        ));
    }

    #[test]
    fn dispatch_after_repair_commit_reenters_incremental_review_before_candidate_review() {
        let item = test_item();
        let revision = test_revision();

        let mut author_initial = test_job("author_initial", OutputArtifactKind::Commit);
        author_initial.status = JobStatus::Completed;
        author_initial.phase_kind = PhaseKind::Author;
        author_initial.workspace_kind = WorkspaceKind::Authoring;
        author_initial.execution_permission = ExecutionPermission::MayMutate;
        author_initial.output_commit_oid = Some("commit-1".into());
        author_initial.ended_at = Some(Utc::now());

        let mut review_incremental = test_job(
            "review_incremental_initial",
            OutputArtifactKind::ReviewReport,
        );
        review_incremental.id = JobId::from_uuid(Uuid::now_v7());
        review_incremental.status = JobStatus::Completed;
        review_incremental.phase_kind = PhaseKind::Review;
        review_incremental.workspace_kind = WorkspaceKind::Review;
        review_incremental.execution_permission = ExecutionPermission::MustNotMutate;
        review_incremental.outcome_class = Some(OutcomeClass::Findings);
        review_incremental.result_schema_version = Some("review_report:v1".into());
        review_incremental.result_payload = Some(json!({
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
        }));
        review_incremental.ended_at = Some(Utc::now());

        let mut repair_candidate = test_job("repair_candidate", OutputArtifactKind::Commit);
        repair_candidate.id = JobId::from_uuid(Uuid::now_v7());
        repair_candidate.status = JobStatus::Completed;
        repair_candidate.phase_kind = PhaseKind::Author;
        repair_candidate.workspace_kind = WorkspaceKind::Authoring;
        repair_candidate.execution_permission = ExecutionPermission::MayMutate;
        repair_candidate.output_commit_oid = Some("commit-2".into());
        repair_candidate.ended_at = Some(Utc::now());

        let job = dispatch_job(
            &item,
            &revision,
            &[author_initial, review_incremental, repair_candidate],
            &[],
            &[],
            DispatchJobCommand { step_id: None },
        )
        .expect("dispatch after repair");

        assert_eq!(job.step_id, "review_incremental_repair");
        assert_eq!(job.input_base_commit_oid.as_deref(), Some("commit-1"));
        assert_eq!(job.input_head_commit_oid.as_deref(), Some("commit-2"));
    }

    #[test]
    fn dispatch_after_clean_incremental_repair_advances_to_candidate_review_then_validation() {
        let item = test_item();
        let revision = test_revision();

        let mut repair_candidate = test_job("repair_candidate", OutputArtifactKind::Commit);
        repair_candidate.status = JobStatus::Completed;
        repair_candidate.phase_kind = PhaseKind::Author;
        repair_candidate.workspace_kind = WorkspaceKind::Authoring;
        repair_candidate.execution_permission = ExecutionPermission::MayMutate;
        repair_candidate.output_commit_oid = Some("commit-2".into());
        repair_candidate.ended_at = Some(Utc::now());

        let mut review_incremental_repair = test_job(
            "review_incremental_repair",
            OutputArtifactKind::ReviewReport,
        );
        review_incremental_repair.id = JobId::from_uuid(Uuid::now_v7());
        review_incremental_repair.status = JobStatus::Completed;
        review_incremental_repair.phase_kind = PhaseKind::Review;
        review_incremental_repair.workspace_kind = WorkspaceKind::Review;
        review_incremental_repair.execution_permission = ExecutionPermission::MustNotMutate;
        review_incremental_repair.outcome_class = Some(OutcomeClass::Clean);
        review_incremental_repair.result_schema_version = Some("review_report:v1".into());
        review_incremental_repair.result_payload = Some(json!({
            "outcome": "clean",
            "summary": "incremental clean",
            "review_subject": {
                "base_commit_oid": "seed",
                "head_commit_oid": "commit-2"
            },
            "overall_risk": "low",
            "findings": []
        }));
        review_incremental_repair.ended_at = Some(Utc::now());

        let candidate_review_job = dispatch_job(
            &item,
            &revision,
            &[repair_candidate.clone(), review_incremental_repair.clone()],
            &[],
            &[],
            DispatchJobCommand { step_id: None },
        )
        .expect("dispatch candidate review");
        assert_eq!(candidate_review_job.step_id, "review_candidate_repair");

        let mut review_candidate_repair =
            test_job("review_candidate_repair", OutputArtifactKind::ReviewReport);
        review_candidate_repair.id = JobId::from_uuid(Uuid::now_v7());
        review_candidate_repair.status = JobStatus::Completed;
        review_candidate_repair.phase_kind = PhaseKind::Review;
        review_candidate_repair.workspace_kind = WorkspaceKind::Review;
        review_candidate_repair.execution_permission = ExecutionPermission::MustNotMutate;
        review_candidate_repair.outcome_class = Some(OutcomeClass::Clean);
        review_candidate_repair.result_schema_version = Some("review_report:v1".into());
        review_candidate_repair.result_payload = Some(json!({
            "outcome": "clean",
            "summary": "candidate clean",
            "review_subject": {
                "base_commit_oid": "seed",
                "head_commit_oid": "commit-2"
            },
            "overall_risk": "low",
            "findings": []
        }));
        review_candidate_repair.ended_at = Some(Utc::now());

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
        assert_eq!(validation_job.step_id, "validate_candidate_repair");
        assert_eq!(
            validation_job.input_base_commit_oid.as_deref(),
            Some("seed")
        );
        assert_eq!(
            validation_job.input_head_commit_oid.as_deref(),
            Some("commit-2")
        );
    }

    #[tokio::test]
    async fn completion_supports_commit_jobs_without_report_payloads() {
        let mut job = test_job("repair_candidate", OutputArtifactKind::Commit);
        job.phase_kind = PhaseKind::Author;
        job.workspace_kind = WorkspaceKind::Authoring;
        job.execution_permission = ExecutionPermission::MayMutate;
        let repository = FakeRepository::new(test_context(job));
        let git = FakeGitPort::default().with_commit_exists(true);
        let service = CompleteJobService::new(repository.clone(), git, FakeProjectLocks::default());

        let result = service
            .execute(CompleteJobCommand {
                job_id: JobId::from_uuid(Uuid::nil()),
                outcome_class: OutcomeClass::Clean,
                result_schema_version: None,
                result_payload: None,
                output_commit_oid: Some("commit-oid".into()),
            })
            .await
            .expect("commit jobs should complete");

        let mutation = repository.last_mutation().expect("captured mutation");
        assert_eq!(result.finding_count, 0);
        assert_eq!(mutation.output_commit_oid.as_deref(), Some("commit-oid"));
        assert!(mutation.result_schema_version.is_none());
    }

    #[tokio::test]
    async fn completion_sets_pending_approval_for_clean_integrated_validation() {
        let mut context = test_context(test_job(
            "validate_integrated",
            OutputArtifactKind::ValidationReport,
        ));
        context.job.outcome_class = Some(OutcomeClass::Clean);
        context.convergences[0].input_target_commit_oid = Some("target".into());
        let repository = FakeRepository::new(context);
        let git = FakeGitPort::default();
        let service = CompleteJobService::new(repository.clone(), git, FakeProjectLocks::default());

        service
            .execute(valid_validation_command())
            .await
            .expect("clean integrated validation should complete");

        let mutation = repository.last_mutation().expect("captured mutation");
        let guard = mutation
            .prepared_convergence_guard
            .expect("prepared convergence guard");
        assert_eq!(guard.next_approval_state, Some(ApprovalState::Pending));
    }

    #[tokio::test]
    async fn completion_rejects_clean_integrated_validation_without_prepared_convergence() {
        let mut context = test_context(test_job(
            "validate_integrated",
            OutputArtifactKind::ValidationReport,
        ));
        context.job.outcome_class = Some(OutcomeClass::Clean);
        context.convergences.clear();
        let service = test_service(context);

        let result = service.execute(valid_validation_command()).await;

        assert!(matches!(
            result,
            Err(CompleteJobError::UseCase(
                UseCaseError::PreparedConvergenceMissing
            ))
        ));
    }

    #[tokio::test]
    async fn completion_rejects_clean_integrated_validation_when_target_ref_has_moved() {
        let mut context = test_context(test_job(
            "validate_integrated",
            OutputArtifactKind::ValidationReport,
        ));
        context.job.outcome_class = Some(OutcomeClass::Clean);
        context.convergences[0].input_target_commit_oid = Some("target".into());
        let repository = FakeRepository::new(context);
        let git = FakeGitPort::default().with_hold_error(TargetRefHoldError::Stale);
        let service = CompleteJobService::new(repository, git, FakeProjectLocks::default());

        let result = service.execute(valid_validation_command()).await;

        assert!(matches!(
            result,
            Err(CompleteJobError::UseCase(
                UseCaseError::PreparedConvergenceStale
            ))
        ));
    }

    #[tokio::test]
    async fn completion_holds_target_ref_through_transaction_apply() {
        let mut context = test_context(test_job(
            "validate_integrated",
            OutputArtifactKind::ValidationReport,
        ));
        context.job.outcome_class = Some(OutcomeClass::Clean);
        context.convergences[0].input_target_commit_oid = Some("target".into());
        let hold_active = Arc::new(AtomicBool::new(false));
        let hold_released = Arc::new(AtomicBool::new(false));
        let repository =
            FakeRepository::new(context).assert_hold_active_on_apply(hold_active.clone());
        let git =
            FakeGitPort::default().with_hold_state(hold_active.clone(), hold_released.clone());
        let service = CompleteJobService::new(repository, git, FakeProjectLocks::default());

        service
            .execute(valid_validation_command())
            .await
            .expect("job completion should succeed");

        assert!(
            hold_released.load(Ordering::SeqCst),
            "target ref hold should be released after apply"
        );
    }

    #[tokio::test]
    async fn completion_fails_when_target_ref_hold_release_fails_after_apply() {
        let mut context = test_context(test_job(
            "validate_integrated",
            OutputArtifactKind::ValidationReport,
        ));
        context.job.outcome_class = Some(OutcomeClass::Clean);
        context.convergences[0].input_target_commit_oid = Some("target".into());
        let hold_active = Arc::new(AtomicBool::new(false));
        let hold_released = Arc::new(AtomicBool::new(false));
        let repository =
            FakeRepository::new(context).assert_hold_active_on_apply(hold_active.clone());
        let git = FakeGitPort::default()
            .with_hold_state(hold_active, hold_released.clone())
            .with_release_error(GitPortError::Internal("release timed out".into()));
        let service = CompleteJobService::new(repository.clone(), git, FakeProjectLocks::default());

        let result = service.execute(valid_validation_command()).await;

        assert!(matches!(
            result,
            Err(CompleteJobError::UseCase(UseCaseError::Internal(message)))
                if message == "git operation failed: release timed out"
        ));
        assert!(
            repository.last_mutation().is_some(),
            "completion mutation should still be applied"
        );
        assert!(
            !hold_released.load(Ordering::SeqCst),
            "target ref hold release should report failure"
        );
    }

    #[tokio::test]
    async fn completion_returns_apply_error_when_apply_and_release_hold_both_fail() {
        let mut context = test_context(test_job(
            "validate_integrated",
            OutputArtifactKind::ValidationReport,
        ));
        context.job.outcome_class = Some(OutcomeClass::Clean);
        context.convergences[0].input_target_commit_oid = Some("target".into());
        let release_calls = Arc::new(AtomicUsize::new(0));
        let repository = FakeRepository::new(context)
            .with_apply_error(RepositoryError::Conflict("job_revision_stale".into()));
        let git = FakeGitPort::default()
            .with_release_calls(release_calls.clone())
            .with_release_error(GitPortError::Internal("release timed out".into()));
        let service = CompleteJobService::new(repository, git, FakeProjectLocks::default());

        let result = service.execute(valid_validation_command()).await;

        assert!(matches!(
            result,
            Err(CompleteJobError::UseCase(UseCaseError::ProtocolViolation(message)))
                if message == "job completion does not match the current item revision"
        ));
        assert_eq!(
            release_calls.load(Ordering::SeqCst),
            1,
            "release should still be attempted when apply fails"
        );
    }

    #[tokio::test]
    async fn completion_retry_after_post_commit_hold_release_failure_returns_job_not_active() {
        let mut context = test_context(test_job(
            "validate_integrated",
            OutputArtifactKind::ValidationReport,
        ));
        context.job.outcome_class = Some(OutcomeClass::Clean);
        context.convergences[0].input_target_commit_oid = Some("target".into());
        let repository = FakeRepository::new(context);
        let git = FakeGitPort::default()
            .with_release_error(GitPortError::Internal("release timed out".into()));
        let service = CompleteJobService::new(repository.clone(), git, FakeProjectLocks::default());

        let first_attempt = service.execute(valid_validation_command()).await;
        let retry = service.execute(valid_validation_command()).await;

        assert!(matches!(
            first_attempt,
            Err(CompleteJobError::UseCase(UseCaseError::Internal(message)))
                if message == "git operation failed: release timed out"
        ));
        assert!(matches!(
            retry,
            Err(CompleteJobError::UseCase(UseCaseError::JobNotActive))
        ));
        assert_eq!(
            repository.apply_count(),
            1,
            "hold-bearing retries should not reapply completion"
        );
    }

    #[tokio::test]
    async fn completion_returns_matching_completed_job_as_idempotent_success() {
        let mut job = test_job("investigate_item", OutputArtifactKind::FindingReport);
        job.phase_kind = PhaseKind::Investigate;
        job.workspace_kind = WorkspaceKind::Review;
        job.status = JobStatus::Completed;
        job.outcome_class = Some(OutcomeClass::Findings);
        job.result_schema_version = Some("finding_report:v1".into());
        job.result_payload = Some(json!({
            "outcome": "findings",
            "summary": "Found issues",
            "findings": [{
                "finding_key": "f-1",
                "code": "BUG001",
                "severity": "high",
                "summary": "first",
                "paths": ["src/lib.rs"],
                "evidence": ["broken"]
            }]
        }));
        let repository = FakeRepository::new(test_context(job)).with_completion_finding_count(1);
        let service = CompleteJobService::new(
            repository,
            FakeGitPort::default(),
            FakeProjectLocks::default(),
        );

        let result = service
            .execute(completed_finding_report_command())
            .await
            .expect("matching completed job should be idempotent");

        assert_eq!(result.finding_count, 1);
    }

    #[tokio::test]
    async fn completion_rejects_mismatched_completed_job_retry() {
        let mut job = test_job("investigate_item", OutputArtifactKind::FindingReport);
        job.phase_kind = PhaseKind::Investigate;
        job.workspace_kind = WorkspaceKind::Review;
        job.status = JobStatus::Completed;
        job.outcome_class = Some(OutcomeClass::Findings);
        job.result_schema_version = Some("finding_report:v1".into());
        job.result_payload = Some(json!({
            "outcome": "findings",
            "summary": "Found issues",
            "findings": [{
                "finding_key": "f-1",
                "code": "BUG001",
                "severity": "high",
                "summary": "first",
                "paths": ["src/lib.rs"],
                "evidence": ["broken"]
            }]
        }));
        let service = test_service(test_context(job));
        let mut mismatched_command = completed_finding_report_command();
        mismatched_command.result_payload = Some(json!({
            "outcome": "findings",
            "summary": "Changed summary",
            "findings": [{
                "finding_key": "f-1",
                "code": "BUG001",
                "severity": "high",
                "summary": "first",
                "paths": ["src/lib.rs"],
                "evidence": ["broken"]
            }]
        }));

        let result = service.execute(mismatched_command).await;

        assert!(matches!(
            result,
            Err(CompleteJobError::UseCase(UseCaseError::JobNotActive))
        ));
    }

    #[tokio::test]
    async fn completion_returns_job_not_active_for_malformed_inactive_job_requests() {
        let mut context = test_context(test_job(
            "validate_integrated",
            OutputArtifactKind::ValidationReport,
        ));
        context.job.status = JobStatus::Failed;
        let service = test_service(context);

        let result = service
            .execute(CompleteJobCommand {
                job_id: JobId::from_uuid(Uuid::nil()),
                outcome_class: OutcomeClass::Clean,
                result_schema_version: None,
                result_payload: None,
                output_commit_oid: None,
            })
            .await;

        assert!(matches!(
            result,
            Err(CompleteJobError::UseCase(UseCaseError::JobNotActive))
        ));
    }

    #[tokio::test]
    async fn completion_returns_job_not_active_for_malformed_completed_non_hold_retries() {
        let mut job = test_job("investigate_item", OutputArtifactKind::FindingReport);
        job.phase_kind = PhaseKind::Investigate;
        job.workspace_kind = WorkspaceKind::Review;
        job.status = JobStatus::Completed;
        job.outcome_class = Some(OutcomeClass::Findings);
        job.result_schema_version = Some("finding_report:v1".into());
        job.result_payload = Some(json!({
            "outcome": "findings",
            "summary": "Found issues",
            "findings": [{
                "finding_key": "f-1",
                "code": "BUG001",
                "severity": "high",
                "summary": "first",
                "paths": ["src/lib.rs"],
                "evidence": ["broken"]
            }]
        }));
        let service = test_service(test_context(job));

        let result = service
            .execute(CompleteJobCommand {
                job_id: JobId::from_uuid(Uuid::nil()),
                outcome_class: OutcomeClass::Findings,
                result_schema_version: Some("finding_report:v1".into()),
                result_payload: None,
                output_commit_oid: None,
            })
            .await;

        assert!(matches!(
            result,
            Err(CompleteJobError::UseCase(UseCaseError::JobNotActive))
        ));
    }

    #[tokio::test]
    async fn completion_maps_transactional_revision_drift_to_protocol_violation() {
        let mut context = test_context(test_job(
            "validate_integrated",
            OutputArtifactKind::ValidationReport,
        ));
        context.job.outcome_class = Some(OutcomeClass::Clean);
        let repository = FakeRepository::new(context)
            .with_apply_error(RepositoryError::Conflict("job_revision_stale".into()));
        let service = CompleteJobService::new(
            repository,
            FakeGitPort::default(),
            FakeProjectLocks::default(),
        );

        let result = service.execute(valid_validation_command()).await;

        assert!(matches!(
            result,
            Err(CompleteJobError::UseCase(UseCaseError::ProtocolViolation(message)))
                if message == "job completion does not match the current item revision"
        ));
    }

    #[tokio::test]
    async fn completion_marks_successful_retry_to_clear_item_escalation() {
        let mut context = test_context(test_job(
            "validate_candidate_initial",
            OutputArtifactKind::ValidationReport,
        ));
        context.job.retry_no = 1;
        context.item.escalation_state = EscalationState::OperatorRequired;
        context.item.escalation_reason = Some(ingot_domain::item::EscalationReason::StepFailed);
        let repository = FakeRepository::new(context);
        let service = CompleteJobService::new(
            repository.clone(),
            FakeGitPort::default(),
            FakeProjectLocks::default(),
        );

        service
            .execute(valid_validation_command())
            .await
            .expect("completion succeeds");

        let mutation = repository.last_mutation().expect("last mutation");
        assert!(mutation.clear_item_escalation);
    }

    #[tokio::test]
    async fn completion_does_not_clear_item_escalation_for_initial_success() {
        let mut context = test_context(test_job(
            "validate_candidate_initial",
            OutputArtifactKind::ValidationReport,
        ));
        context.item.escalation_state = EscalationState::OperatorRequired;
        context.item.escalation_reason = Some(ingot_domain::item::EscalationReason::StepFailed);
        let repository = FakeRepository::new(context);
        let service = CompleteJobService::new(
            repository.clone(),
            FakeGitPort::default(),
            FakeProjectLocks::default(),
        );

        service
            .execute(valid_validation_command())
            .await
            .expect("completion succeeds");

        let mutation = repository.last_mutation().expect("last mutation");
        assert!(!mutation.clear_item_escalation);
    }

    fn valid_validation_command() -> CompleteJobCommand {
        CompleteJobCommand {
            job_id: JobId::from_uuid(Uuid::nil()),
            outcome_class: OutcomeClass::Clean,
            result_schema_version: Some("validation_report:v1".into()),
            result_payload: Some(json!({
                "outcome": "clean",
                "summary": "ok",
                "checks": [{
                    "name": "lint",
                    "status": "pass",
                    "summary": "ok"
                }],
                "findings": []
            })),
            output_commit_oid: None,
        }
    }

    fn completed_finding_report_command() -> CompleteJobCommand {
        CompleteJobCommand {
            job_id: JobId::from_uuid(Uuid::nil()),
            outcome_class: OutcomeClass::Findings,
            result_schema_version: Some("finding_report:v1".into()),
            result_payload: Some(json!({
                "outcome": "findings",
                "summary": "Found issues",
                "findings": [{
                    "finding_key": "f-1",
                    "code": "BUG001",
                    "severity": "high",
                    "summary": "first",
                    "paths": ["src/lib.rs"],
                    "evidence": ["broken"]
                }]
            })),
            output_commit_oid: None,
        }
    }

    fn test_service(
        context: JobCompletionContext,
    ) -> CompleteJobService<FakeRepository, FakeGitPort, FakeProjectLocks> {
        CompleteJobService::new(
            FakeRepository::new(context),
            FakeGitPort::default(),
            FakeProjectLocks::default(),
        )
    }

    fn test_context(job: Job) -> JobCompletionContext {
        JobCompletionContext {
            job,
            item: test_item(),
            project: test_project(),
            revision: test_revision(),
            convergences: vec![test_prepared_convergence()],
        }
    }

    fn test_project() -> Project {
        Project {
            id: ProjectId::from_uuid(Uuid::nil()),
            name: "Test".into(),
            path: std::env::temp_dir()
                .join(format!("ingot-usecases-{}", Uuid::now_v7()))
                .display()
                .to_string(),
            default_branch: "main".into(),
            color: "#000000".into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn test_item() -> Item {
        Item {
            id: ItemId::from_uuid(Uuid::nil()),
            project_id: ProjectId::from_uuid(Uuid::nil()),
            classification: Classification::Change,
            workflow_version: "delivery:v1".into(),
            lifecycle_state: LifecycleState::Open,
            parking_state: ParkingState::Active,
            done_reason: None,
            resolution_source: None,
            approval_state: ApprovalState::NotRequested,
            escalation_state: EscalationState::None,
            escalation_reason: None,
            current_revision_id: ItemRevisionId::from_uuid(Uuid::nil()),
            origin_kind: OriginKind::Manual,
            origin_finding_id: None,
            priority: Priority::Major,
            labels: vec![],
            operator_notes: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            closed_at: None,
        }
    }

    fn test_revision() -> ItemRevision {
        ItemRevision {
            id: ItemRevisionId::from_uuid(Uuid::nil()),
            item_id: ItemId::from_uuid(Uuid::nil()),
            revision_no: 1,
            title: "Title".into(),
            description: "Description".into(),
            acceptance_criteria: "AC".into(),
            target_ref: "refs/heads/main".into(),
            approval_policy: ApprovalPolicy::Required,
            policy_snapshot: json!({}),
            template_map_snapshot: json!({}),
            seed_commit_oid: "seed".into(),
            seed_target_commit_oid: Some("target".into()),
            supersedes_revision_id: None,
            created_at: Utc::now(),
        }
    }

    fn test_job(step_id: &str, output_artifact_kind: OutputArtifactKind) -> Job {
        Job {
            id: JobId::from_uuid(Uuid::nil()),
            project_id: ProjectId::from_uuid(Uuid::nil()),
            item_id: ItemId::from_uuid(Uuid::nil()),
            item_revision_id: ItemRevisionId::from_uuid(Uuid::nil()),
            step_id: step_id.into(),
            semantic_attempt_no: 1,
            retry_no: 0,
            supersedes_job_id: None,
            status: JobStatus::Running,
            outcome_class: Some(OutcomeClass::Clean),
            phase_kind: PhaseKind::Validate,
            workspace_id: Some(WorkspaceId::from_uuid(Uuid::now_v7())),
            workspace_kind: WorkspaceKind::Integration,
            execution_permission: ExecutionPermission::MustNotMutate,
            context_policy: ContextPolicy::ResumeContext,
            phase_template_slug: "validate-integrated".into(),
            phase_template_digest: None,
            prompt_snapshot: None,
            input_base_commit_oid: Some("target".into()),
            input_head_commit_oid: Some("prepared-head".into()),
            output_artifact_kind,
            output_commit_oid: None,
            result_schema_version: None,
            result_payload: None,
            agent_id: None,
            process_pid: None,
            lease_owner_id: None,
            heartbeat_at: None,
            lease_expires_at: None,
            error_code: None,
            error_message: None,
            created_at: Utc::now(),
            started_at: None,
            ended_at: None,
        }
    }

    fn test_prepared_convergence() -> Convergence {
        Convergence {
            id: ConvergenceId::from_uuid(Uuid::nil()),
            project_id: ProjectId::from_uuid(Uuid::nil()),
            item_id: ItemId::from_uuid(Uuid::nil()),
            item_revision_id: ItemRevisionId::from_uuid(Uuid::nil()),
            source_workspace_id: WorkspaceId::from_uuid(Uuid::now_v7()),
            integration_workspace_id: Some(WorkspaceId::from_uuid(Uuid::now_v7())),
            source_head_commit_oid: "prepared-head".into(),
            target_ref: "refs/heads/main".into(),
            strategy: ConvergenceStrategy::RebaseThenFastForward,
            status: ConvergenceStatus::Prepared,
            input_target_commit_oid: Some("target".into()),
            prepared_commit_oid: Some("prepared-head".into()),
            final_target_commit_oid: None,
            target_head_valid: None,
            conflict_summary: None,
            created_at: Utc::now(),
            completed_at: None,
        }
    }

    #[derive(Clone)]
    struct FakeRepository {
        state: Arc<Mutex<FakeRepositoryState>>,
    }

    struct FakeRepositoryState {
        context: JobCompletionContext,
        last_mutation: Option<JobCompletionMutation>,
        apply_error: Option<RepositoryError>,
        hold_active: Option<Arc<AtomicBool>>,
        completion_finding_count: usize,
        apply_count: usize,
    }

    impl FakeRepository {
        fn new(context: JobCompletionContext) -> Self {
            Self {
                state: Arc::new(Mutex::new(FakeRepositoryState {
                    context,
                    last_mutation: None,
                    apply_error: None,
                    hold_active: None,
                    completion_finding_count: 0,
                    apply_count: 0,
                })),
            }
        }

        fn assert_hold_active_on_apply(self, hold_active: Arc<AtomicBool>) -> Self {
            self.state.lock().expect("state lock").hold_active = Some(hold_active);
            self
        }

        fn with_apply_error(self, apply_error: RepositoryError) -> Self {
            self.state.lock().expect("state lock").apply_error = Some(apply_error);
            self
        }

        fn with_completion_finding_count(self, completion_finding_count: usize) -> Self {
            self.state
                .lock()
                .expect("state lock")
                .completion_finding_count = completion_finding_count;
            self
        }

        fn last_mutation(&self) -> Option<JobCompletionMutation> {
            self.state.lock().expect("state lock").last_mutation.clone()
        }

        fn apply_count(&self) -> usize {
            self.state.lock().expect("state lock").apply_count
        }
    }

    impl JobCompletionRepository for FakeRepository {
        fn load_job_completion_context(
            &self,
            _job_id: JobId,
        ) -> impl std::future::Future<Output = Result<JobCompletionContext, RepositoryError>> + Send
        {
            let context = self.state.lock().expect("state lock").context.clone();
            async move { Ok(context) }
        }

        fn load_completed_job_completion(
            &self,
            _job_id: JobId,
        ) -> impl std::future::Future<
            Output = Result<Option<ingot_domain::ports::CompletedJobCompletion>, RepositoryError>,
        > + Send {
            let completed = {
                let state = self.state.lock().expect("state lock");
                (state.context.job.status == JobStatus::Completed).then(|| {
                    ingot_domain::ports::CompletedJobCompletion {
                        job: state.context.job.clone(),
                        finding_count: state.completion_finding_count,
                    }
                })
            };
            async move { Ok(completed) }
        }

        fn apply_job_completion(
            &self,
            mutation: JobCompletionMutation,
        ) -> impl std::future::Future<Output = Result<(), RepositoryError>> + Send {
            let state = self.state.clone();
            async move {
                let mut state = state.lock().expect("state lock");
                state.apply_count += 1;
                if let Some(hold_active) = &state.hold_active {
                    assert!(
                        hold_active.load(Ordering::SeqCst),
                        "target ref hold should still be active during apply"
                    );
                }
                state.last_mutation = Some(mutation.clone());
                if let Some(error) = state.apply_error.take() {
                    return Err(error);
                }
                state.context.job.status = JobStatus::Completed;
                state.context.job.outcome_class = Some(mutation.outcome_class);
                state.context.job.result_schema_version = mutation.result_schema_version.clone();
                state.context.job.result_payload = mutation.result_payload.clone();
                state.context.job.output_commit_oid = mutation.output_commit_oid.clone();
                if mutation.clear_item_escalation {
                    state.context.item.escalation_state = ingot_domain::item::EscalationState::None;
                    state.context.item.escalation_reason = None;
                }
                state.completion_finding_count = mutation.findings.len();
                Ok(())
            }
        }
    }

    #[derive(Clone, Default)]
    struct FakeGitPort {
        commit_exists: bool,
        hold_error: Option<Arc<Mutex<Option<TargetRefHoldError>>>>,
        hold_active: Option<Arc<AtomicBool>>,
        hold_released: Option<Arc<AtomicBool>>,
        release_error: Option<Arc<Mutex<Option<GitPortError>>>>,
        release_calls: Option<Arc<AtomicUsize>>,
    }

    #[derive(Debug)]
    struct FakeHold;

    impl FakeGitPort {
        fn with_commit_exists(mut self, commit_exists: bool) -> Self {
            self.commit_exists = commit_exists;
            self
        }

        fn with_hold_error(mut self, error: TargetRefHoldError) -> Self {
            self.hold_error = Some(Arc::new(Mutex::new(Some(error))));
            self
        }

        fn with_hold_state(
            mut self,
            hold_active: Arc<AtomicBool>,
            hold_released: Arc<AtomicBool>,
        ) -> Self {
            self.hold_active = Some(hold_active);
            self.hold_released = Some(hold_released);
            self
        }

        fn with_release_error(mut self, error: GitPortError) -> Self {
            self.release_error = Some(Arc::new(Mutex::new(Some(error))));
            self
        }

        fn with_release_calls(mut self, release_calls: Arc<AtomicUsize>) -> Self {
            self.release_calls = Some(release_calls);
            self
        }
    }

    impl JobCompletionGitPort for FakeGitPort {
        type Hold = FakeHold;

        fn commit_exists(
            &self,
            _repo_path: &Path,
            _commit_oid: &str,
        ) -> impl std::future::Future<Output = Result<bool, GitPortError>> + Send {
            let commit_exists = self.commit_exists;
            async move { Ok(commit_exists) }
        }

        fn verify_and_hold_target_ref(
            &self,
            _repo_path: &Path,
            _target_ref: &str,
            _expected_oid: &str,
        ) -> impl std::future::Future<Output = Result<Self::Hold, TargetRefHoldError>> + Send
        {
            let hold_error = self.hold_error.clone();
            let hold_active = self.hold_active.clone();
            async move {
                if let Some(hold_error) = hold_error
                    && let Some(error) = hold_error.lock().expect("hold error lock").take()
                {
                    return Err(error);
                }

                if let Some(hold_active) = hold_active {
                    hold_active.store(true, Ordering::SeqCst);
                }

                Ok(FakeHold)
            }
        }

        fn release_hold(
            &self,
            _hold: Self::Hold,
        ) -> impl std::future::Future<Output = Result<(), GitPortError>> + Send {
            let hold_active = self.hold_active.clone();
            let hold_released = self.hold_released.clone();
            let release_error = self.release_error.clone();
            let release_calls = self.release_calls.clone();
            async move {
                if let Some(release_calls) = release_calls {
                    release_calls.fetch_add(1, Ordering::SeqCst);
                }
                if let Some(release_error) = release_error
                    && let Some(error) = release_error.lock().expect("release error lock").take()
                {
                    return Err(error);
                }

                if let Some(hold_active) = hold_active {
                    hold_active.store(false, Ordering::SeqCst);
                }
                if let Some(hold_released) = hold_released {
                    hold_released.store(true, Ordering::SeqCst);
                }
                Ok(())
            }
        }
    }

    #[derive(Clone, Default)]
    struct FakeProjectLocks {
        acquire_count: Arc<AtomicUsize>,
    }

    impl ProjectMutationLockPort for FakeProjectLocks {
        type Guard = ();

        fn acquire_project_mutation(
            &self,
            _project_id: ingot_domain::ids::ProjectId,
        ) -> impl std::future::Future<Output = Self::Guard> + Send {
            self.acquire_count.fetch_add(1, Ordering::SeqCst);
            async {}
        }
    }
}
