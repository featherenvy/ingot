use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{AgentId, ItemId, ItemRevisionId, JobId, ProjectId, WorkspaceId};
use crate::workspace::WorkspaceKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Assigned,
    Running,
    Completed,
    Failed,
    Cancelled,
    Expired,
    Superseded,
}

impl JobStatus {
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Expired | Self::Superseded
        )
    }

    #[must_use]
    pub fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Assigned | Self::Running)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeClass {
    Clean,
    Findings,
    TransientFailure,
    TerminalFailure,
    ProtocolViolation,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseKind {
    Author,
    Validate,
    Review,
    Investigate,
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionPermission {
    MayMutate,
    MustNotMutate,
    DaemonOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextPolicy {
    Fresh,
    ResumeContext,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputArtifactKind {
    Commit,
    ReviewReport,
    ValidationReport,
    FindingReport,
    None,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JobInput {
    #[default]
    None,
    AuthoringHead {
        head_commit_oid: String,
    },
    CandidateSubject {
        base_commit_oid: String,
        head_commit_oid: String,
    },
    IntegratedSubject {
        base_commit_oid: String,
        head_commit_oid: String,
    },
}

impl JobInput {
    #[must_use]
    pub fn none() -> Self {
        Self::None
    }

    #[must_use]
    pub fn authoring_head(head_commit_oid: impl Into<String>) -> Self {
        Self::AuthoringHead {
            head_commit_oid: head_commit_oid.into(),
        }
    }

    pub fn candidate_subject(
        base_commit_oid: impl Into<String>,
        head_commit_oid: impl Into<String>,
    ) -> Self {
        Self::CandidateSubject {
            base_commit_oid: base_commit_oid.into(),
            head_commit_oid: head_commit_oid.into(),
        }
    }

    pub fn integrated_subject(
        base_commit_oid: impl Into<String>,
        head_commit_oid: impl Into<String>,
    ) -> Self {
        Self::IntegratedSubject {
            base_commit_oid: base_commit_oid.into(),
            head_commit_oid: head_commit_oid.into(),
        }
    }

    #[must_use]
    pub fn base_commit_oid(&self) -> Option<&str> {
        match self {
            Self::CandidateSubject {
                base_commit_oid, ..
            }
            | Self::IntegratedSubject {
                base_commit_oid, ..
            } => Some(base_commit_oid.as_str()),
            Self::None | Self::AuthoringHead { .. } => None,
        }
    }

    #[must_use]
    pub fn head_commit_oid(&self) -> Option<&str> {
        match self {
            Self::AuthoringHead { head_commit_oid }
            | Self::CandidateSubject {
                head_commit_oid, ..
            }
            | Self::IntegratedSubject {
                head_commit_oid, ..
            } => Some(head_commit_oid.as_str()),
            Self::None => None,
        }
    }

    #[must_use]
    pub fn with_head(self, head_commit_oid: impl Into<String>) -> Self {
        let head_commit_oid = head_commit_oid.into();
        match self {
            Self::None | Self::AuthoringHead { .. } => Self::AuthoringHead { head_commit_oid },
            Self::CandidateSubject {
                base_commit_oid, ..
            } => Self::CandidateSubject {
                base_commit_oid,
                head_commit_oid,
            },
            Self::IntegratedSubject {
                base_commit_oid, ..
            } => Self::IntegratedSubject {
                base_commit_oid,
                head_commit_oid,
            },
        }
    }

    #[must_use]
    pub fn with_candidate_subject(
        self,
        base_commit_oid: impl Into<String>,
        head_commit_oid: impl Into<String>,
    ) -> Self {
        let _ = self;
        Self::candidate_subject(base_commit_oid, head_commit_oid)
    }
}

// --- JobState types ---

/// Set when a job is assigned to a workspace/agent. Persists into terminal states.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobAssignment {
    pub workspace_id: WorkspaceId,
    pub agent_id: Option<AgentId>,
    pub prompt_snapshot: Option<String>,
    pub phase_template_digest: Option<String>,
}

impl JobAssignment {
    #[must_use]
    pub fn new(workspace_id: WorkspaceId) -> Self {
        Self {
            workspace_id,
            agent_id: None,
            prompt_snapshot: None,
            phase_template_digest: None,
        }
    }

    #[must_use]
    pub fn with_agent(mut self, agent_id: AgentId) -> Self {
        self.agent_id = Some(agent_id);
        self
    }

    #[must_use]
    pub fn with_prompt_snapshot(mut self, prompt_snapshot: impl Into<String>) -> Self {
        self.prompt_snapshot = Some(prompt_snapshot.into());
        self
    }

    #[must_use]
    pub fn with_phase_template_digest(mut self, phase_template_digest: impl Into<String>) -> Self {
        self.phase_template_digest = Some(phase_template_digest.into());
        self
    }
}

/// Active execution lease. Present only during Running.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobLease {
    pub process_pid: Option<u32>,
    pub lease_owner_id: String,
    pub heartbeat_at: DateTime<Utc>,
    pub lease_expires_at: DateTime<Utc>,
    pub started_at: DateTime<Utc>,
}

/// Terminal status for non-completed terminal jobs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalStatus {
    Failed,
    Cancelled,
    Expired,
    Superseded,
}

impl TerminalStatus {
    #[must_use]
    pub fn to_job_status(self) -> JobStatus {
        match self {
            Self::Failed => JobStatus::Failed,
            Self::Cancelled => JobStatus::Cancelled,
            Self::Expired => JobStatus::Expired,
            Self::Superseded => JobStatus::Superseded,
        }
    }

    #[must_use]
    pub fn from_job_status(status: JobStatus) -> Option<Self> {
        match status {
            JobStatus::Failed => Some(Self::Failed),
            JobStatus::Cancelled => Some(Self::Cancelled),
            JobStatus::Expired => Some(Self::Expired),
            JobStatus::Superseded => Some(Self::Superseded),
            JobStatus::Queued | JobStatus::Assigned | JobStatus::Running | JobStatus::Completed => {
                None
            }
        }
    }
}

/// Lifecycle state of a Job, replacing the flat `status` + 17 optional fields.
#[derive(Debug, Clone)]
pub enum JobState {
    Queued,

    Assigned(JobAssignment),

    Running {
        assignment: JobAssignment,
        lease: JobLease,
    },

    Completed {
        assignment: Option<JobAssignment>,
        started_at: Option<DateTime<Utc>>,
        outcome_class: OutcomeClass,
        ended_at: DateTime<Utc>,
        output_commit_oid: Option<String>,
        result_schema_version: Option<String>,
        result_payload: Option<serde_json::Value>,
    },

    /// Covers Failed, Cancelled, Expired, Superseded.
    Terminated {
        terminal_status: TerminalStatus,
        assignment: Option<JobAssignment>,
        started_at: Option<DateTime<Utc>>,
        outcome_class: Option<OutcomeClass>,
        ended_at: DateTime<Utc>,
        error_code: Option<String>,
        error_message: Option<String>,
    },
}

impl JobState {
    #[must_use]
    pub fn status(&self) -> JobStatus {
        match self {
            Self::Queued => JobStatus::Queued,
            Self::Assigned(_) => JobStatus::Assigned,
            Self::Running { .. } => JobStatus::Running,
            Self::Completed { .. } => JobStatus::Completed,
            Self::Terminated {
                terminal_status, ..
            } => terminal_status.to_job_status(),
        }
    }

    #[must_use]
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            Self::Queued | Self::Assigned(_) | Self::Running { .. }
        )
    }

    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed { .. } | Self::Terminated { .. })
    }

    #[must_use]
    pub fn outcome_class(&self) -> Option<OutcomeClass> {
        match self {
            Self::Completed { outcome_class, .. } => Some(*outcome_class),
            Self::Terminated { outcome_class, .. } => *outcome_class,
            _ => None,
        }
    }

    #[must_use]
    pub fn ended_at(&self) -> Option<DateTime<Utc>> {
        match self {
            Self::Completed { ended_at, .. } | Self::Terminated { ended_at, .. } => Some(*ended_at),
            _ => None,
        }
    }

    #[must_use]
    pub fn started_at(&self) -> Option<DateTime<Utc>> {
        match self {
            Self::Running { lease, .. } => Some(lease.started_at),
            Self::Completed { started_at, .. } | Self::Terminated { started_at, .. } => *started_at,
            _ => None,
        }
    }

    #[must_use]
    pub fn output_commit_oid(&self) -> Option<&str> {
        match self {
            Self::Completed {
                output_commit_oid, ..
            } => output_commit_oid.as_deref(),
            _ => None,
        }
    }

    #[must_use]
    pub fn result_schema_version(&self) -> Option<&str> {
        match self {
            Self::Completed {
                result_schema_version,
                ..
            } => result_schema_version.as_deref(),
            _ => None,
        }
    }

    #[must_use]
    pub fn result_payload(&self) -> Option<&serde_json::Value> {
        match self {
            Self::Completed { result_payload, .. } => result_payload.as_ref(),
            _ => None,
        }
    }

    #[must_use]
    pub fn error_code(&self) -> Option<&str> {
        match self {
            Self::Terminated { error_code, .. } => error_code.as_deref(),
            _ => None,
        }
    }

    #[must_use]
    pub fn error_message(&self) -> Option<&str> {
        match self {
            Self::Terminated { error_message, .. } => error_message.as_deref(),
            _ => None,
        }
    }

    #[must_use]
    pub fn workspace_id(&self) -> Option<WorkspaceId> {
        self.assignment().map(|a| a.workspace_id)
    }

    #[must_use]
    pub fn agent_id(&self) -> Option<AgentId> {
        self.assignment().and_then(|a| a.agent_id)
    }

    #[must_use]
    pub fn assignment(&self) -> Option<&JobAssignment> {
        match self {
            Self::Assigned(assignment) => Some(assignment),
            Self::Running { assignment, .. } => Some(assignment),
            Self::Completed { assignment, .. } => assignment.as_ref(),
            Self::Terminated { assignment, .. } => assignment.as_ref(),
            Self::Queued => None,
        }
    }

    #[must_use]
    pub fn lease(&self) -> Option<&JobLease> {
        match self {
            Self::Running { lease, .. } => Some(lease),
            _ => None,
        }
    }

    #[must_use]
    pub fn prompt_snapshot(&self) -> Option<&str> {
        self.assignment().and_then(|a| a.prompt_snapshot.as_deref())
    }

    #[must_use]
    pub fn phase_template_digest(&self) -> Option<&str> {
        self.assignment()
            .and_then(|a| a.phase_template_digest.as_deref())
    }

    #[must_use]
    pub fn process_pid(&self) -> Option<u32> {
        self.lease().and_then(|l| l.process_pid)
    }

    #[must_use]
    pub fn lease_owner_id(&self) -> Option<&str> {
        self.lease().map(|l| l.lease_owner_id.as_str())
    }

    #[must_use]
    pub fn heartbeat_at(&self) -> Option<DateTime<Utc>> {
        self.lease().map(|l| l.heartbeat_at)
    }

    #[must_use]
    pub fn lease_expires_at(&self) -> Option<DateTime<Utc>> {
        self.lease().map(|l| l.lease_expires_at)
    }

    fn into_assignment_and_started_at(self) -> (Option<JobAssignment>, Option<DateTime<Utc>>) {
        match self {
            Self::Queued => (None, None),
            Self::Assigned(assignment) => (Some(assignment), None),
            Self::Running { assignment, lease } => (Some(assignment), Some(lease.started_at)),
            Self::Completed {
                assignment,
                started_at,
                ..
            }
            | Self::Terminated {
                assignment,
                started_at,
                ..
            } => (assignment, started_at),
        }
    }

    #[must_use]
    pub fn into_completed(
        self,
        outcome_class: OutcomeClass,
        ended_at: DateTime<Utc>,
        output_commit_oid: Option<String>,
        result_schema_version: Option<String>,
        result_payload: Option<serde_json::Value>,
    ) -> Self {
        let (assignment, started_at) = self.into_assignment_and_started_at();
        Self::Completed {
            assignment,
            started_at,
            outcome_class,
            ended_at,
            output_commit_oid,
            result_schema_version,
            result_payload,
        }
    }

    #[must_use]
    pub fn into_terminated(
        self,
        terminal_status: TerminalStatus,
        ended_at: DateTime<Utc>,
        outcome_class: Option<OutcomeClass>,
        error_code: Option<String>,
        error_message: Option<String>,
    ) -> Self {
        let (assignment, started_at) = self.into_assignment_and_started_at();
        Self::Terminated {
            terminal_status,
            assignment,
            started_at,
            outcome_class,
            ended_at,
            error_code,
            error_message,
        }
    }
}

