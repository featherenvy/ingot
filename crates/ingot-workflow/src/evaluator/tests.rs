use chrono::Utc;
use ingot_domain::finding::FindingTriageState;
use ingot_domain::ids::{ItemId, ItemRevisionId, ProjectId};
use ingot_domain::item::ApprovalState;
use ingot_domain::job::{Job, JobInput, JobStatus, OutcomeClass, OutputArtifactKind, PhaseKind};
use ingot_domain::revision::ApprovalPolicy;
use ingot_domain::step_id::StepId;
use ingot_domain::test_support::{
    ConvergenceBuilder, FindingBuilder, JobBuilder, RevisionBuilder, nil_item,
};
use uuid::Uuid;

use super::{BoardStatus, Evaluator, PhaseStatus};
use crate::{NamedRecommendedAction, RecommendedAction};

#[test]
fn unknown_system_actions_degrade_to_operator_intervention() {
    let mut diagnostics = Vec::new();

    let projection = super::projection::system_action_projection(
        Some(StepId::ValidateCandidateInitial),
        "unknown_internal_action",
        &mut diagnostics,
    );

    assert_eq!(projection.phase_status, PhaseStatus::Unknown);
    assert_eq!(
        projection.next_recommended_action,
        RecommendedAction::named(NamedRecommendedAction::OperatorIntervention)
    );
    assert_eq!(
        diagnostics,
        vec!["unknown internal recommended action: unknown_internal_action".to_owned()]
    );
}

#[test]
fn report_only_jobs_keep_the_closure_position_visible() {
    let evaluator = Evaluator::new();
    let item = nil_item();
    let revision = test_revision(ApprovalPolicy::Required);
    let jobs = vec![
        test_job(
            StepId::ReviewIncrementalInitial,
            PhaseKind::Review,
            JobStatus::Completed,
            Some(OutcomeClass::Clean),
        ),
        test_job(
            StepId::InvestigateItem,
            PhaseKind::Investigate,
            JobStatus::Running,
            None,
        ),
    ];

    let evaluation = evaluator.evaluate(&item, &revision, &jobs, &[], &[]);

    assert_eq!(
        evaluation.current_step_id.map(StepId::as_str),
        Some(StepId::ReviewIncrementalInitial.as_str())
    );
    assert_eq!(evaluation.current_phase_kind, Some(PhaseKind::Investigate));
    assert_eq!(evaluation.phase_status, Some(PhaseStatus::Running));
    assert_eq!(evaluation.board_status, BoardStatus::Working);
}

#[test]
fn idle_items_expose_investigation_as_auxiliary_dispatch() {
    let evaluator = Evaluator::new();
    let item = nil_item();
    let revision = test_revision(ApprovalPolicy::Required);
    let jobs = vec![test_job(
        StepId::ReviewIncrementalInitial,
        PhaseKind::Review,
        JobStatus::Completed,
        Some(OutcomeClass::Clean),
    )];

    let evaluation = evaluator.evaluate(&item, &revision, &jobs, &[], &[]);

    assert_eq!(
        evaluation.dispatchable_step_id.map(StepId::as_str),
        Some(StepId::ReviewCandidateInitial.as_str())
    );
    assert_eq!(
        evaluation.auxiliary_dispatchable_step_ids,
        vec![StepId::InvestigateItem]
    );
}

#[test]
fn clean_authoring_commits_flow_into_incremental_review() {
    let evaluator = Evaluator::new();
    let item = nil_item();
    let revision = test_revision(ApprovalPolicy::Required);
    let jobs = vec![test_job(
        StepId::AuthorInitial,
        PhaseKind::Author,
        JobStatus::Completed,
        Some(OutcomeClass::Clean),
    )];

    let evaluation = evaluator.evaluate(&item, &revision, &jobs, &[], &[]);

    assert_eq!(
        evaluation.dispatchable_step_id.map(StepId::as_str),
        Some(StepId::ReviewIncrementalInitial.as_str())
    );
    assert_eq!(
        evaluation.next_recommended_action,
        RecommendedAction::dispatch(StepId::ReviewIncrementalInitial)
    );
}

