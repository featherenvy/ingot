use crate::ids;
use crate::job::{
    ContextPolicy, ExecutionPermission, Job, JobAssignment, JobInput, JobLease, JobState,
    JobStatus, OutcomeClass, OutputArtifactKind, PhaseKind, TerminalStatus,
};
use crate::workspace::WorkspaceKind;
use chrono::{DateTime, Utc};

use super::timestamps::default_timestamp;

pub struct JobBuilder {
    id: ids::JobId,
    project_id: ids::ProjectId,
    item_id: ids::ItemId,
    item_revision_id: ids::ItemRevisionId,
    step_id: String,
    semantic_attempt_no: u32,
    retry_no: u32,
    supersedes_job_id: Option<ids::JobId>,
    status: JobStatus,
    outcome_class: Option<OutcomeClass>,
    phase_kind: PhaseKind,
    workspace_id: Option<ids::WorkspaceId>,
    workspace_kind: WorkspaceKind,
    execution_permission: ExecutionPermission,
    context_policy: ContextPolicy,
    phase_template_slug: String,
    phase_template_digest: Option<String>,
    prompt_snapshot: Option<String>,
    job_input: JobInput,
    output_artifact_kind: OutputArtifactKind,
    output_commit_oid: Option<String>,
    result_schema_version: Option<String>,
    result_payload: Option<serde_json::Value>,
    agent_id: Option<ids::AgentId>,
    process_pid: Option<u32>,
    lease_owner_id: Option<String>,
    heartbeat_at: Option<DateTime<Utc>>,
    lease_expires_at: Option<DateTime<Utc>>,
    error_code: Option<String>,
    error_message: Option<String>,
    created_at: DateTime<Utc>,
    started_at: Option<DateTime<Utc>>,
    ended_at: Option<DateTime<Utc>>,
}

impl JobBuilder {
    pub fn new(
        project_id: ids::ProjectId,
        item_id: ids::ItemId,
        item_revision_id: ids::ItemRevisionId,
        step_id: impl Into<String>,
    ) -> Self {
        Self {
            id: ids::JobId::new(),
            project_id,
            item_id,
            item_revision_id,
            step_id: step_id.into(),
            semantic_attempt_no: 1,
            retry_no: 0,
            supersedes_job_id: None,
            status: JobStatus::Queued,
            outcome_class: None,
            phase_kind: PhaseKind::Author,
            workspace_id: None,
            workspace_kind: WorkspaceKind::Authoring,
            execution_permission: ExecutionPermission::MayMutate,
            context_policy: ContextPolicy::Fresh,
            phase_template_slug: "template".into(),
            phase_template_digest: None,
            prompt_snapshot: None,
            job_input: JobInput::None,
            output_artifact_kind: OutputArtifactKind::None,
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
            created_at: default_timestamp(),
            started_at: None,
            ended_at: None,
        }
    }

    pub fn id(mut self, id: ids::JobId) -> Self {
        self.id = id;
        self
    }

    pub fn supersedes_job_id(mut self, supersedes_job_id: ids::JobId) -> Self {
        self.supersedes_job_id = Some(supersedes_job_id);
        self
    }

    pub fn retry_no(mut self, retry_no: u32) -> Self {
        self.retry_no = retry_no;
        self
    }

    pub fn status(mut self, status: JobStatus) -> Self {
        self.status = status;
        self
    }

    pub fn outcome_class(mut self, outcome_class: OutcomeClass) -> Self {
        self.outcome_class = Some(outcome_class);
        self
    }

    pub fn phase_kind(mut self, phase_kind: PhaseKind) -> Self {
        self.phase_kind = phase_kind;
        self
    }

    pub fn workspace_id(mut self, workspace_id: ids::WorkspaceId) -> Self {
        self.workspace_id = Some(workspace_id);
        self
    }

    pub fn workspace_kind(mut self, workspace_kind: WorkspaceKind) -> Self {
        self.workspace_kind = workspace_kind;
        self
    }

    pub fn execution_permission(mut self, execution_permission: ExecutionPermission) -> Self {
        self.execution_permission = execution_permission;
        self
    }

    pub fn context_policy(mut self, context_policy: ContextPolicy) -> Self {
        self.context_policy = context_policy;
        self
    }

    pub fn phase_template_slug(mut self, phase_template_slug: impl Into<String>) -> Self {
        self.phase_template_slug = phase_template_slug.into();
        self
    }

    pub fn job_input(mut self, job_input: JobInput) -> Self {
        self.job_input = job_input;
        self
    }

    pub fn output_artifact_kind(mut self, output_artifact_kind: OutputArtifactKind) -> Self {
        self.output_artifact_kind = output_artifact_kind;
        self
    }

    pub fn output_commit_oid(mut self, output_commit_oid: impl Into<String>) -> Self {
        self.output_commit_oid = Some(output_commit_oid.into());
        self
    }

    pub fn result_payload(mut self, result_payload: serde_json::Value) -> Self {
        self.result_payload = Some(result_payload);
        self
    }

