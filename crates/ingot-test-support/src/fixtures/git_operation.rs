use chrono::{DateTime, Utc};
use ingot_domain::git_operation::{GitEntityType, GitOperation, GitOperationStatus, OperationKind};
use ingot_domain::ids;

use super::timestamps::default_timestamp;

pub struct GitOperationBuilder {
    id: ids::GitOperationId,
    project_id: ids::ProjectId,
    operation_kind: OperationKind,
    entity_type: GitEntityType,
    entity_id: String,
    workspace_id: Option<ids::WorkspaceId>,
    ref_name: Option<String>,
    expected_old_oid: Option<String>,
    new_oid: Option<String>,
    commit_oid: Option<String>,
    status: GitOperationStatus,
    metadata: Option<serde_json::Value>,
    created_at: DateTime<Utc>,
    completed_at: Option<DateTime<Utc>>,
}

impl GitOperationBuilder {
    pub fn new(
        project_id: ids::ProjectId,
        operation_kind: OperationKind,
        entity_type: GitEntityType,
        entity_id: impl Into<String>,
    ) -> Self {
        Self {
            id: ids::GitOperationId::new(),
            project_id,
            operation_kind,
            entity_type,
            entity_id: entity_id.into(),
            workspace_id: None,
            ref_name: None,
            expected_old_oid: None,
            new_oid: None,
            commit_oid: None,
            status: GitOperationStatus::Applied,
            metadata: None,
            created_at: default_timestamp(),
            completed_at: None,
        }
    }

    pub fn id(mut self, id: ids::GitOperationId) -> Self {
        self.id = id;
        self
    }

    pub fn workspace_id(mut self, workspace_id: ids::WorkspaceId) -> Self {
        self.workspace_id = Some(workspace_id);
        self
    }

    pub fn ref_name(mut self, ref_name: impl Into<String>) -> Self {
        self.ref_name = Some(ref_name.into());
        self
    }

    pub fn expected_old_oid(mut self, expected_old_oid: impl Into<String>) -> Self {
        self.expected_old_oid = Some(expected_old_oid.into());
        self
    }

    pub fn new_oid(mut self, new_oid: impl Into<String>) -> Self {
        self.new_oid = Some(new_oid.into());
        self
    }

    pub fn commit_oid(mut self, commit_oid: impl Into<String>) -> Self {
        self.commit_oid = Some(commit_oid.into());
        self
    }

    pub fn status(mut self, status: GitOperationStatus) -> Self {
        self.status = status;
        self
    }

    pub fn metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = Some(metadata);
        self
    }

    pub fn created_at(mut self, created_at: DateTime<Utc>) -> Self {
        self.created_at = created_at;
        self
    }

    pub fn completed_at(mut self, completed_at: DateTime<Utc>) -> Self {
        self.completed_at = Some(completed_at);
        self
    }

    pub fn build(self) -> GitOperation {
        GitOperation {
            id: self.id,
            project_id: self.project_id,
            operation_kind: self.operation_kind,
            entity_type: self.entity_type,
            entity_id: self.entity_id,
            workspace_id: self.workspace_id,
            ref_name: self.ref_name,
            expected_old_oid: self.expected_old_oid,
            new_oid: self.new_oid,
            commit_oid: self.commit_oid,
            status: self.status,
            metadata: self.metadata,
            created_at: self.created_at,
            completed_at: self.completed_at,
        }
    }
}