#[test]
fn clean_whole_candidate_review_flows_to_candidate_validation() {
    let evaluator = Evaluator::new();
    let item = nil_item();
    let revision = test_revision(ApprovalPolicy::Required);
    let jobs = vec![test_job(
        StepId::ReviewCandidateInitial,
        PhaseKind::Review,
        JobStatus::Completed,
        Some(OutcomeClass::Clean),
    )];

    let evaluation = evaluator.evaluate(&item, &revision, &jobs, &[], &[]);

    assert_eq!(
        evaluation.dispatchable_step_id.map(StepId::as_str),
        Some(StepId::ValidateCandidateInitial.as_str())
    );
    assert_eq!(
        evaluation.next_recommended_action,
        RecommendedAction::dispatch(StepId::ValidateCandidateInitial)
    );
}

#[test]
fn daemon_only_next_steps_are_not_projected_as_dispatchable_jobs() {
    let evaluator = Evaluator::new();
    let item = nil_item();
    let revision = test_revision(ApprovalPolicy::Required);
    let jobs = vec![test_job(
        StepId::ValidateCandidateInitial,
        PhaseKind::Validate,
        JobStatus::Completed,
        Some(OutcomeClass::Clean),
    )];

    let evaluation = evaluator.evaluate(&item, &revision, &jobs, &[], &[]);

    assert_eq!(
        evaluation.next_recommended_action,
        RecommendedAction::named(NamedRecommendedAction::PrepareConvergence)
    );
    assert_eq!(evaluation.dispatchable_step_id, None);
    assert_eq!(
        evaluation.phase_status,
        Some(PhaseStatus::AwaitingConvergence)
    );
}

#[test]
fn stale_prepared_convergences_project_invalidation() {
    let evaluator = Evaluator::new();
    let mut item = nil_item();
    item.approval_state = ApprovalState::Pending;

    let revision = test_revision(ApprovalPolicy::Required);
    let jobs = vec![test_job(
        StepId::ValidateIntegrated,
        PhaseKind::Validate,
        JobStatus::Completed,
        Some(OutcomeClass::Clean),
    )];
    let convergences = vec![test_prepared_convergence(false)];

    let evaluation = evaluator.evaluate(&item, &revision, &jobs, &[], &convergences);

    assert_eq!(
        evaluation.next_recommended_action,
        RecommendedAction::named(NamedRecommendedAction::InvalidatePreparedConvergence)
    );
    assert_eq!(evaluation.board_status, BoardStatus::Working);
    assert!(evaluation.allowed_actions.is_empty());
}

#[test]
fn finalized_convergences_awaiting_checkout_sync_stay_in_working_state() {
    let evaluator = Evaluator::new();
    let item = nil_item();
    let revision = test_revision(ApprovalPolicy::NotRequired);
    let jobs = vec![test_job(
        StepId::ValidateIntegrated,
        PhaseKind::Validate,
        JobStatus::Completed,
        Some(OutcomeClass::Clean),
    )];
    let convergences = vec![test_blocked_finalized_convergence()];

    let evaluation = evaluator.evaluate(&item, &revision, &jobs, &[], &convergences);

    assert_eq!(evaluation.board_status, BoardStatus::Working);
    assert_eq!(
        evaluation.phase_status,
        Some(PhaseStatus::AwaitingCheckoutSync)
    );
    assert_eq!(
        evaluation.next_recommended_action,
        RecommendedAction::named(NamedRecommendedAction::ResolveCheckoutSync)
    );
    assert_eq!(evaluation.dispatchable_step_id, None);
}

#[test]
fn integrated_validation_findings_follow_graph_to_repair() {
    let evaluator = Evaluator::new();
    let item = nil_item();
    let revision = test_revision(ApprovalPolicy::Required);
    let job = test_job(
        StepId::ValidateIntegrated,
        PhaseKind::Validate,
        JobStatus::Completed,
        Some(OutcomeClass::Findings),
    );
    let jobs = vec![job.clone()];
    let findings = vec![test_finding(&job, FindingTriageState::FixNow)];
    let convergences = vec![test_prepared_convergence(true)];

    let evaluation = evaluator.evaluate(&item, &revision, &jobs, &findings, &convergences);

    assert_eq!(
        evaluation.dispatchable_step_id.map(StepId::as_str),
        Some(StepId::RepairAfterIntegration.as_str())
    );
    assert_eq!(
        evaluation.next_recommended_action,
        RecommendedAction::dispatch(StepId::RepairAfterIntegration)
    );
}