// --- Serde: backward-compatible JSON via JobWire ---

/// Flat wire representation matching the current JSON shape. Zero API change.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct JobWire {
    pub id: JobId,
    pub project_id: ProjectId,
    pub item_id: ItemId,
    pub item_revision_id: ItemRevisionId,
    pub step_id: String,
    pub semantic_attempt_no: u32,
    pub retry_no: u32,
    pub supersedes_job_id: Option<JobId>,
    pub status: JobStatus,
    pub outcome_class: Option<OutcomeClass>,
    pub phase_kind: PhaseKind,
    pub workspace_id: Option<WorkspaceId>,
    pub workspace_kind: WorkspaceKind,
    pub execution_permission: ExecutionPermission,
    pub context_policy: ContextPolicy,
    pub phase_template_slug: String,
    pub phase_template_digest: Option<String>,
    pub prompt_snapshot: Option<String>,
    pub job_input: JobInput,
    pub output_artifact_kind: OutputArtifactKind,
    pub output_commit_oid: Option<String>,
    pub result_schema_version: Option<String>,
    pub result_payload: Option<serde_json::Value>,
    pub agent_id: Option<AgentId>,
    pub process_pid: Option<u32>,
    pub lease_owner_id: Option<String>,
    pub heartbeat_at: Option<DateTime<Utc>>,
    pub lease_expires_at: Option<DateTime<Utc>>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
}

