use ingot_domain::agent::{AdapterKind, AgentCapability};
use ingot_domain::commit_oid::CommitOid;
use ingot_domain::finding::{Finding, FindingTriageState};
use ingot_domain::git_ref::GitRef;
use ingot_domain::ids::{AgentId, FindingId, ItemId, JobId, ProjectId, WorkspaceId};
use ingot_domain::item::{Classification, Item, Priority};
use ingot_domain::job::{Job, OutcomeClass};
use ingot_domain::revision::{ApprovalPolicy, ItemRevision};
use ingot_domain::revision_context::RevisionContextSummary;
use ingot_domain::workspace::Workspace;
use ingot_workflow::Evaluation;
use serde::{Deserialize, Serialize};

use super::support::parse_id;
use crate::error::ApiError;

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct ItemSummaryResponse {
    pub item: Item,
    pub title: String,
    pub evaluation: Evaluation,
    pub queue: QueueStatusResponse,
}

#[derive(Debug, Serialize)]
pub struct ConvergenceResponse {
    pub id: String,
    pub status: String,
    pub input_target_commit_oid: Option<CommitOid>,
    pub prepared_commit_oid: Option<CommitOid>,
    pub final_target_commit_oid: Option<CommitOid>,
    pub target_head_valid: bool,
}

