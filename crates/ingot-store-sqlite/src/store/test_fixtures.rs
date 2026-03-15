use ingot_domain::convergence::Convergence;
use ingot_domain::finding::Finding;
use ingot_domain::item::Item;
use ingot_domain::job::Job;
use ingot_domain::ports::RepositoryError;
use ingot_domain::project::Project;
use ingot_domain::revision::ItemRevision;
use ingot_domain::workspace::Workspace;

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
