use ingot_domain::job::{ContextPolicy, ExecutionPermission, OutputArtifactKind, PhaseKind};
use ingot_domain::step_id::StepId;
use ingot_domain::workspace::WorkspaceKind;

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

const fn investigation_step(step_id: StepId, template_slug: &'static str) -> StepContract {
    StepContract {
        step_id,
        phase_kind: PhaseKind::Investigate,
        workspace_kind: WorkspaceKind::Review,
        execution_permission: ExecutionPermission::MustNotMutate,
        context_policy: ContextPolicy::Fresh,
        output_artifact_kind: OutputArtifactKind::InvestigationReport,
        closure_relevance: ClosureRelevance::ClosureRelevant,
        default_template_slug: Some(template_slug),
    }
}

pub static INVESTIGATION_V1_STEPS: &[StepContract] = &[
    investigation_step(StepId::InvestigateProject, "investigate-project"),
    investigation_step(StepId::ReinvestigateProject, "reinvestigate-project"),
];

pub static DELIVERY_V1_STEPS: &[StepContract] = &[
    author_step(
        StepId::AuthorInitial,
        ContextPolicy::Fresh,
        "author-initial",
    ),
    review_step(StepId::ReviewIncrementalInitial, "review-incremental"),
    review_step(StepId::ReviewCandidateInitial, "review-candidate"),
    validate_step(StepId::ValidateCandidateInitial, WorkspaceKind::Authoring),
    author_step(
        StepId::RepairCandidate,
        ContextPolicy::ResumeContext,
        "repair-candidate",
    ),
    review_step(StepId::ReviewIncrementalRepair, "review-incremental"),
    review_step(StepId::ReviewCandidateRepair, "review-candidate"),
    validate_step(StepId::ValidateCandidateRepair, WorkspaceKind::Authoring),
    report_only_step(
        StepId::InvestigateItem,
        PhaseKind::Investigate,
        OutputArtifactKind::FindingReport,
        "investigate-item",
    ),
    system_step(StepId::PrepareConvergence),
    validate_step(StepId::ValidateIntegrated, WorkspaceKind::Integration),
    author_step(
        StepId::RepairAfterIntegration,
        ContextPolicy::ResumeContext,
        "repair-integrated",
    ),
    review_step(
        StepId::ReviewIncrementalAfterIntegrationRepair,
        "review-incremental",
    ),
    review_step(StepId::ReviewAfterIntegrationRepair, "review-candidate"),
    validate_step(
        StepId::ValidateAfterIntegrationRepair,
        WorkspaceKind::Authoring,
    ),
];

pub fn find_step(step_id: StepId) -> &'static StepContract {
    DELIVERY_V1_STEPS
        .iter()
        .chain(INVESTIGATION_V1_STEPS.iter())
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