fn build_assignment(
    workspace_id: Option<WorkspaceId>,
    agent_id: Option<AgentId>,
    prompt_snapshot: Option<String>,
    phase_template_digest: Option<String>,
) -> Option<JobAssignment> {
    workspace_id.map(|workspace_id| JobAssignment {
        workspace_id,
        agent_id,
        prompt_snapshot,
        phase_template_digest,
    })
}

fn required_field<T>(field: &'static str, value: Option<T>) -> Result<T, String> {
    value.ok_or_else(|| format!("job {field} is required for this status"))
}

impl TryFrom<JobWire> for Job {
    type Error = String;

    fn try_from(w: JobWire) -> Result<Self, Self::Error> {
        let assignment = build_assignment(
            w.workspace_id,
            w.agent_id,
            w.prompt_snapshot,
            w.phase_template_digest,
        );
        let state = match w.status {
            JobStatus::Queued => JobState::Queued,
            JobStatus::Assigned => JobState::Assigned(required_field("workspace_id", assignment)?),
            JobStatus::Running => JobState::Running {
                assignment: required_field("workspace_id", assignment)?,
                lease: JobLease {
                    process_pid: w.process_pid,
                    lease_owner_id: required_field("lease_owner_id", w.lease_owner_id)?,
                    heartbeat_at: required_field("heartbeat_at", w.heartbeat_at)?,
                    lease_expires_at: required_field("lease_expires_at", w.lease_expires_at)?,
                    started_at: required_field("started_at", w.started_at)?,
                },
            },
            JobStatus::Completed => JobState::Completed {
                assignment,
                started_at: w.started_at,
                outcome_class: required_field("outcome_class", w.outcome_class)?,
                ended_at: required_field("ended_at", w.ended_at)?,
                output_commit_oid: w.output_commit_oid,
                result_schema_version: w.result_schema_version,
                result_payload: w.result_payload,
            },
            status @ (JobStatus::Failed
            | JobStatus::Cancelled
            | JobStatus::Expired
            | JobStatus::Superseded) => JobState::Terminated {
                terminal_status: TerminalStatus::from_job_status(status)
                    .expect("terminal job status"),
                assignment,
                started_at: w.started_at,
                outcome_class: w.outcome_class,
                ended_at: required_field("ended_at", w.ended_at)?,
                error_code: w.error_code,
                error_message: w.error_message,
            },
        };

        Ok(Job {
            id: w.id,
            project_id: w.project_id,
            item_id: w.item_id,
            item_revision_id: w.item_revision_id,
            step_id: w.step_id,
            semantic_attempt_no: w.semantic_attempt_no,
            retry_no: w.retry_no,
            supersedes_job_id: w.supersedes_job_id,
            phase_kind: w.phase_kind,
            workspace_kind: w.workspace_kind,
            execution_permission: w.execution_permission,
            context_policy: w.context_policy,
            phase_template_slug: w.phase_template_slug,
            output_artifact_kind: w.output_artifact_kind,
            job_input: w.job_input,
            created_at: w.created_at,
            state,
        })
    }
}