#[test]
fn untriaged_findings_block_dispatch_in_triage_state() {
    let evaluator = Evaluator::new();
    let item = nil_item();
    let revision = test_revision(ApprovalPolicy::Required);
    let job = test_job(
        StepId::ReviewCandidateInitial,
        PhaseKind::Review,
        JobStatus::Completed,
        Some(OutcomeClass::Findings),
    );
    let jobs = vec![job.clone()];
    let findings = vec![test_finding(&job, FindingTriageState::Untriaged)];

    let evaluation = evaluator.evaluate(&item, &revision, &jobs, &findings, &[]);

    assert_eq!(evaluation.phase_status, Some(PhaseStatus::Triaging));
    assert_eq!(
        evaluation.next_recommended_action,
        RecommendedAction::named(NamedRecommendedAction::TriageFindings)
    );
    assert_eq!(evaluation.dispatchable_step_id, None);
}

#[test]
fn non_blocking_triaged_findings_follow_clean_edge() {
    let evaluator = Evaluator::new();
    let item = nil_item();
    let revision = test_revision(ApprovalPolicy::Required);
    let job = test_job(
        StepId::ReviewCandidateInitial,
        PhaseKind::Review,
        JobStatus::Completed,
        Some(OutcomeClass::Findings),
    );
    let jobs = vec![job.clone()];
    let findings = vec![test_finding(&job, FindingTriageState::WontFix)];

    let evaluation = evaluator.evaluate(&item, &revision, &jobs, &findings, &[]);

    assert_eq!(
        evaluation.dispatchable_step_id.map(StepId::as_str),
        Some(StepId::ValidateCandidateInitial.as_str())
    );
}

#[test]
fn post_integration_repairs_reenter_incremental_review() {
    let evaluator = Evaluator::new();
    let item = nil_item();
    let revision = test_revision(ApprovalPolicy::Required);
    let jobs = vec![test_job(
        StepId::RepairAfterIntegration,
        PhaseKind::Author,
        JobStatus::Completed,
        Some(OutcomeClass::Clean),
    )];

    let evaluation = evaluator.evaluate(&item, &revision, &jobs, &[], &[]);

    assert_eq!(
        evaluation.dispatchable_step_id.map(StepId::as_str),
        Some(StepId::ReviewIncrementalAfterIntegrationRepair.as_str())
    );
    assert_eq!(
        evaluation.next_recommended_action,
        RecommendedAction::dispatch(StepId::ReviewIncrementalAfterIntegrationRepair)
    );
}

#[test]
fn terminal_jobs_without_outcomes_do_not_advance_workflow() {
    let evaluator = Evaluator::new();
    let item = nil_item();
    let revision = test_revision(ApprovalPolicy::Required);
    let jobs = vec![test_job(
        StepId::ValidateCandidateInitial,
        PhaseKind::Validate,
        JobStatus::Expired,
        None,
    )];

    let evaluation = evaluator.evaluate(&item, &revision, &jobs, &[], &[]);

    assert_eq!(evaluation.dispatchable_step_id, None);
    assert_eq!(evaluation.phase_status, Some(PhaseStatus::Unknown));
    assert!(
        evaluation
            .diagnostics
            .iter()
            .any(|value| value.contains("has no outcome_class"))
    );
}

#[test]
fn cancelled_jobs_do_not_auto_redispatch() {
    let evaluator = Evaluator::new();
    let item = nil_item();
    let revision = test_revision(ApprovalPolicy::Required);
    let jobs = vec![test_job(
        StepId::ValidateCandidateInitial,
        PhaseKind::Validate,
        JobStatus::Cancelled,
        Some(OutcomeClass::Cancelled),
    )];

    let evaluation = evaluator.evaluate(&item, &revision, &jobs, &[], &[]);

    assert_eq!(evaluation.dispatchable_step_id, None);
    assert_eq!(evaluation.next_recommended_action, RecommendedAction::None);
    assert_eq!(
        evaluation.current_step_id.map(StepId::as_str),
        Some(StepId::ValidateCandidateInitial.as_str())
    );
}

