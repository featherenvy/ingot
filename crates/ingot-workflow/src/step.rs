use ingot_domain::job::{ContextPolicy, ExecutionPermission, OutputArtifactKind, PhaseKind};
use ingot_domain::step_id::StepId;
use ingot_domain::workspace::WorkspaceKind;

pub const AUTHOR_INITIAL: StepId = StepId::AuthorInitial;
pub const REVIEW_INCREMENTAL_INITIAL: StepId = StepId::ReviewIncrementalInitial;
pub const REVIEW_CANDIDATE_INITIAL: StepId = StepId::ReviewCandidateInitial;
pub const VALIDATE_CANDIDATE_INITIAL: StepId = StepId::ValidateCandidateInitial;
pub const REPAIR_CANDIDATE: StepId = StepId::RepairCandidate;
pub const REVIEW_INCREMENTAL_REPAIR: StepId = StepId::ReviewIncrementalRepair;
pub const REVIEW_CANDIDATE_REPAIR: StepId = StepId::ReviewCandidateRepair;
pub const VALIDATE_CANDIDATE_REPAIR: StepId = StepId::ValidateCandidateRepair;
pub const INVESTIGATE_ITEM: StepId = StepId::InvestigateItem;
pub const PREPARE_CONVERGENCE: StepId = StepId::PrepareConvergence;
pub const VALIDATE_INTEGRATED: StepId = StepId::ValidateIntegrated;
pub const REPAIR_AFTER_INTEGRATION: StepId = StepId::RepairAfterIntegration;
pub const REVIEW_INCREMENTAL_AFTER_INTEGRATION_REPAIR: StepId =
    StepId::ReviewIncrementalAfterIntegrationRepair;
pub const REVIEW_AFTER_INTEGRATION_REPAIR: StepId = StepId::ReviewAfterIntegrationRepair;
pub const VALIDATE_AFTER_INTEGRATION_REPAIR: StepId = StepId::ValidateAfterIntegrationRepair;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClosureRelevance {
    ClosureRelevant,
    ReportOnly,
}

#[derive(Debug, Clone)]
pub struct StepContract {
    pub step_id: StepId,
    pub phase_kind: PhaseKind,
    pub workspace_kind: WorkspaceKind,
    pub execution_permission: ExecutionPermission,
    pub context_policy: ContextPolicy,
    pub output_artifact_kind: OutputArtifactKind,
    pub closure_relevance: ClosureRelevance,
    pub default_template_slug: Option<&'static str>,
}

impl StepContract {
    pub fn is_dispatchable_job(&self) -> bool {
        self.phase_kind != PhaseKind::System
    }
}

const fn author_step(
    step_id: StepId,
    context_policy: ContextPolicy,
    template_slug: &'static str,
) -> StepContract {
    StepContract {
        step_id,
        phase_kind: PhaseKind::Author,
        workspace_kind: WorkspaceKind::Authoring,
        execution_permission: ExecutionPermission::MayMutate,
        context_policy,
        output_artifact_kind: OutputArtifactKind::Commit,
        closure_relevance: ClosureRelevance::ClosureRelevant,
        default_template_slug: Some(template_slug),
    }
}

const fn review_step(step_id: StepId, template_slug: &'static str) -> StepContract {
    StepContract {
        step_id,
        phase_kind: PhaseKind::Review,
        workspace_kind: WorkspaceKind::Review,
        execution_permission: ExecutionPermission::MustNotMutate,
        context_policy: ContextPolicy::Fresh,
        output_artifact_kind: OutputArtifactKind::ReviewReport,
        closure_relevance: ClosureRelevance::ClosureRelevant,
        default_template_slug: Some(template_slug),
    }
}

const fn validate_step(step_id: StepId, workspace_kind: WorkspaceKind) -> StepContract {
    StepContract {
        step_id,
        phase_kind: PhaseKind::Validate,
        workspace_kind,
        execution_permission: ExecutionPermission::DaemonOnly,
        context_policy: ContextPolicy::None,
        output_artifact_kind: OutputArtifactKind::ValidationReport,
        closure_relevance: ClosureRelevance::ClosureRelevant,
        default_template_slug: None,
    }
}

const fn report_only_step(
    step_id: StepId,
    phase_kind: PhaseKind,
    output_artifact_kind: OutputArtifactKind,
    template_slug: &'static str,
) -> StepContract {
    StepContract {
        step_id,
        phase_kind,
        workspace_kind: WorkspaceKind::Review,
        execution_permission: ExecutionPermission::MustNotMutate,
        context_policy: ContextPolicy::Fresh,
        output_artifact_kind,
        closure_relevance: ClosureRelevance::ReportOnly,
        default_template_slug: Some(template_slug),
    }
}

const fn system_step(step_id: StepId) -> StepContract {
    StepContract {
        step_id,
        phase_kind: PhaseKind::System,
        workspace_kind: WorkspaceKind::Integration,
        execution_permission: ExecutionPermission::DaemonOnly,
        context_policy: ContextPolicy::None,
        output_artifact_kind: OutputArtifactKind::None,
        closure_relevance: ClosureRelevance::ClosureRelevant,
        default_template_slug: None,
    }
}

pub static DELIVERY_V1_STEPS: &[StepContract] = &[
    author_step(AUTHOR_INITIAL, ContextPolicy::Fresh, "author-initial"),
    review_step(REVIEW_INCREMENTAL_INITIAL, "review-incremental"),
    review_step(REVIEW_CANDIDATE_INITIAL, "review-candidate"),
    validate_step(VALIDATE_CANDIDATE_INITIAL, WorkspaceKind::Authoring),
    author_step(
        REPAIR_CANDIDATE,
        ContextPolicy::ResumeContext,
        "repair-candidate",
    ),
    review_step(REVIEW_INCREMENTAL_REPAIR, "review-incremental"),
    review_step(REVIEW_CANDIDATE_REPAIR, "review-candidate"),
    validate_step(VALIDATE_CANDIDATE_REPAIR, WorkspaceKind::Authoring),
    report_only_step(
        INVESTIGATE_ITEM,
        PhaseKind::Investigate,
        OutputArtifactKind::FindingReport,
        "investigate-item",
    ),
    system_step(PREPARE_CONVERGENCE),
    validate_step(VALIDATE_INTEGRATED, WorkspaceKind::Integration),
    author_step(
        REPAIR_AFTER_INTEGRATION,
        ContextPolicy::ResumeContext,
        "repair-integrated",
    ),
    review_step(
        REVIEW_INCREMENTAL_AFTER_INTEGRATION_REPAIR,
        "review-incremental",
    ),
    review_step(REVIEW_AFTER_INTEGRATION_REPAIR, "review-candidate"),
    validate_step(VALIDATE_AFTER_INTEGRATION_REPAIR, WorkspaceKind::Authoring),
];

pub fn find_step(step_id: StepId) -> &'static StepContract {
    DELIVERY_V1_STEPS
        .iter()
        .find(|s| s.step_id == step_id)
        .expect("all StepId variants must have a workflow contract")
}

pub fn is_closure_relevant_review_step(step_id: StepId) -> bool {
    let contract = find_step(step_id);
    contract.phase_kind == PhaseKind::Review
        && contract.closure_relevance == ClosureRelevance::ClosureRelevant
}

pub fn is_closure_relevant_validate_step(step_id: StepId) -> bool {
    let contract = find_step(step_id);
    contract.phase_kind == PhaseKind::Validate
        && contract.closure_relevance == ClosureRelevance::ClosureRelevant
}
