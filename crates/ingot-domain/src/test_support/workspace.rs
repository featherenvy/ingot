use crate::commit_oid::CommitOid;
use crate::git_ref::GitRef;
use crate::ids;
use crate::workspace::{
    RetentionPolicy, Workspace, WorkspaceCommitState, WorkspaceKind, WorkspaceState,
    WorkspaceStatus, WorkspaceStrategy,
};
use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::timestamps::default_timestamp;

pub struct WorkspaceBuilder {
    id: ids::WorkspaceId,
    project_id: ids::ProjectId,
    kind: WorkspaceKind,
    path: String,
    created_for_revision_id: Option<ids::ItemRevisionId>,
    target_ref: Option<GitRef>,
    workspace_ref: Option<GitRef>,
    base_commit_oid: Option<CommitOid>,
    head_commit_oid: Option<CommitOid>,
    retention_policy: RetentionPolicy,
    status: WorkspaceStatus,
    current_job_id: Option<ids::JobId>,
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
            target_ref: Some(GitRef::new("refs/heads/main")),
            workspace_ref: Some(GitRef::new(format!("refs/ingot/workspaces/{}", Uuid::now_v7().simple()))),
            base_commit_oid: None,
            head_commit_oid: None,
            retention_policy: RetentionPolicy::Persistent,
            status: WorkspaceStatus::Ready,
            current_job_id: None,
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
        self.base_commit_oid = Some(CommitOid::new(commit_oid.into()));
        self
    }

    pub fn head_commit_oid(mut self, commit_oid: impl Into<String>) -> Self {
        self.head_commit_oid = Some(CommitOid::new(commit_oid.into()));
        self
    }

    pub fn path(mut self, path: impl Into<String>) -> Self {
        self.path = path.into();
        self
    }

    pub fn workspace_ref(mut self, workspace_ref: impl Into<String>) -> Self {
        self.workspace_ref = Some(GitRef::new(workspace_ref.into()));
        self
    }

    pub fn retention_policy(mut self, retention_policy: RetentionPolicy) -> Self {
        self.retention_policy = retention_policy;
        self
    }

    pub fn no_target_ref(mut self) -> Self {
        self.target_ref = None;
        self
    }

    pub fn no_workspace_ref(mut self) -> Self {
        self.workspace_ref = None;
        self
    }

    pub fn current_job_id(mut self, job_id: ids::JobId) -> Self {
        self.current_job_id = Some(job_id);
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
        let commits =
            WorkspaceCommitState::from_option_parts(self.base_commit_oid, self.head_commit_oid);
        let required_commits = matches!(
            self.status,
            WorkspaceStatus::Ready
                | WorkspaceStatus::Busy
                | WorkspaceStatus::RetainedForDebug
        );
        let state = WorkspaceState::from_parts(
            self.status,
            if required_commits {
                Some(commits.unwrap_or_else(|| {
                    let placeholder = CommitOid::new("workspace-placeholder");
                    WorkspaceCommitState::new(placeholder.clone(), placeholder)
                }))
            } else {
                commits
            },
            self.current_job_id,
        )
        .unwrap_or_else(|error| panic!("WorkspaceBuilder: {error}"));

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
            retention_policy: self.retention_policy,
            created_at: self.created_at,
            updated_at: self.updated_at,
            state,
        }
    }
}
