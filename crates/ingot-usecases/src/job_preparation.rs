use ingot_domain::agent::{AdapterKind, Agent, AgentCapability, AgentStatus};
use ingot_domain::ids::{AgentId, WorkspaceId};
use ingot_domain::item::Item;
use ingot_domain::job::{
    ExecutionPermission, Job, JobAssignment, JobStatus, OutputArtifactKind, PhaseKind,
};
use ingot_domain::workspace::WorkspaceKind;

pub fn should_prepare_agent_execution(job: &Job, item: &Item) -> bool {
    job.state.status() == JobStatus::Queued
        && job.execution_permission != ExecutionPermission::DaemonOnly
        && is_supported_runtime_job(job)
        && item.current_revision_id == job.item_revision_id
}

pub fn should_prepare_harness_validation(job: &Job, item: &Item) -> bool {
    job.state.status() == JobStatus::Queued
        && is_daemon_only_validation(job)
        && item.current_revision_id == job.item_revision_id
}

pub fn select_runtime_agent(agents: &[Agent], job: &Job) -> Option<Agent> {
    let mut compatible = agents
        .iter()
        .filter(|agent| agent.status == AgentStatus::Available)
        .filter(|agent| agent.adapter_kind == AdapterKind::Codex)
        .filter(|agent| supports_job(agent, job))
        .cloned()
        .collect::<Vec<_>>();
    compatible.sort_by(|left, right| left.slug.cmp(&right.slug));
    compatible.into_iter().next()
}

pub fn assign_agent_execution(
    job: &mut Job,
    workspace_id: WorkspaceId,
    agent_id: AgentId,
    prompt_snapshot: impl Into<String>,
    phase_template_digest: impl Into<String>,
) {
    job.assign(
        JobAssignment::new(workspace_id)
            .with_agent(agent_id)
            .with_prompt_snapshot(prompt_snapshot)
            .with_phase_template_digest(phase_template_digest),
    );
}

pub fn assign_daemon_validation(job: &mut Job, workspace_id: WorkspaceId) {
    job.assign(JobAssignment::new(workspace_id));
}

