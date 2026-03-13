use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{FindingId, ItemId, ItemRevisionId, JobId, ProjectId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingSubjectKind {
    Candidate,
    Integrated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingSeverity {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingTriageState {
    Untriaged,
    FixNow,
    WontFix,
    Backlog,
    Duplicate,
    DismissedInvalid,
    NeedsInvestigation,
}

impl FindingTriageState {
    pub fn is_unresolved(self) -> bool {
        matches!(self, Self::Untriaged | Self::NeedsInvestigation)
    }

    pub fn blocks_closure(self) -> bool {
        matches!(
            self,
            Self::Untriaged | Self::FixNow | Self::NeedsInvestigation
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub id: FindingId,
    pub project_id: ProjectId,
    pub source_item_id: ItemId,
    pub source_item_revision_id: ItemRevisionId,
    pub source_job_id: JobId,
    pub source_step_id: String,
    pub source_report_schema_version: String,
    pub source_finding_key: String,
    pub source_subject_kind: FindingSubjectKind,
    pub source_subject_base_commit_oid: Option<String>,
    pub source_subject_head_commit_oid: String,
    pub code: String,
    pub severity: FindingSeverity,
    pub summary: String,
    pub paths: Vec<String>,
    pub evidence: serde_json::Value,
    pub triage_state: FindingTriageState,
    pub linked_item_id: Option<ItemId>,
    pub triage_note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub triaged_at: Option<DateTime<Utc>>,
}
