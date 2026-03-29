use std::path::PathBuf;

use ingot_domain::commit_oid::CommitOid;
use ingot_domain::convergence::Convergence;
use ingot_domain::finding::Finding;
use ingot_domain::ids::WorkspaceId;
use ingot_domain::item::Item;
use ingot_domain::job::Job;
use ingot_domain::ports::RepositoryError;
use ingot_domain::project::Project;
use ingot_domain::revision::ItemRevision;
use ingot_domain::workspace::{
    RetentionPolicy, Workspace, WorkspaceCommitState, WorkspaceState, WorkspaceStrategy,
};
use ingot_store_sqlite::Database;

use crate::env::migrated_test_db_with_path;
use crate::git::unique_temp_path;

pub fn temp_db_path(prefix: &str) -> PathBuf {
    unique_temp_path(prefix).with_extension("db")
}

pub async fn migrated_test_db(prefix: &str) -> Database {
    migrated_test_db_with_path(prefix).await.0
}

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
        ensure_job_workspace(db, &self).await?;
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

pub async fn ensure_job_workspace(db: &Database, job: &Job) -> Result<(), RepositoryError> {
    let Some(workspace_id) = job.state.workspace_id() else {
        return Ok(());
    };

    if db.get_workspace(workspace_id).await.is_ok() {
        return Ok(());
    }

    let workspace = placeholder_workspace(job, workspace_id);
    db.create_workspace(&workspace).await
}

pub fn placeholder_workspace(job: &Job, workspace_id: WorkspaceId) -> Workspace {
    Workspace {
        id: workspace_id,
        project_id: job.project_id,
        kind: job.workspace_kind,
        retention_policy: RetentionPolicy::Persistent,
        strategy: WorkspaceStrategy::Worktree,
        created_for_revision_id: Some(job.item_revision_id),
        parent_workspace_id: None,
        path: "/tmp/test-workspace".into(),
        workspace_ref: None,
        target_ref: None,
        state: placeholder_workspace_state(job),
        updated_at: job.created_at,
        created_at: job.created_at,
    }
}

fn placeholder_workspace_state(job: &Job) -> WorkspaceState {
    let placeholder = CommitOid::new("workspace-placeholder");
    let commits = WorkspaceCommitState::new(placeholder.clone(), placeholder);

    if job.state.is_active() {
        WorkspaceState::Busy {
            commits,
            current_job_id: job.id,
        }
    } else {
        WorkspaceState::Ready { commits }
    }
}
