use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{ItemId, ItemRevisionId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalPolicy {
    Required,
    NotRequired,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthoringBaseSeed {
    Explicit {
        seed_commit_oid: String,
        seed_target_commit_oid: String,
    },
    Implicit {
        seed_target_commit_oid: String,
    },
}

impl AuthoringBaseSeed {
    #[must_use]
    pub fn from_parts(seed_commit_oid: Option<String>, seed_target_commit_oid: String) -> Self {
        match seed_commit_oid {
            Some(seed_commit_oid) => Self::Explicit {
                seed_commit_oid,
                seed_target_commit_oid,
            },
            None => Self::Implicit {
                seed_target_commit_oid,
            },
        }
    }

    #[must_use]
    pub fn seed_commit_oid(&self) -> Option<&str> {
        match self {
            Self::Explicit {
                seed_commit_oid, ..
            } => Some(seed_commit_oid),
            Self::Implicit { .. } => None,
        }
    }

    #[must_use]
    pub fn seed_target_commit_oid(&self) -> &str {
        match self {
            Self::Explicit {
                seed_target_commit_oid,
                ..
            }
            | Self::Implicit {
                seed_target_commit_oid,
            } => seed_target_commit_oid,
        }
    }

    #[must_use]
    pub fn is_explicit(&self) -> bool {
        matches!(self, Self::Explicit { .. })
    }
}

// --- Serde: backward-compatible flat JSON via ItemRevisionWire ---

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ItemRevisionWire {
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
    pub seed_commit_oid: Option<String>,
    pub seed_target_commit_oid: String,
    pub supersedes_revision_id: Option<ItemRevisionId>,
    pub created_at: DateTime<Utc>,
}

impl From<ItemRevisionWire> for ItemRevision {
    fn from(w: ItemRevisionWire) -> Self {
        Self {
            id: w.id,
            item_id: w.item_id,
            revision_no: w.revision_no,
            title: w.title,
            description: w.description,
            acceptance_criteria: w.acceptance_criteria,
            target_ref: w.target_ref,
            approval_policy: w.approval_policy,
            policy_snapshot: w.policy_snapshot,
            template_map_snapshot: w.template_map_snapshot,
            seed: AuthoringBaseSeed::from_parts(w.seed_commit_oid, w.seed_target_commit_oid),
            supersedes_revision_id: w.supersedes_revision_id,
            created_at: w.created_at,
        }
    }
}

impl From<ItemRevision> for ItemRevisionWire {
    fn from(revision: ItemRevision) -> Self {
        let ItemRevision {
            id,
            item_id,
            revision_no,
            title,
            description,
            acceptance_criteria,
            target_ref,
            approval_policy,
            policy_snapshot,
            template_map_snapshot,
            seed,
            supersedes_revision_id,
            created_at,
        } = revision;

        Self {
            id,
            item_id,
            revision_no,
            title,
            description,
            acceptance_criteria,
            target_ref,
            approval_policy,
            policy_snapshot,
            template_map_snapshot,
            seed_commit_oid: seed.seed_commit_oid().map(ToOwned::to_owned),
            seed_target_commit_oid: seed.seed_target_commit_oid().to_owned(),
            supersedes_revision_id,
            created_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(from = "ItemRevisionWire", into = "ItemRevisionWire")]
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
    pub seed: AuthoringBaseSeed,
    pub supersedes_revision_id: Option<ItemRevisionId>,
    pub created_at: DateTime<Utc>,
}
