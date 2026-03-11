use ingot_domain::job::{ContextPolicy, ExecutionPermission, OutputArtifactKind, PhaseKind};
use ingot_domain::workspace::WorkspaceKind;

pub type StepId = &'static str;

pub const AUTHOR_INITIAL: StepId = "author_initial";
pub const VALIDATE_CANDIDATE_INITIAL: StepId = "validate_candidate_initial";
pub const REVIEW_CANDIDATE_INITIAL: StepId = "review_candidate_initial";
pub const REPAIR_CANDIDATE: StepId = "repair_candidate";
pub const VALIDATE_CANDIDATE_REPAIR: StepId = "validate_candidate_repair";
pub const REVIEW_CANDIDATE_REPAIR: StepId = "review_candidate_repair";
pub const PREPARE_CONVERGENCE: StepId = "prepare_convergence";
pub const VALIDATE_INTEGRATED: StepId = "validate_integrated";
pub const REPAIR_AFTER_INTEGRATION: StepId = "repair_after_integration";
pub const VALIDATE_AFTER_INTEGRATION_REPAIR: StepId = "validate_after_integration_repair";
pub const REVIEW_AFTER_INTEGRATION_REPAIR: StepId = "review_after_integration_repair";

#[derive(Debug, Clone)]
pub struct StepContract {
    pub step_id: StepId,
    pub phase_kind: PhaseKind,
    pub workspace_kind: WorkspaceKind,
    pub execution_permission: ExecutionPermission,
    pub context_policy: ContextPolicy,
    pub output_artifact_kind: OutputArtifactKind,
    pub default_template_slug: Option<&'static str>,
    pub is_system_step: bool,
}

pub static DELIVERY_V1_STEPS: &[StepContract] = &[
    StepContract {
        step_id: AUTHOR_INITIAL,
        phase_kind: PhaseKind::Author,
        workspace_kind: WorkspaceKind::Authoring,
        execution_permission: ExecutionPermission::MayMutate,
        context_policy: ContextPolicy::Fresh,
        output_artifact_kind: OutputArtifactKind::Commit,
        default_template_slug: Some("author-initial"),
        is_system_step: false,
    },
    StepContract {
        step_id: VALIDATE_CANDIDATE_INITIAL,
        phase_kind: PhaseKind::Validate,
        workspace_kind: WorkspaceKind::Authoring,
        execution_permission: ExecutionPermission::MustNotMutate,
        context_policy: ContextPolicy::ResumeContext,
        output_artifact_kind: OutputArtifactKind::ValidationReport,
        default_template_slug: Some("validate-candidate"),
        is_system_step: false,
    },
    StepContract {
        step_id: REVIEW_CANDIDATE_INITIAL,
        phase_kind: PhaseKind::Review,
        workspace_kind: WorkspaceKind::Review,
        execution_permission: ExecutionPermission::MustNotMutate,
        context_policy: ContextPolicy::Fresh,
        output_artifact_kind: OutputArtifactKind::ReviewReport,
        default_template_slug: Some("review-candidate"),
        is_system_step: false,
    },
    StepContract {
        step_id: REPAIR_CANDIDATE,
        phase_kind: PhaseKind::Author,
        workspace_kind: WorkspaceKind::Authoring,
        execution_permission: ExecutionPermission::MayMutate,
        context_policy: ContextPolicy::ResumeContext,
        output_artifact_kind: OutputArtifactKind::Commit,
        default_template_slug: Some("repair-candidate"),
        is_system_step: false,
    },
    StepContract {
        step_id: VALIDATE_CANDIDATE_REPAIR,
        phase_kind: PhaseKind::Validate,
        workspace_kind: WorkspaceKind::Authoring,
        execution_permission: ExecutionPermission::MustNotMutate,
        context_policy: ContextPolicy::ResumeContext,
        output_artifact_kind: OutputArtifactKind::ValidationReport,
        default_template_slug: Some("validate-candidate"),
        is_system_step: false,
    },
    StepContract {
        step_id: REVIEW_CANDIDATE_REPAIR,
        phase_kind: PhaseKind::Review,
        workspace_kind: WorkspaceKind::Review,
        execution_permission: ExecutionPermission::MustNotMutate,
        context_policy: ContextPolicy::Fresh,
        output_artifact_kind: OutputArtifactKind::ReviewReport,
        default_template_slug: Some("review-candidate"),
        is_system_step: false,
    },
    StepContract {
        step_id: PREPARE_CONVERGENCE,
        phase_kind: PhaseKind::Author, // system step, phase_kind not used
        workspace_kind: WorkspaceKind::Integration,
        execution_permission: ExecutionPermission::MustNotMutate,
        context_policy: ContextPolicy::None,
        output_artifact_kind: OutputArtifactKind::None,
        default_template_slug: None,
        is_system_step: true,
    },
    StepContract {
        step_id: VALIDATE_INTEGRATED,
        phase_kind: PhaseKind::Validate,
        workspace_kind: WorkspaceKind::Integration,
        execution_permission: ExecutionPermission::MustNotMutate,
        context_policy: ContextPolicy::ResumeContext,
        output_artifact_kind: OutputArtifactKind::ValidationReport,
        default_template_slug: Some("validate-integrated"),
        is_system_step: false,
    },
    StepContract {
        step_id: REPAIR_AFTER_INTEGRATION,
        phase_kind: PhaseKind::Author,
        workspace_kind: WorkspaceKind::Authoring,
        execution_permission: ExecutionPermission::MayMutate,
        context_policy: ContextPolicy::ResumeContext,
        output_artifact_kind: OutputArtifactKind::Commit,
        default_template_slug: Some("repair-integrated"),
        is_system_step: false,
    },
    StepContract {
        step_id: VALIDATE_AFTER_INTEGRATION_REPAIR,
        phase_kind: PhaseKind::Validate,
        workspace_kind: WorkspaceKind::Authoring,
        execution_permission: ExecutionPermission::MustNotMutate,
        context_policy: ContextPolicy::ResumeContext,
        output_artifact_kind: OutputArtifactKind::ValidationReport,
        default_template_slug: Some("validate-candidate"),
        is_system_step: false,
    },
    StepContract {
        step_id: REVIEW_AFTER_INTEGRATION_REPAIR,
        phase_kind: PhaseKind::Review,
        workspace_kind: WorkspaceKind::Review,
        execution_permission: ExecutionPermission::MustNotMutate,
        context_policy: ContextPolicy::Fresh,
        output_artifact_kind: OutputArtifactKind::ReviewReport,
        default_template_slug: Some("review-candidate"),
        is_system_step: false,
    },
];

pub fn find_step(step_id: &str) -> Option<&'static StepContract> {
    DELIVERY_V1_STEPS.iter().find(|s| s.step_id == step_id)
}
