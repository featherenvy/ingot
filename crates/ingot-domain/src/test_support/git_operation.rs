use crate::commit_oid::CommitOid;
use crate::git_ref::GitRef;
use crate::git_operation::{
    ConvergenceReplayMetadata, GitEntityType, GitOperation, GitOperationStatus, OperationKind,
    OperationPayload,
};
use crate::ids;
use chrono::{DateTime, Utc};

use super::timestamps::default_timestamp;

pub struct GitOperationBuilder {
    id: ids::GitOperationId,
    project_id: ids::ProjectId,
    operation_kind: OperationKind,
    _entity_type: GitEntityType,
    entity_id: String,
    workspace_id: Option<ids::WorkspaceId>,
    ref_name: Option<GitRef>,
    expected_old_oid: Option<CommitOid>,
    new_oid: Option<CommitOid>,
    commit_oid: Option<CommitOid>,
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
            _entity_type: entity_type,
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

    pub fn ref_name(mut self, ref_name: impl Into<GitRef>) -> Self {
        self.ref_name = Some(ref_name.into());
        self
    }

    pub fn expected_old_oid(mut self, expected_old_oid: impl Into<CommitOid>) -> Self {
        self.expected_old_oid = Some(expected_old_oid.into());
        self
    }

    pub fn new_oid(mut self, new_oid: impl Into<CommitOid>) -> Self {
        self.new_oid = Some(new_oid.into());
        self
    }

    pub fn commit_oid(mut self, commit_oid: impl Into<CommitOid>) -> Self {
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
        let replay_metadata = self.metadata.and_then(|metadata| {
            serde_json::from_value::<ConvergenceReplayMetadata>(metadata).ok()
        });

        let payload = match self.operation_kind {
            OperationKind::CreateJobCommit => OperationPayload::CreateJobCommit {
                workspace_id: self
                    .workspace_id
                    .expect("CreateJobCommit requires workspace_id"),
                ref_name: self.ref_name.expect("CreateJobCommit requires ref_name"),
                expected_old_oid: self
                    .expected_old_oid
                    .expect("CreateJobCommit requires expected_old_oid"),
                new_oid: self.new_oid,
                commit_oid: self.commit_oid,
            },
            OperationKind::PrepareConvergenceCommit => OperationPayload::PrepareConvergenceCommit {
                workspace_id: self
                    .workspace_id
                    .expect("PrepareConvergenceCommit requires workspace_id"),
                ref_name: self.ref_name,
                expected_old_oid: self
                    .expected_old_oid
                    .expect("PrepareConvergenceCommit requires expected_old_oid"),
                new_oid: self.new_oid,
                commit_oid: self.commit_oid,
                replay_metadata,
            },
            OperationKind::FinalizeTargetRef => OperationPayload::FinalizeTargetRef {
                workspace_id: self.workspace_id,
                ref_name: self.ref_name.expect("FinalizeTargetRef requires ref_name"),
                expected_old_oid: self
                    .expected_old_oid
                    .expect("FinalizeTargetRef requires expected_old_oid"),
                new_oid: self.new_oid.expect("FinalizeTargetRef requires new_oid"),
                commit_oid: self.commit_oid,
            },
            OperationKind::CreateInvestigationRef => OperationPayload::CreateInvestigationRef {
                ref_name: self
                    .ref_name
                    .expect("CreateInvestigationRef requires ref_name"),
                new_oid: self
                    .new_oid
                    .expect("CreateInvestigationRef requires new_oid"),
                commit_oid: self.commit_oid,
            },
            OperationKind::RemoveInvestigationRef => OperationPayload::RemoveInvestigationRef {
                ref_name: self
                    .ref_name
                    .expect("RemoveInvestigationRef requires ref_name"),
                expected_old_oid: self
                    .expected_old_oid
                    .expect("RemoveInvestigationRef requires expected_old_oid"),
            },
            OperationKind::ResetWorkspace => OperationPayload::ResetWorkspace {
                workspace_id: self
                    .workspace_id
                    .expect("ResetWorkspace requires workspace_id"),
                ref_name: self.ref_name,
                expected_old_oid: self.expected_old_oid,
                new_oid: self.new_oid.expect("ResetWorkspace requires new_oid"),
            },
            OperationKind::RemoveWorkspaceRef => OperationPayload::RemoveWorkspaceRef {
                workspace_id: self
                    .workspace_id
                    .expect("RemoveWorkspaceRef requires workspace_id"),
                ref_name: self.ref_name.expect("RemoveWorkspaceRef requires ref_name"),
                expected_old_oid: self
                    .expected_old_oid
                    .expect("RemoveWorkspaceRef requires expected_old_oid"),
            },
        };

        GitOperation {
            id: self.id,
            project_id: self.project_id,
            entity_id: self.entity_id,
            payload,
            status: self.status,
            created_at: self.created_at,
            completed_at: self.completed_at,
        }
    }
}