#[test]
fn transient_failures_do_not_auto_redispatch_without_retry_policy() {
    let evaluator = Evaluator::new();
    let item = nil_item();
    let revision = test_revision(ApprovalPolicy::Required);
    let jobs = vec![test_job(
        StepId::ValidateCandidateInitial,
        PhaseKind::Validate,
        JobStatus::Failed,
        Some(OutcomeClass::TransientFailure),
    )];

    let evaluation = evaluator.evaluate(&item, &revision, &jobs, &[], &[]);

    assert_eq!(evaluation.dispatchable_step_id, None);
    assert_eq!(evaluation.next_recommended_action, RecommendedAction::None);
    assert_eq!(
        evaluation.current_step_id.map(StepId::as_str),
        Some(StepId::ValidateCandidateInitial.as_str())
    );
}

fn test_revision(approval_policy: ApprovalPolicy) -> ingot_domain::revision::ItemRevision {
    RevisionBuilder::nil()
        .approval_policy(approval_policy)
        .explicit_seed("abc123")
        .seed_target_commit_oid(Some("def456"))
        .build()
}

fn test_job(
    step_id: StepId,
    phase_kind: PhaseKind,
    status: JobStatus,
    outcome_class: Option<OutcomeClass>,
) -> Job {
    let nil = Uuid::nil();
    let output_artifact_kind = match step_id {
        StepId::InvestigateItem => OutputArtifactKind::FindingReport,
        StepId::InvestigateProject | StepId::ReinvestigateProject => {
            OutputArtifactKind::InvestigationReport
        }
        StepId::AuthorInitial | StepId::RepairCandidate | StepId::RepairAfterIntegration => {
            OutputArtifactKind::Commit
        }
        StepId::ValidateIntegrated
        | StepId::ValidateCandidateInitial
        | StepId::ValidateCandidateRepair
        | StepId::ValidateAfterIntegrationRepair => OutputArtifactKind::ValidationReport,
        _ => OutputArtifactKind::ReviewReport,
    };
    let mut builder = JobBuilder::new(
        ProjectId::from_uuid(nil),
        ItemId::from_uuid(nil),
        ItemRevisionId::from_uuid(nil),
        step_id,
    )
    .status(status)
    .phase_kind(phase_kind)
    .execution_permission(ingot_domain::job::ExecutionPermission::MustNotMutate)
    .job_input(JobInput::candidate_subject("base".into(), "head".into()))
    .output_artifact_kind(output_artifact_kind)
    .ended_at(Utc::now());
    if let Some(oc) = outcome_class {
        builder = builder.outcome_class(oc);
    }
    builder.build()
}

fn test_prepared_convergence(target_head_valid: bool) -> ingot_domain::convergence::Convergence {
    ConvergenceBuilder::new(
        ProjectId::from_uuid(Uuid::nil()),
        ItemId::from_uuid(Uuid::nil()),
        ItemRevisionId::from_uuid(Uuid::nil()),
    )
    .target_head_valid(target_head_valid)
    .build()
}

fn test_blocked_finalized_convergence() -> ingot_domain::convergence::Convergence {
    ConvergenceBuilder::new(
        ProjectId::from_uuid(Uuid::nil()),
        ItemId::from_uuid(Uuid::nil()),
        ItemRevisionId::from_uuid(Uuid::nil()),
    )
    .status(ingot_domain::convergence::ConvergenceStatus::Finalized)
    .final_target_commit_oid("final")
    .checkout_adoption_blocked_at("registered checkout blocked", Utc::now())
    .build()
}

fn investigation_item() -> ingot_domain::item::Item {
    let mut item = nil_item();
    item.classification = ingot_domain::item::Classification::Investigation;
    item.workflow_version = ingot_domain::item::WorkflowVersion::InvestigationV1;
    item
}

fn test_finding(job: &Job, triage_state: FindingTriageState) -> ingot_domain::finding::Finding {
    let mut builder = FindingBuilder::new(
        ProjectId::from_uuid(Uuid::nil()),
        ItemId::from_uuid(Uuid::nil()),
        ItemRevisionId::from_uuid(Uuid::nil()),
        job.id,
    )
    .source_step_id(job.step_id)
    .source_finding_key("finding-1")
    .triage_state(triage_state);
    if triage_state == FindingTriageState::WontFix {
        builder = builder.triage_note("accepted");
    }
    if triage_state != FindingTriageState::Untriaged {
        builder = builder.triaged_at(Utc::now());
    }
    builder.build()
}

// ── Investigation workflow tests ──────────────────────────────────────────────