impl From<Job> for JobWire {
    fn from(job: Job) -> Self {
        let status = job.state.status();
        let outcome_class = job.state.outcome_class();
        let workspace_id = job.state.workspace_id();
        let agent_id = job.state.agent_id();
        let prompt_snapshot = job.state.prompt_snapshot().map(ToOwned::to_owned);
        let phase_template_digest = job.state.phase_template_digest().map(ToOwned::to_owned);
        let output_commit_oid = job.state.output_commit_oid().map(ToOwned::to_owned);
        let result_schema_version = job.state.result_schema_version().map(ToOwned::to_owned);
        let result_payload = job.state.result_payload().cloned();
        let process_pid = job.state.process_pid();
        let lease_owner_id = job.state.lease_owner_id().map(ToOwned::to_owned);
        let heartbeat_at = job.state.heartbeat_at();
        let lease_expires_at = job.state.lease_expires_at();
        let error_code = job.state.error_code().map(ToOwned::to_owned);
        let error_message = job.state.error_message().map(ToOwned::to_owned);
        let started_at = job.state.started_at();
        let ended_at = job.state.ended_at();

        JobWire {
            id: job.id,
            project_id: job.project_id,
            item_id: job.item_id,
            item_revision_id: job.item_revision_id,
            step_id: job.step_id,
            semantic_attempt_no: job.semantic_attempt_no,
            retry_no: job.retry_no,
            supersedes_job_id: job.supersedes_job_id,
            status,
            outcome_class,
            phase_kind: job.phase_kind,
            workspace_id,
            workspace_kind: job.workspace_kind,
            execution_permission: job.execution_permission,
            context_policy: job.context_policy,
            phase_template_slug: job.phase_template_slug,
            phase_template_digest,
            prompt_snapshot,
            job_input: job.job_input,
            output_artifact_kind: job.output_artifact_kind,
            output_commit_oid,
            result_schema_version,
            result_payload,
            agent_id,
            process_pid,
            lease_owner_id,
            heartbeat_at,
            lease_expires_at,
            error_code,
            error_message,
            created_at: job.created_at,
            started_at,
            ended_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(try_from = "JobWire", into = "JobWire")]
pub struct Job {
    // Core identity (always present)
    pub id: JobId,
    pub project_id: ProjectId,
    pub item_id: ItemId,
    pub item_revision_id: ItemRevisionId,
    pub step_id: String,
    pub semantic_attempt_no: u32,
    pub retry_no: u32,
    pub supersedes_job_id: Option<JobId>,
    pub phase_kind: PhaseKind,
    pub workspace_kind: WorkspaceKind,
    pub execution_permission: ExecutionPermission,
    pub context_policy: ContextPolicy,
    pub phase_template_slug: String,
    pub output_artifact_kind: OutputArtifactKind,
    pub job_input: JobInput,
    pub created_at: DateTime<Utc>,

    // Lifecycle state (replaces status + 17 Option fields)
    pub state: JobState,
}

impl Job {
    pub fn assign(&mut self, assignment: JobAssignment) {
        self.state = JobState::Assigned(assignment);
    }

    pub fn complete(
        &mut self,
        outcome_class: OutcomeClass,
        ended_at: DateTime<Utc>,
        output_commit_oid: Option<String>,
        result_schema_version: Option<String>,
        result_payload: Option<serde_json::Value>,
    ) {
        let previous_state = std::mem::replace(&mut self.state, JobState::Queued);
        self.state = previous_state.into_completed(
            outcome_class,
            ended_at,
            output_commit_oid,
            result_schema_version,
            result_payload,
        );
    }

    pub fn terminate(
        &mut self,
        terminal_status: TerminalStatus,
        ended_at: DateTime<Utc>,
        outcome_class: Option<OutcomeClass>,
        error_code: Option<String>,
        error_message: Option<String>,
    ) {
        let previous_state = std::mem::replace(&mut self.state, JobState::Queued);
        self.state = previous_state.into_terminated(
            terminal_status,
            ended_at,
            outcome_class,
            error_code,
            error_message,
        );
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use serde_json::json;

    use super::*;

    fn base_job(state: JobState) -> Job {
        Job {
            id: JobId::new(),
            project_id: ProjectId::new(),
            item_id: ItemId::new(),
            item_revision_id: ItemRevisionId::new(),
            step_id: "author_initial".into(),
            semantic_attempt_no: 1,
            retry_no: 0,
            supersedes_job_id: None,
            phase_kind: PhaseKind::Author,
            workspace_kind: WorkspaceKind::Authoring,
            execution_permission: ExecutionPermission::MayMutate,
            context_policy: ContextPolicy::Fresh,
            phase_template_slug: "template".into(),
            output_artifact_kind: OutputArtifactKind::None,
            job_input: JobInput::None,
            created_at: Utc::now(),
            state,
        }
    }

    #[test]
    fn deserialize_rejects_assigned_jobs_without_workspace_id() {
        let mut value = serde_json::to_value(base_job(JobState::Assigned(JobAssignment::new(
            WorkspaceId::new(),
        ))))
        .expect("serialize assigned job");
        value
            .as_object_mut()
            .expect("job json object")
            .remove("workspace_id");

        let error = serde_json::from_value::<Job>(value).expect_err("missing workspace_id");
        assert!(
            error.to_string().contains("workspace_id"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn deserialize_rejects_running_jobs_without_workspace_id() {
        let mut value = serde_json::to_value(base_job(JobState::Running {
            assignment: JobAssignment::new(WorkspaceId::new()),
            lease: JobLease {
                process_pid: Some(42),
                lease_owner_id: "lease-owner".into(),
                heartbeat_at: Utc::now(),
                lease_expires_at: Utc::now(),
                started_at: Utc::now(),
            },
        }))
        .expect("serialize running job");
        value
            .as_object_mut()
            .expect("job json object")
            .remove("workspace_id");

        let error = serde_json::from_value::<Job>(value).expect_err("missing workspace_id");
        assert!(
            error.to_string().contains("workspace_id"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn deserialize_rejects_completed_jobs_without_outcome_class() {
        let mut value = serde_json::to_value(base_job(JobState::Completed {
            assignment: None,
            started_at: Some(Utc::now()),
            outcome_class: OutcomeClass::Findings,
            ended_at: Utc::now(),
            output_commit_oid: None,
            result_schema_version: None,
            result_payload: Some(json!({ "outcome": "findings" })),
        }))
        .expect("serialize completed job");
        value
            .as_object_mut()
            .expect("job json object")
            .remove("outcome_class");

        let error = serde_json::from_value::<Job>(value).expect_err("missing outcome_class");
        assert!(
            error.to_string().contains("outcome_class"),
            "unexpected error: {error}"
        );
    }
}
