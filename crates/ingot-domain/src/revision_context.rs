use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::commit_oid::CommitOid;
use crate::ids::{ItemRevisionId, JobId};
use crate::job::OutcomeClass;
use crate::step_id::StepId;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevisionContextResultSummary {
    pub job_id: JobId,
    pub schema_version: String,
    pub outcome: OutcomeClass,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevisionContextAcceptedResultRef {
    pub job_id: JobId,
    pub step_id: StepId,
    pub schema_version: String,
    pub outcome: OutcomeClass,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevisionContextSummary {
    pub updated_at: DateTime<Utc>,
    pub changed_paths: Vec<String>,
    pub latest_validation: Option<RevisionContextResultSummary>,
    pub latest_review: Option<RevisionContextResultSummary>,
    pub accepted_result_refs: Vec<RevisionContextAcceptedResultRef>,
    pub operator_notes_excerpt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevisionContextPayload {
    pub authoring_head_commit_oid: Option<CommitOid>,
    pub changed_paths: Vec<String>,
    pub latest_validation: Option<RevisionContextResultSummary>,
    pub latest_review: Option<RevisionContextResultSummary>,
    pub accepted_result_refs: Vec<RevisionContextAcceptedResultRef>,
    pub operator_notes_excerpt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevisionContext {
    pub item_revision_id: ItemRevisionId,
    pub schema_version: String,
    pub payload: RevisionContextPayload,
    pub updated_from_job_id: Option<JobId>,
    pub updated_at: DateTime<Utc>,
}
