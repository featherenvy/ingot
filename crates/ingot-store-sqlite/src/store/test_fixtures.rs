use ingot_domain::commit_oid::CommitOid;
use ingot_domain::convergence::Convergence;
use ingot_domain::finding::Finding;
use ingot_domain::item::Item;
use ingot_domain::job::Job;
use ingot_domain::ports::RepositoryError;
use ingot_domain::project::Project;
use ingot_domain::revision::ItemRevision;
use ingot_domain::workspace::{Workspace, WorkspaceCommitState, WorkspaceState};

use crate::db::Database;

#[allow(async_fn_in_trait)]
pub trait PersistFixture: Sized {
    async fn persist(self, db: &Database) -> Result<Self, RepositoryError>;
}

impl PersistFixture for Project {
    async fn persist(self, db: &Database) -> Result<Self, RepositoryError> {
        db.create_project(&self).await?;
        Ok(self)
    }
}

impl PersistFixture for Item {
    async fn persist(self, db: &Database) -> Result<Self, RepositoryError> {
        db.create_item(&self).await?;
        Ok(self)
    }
}

impl PersistFixture for ItemRevision {
    async fn persist(self, db: &Database) -> Result<Self, RepositoryError> {
        db.create_revision(&self).await?;
        Ok(self)
    }
}

impl PersistFixture for (Item, ItemRevision) {
    async fn persist(self, db: &Database) -> Result<Self, RepositoryError> {
        db.create_item_with_revision(&self.0, &self.1).await?;
        Ok(self)
    }
}

impl PersistFixture for Job {
    async fn persist(self, db: &Database) -> Result<Self, RepositoryError> {
        // Auto-create workspace if the job state references one that may not exist
        if let Some(workspace_id) = self.state.workspace_id() {
            if db.get_workspace(workspace_id).await.is_err() {
                let workspace_is_active = self.state.is_active();
                let placeholder = CommitOid::new("workspace-placeholder");
                let placeholder_commits =
                    WorkspaceCommitState::new(placeholder.clone(), placeholder);
                let state = if workspace_is_active {
                    WorkspaceState::Busy {
                        commits: placeholder_commits.clone(),
                        current_job_id: self.id,
                    }
                } else {
                    WorkspaceState::Ready {
                        commits: placeholder_commits,
                    }
                };
                let workspace = Workspace {
                    id: workspace_id,
                    project_id: self.project_id,
                    kind: self.workspace_kind,
                    retention_policy: ingot_domain::workspace::RetentionPolicy::Persistent,
                    strategy: ingot_domain::workspace::WorkspaceStrategy::Worktree,
                    created_for_revision_id: Some(self.item_revision_id),
                    parent_workspace_id: None,
                    path: "/tmp/test-workspace".into(),
                    workspace_ref: None,
                    target_ref: None,
                    state,
                    updated_at: self.created_at,
                    created_at: self.created_at,
                };
                db.create_workspace(&workspace).await?;
            }
        }
        db.create_job(&self).await?;
        Ok(self)
    }
}

impl PersistFixture for Workspace {
    async fn persist(self, db: &Database) -> Result<Self, RepositoryError> {
        db.create_workspace(&self).await?;
        Ok(self)
    }
}

impl PersistFixture for Convergence {
    async fn persist(self, db: &Database) -> Result<Self, RepositoryError> {
        db.create_convergence(&self).await?;
        Ok(self)
    }
}

impl PersistFixture for Finding {
    async fn persist(self, db: &Database) -> Result<Self, RepositoryError> {
        db.create_finding(&self).await?;
        Ok(self)
    }
}