#[test]
fn investigation_new_item_dispatches_investigate_project() {
    let evaluator = Evaluator::new();
    let item = investigation_item();
    let revision = test_revision(ApprovalPolicy::Required);

    let evaluation = evaluator.evaluate(&item, &revision, &[], &[], &[]);

    // finish() promotes Inbox -> Working when dispatchable_step_id is present
    assert_eq!(evaluation.board_status, BoardStatus::Working);
    assert_eq!(evaluation.phase_status, Some(PhaseStatus::New));
    assert_eq!(
        evaluation.dispatchable_step_id.map(StepId::as_str),
        Some(StepId::InvestigateProject.as_str())
    );
    assert_eq!(
        evaluation.next_recommended_action,
        RecommendedAction::dispatch(StepId::InvestigateProject)
    );
}

#[test]
fn investigation_active_job_shows_running() {
    let evaluator = Evaluator::new();
    let item = investigation_item();
    let revision = test_revision(ApprovalPolicy::Required);
    let jobs = vec![test_job(
        StepId::InvestigateProject,
        PhaseKind::Investigate,
        JobStatus::Running,
        None,
    )];

    let evaluation = evaluator.evaluate(&item, &revision, &jobs, &[], &[]);

    assert_eq!(evaluation.board_status, BoardStatus::Working);
    assert_eq!(evaluation.phase_status, Some(PhaseStatus::Running));
    assert!(
        evaluation
            .allowed_actions
            .contains(&super::AllowedAction::CancelJob)
    );
    assert_eq!(evaluation.dispatchable_step_id, None);
}

#[test]
fn investigation_clean_outcome_shows_terminal_readiness() {
    let evaluator = Evaluator::new();
    let item = investigation_item();
    let revision = test_revision(ApprovalPolicy::Required);
    let jobs = vec![test_job(
        StepId::InvestigateProject,
        PhaseKind::Investigate,
        JobStatus::Completed,
        Some(OutcomeClass::Clean),
    )];

    let evaluation = evaluator.evaluate(&item, &revision, &jobs, &[], &[]);

    assert!(evaluation.terminal_readiness);
    assert_eq!(evaluation.phase_status, Some(PhaseStatus::Idle));
    assert_eq!(evaluation.board_status, BoardStatus::Working);
    assert_eq!(evaluation.dispatchable_step_id, None);
}

#[test]
fn investigation_findings_with_untriaged_shows_triaging() {
    let evaluator = Evaluator::new();
    let item = investigation_item();
    let revision = test_revision(ApprovalPolicy::Required);
    let job = test_job(
        StepId::InvestigateProject,
        PhaseKind::Investigate,
        JobStatus::Completed,
        Some(OutcomeClass::Findings),
    );
    let jobs = vec![job.clone()];
    let findings = vec![test_finding(&job, FindingTriageState::Untriaged)];

    let evaluation = evaluator.evaluate(&item, &revision, &jobs, &findings, &[]);

    assert_eq!(evaluation.phase_status, Some(PhaseStatus::Triaging));
    assert_eq!(
        evaluation.next_recommended_action,
        RecommendedAction::named(NamedRecommendedAction::TriageFindings)
    );
    assert_eq!(evaluation.dispatchable_step_id, None);
}

#[test]
fn investigation_all_findings_triaged_non_blocking_shows_terminal() {
    let evaluator = Evaluator::new();
    let item = investigation_item();
    let revision = test_revision(ApprovalPolicy::Required);
    let job = test_job(
        StepId::InvestigateProject,
        PhaseKind::Investigate,
        JobStatus::Completed,
        Some(OutcomeClass::Findings),
    );
    let jobs = vec![job.clone()];
    let findings = vec![test_finding(&job, FindingTriageState::WontFix)];

    let evaluation = evaluator.evaluate(&item, &revision, &jobs, &findings, &[]);

    assert!(evaluation.terminal_readiness);
    assert_eq!(evaluation.phase_status, Some(PhaseStatus::Idle));
    assert_eq!(evaluation.dispatchable_step_id, None);
}