pub fn is_supported_runtime_job(job: &Job) -> bool {
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

pub fn is_daemon_only_validation(job: &Job) -> bool {
    job.execution_permission == ExecutionPermission::DaemonOnly
        && job.phase_kind == PhaseKind::Validate
}

fn supports_job(agent: &Agent, job: &Job) -> bool {
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

#[cfg(test)]
mod tests {
    use ingot_test_support::fixtures::{AgentBuilder, JobBuilder, nil_item};

    use super::*;

    #[test]
    fn agent_execution_requires_current_queued_supported_job() {
        let item = nil_item();
        let job = JobBuilder::new(
            item.project_id,
            item.id,
            item.current_revision_id,
            "author_initial",
        )
        .workspace_kind(WorkspaceKind::Authoring)
        .execution_permission(ExecutionPermission::MayMutate)
        .output_artifact_kind(OutputArtifactKind::Commit)
        .build();

        assert!(should_prepare_agent_execution(&job, &item));

        let stale_job = JobBuilder::new(
            item.project_id,
            item.id,
            ingot_domain::ids::ItemRevisionId::new(),
            "author_initial",
        )
        .workspace_kind(WorkspaceKind::Authoring)
        .execution_permission(ExecutionPermission::MayMutate)
        .output_artifact_kind(OutputArtifactKind::Commit)
        .build();
        assert!(!should_prepare_agent_execution(&stale_job, &item));

        let daemon_job = JobBuilder::new(
            item.project_id,
            item.id,
            item.current_revision_id,
            "validate_candidate_initial",
        )
        .phase_kind(PhaseKind::Validate)
        .workspace_kind(WorkspaceKind::Authoring)
        .execution_permission(ExecutionPermission::DaemonOnly)
        .output_artifact_kind(OutputArtifactKind::ValidationReport)
        .build();
        assert!(!should_prepare_agent_execution(&daemon_job, &item));
    }

    #[test]
    fn harness_validation_requires_current_queued_daemon_validation() {
        let item = nil_item();
        let job = JobBuilder::new(
            item.project_id,
            item.id,
            item.current_revision_id,
            "validate_candidate_initial",
        )
        .phase_kind(PhaseKind::Validate)
        .workspace_kind(WorkspaceKind::Authoring)
        .execution_permission(ExecutionPermission::DaemonOnly)
        .output_artifact_kind(OutputArtifactKind::ValidationReport)
        .build();

        assert!(should_prepare_harness_validation(&job, &item));

        let mut wrong_phase = job.clone();
        wrong_phase.phase_kind = PhaseKind::Review;
        assert!(!should_prepare_harness_validation(&wrong_phase, &item));
    }

    #[test]
    fn runtime_agent_selection_prefers_available_codex_agents_with_matching_capabilities() {
        let item = nil_item();
        let job = JobBuilder::new(
            item.project_id,
            item.id,
            item.current_revision_id,
            "review_candidate_initial",
        )
        .workspace_kind(WorkspaceKind::Review)
        .execution_permission(ExecutionPermission::MustNotMutate)
        .output_artifact_kind(OutputArtifactKind::ReviewReport)
        .build();

        let unavailable = AgentBuilder::new(
            "aaa",
            vec![
                AgentCapability::ReadOnlyJobs,
                AgentCapability::StructuredOutput,
            ],
        )
        .status(AgentStatus::Unavailable)
        .build();
        let wrong_adapter = AgentBuilder::new(
            "bbb",
            vec![
                AgentCapability::ReadOnlyJobs,
                AgentCapability::StructuredOutput,
            ],
        )
        .adapter_kind(AdapterKind::ClaudeCode)
        .build();
        let wrong_caps = AgentBuilder::new("ccc", vec![AgentCapability::StructuredOutput]).build();
        let preferred = AgentBuilder::new(
            "ddd",
            vec![
                AgentCapability::ReadOnlyJobs,
                AgentCapability::StructuredOutput,
            ],
        )
        .build();
        let later = AgentBuilder::new(
            "zzz",
            vec![
                AgentCapability::ReadOnlyJobs,
                AgentCapability::StructuredOutput,
            ],
        )
        .build();

        let selected = select_runtime_agent(
            &[
                later,
                wrong_adapter,
                preferred.clone(),
                wrong_caps,
                unavailable,
            ],
            &job,
        )
        .expect("compatible agent");

        assert_eq!(selected.id, preferred.id);
    }

    #[test]
    fn assignment_helpers_persist_expected_metadata() {
        let item = nil_item();
        let mut agent_job = JobBuilder::new(
            item.project_id,
            item.id,
            item.current_revision_id,
            "author_initial",
        )
        .workspace_kind(WorkspaceKind::Authoring)
        .execution_permission(ExecutionPermission::MayMutate)
        .output_artifact_kind(OutputArtifactKind::Commit)
        .build();
        let workspace_id = ingot_domain::ids::WorkspaceId::new();
        let agent_id = ingot_domain::ids::AgentId::new();
        assign_agent_execution(
            &mut agent_job,
            workspace_id,
            agent_id,
            "prompt body",
            "template-digest",
        );
        assert_eq!(agent_job.state.workspace_id(), Some(workspace_id));
        assert_eq!(agent_job.state.agent_id(), Some(agent_id));
        assert_eq!(agent_job.state.prompt_snapshot(), Some("prompt body"));
        assert_eq!(
            agent_job.state.phase_template_digest(),
            Some("template-digest")
        );

        let mut daemon_job = JobBuilder::new(
            item.project_id,
            item.id,
            item.current_revision_id,
            "validate_candidate_initial",
        )
        .phase_kind(PhaseKind::Validate)
        .workspace_kind(WorkspaceKind::Authoring)
        .execution_permission(ExecutionPermission::DaemonOnly)
        .output_artifact_kind(OutputArtifactKind::ValidationReport)
        .build();
        let daemon_workspace_id = ingot_domain::ids::WorkspaceId::new();
        assign_daemon_validation(&mut daemon_job, daemon_workspace_id);
        assert_eq!(daemon_job.state.workspace_id(), Some(daemon_workspace_id));
        assert_eq!(daemon_job.state.agent_id(), None);
        assert_eq!(daemon_job.state.prompt_snapshot(), None);
    }
}