#[derive(Debug, Serialize)]
pub struct QueueStatusResponse {
    pub state: Option<String>,
    pub position: Option<u32>,
    pub lane_owner_item_id: Option<String>,
    pub lane_target_ref: Option<GitRef>,
    pub checkout_sync_blocked: bool,
    pub checkout_sync_message: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ItemDetailResponse {
    pub item: Item,
    pub current_revision: ItemRevision,
    pub evaluation: Evaluation,
    pub queue: QueueStatusResponse,
    pub revision_history: Vec<ItemRevision>,
    pub jobs: Vec<Job>,
    pub findings: Vec<Finding>,
    pub workspaces: Vec<Workspace>,
    pub convergences: Vec<ConvergenceResponse>,
    pub revision_context_summary: Option<RevisionContextSummary>,
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct PromoteFindingResponse {
    pub item: Item,
    pub current_revision: ItemRevision,
    pub finding: Finding,
}

#[derive(Debug, Serialize)]
pub struct CompleteJobResponse {
    pub finding_count: usize,
}

#[derive(Debug, Serialize)]
pub struct JobLogsResponse {
    pub prompt: Option<String>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub result: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Path parameter types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(super) struct AgentPathParams {
    pub(super) agent_id: AgentId,
}

#[derive(Debug, Deserialize)]
pub(super) struct FindingPathParams {
    pub(super) finding_id: FindingId,
}

#[derive(Debug, Deserialize)]
pub(super) struct JobPathParams {
    pub(super) job_id: JobId,
}

#[derive(Debug, Deserialize)]
pub(super) struct ProjectPathParams {
    pub(super) project_id: ProjectId,
}

#[derive(Debug, Deserialize)]
pub(super) struct ProjectItemPathParams {
    pub(super) project_id: ProjectId,
    pub(super) item_id: ItemId,
}

#[derive(Debug, Deserialize)]
pub(super) struct ProjectItemJobPathParams {
    pub(super) project_id: ProjectId,
    pub(super) item_id: ItemId,
    pub(super) job_id: JobId,
}

#[derive(Debug, Deserialize)]
pub(super) struct ProjectWorkspacePathParams {
    pub(super) project_id: ProjectId,
    pub(super) workspace_id: WorkspaceId,
}

// ---------------------------------------------------------------------------
// Request types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ActivityQuery {
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct CreateProjectRequest {
    pub name: Option<String>,
    pub path: String,
    pub default_branch: Option<String>,
    pub color: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct UpdateProjectRequest {
    pub name: Option<String>,
    pub path: Option<String>,
    pub default_branch: Option<String>,
    pub color: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct UpdateItemRequest {
    pub classification: Option<Classification>,
    pub priority: Option<Priority>,
    pub labels: Option<Vec<String>>,
    pub operator_notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateAgentRequest {
    pub slug: Option<String>,
    pub name: String,
    pub adapter_kind: AdapterKind,
    pub provider: String,
    pub model: String,
    pub cli_path: String,
    pub capabilities: Option<Vec<AgentCapability>>,
}

#[derive(Debug, Default, Deserialize)]
pub struct UpdateAgentRequest {
    pub slug: Option<String>,
    pub name: Option<String>,
    pub adapter_kind: Option<AdapterKind>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub cli_path: Option<String>,
    pub capabilities: Option<Vec<AgentCapability>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateItemRequest {
    pub title: String,
    pub description: String,
    pub acceptance_criteria: String,
    pub classification: Option<Classification>,
    pub priority: Option<Priority>,
    pub labels: Option<Vec<String>>,
    pub operator_notes: Option<String>,
    pub target_ref: Option<GitRef>,
    pub approval_policy: Option<ApprovalPolicy>,
    pub seed_commit_oid: Option<CommitOid>,
    pub seed_target_commit_oid: Option<CommitOid>,
}

#[derive(Debug, Deserialize)]
pub struct DismissFindingRequest {
    pub dismissal_reason: String,
}

#[derive(Debug)]
pub struct TriageFindingRequest {
    pub triage_state: FindingTriageState,
    pub triage_note: Option<String>,
    pub linked_item_id: Option<ItemId>,
    pub target_ref: Option<GitRef>,
    pub approval_policy: Option<ApprovalPolicy>,
}

#[derive(Debug, Deserialize)]
pub(super) struct TriageFindingRequestPayload {
    pub triage_state: FindingTriageState,
    pub triage_note: Option<String>,
    pub linked_item_id: Option<String>,
    pub target_ref: Option<GitRef>,
    pub approval_policy: Option<ApprovalPolicy>,
}

impl TryFrom<TriageFindingRequestPayload> for TriageFindingRequest {
    type Error = ApiError;

    fn try_from(payload: TriageFindingRequestPayload) -> Result<Self, Self::Error> {
        Ok(Self {
            triage_state: payload.triage_state,
            triage_note: payload.triage_note,
            linked_item_id: payload
                .linked_item_id
                .as_deref()
                .map(|value| parse_id::<ItemId>(value, "linked_item"))
                .transpose()?,
            target_ref: payload.target_ref,
            approval_policy: payload.approval_policy,
        })
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct DispatchJobRequest {
    pub step_id: Option<ingot_domain::step_id::StepId>,
}

#[derive(Debug, Deserialize)]
pub struct PromoteFindingRequest {
    pub target_ref: Option<GitRef>,
    pub approval_policy: Option<ApprovalPolicy>,
}

#[derive(Debug, Default, Deserialize)]
pub struct RejectApprovalRequest {
    pub title: Option<String>,
    pub description: Option<String>,
    pub acceptance_criteria: Option<String>,
    pub target_ref: Option<GitRef>,
    pub approval_policy: Option<ApprovalPolicy>,
    pub seed_commit_oid: Option<CommitOid>,
    pub seed_target_commit_oid: Option<CommitOid>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ReviseItemRequest {
    pub title: Option<String>,
    pub description: Option<String>,
    pub acceptance_criteria: Option<String>,
    pub target_ref: Option<GitRef>,
    pub approval_policy: Option<ApprovalPolicy>,
    pub seed_commit_oid: Option<CommitOid>,
    pub seed_target_commit_oid: Option<CommitOid>,
}

impl From<RejectApprovalRequest> for ReviseItemRequest {
    fn from(request: RejectApprovalRequest) -> Self {
        Self {
            title: request.title,
            description: request.description,
            acceptance_criteria: request.acceptance_criteria,
            target_ref: request.target_ref,
            approval_policy: request.approval_policy,
            seed_commit_oid: request.seed_commit_oid,
            seed_target_commit_oid: request.seed_target_commit_oid,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct CompleteJobRequest {
    pub outcome_class: OutcomeClass,
    pub result_schema_version: Option<String>,
    pub result_payload: Option<serde_json::Value>,
    pub output_commit_oid: Option<CommitOid>,
}

#[derive(Debug, Deserialize)]
pub struct FailJobRequest {
    pub outcome_class: OutcomeClass,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
}