#[test]
fn investigation_fix_now_findings_dispatch_reinvestigate() {
    let evaluator = Evaluator::new();
    let item = investigation_item();
    let revision = test_revision(ApprovalPolicy::Required);
    let job = test_job(
        StepId::InvestigateProject,
        PhaseKind::Investigate,
        JobStatus::Completed,
        Some(OutcomeClass::Findings),
    );
    let jobs = vec![job.clone()];
    let findings = vec![test_finding(&job, FindingTriageState::FixNow)];

    let evaluation = evaluator.evaluate(&item, &revision, &jobs, &findings, &[]);

    assert_eq!(
        evaluation.dispatchable_step_id.map(StepId::as_str),
        Some(StepId::ReinvestigateProject.as_str())
    );
    assert_eq!(
        evaluation.next_recommended_action,
        RecommendedAction::dispatch(StepId::ReinvestigateProject)
    );
}

#[test]
fn investigation_needs_investigation_shows_triaging() {
    let evaluator = Evaluator::new();
    let item = investigation_item();
    let revision = test_revision(ApprovalPolicy::Required);
    let job = test_job(
        StepId::InvestigateProject,
        PhaseKind::Investigate,
        JobStatus::Completed,
        Some(OutcomeClass::Findings),
    );
    let jobs = vec![job.clone()];
    // NeedsInvestigation is_unresolved()==true, so it gates at the triage check
    // just like Untriaged, requiring operator triage before dispatch.
    let findings = vec![test_finding(&job, FindingTriageState::NeedsInvestigation)];

    let evaluation = evaluator.evaluate(&item, &revision, &jobs, &findings, &[]);

    assert_eq!(evaluation.phase_status, Some(PhaseStatus::Triaging));
    assert_eq!(
        evaluation.next_recommended_action,
        RecommendedAction::named(NamedRecommendedAction::TriageFindings)
    );
    assert_eq!(evaluation.dispatchable_step_id, None);
}

#[test]
fn investigation_escalated_shows_operator_intervention() {
    let evaluator = Evaluator::new();
    let mut item = investigation_item();
    item.escalation = ingot_domain::item::Escalation::OperatorRequired {
        reason: ingot_domain::item::EscalationReason::ManualDecisionRequired,
    };
    let revision = test_revision(ApprovalPolicy::Required);

    let evaluation = evaluator.evaluate(&item, &revision, &[], &[], &[]);

    assert_eq!(evaluation.phase_status, Some(PhaseStatus::Escalated));
    assert_eq!(
        evaluation.next_recommended_action,
        RecommendedAction::named(NamedRecommendedAction::OperatorIntervention)
    );
    assert_eq!(evaluation.board_status, BoardStatus::Working);
}

#[test]
fn investigation_deferred_shows_deferred() {
    let evaluator = Evaluator::new();
    let mut item = investigation_item();
    item.parking_state = ingot_domain::item::ParkingState::Deferred;
    let revision = test_revision(ApprovalPolicy::Required);

    let evaluation = evaluator.evaluate(&item, &revision, &[], &[], &[]);

    assert_eq!(evaluation.phase_status, Some(PhaseStatus::Deferred));
    assert_eq!(evaluation.board_status, BoardStatus::Inbox);
    assert_eq!(evaluation.next_recommended_action, RecommendedAction::None);
    assert!(
        evaluation
            .allowed_actions
            .contains(&super::AllowedAction::Resume)
    );
}

#[test]
fn investigation_items_have_no_auxiliary_steps() {
    let evaluator = Evaluator::new();
    let item = investigation_item();
    let revision = test_revision(ApprovalPolicy::Required);

    // New item — no jobs
    let evaluation = evaluator.evaluate(&item, &revision, &[], &[], &[]);
    assert!(evaluation.auxiliary_dispatchable_step_ids.is_empty());

    // Completed clean job — idle state
    let jobs = vec![test_job(
        StepId::InvestigateProject,
        PhaseKind::Investigate,
        JobStatus::Completed,
        Some(OutcomeClass::Clean),
    )];
    let evaluation = evaluator.evaluate(&item, &revision, &jobs, &[], &[]);
    assert!(evaluation.auxiliary_dispatchable_step_ids.is_empty());

    // Findings with dispatch available
    let job = test_job(
        StepId::InvestigateProject,
        PhaseKind::Investigate,
        JobStatus::Completed,
        Some(OutcomeClass::Findings),
    );
    let jobs = vec![job.clone()];
    let findings = vec![test_finding(&job, FindingTriageState::FixNow)];
    let evaluation = evaluator.evaluate(&item, &revision, &jobs, &findings, &[]);
    assert!(evaluation.auxiliary_dispatchable_step_ids.is_empty());
}