    pub fn result_schema_version(mut self, result_schema_version: impl Into<String>) -> Self {
        self.result_schema_version = Some(result_schema_version.into());
        self
    }

    pub fn agent_id(mut self, agent_id: ids::AgentId) -> Self {
        self.agent_id = Some(agent_id);
        self
    }

    pub fn lease_owner_id(mut self, lease_owner_id: impl Into<String>) -> Self {
        self.lease_owner_id = Some(lease_owner_id.into());
        self
    }

    pub fn heartbeat_at(mut self, heartbeat_at: DateTime<Utc>) -> Self {
        self.heartbeat_at = Some(heartbeat_at);
        self
    }

    pub fn lease_expires_at(mut self, lease_expires_at: DateTime<Utc>) -> Self {
        self.lease_expires_at = Some(lease_expires_at);
        self
    }

    pub fn error_code(mut self, error_code: impl Into<String>) -> Self {
        self.error_code = Some(error_code.into());
        self
    }

    pub fn error_message(mut self, error_message: impl Into<String>) -> Self {
        self.error_message = Some(error_message.into());
        self
    }

    pub fn created_at(mut self, created_at: DateTime<Utc>) -> Self {
        self.created_at = created_at;
        self
    }

    pub fn started_at(mut self, started_at: DateTime<Utc>) -> Self {
        self.started_at = Some(started_at);
        self
    }

    pub fn ended_at(mut self, ended_at: DateTime<Utc>) -> Self {
        self.ended_at = Some(ended_at);
        self
    }

    pub fn build(self) -> Job {
        let assignment = self.workspace_id.map(|workspace_id| JobAssignment {
            workspace_id,
            agent_id: self.agent_id,
            prompt_snapshot: self.prompt_snapshot,
            phase_template_digest: self.phase_template_digest,
        });

        let state = match self.status {
            JobStatus::Queued => JobState::Queued,
            JobStatus::Assigned => match assignment {
                Some(a) => JobState::Assigned(a),
                None => JobState::Queued,
            },
            JobStatus::Running => {
                let lease_owner_id = self.lease_owner_id.unwrap_or_else(|| "test".into());
                let assignment = assignment.unwrap_or_else(|| JobAssignment {
                    workspace_id: ids::WorkspaceId::new(),
                    agent_id: self.agent_id,
                    prompt_snapshot: None,
                    phase_template_digest: None,
                });
                JobState::Running {
                    assignment,
                    lease: JobLease {
                        process_pid: self.process_pid,
                        lease_owner_id,
                        heartbeat_at: self.heartbeat_at.unwrap_or_else(Utc::now),
                        lease_expires_at: self.lease_expires_at.unwrap_or_else(Utc::now),
                        started_at: self.started_at.unwrap_or_else(Utc::now),
                    },
                }
            }
            JobStatus::Completed => JobState::Completed {
                assignment,
                started_at: self.started_at,
                outcome_class: self.outcome_class.unwrap_or(OutcomeClass::Clean),
                ended_at: self.ended_at.unwrap_or_else(Utc::now),
                output_commit_oid: self.output_commit_oid,
                result_schema_version: self.result_schema_version,
                result_payload: self.result_payload,
            },
            status @ (JobStatus::Failed
            | JobStatus::Cancelled
            | JobStatus::Expired
            | JobStatus::Superseded) => JobState::Terminated {
                terminal_status: TerminalStatus::from_job_status(status)
                    .expect("terminal job status"),
                assignment,
                started_at: self.started_at,
                outcome_class: self.outcome_class,
                ended_at: self.ended_at.unwrap_or_else(Utc::now),
                error_code: self.error_code,
                error_message: self.error_message,
            },
        };

        Job {
            id: self.id,
            project_id: self.project_id,
            item_id: self.item_id,
            item_revision_id: self.item_revision_id,
            step_id: self.step_id,
            semantic_attempt_no: self.semantic_attempt_no,
            retry_no: self.retry_no,
            supersedes_job_id: self.supersedes_job_id,
            phase_kind: self.phase_kind,
            workspace_kind: self.workspace_kind,
            execution_permission: self.execution_permission,
            context_policy: self.context_policy,
            phase_template_slug: self.phase_template_slug,
            output_artifact_kind: self.output_artifact_kind,
            job_input: self.job_input,
            created_at: self.created_at,
            state,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::job::JobInput;

    use super::super::timestamps::default_timestamp;
    use super::JobBuilder;

    #[test]
    fn job_builder_constructs_candidate_subject_jobs() {
        let project_id = crate::ids::ProjectId::new();
        let item_id = crate::ids::ItemId::new();
        let revision_id = crate::ids::ItemRevisionId::new();
        let job = JobBuilder::new(project_id, item_id, revision_id, "review_candidate_initial")
            .job_input(JobInput::candidate_subject("base", "head"))
            .created_at(default_timestamp())
            .build();

        assert_eq!(job.job_input.base_commit_oid(), Some("base"));
        assert_eq!(job.job_input.head_commit_oid(), Some("head"));
    }
}
