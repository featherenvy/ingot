use chrono::{DateTime, Utc};
use ingot_domain::convergence::{Convergence, ConvergenceStatus, ConvergenceStrategy};
use ingot_domain::ids;

use super::timestamps::default_timestamp;

pub struct ConvergenceBuilder {
    id: ids::ConvergenceId,
    project_id: ids::ProjectId,
    item_id: ids::ItemId,
    item_revision_id: ids::ItemRevisionId,
    source_workspace_id: ids::WorkspaceId,
    integration_workspace_id: Option<ids::WorkspaceId>,
    source_head_commit_oid: String,
    target_ref: String,
    strategy: ConvergenceStrategy,
    status: ConvergenceStatus,
    input_target_commit_oid: Option<String>,
    prepared_commit_oid: Option<String>,
    final_target_commit_oid: Option<String>,
    target_head_valid: Option<bool>,
    conflict_summary: Option<String>,
    created_at: DateTime<Utc>,
    completed_at: Option<DateTime<Utc>>,
}

impl ConvergenceBuilder {
    pub fn new(
        project_id: ids::ProjectId,
        item_id: ids::ItemId,
        item_revision_id: ids::ItemRevisionId,
    ) -> Self {
        Self {
            id: ids::ConvergenceId::new(),
            project_id,
            item_id,
            item_revision_id,
            source_workspace_id: ids::WorkspaceId::new(),
            integration_workspace_id: Some(ids::WorkspaceId::new()),
            source_head_commit_oid: "head".into(),
            target_ref: "refs/heads/main".into(),
            strategy: ConvergenceStrategy::RebaseThenFastForward,
            status: ConvergenceStatus::Prepared,
            input_target_commit_oid: Some("base".into()),
            prepared_commit_oid: Some("prepared".into()),
            final_target_commit_oid: None,
            target_head_valid: None,
            conflict_summary: None,
            created_at: default_timestamp(),
            completed_at: None,
        }
    }

    pub fn id(mut self, id: ids::ConvergenceId) -> Self {
        self.id = id;
        self
    }

    pub fn source_workspace_id(mut self, id: ids::WorkspaceId) -> Self {
        self.source_workspace_id = id;
        self
    }

    pub fn integration_workspace_id(mut self, id: ids::WorkspaceId) -> Self {
        self.integration_workspace_id = Some(id);
        self
    }

    pub fn source_head_commit_oid(mut self, oid: impl Into<String>) -> Self {
        self.source_head_commit_oid = oid.into();
        self
    }

    pub fn status(mut self, status: ConvergenceStatus) -> Self {
        self.status = status;
        self
    }

    pub fn input_target_commit_oid(mut self, oid: impl Into<String>) -> Self {
        self.input_target_commit_oid = Some(oid.into());
        self
    }

    pub fn prepared_commit_oid(mut self, oid: impl Into<String>) -> Self {
        self.prepared_commit_oid = Some(oid.into());
        self
    }

    pub fn no_integration_workspace_id(mut self) -> Self {
        self.integration_workspace_id = None;
        self
    }

    pub fn no_prepared_commit_oid(mut self) -> Self {
        self.prepared_commit_oid = None;
        self
    }

    pub fn target_head_valid(mut self, valid: bool) -> Self {
        self.target_head_valid = Some(valid);
        self
    }

    pub fn completed_at(mut self, completed_at: DateTime<Utc>) -> Self {
        self.completed_at = Some(completed_at);
        self
    }

    pub fn created_at(mut self, created_at: DateTime<Utc>) -> Self {
        self.created_at = created_at;
        self
    }

    pub fn build(self) -> Convergence {
        Convergence {
            id: self.id,
            project_id: self.project_id,
            item_id: self.item_id,
            item_revision_id: self.item_revision_id,
            source_workspace_id: self.source_workspace_id,
            integration_workspace_id: self.integration_workspace_id,
            source_head_commit_oid: self.source_head_commit_oid,
            target_ref: self.target_ref,
            strategy: self.strategy,
            status: self.status,
            input_target_commit_oid: self.input_target_commit_oid,
            prepared_commit_oid: self.prepared_commit_oid,
            final_target_commit_oid: self.final_target_commit_oid,
            target_head_valid: self.target_head_valid,
            conflict_summary: self.conflict_summary,
            created_at: self.created_at,
            completed_at: self.completed_at,
        }
    }
}
