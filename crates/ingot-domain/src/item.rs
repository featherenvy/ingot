use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{FindingId, ItemId, ItemRevisionId, ProjectId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Classification {
    Change,
    Bug,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleState {
    Open,
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParkingState {
    Active,
    Deferred,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DoneReason {
    Completed,
    Dismissed,
    Invalidated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionSource {
    SystemCommand,
    ApprovalCommand,
    ManualCommand,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalState {
    NotRequired,
    NotRequested,
    Pending,
    Approved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalationState {
    None,
    OperatorRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalationReason {
    CandidateReworkBudgetExhausted,
    IntegrationReworkBudgetExhausted,
    ConvergenceConflict,
    StepFailed,
    ProtocolViolation,
    ManualDecisionRequired,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Critical,
    Major,
    Minor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OriginKind {
    Manual,
    PromotedFinding,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    pub id: ItemId,
    pub project_id: ProjectId,
    pub classification: Classification,
    pub workflow_version: String,
    pub lifecycle_state: LifecycleState,
    pub parking_state: ParkingState,
    pub done_reason: Option<DoneReason>,
    pub resolution_source: Option<ResolutionSource>,
    pub approval_state: ApprovalState,
    pub escalation_state: EscalationState,
    pub escalation_reason: Option<EscalationReason>,
    pub current_revision_id: ItemRevisionId,
    pub origin_kind: OriginKind,
    pub origin_finding_id: Option<FindingId>,
    pub priority: Priority,
    pub labels: Vec<String>,
    pub operator_notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
}
