use chrono::{DateTime, Utc};
use ingot_domain::ids;
use ingot_domain::workspace::{
    RetentionPolicy, Workspace, WorkspaceKind, WorkspaceStatus, WorkspaceStrategy,
};
use uuid::Uuid;

use super::timestamps::default_timestamp;

pub struct WorkspaceBuilder {
    id: ids::WorkspaceId,
    project_id: ids::ProjectId,
    kind: WorkspaceKind,
    path: String,
    created_for_revision_id: Option<ids::ItemRevisionId>,
    target_ref: Option<String>,
    workspace_ref: Option<String>,
    base_commit_oid: Option<String>,
    head_commit_oid: Option<String>,
    retention_policy: RetentionPolicy,
    status: WorkspaceStatus,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl WorkspaceBuilder {
    pub fn new(project_id: ids::ProjectId, kind: WorkspaceKind) -> Self {
        let now = default_timestamp();
        Self {
            id: ids::WorkspaceId::new(),
            project_id,
            kind,
            path: std::env::temp_dir()
                .join(format!("ingot-test-workspace-{}", Uuid::now_v7()))
                .display()
                .to_string(),
            created_for_revision_id: None,
            target_ref: Some("refs/heads/main".into()),
            workspace_ref: Some(format!("refs/ingot/workspaces/{}", Uuid::now_v7().simple())),
            base_commit_oid: None,
            head_commit_oid: None,
            retention_policy: RetentionPolicy::Persistent,
            status: WorkspaceStatus::Ready,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn id(mut self, id: ids::WorkspaceId) -> Self {
        self.id = id;
        self
    }

    pub fn created_for_revision_id(mut self, revision_id: ids::ItemRevisionId) -> Self {
        self.created_for_revision_id = Some(revision_id);
        self
    }

    pub fn base_commit_oid(mut self, commit_oid: impl Into<String>) -> Self {
        self.base_commit_oid = Some(commit_oid.into());
        self
    }

    pub fn head_commit_oid(mut self, commit_oid: impl Into<String>) -> Self {
        self.head_commit_oid = Some(commit_oid.into());
        self
    }

    pub fn path(mut self, path: impl Into<String>) -> Self {
        self.path = path.into();
        self
    }

    pub fn workspace_ref(mut self, workspace_ref: impl Into<String>) -> Self {
        self.workspace_ref = Some(workspace_ref.into());
        self
    }

    pub fn status(mut self, status: WorkspaceStatus) -> Self {
        self.status = status;
        self
    }

    pub fn created_at(mut self, created_at: DateTime<Utc>) -> Self {
        self.created_at = created_at;
        self.updated_at = created_at;
        self
    }

    pub fn build(self) -> Workspace {
        Workspace {
            id: self.id,
            project_id: self.project_id,
            kind: self.kind,
            strategy: WorkspaceStrategy::Worktree,
            path: self.path,
            created_for_revision_id: self.created_for_revision_id,
            parent_workspace_id: None,
            target_ref: self.target_ref,
            workspace_ref: self.workspace_ref,
            base_commit_oid: self.base_commit_oid,
            head_commit_oid: self.head_commit_oid,
            retention_policy: self.retention_policy,
            status: self.status,
            current_job_id: None,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}
