use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{ItemId, ItemRevisionId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalPolicy {
    Required,
    NotRequired,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemRevision {
    pub id: ItemRevisionId,
    pub item_id: ItemId,
    pub revision_no: u32,
    pub title: String,
    pub description: String,
    pub acceptance_criteria: String,
    pub target_ref: String,
    pub approval_policy: ApprovalPolicy,
    pub policy_snapshot: serde_json::Value,
    pub template_map_snapshot: serde_json::Value,
    pub seed_commit_oid: String,
    pub seed_target_commit_oid: Option<String>,
    pub supersedes_revision_id: Option<ItemRevisionId>,
    pub created_at: DateTime<Utc>,
}
