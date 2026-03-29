use std::path::PathBuf;

use ingot_domain::agent::{Agent, AgentCapability};
use ingot_domain::commit_oid::CommitOid;
use ingot_domain::item::EscalationReason;
use ingot_domain::job::{
    ExecutionPermission, Job, JobAssignment, JobStatus, OutcomeClass, OutputArtifactKind, PhaseKind,
};
use ingot_domain::project::Project;
use ingot_domain::revision::ItemRevision;
use ingot_domain::revision_context::RevisionContext;
use ingot_domain::step_id::StepId;
use ingot_domain::workspace::{Workspace, WorkspaceKind};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone)]
pub(crate) struct PreparedRun {
    pub(crate) job: Job,
    pub(crate) item: ingot_domain::item::Item,
    pub(crate) revision: ItemRevision,
    pub(crate) project: Project,
    pub(crate) canonical_repo_path: PathBuf,
    pub(crate) agent: Agent,
    pub(crate) assignment: JobAssignment,
    pub(crate) workspace: Workspace,
    pub(crate) original_head_commit_oid: CommitOid,
    pub(crate) prompt: String,
    pub(crate) workspace_lifecycle: WorkspaceLifecycle,
}

pub(crate) enum PrepareRunOutcome {
    NotPrepared,
    FailedBeforeLaunch,
    Prepared(Box<PreparedRun>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkspaceLifecycle {
    PersistentAuthoring,
    PersistentIntegration,
    EphemeralReview,
}

pub(crate) fn is_supported_runtime_job(job: &Job) -> bool {
    matches!(
        (
            job.workspace_kind,
            job.execution_permission,
            job.output_artifact_kind,
        ),
        (
            WorkspaceKind::Authoring,
            ExecutionPermission::MayMutate,
            OutputArtifactKind::Commit
        ) | (
            WorkspaceKind::Authoring | WorkspaceKind::Review | WorkspaceKind::Integration,
            ExecutionPermission::MustNotMutate,
            OutputArtifactKind::ReviewReport
                | OutputArtifactKind::ValidationReport
                | OutputArtifactKind::FindingReport,
        ) | (
            WorkspaceKind::Authoring | WorkspaceKind::Integration,
            ExecutionPermission::DaemonOnly,
            OutputArtifactKind::ValidationReport,
        )
    )
}

pub(crate) fn supports_job(agent: &Agent, job: &Job) -> bool {
    if job.execution_permission == ExecutionPermission::DaemonOnly
        || !agent
            .capabilities
            .contains(&AgentCapability::StructuredOutput)
    {
        return false;
    }

    match job.execution_permission {
        ExecutionPermission::MayMutate => {
            agent.capabilities.contains(&AgentCapability::MutatingJobs)
        }
        ExecutionPermission::MustNotMutate => {
            agent.capabilities.contains(&AgentCapability::ReadOnlyJobs)
        }
        ExecutionPermission::DaemonOnly => unreachable!("daemon-only jobs are filtered above"),
    }
}

pub(crate) fn is_inert_assigned_authoring_dispatch_residue(job: &Job) -> bool {
    job.state.status() == JobStatus::Assigned
        && job.phase_kind == PhaseKind::Author
        && job.workspace_kind == WorkspaceKind::Authoring
        && job.execution_permission == ExecutionPermission::MayMutate
        && job.output_artifact_kind == OutputArtifactKind::Commit
        && job.state.workspace_id().is_some()
        && job.state.agent_id().is_none()
        && job.state.prompt_snapshot().is_none()
        && job.state.phase_template_digest().is_none()
        && job.state.process_pid().is_none()
        && job.state.lease_owner_id().is_none()
        && job.state.heartbeat_at().is_none()
        && job.state.lease_expires_at().is_none()
        && job.state.started_at().is_none()
}

pub(crate) fn built_in_template(template_slug: &str, step_id: StepId) -> &'static str {
    match template_slug {
        "author-initial" => {
            "Implement the requested change directly in the repository. Keep the edit set focused on the acceptance criteria and preserve surrounding style."
        }
        "repair-candidate" | "repair-integrated" => {
            "Repair the current candidate based on the latest validation or review feedback while preserving the accepted parts of the prior work."
        }
        "review-incremental" => {
            "Review only the requested incremental diff and report concrete findings against the exact review subject."
        }
        "review-candidate" => {
            "Review the full candidate diff from the seed commit to the current head and report concrete findings when necessary."
        }
        "validate-candidate" | "validate-integrated" => {
            "Run objective validation against the current workspace subject and report failed checks or findings only when they are real."
        }
        "investigate-item" => {
            "Investigate the current subject and produce a finding report only when there is a concrete issue worth tracking."
        }
        _ => match step_id {
            StepId::AuthorInitial => {
                "Implement the requested change directly in the repository. Keep the edit set focused on the acceptance criteria and preserve surrounding style."
            }
            StepId::ReviewIncrementalInitial
            | StepId::ReviewIncrementalRepair
            | StepId::ReviewIncrementalAfterIntegrationRepair => {
                "Review only the requested incremental diff and report concrete findings against the exact review subject."
            }
            StepId::ReviewCandidateInitial
            | StepId::ReviewCandidateRepair
            | StepId::ReviewAfterIntegrationRepair => {
                "Review the full candidate diff from the seed commit to the current head and report concrete findings when necessary."
            }
            StepId::ValidateCandidateInitial
            | StepId::ValidateCandidateRepair
            | StepId::ValidateAfterIntegrationRepair
            | StepId::ValidateIntegrated => {
                "Run objective validation against the current workspace subject and report failed checks or findings only when they are real."
            }
            StepId::InvestigateItem => {
                "Investigate the current subject and produce a finding report only when there is a concrete issue worth tracking."
            }
            _ => {
                "Update the repository for the current authoring step and keep the change set narrowly scoped to the revision contract."
            }
        },
    }
}

pub(crate) fn format_revision_context(revision_context: Option<&RevisionContext>) -> String {
    revision_context
        .map(|context| {
            serde_json::to_string_pretty(&context.payload).unwrap_or_else(|_| "{}".into())
        })
        .unwrap_or_else(|| "none".into())
}

pub(crate) fn commit_subject(title: &str, step_id: StepId) -> String {
    let title = title.trim();
    if title.is_empty() {
        format!("Ingot {step_id}")
    } else {
        format!("Ingot: {title}")
    }
}

pub(crate) fn non_empty_message(message: &str) -> Option<String> {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub(crate) fn outcome_class_name(outcome_class: OutcomeClass) -> &'static str {
    outcome_class.as_str()
}

pub(crate) fn template_digest(template: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(template.as_bytes());
    format!("{:x}", hasher.finalize())
}

pub(crate) fn failure_escalation_reason(
    job: &Job,
    outcome_class: OutcomeClass,
) -> Option<EscalationReason> {
    ingot_usecases::dispatch::failure_escalation_reason(job, outcome_class)
}

pub(crate) fn should_clear_item_escalation_on_success(
    item: &ingot_domain::item::Item,
    job: &Job,
) -> bool {
    ingot_usecases::dispatch::should_clear_item_escalation_on_success(item, job)
}
