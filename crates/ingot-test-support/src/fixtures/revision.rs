use chrono::{DateTime, Utc};
use ingot_domain::ids;
use ingot_domain::revision::{ApprovalPolicy, ItemRevision};
use serde_json::json;
use uuid::Uuid;

use super::timestamps::default_timestamp;

pub struct RevisionBuilder {
    id: ids::ItemRevisionId,
    item_id: ids::ItemId,
    revision_no: u32,
    title: String,
    description: String,
    acceptance_criteria: String,
    target_ref: String,
    approval_policy: ApprovalPolicy,
    policy_snapshot: serde_json::Value,
    template_map_snapshot: serde_json::Value,
    seed_commit_oid: Option<String>,
    seed_target_commit_oid: Option<String>,
    created_at: DateTime<Utc>,
}

impl RevisionBuilder {
    pub fn nil() -> Self {
        let nil = Uuid::nil();
        Self::new(ids::ItemId::from_uuid(nil))
            .id(ids::ItemRevisionId::from_uuid(nil))
    }

    pub fn new(item_id: ids::ItemId) -> Self {
        Self {
            id: ids::ItemRevisionId::new(),
            item_id,
            revision_no: 1,
            title: "Test item".into(),
            description: "Test item".into(),
            acceptance_criteria: "Test item".into(),
            target_ref: "refs/heads/main".into(),
            approval_policy: ApprovalPolicy::Required,
            policy_snapshot: json!({}),
            template_map_snapshot: json!({}),
            seed_commit_oid: None,
            seed_target_commit_oid: None,
            created_at: default_timestamp(),
        }
    }

    pub fn id(mut self, id: ids::ItemRevisionId) -> Self {
        self.id = id;
        self
    }

    pub fn revision_no(mut self, revision_no: u32) -> Self {
        self.revision_no = revision_no;
        self
    }

    pub fn approval_policy(mut self, approval_policy: ApprovalPolicy) -> Self {
        self.approval_policy = approval_policy;
        self
    }

    pub fn explicit_seed(mut self, commit_oid: impl Into<String>) -> Self {
        let commit_oid = commit_oid.into();
        self.seed_commit_oid = Some(commit_oid.clone());
        self.seed_target_commit_oid = Some(commit_oid);
        self
    }

    pub fn seed_commit_oid(mut self, commit_oid: Option<impl Into<String>>) -> Self {
        self.seed_commit_oid = commit_oid.map(Into::into);
        self
    }

    pub fn seed_target_commit_oid(mut self, commit_oid: Option<impl Into<String>>) -> Self {
        self.seed_target_commit_oid = commit_oid.map(Into::into);
        self
    }

    pub fn created_at(mut self, created_at: DateTime<Utc>) -> Self {
        self.created_at = created_at;
        self
    }

    pub fn build(self) -> ItemRevision {
        ItemRevision {
            id: self.id,
            item_id: self.item_id,
            revision_no: self.revision_no,
            title: self.title,
            description: self.description,
            acceptance_criteria: self.acceptance_criteria,
            target_ref: self.target_ref,
            approval_policy: self.approval_policy,
            policy_snapshot: self.policy_snapshot,
            template_map_snapshot: self.template_map_snapshot,
            seed_commit_oid: self.seed_commit_oid,
            seed_target_commit_oid: self.seed_target_commit_oid,
            supersedes_revision_id: None,
            created_at: self.created_at,
        }
    }
}
