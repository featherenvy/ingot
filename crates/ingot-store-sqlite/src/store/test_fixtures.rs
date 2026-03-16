use ingot_domain::convergence::Convergence;
use ingot_domain::finding::Finding;
use ingot_domain::item::Item;
use ingot_domain::job::Job;
use ingot_domain::ports::RepositoryError;
use ingot_domain::project::Project;
use ingot_domain::revision::ItemRevision;
use ingot_domain::workspace::{Workspace, WorkspaceCommitState, WorkspaceState};

use crate::db::Database;

pub(crate) trait PersistFixture: Sized {
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
                let empty_commits = WorkspaceCommitState::empty();
                let state = if workspace_is_active {
                    WorkspaceState::Busy {
                        commits: empty_commits.clone(),
                        current_job_id: self.id,
                    }
                } else {
                    WorkspaceState::Ready {
                        commits: empty_commits,
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

#[cfg(test)]
mod tests {
    use ingot_domain::job::JobStatus;
    use ingot_domain::workspace::WorkspaceStatus;
    use ingot_test_support::fixtures::{ItemBuilder, JobBuilder, ProjectBuilder, RevisionBuilder};
    use ingot_test_support::sqlite::temp_db_path;

    use super::*;

    async fn migrated_test_db(prefix: &str) -> Database {
        let path = temp_db_path(prefix);
        let db = Database::connect(&path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        db
    }

    #[tokio::test]
    async fn persisting_terminal_job_does_not_create_busy_current_workspace() {
        let db = migrated_test_db("ingot-store-fixture").await;

        let project = ProjectBuilder::new("/tmp/test")
            .name("Test")
            .build()
            .persist(&db)
            .await
            .expect("create project");
        let revision = RevisionBuilder::new(ingot_domain::ids::ItemId::new()).build();
        let item = ItemBuilder::new(project.id, revision.id)
            .id(revision.item_id)
            .build();
        let (item, revision) = (item, revision)
            .persist(&db)
            .await
            .expect("create item with revision");

        let workspace_id = ingot_domain::ids::WorkspaceId::new();
        let job = JobBuilder::new(project.id, item.id, revision.id, "author_initial")
            .workspace_id(workspace_id)
            .status(JobStatus::Completed)
            .build()
            .persist(&db)
            .await
            .expect("persist terminal job");

        let workspace = db
            .get_workspace(workspace_id)
            .await
            .expect("auto-created workspace");
        assert_eq!(job.state.workspace_id(), Some(workspace.id));
        assert_eq!(workspace.state.status(), WorkspaceStatus::Ready);
        assert_eq!(workspace.state.current_job_id(), None);
    }
}
