use ingot_domain::convergence::{Convergence, ConvergenceState, ConvergenceStatus};
use ingot_domain::ids::{ConvergenceId, ItemId, ItemRevisionId};
use ingot_domain::ports::{ConvergenceRepository, RepositoryError};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use super::helpers::{db_err, db_write_err, encode_enum, parse_enum, parse_id};
use crate::db::Database;

impl Database {
    pub async fn list_convergences_by_item(
        &self,
        item_id: ItemId,
    ) -> Result<Vec<Convergence>, RepositoryError> {
        let rows =
            sqlx::query("SELECT * FROM convergences WHERE item_id = ? ORDER BY created_at DESC")
                .bind(item_id.to_string())
                .fetch_all(&self.pool)
                .await
                .map_err(db_err)?;

        rows.iter().map(map_convergence).collect()
    }

    pub async fn get_convergence(
        &self,
        convergence_id: ConvergenceId,
    ) -> Result<Convergence, RepositoryError> {
        let row = sqlx::query("SELECT * FROM convergences WHERE id = ?")
            .bind(convergence_id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;

        row.as_ref()
            .map(map_convergence)
            .transpose()?
            .ok_or(RepositoryError::NotFound)
    }

    pub async fn list_active_convergences(&self) -> Result<Vec<Convergence>, RepositoryError> {
        let rows = sqlx::query(
            "SELECT *
             FROM convergences
             WHERE status IN ('queued', 'running')
             ORDER BY created_at ASC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;

        rows.iter().map(map_convergence).collect()
    }

    pub async fn create_convergence(
        &self,
        convergence: &Convergence,
    ) -> Result<(), RepositoryError> {
        let state = &convergence.state;

        sqlx::query(
            "INSERT INTO convergences (
                id, project_id, item_id, item_revision_id, source_workspace_id, integration_workspace_id,
                source_head_commit_oid, target_ref, strategy, status, input_target_commit_oid,
                prepared_commit_oid, final_target_commit_oid, conflict_summary, created_at, completed_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(convergence.id.to_string())
        .bind(convergence.project_id.to_string())
        .bind(convergence.item_id.to_string())
        .bind(convergence.item_revision_id.to_string())
        .bind(convergence.source_workspace_id.to_string())
        .bind(state.integration_workspace_id().map(|id| id.to_string()))
        .bind(&convergence.source_head_commit_oid)
        .bind(&convergence.target_ref)
        .bind(encode_enum(&convergence.strategy)?)
        .bind(encode_enum(&state.status())?)
        .bind(state.input_target_commit_oid())
        .bind(state.prepared_commit_oid())
        .bind(state.final_target_commit_oid())
        .bind(state.conflict_summary())
        .bind(convergence.created_at)
        .bind(state.completed_at())
        .execute(&self.pool)
        .await
        .map_err(db_write_err)?;

        Ok(())
    }

    pub async fn update_convergence(
        &self,
        convergence: &Convergence,
    ) -> Result<(), RepositoryError> {
        let state = &convergence.state;

        let result = sqlx::query(
            "UPDATE convergences
             SET integration_workspace_id = ?, source_head_commit_oid = ?, target_ref = ?, strategy = ?,
                 status = ?, input_target_commit_oid = ?, prepared_commit_oid = ?, final_target_commit_oid = ?,
                 conflict_summary = ?, completed_at = ?
             WHERE id = ?",
        )
        .bind(state.integration_workspace_id().map(|id| id.to_string()))
        .bind(&convergence.source_head_commit_oid)
        .bind(&convergence.target_ref)
        .bind(encode_enum(&convergence.strategy)?)
        .bind(encode_enum(&state.status())?)
        .bind(state.input_target_commit_oid())
        .bind(state.prepared_commit_oid())
        .bind(state.final_target_commit_oid())
        .bind(state.conflict_summary())
        .bind(state.completed_at())
        .bind(convergence.id.to_string())
        .execute(&self.pool)
        .await
        .map_err(db_write_err)?;

        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound);
        }

        Ok(())
    }

    pub async fn list_convergences_by_revision(
        &self,
        revision_id: ItemRevisionId,
    ) -> Result<Vec<Convergence>, RepositoryError> {
        let rows = sqlx::query(
            "SELECT * FROM convergences WHERE item_revision_id = ? ORDER BY created_at DESC",
        )
        .bind(revision_id.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;

        rows.iter().map(map_convergence).collect()
    }

    pub async fn find_active_convergence_for_revision(
        &self,
        revision_id: ItemRevisionId,
    ) -> Result<Option<Convergence>, RepositoryError> {
        let row = sqlx::query(
            "SELECT *
             FROM convergences
             WHERE item_revision_id = ?
               AND status IN ('queued', 'running')
             ORDER BY created_at DESC
             LIMIT 1",
        )
        .bind(revision_id.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;

        row.as_ref().map(map_convergence).transpose()
    }

    pub async fn find_prepared_convergence_for_revision(
        &self,
        revision_id: ItemRevisionId,
    ) -> Result<Option<Convergence>, RepositoryError> {
        let row = sqlx::query(
            "SELECT *
             FROM convergences
             WHERE item_revision_id = ?
               AND status = 'prepared'
             ORDER BY created_at DESC
             LIMIT 1",
        )
        .bind(revision_id.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;

        row.as_ref().map(map_convergence).transpose()
    }
}

impl ConvergenceRepository for Database {
    async fn list_by_revision(
        &self,
        revision_id: ItemRevisionId,
    ) -> Result<Vec<Convergence>, RepositoryError> {
        self.list_convergences_by_revision(revision_id).await
    }
    async fn get(&self, id: ConvergenceId) -> Result<Convergence, RepositoryError> {
        self.get_convergence(id).await
    }
    async fn create(&self, convergence: &Convergence) -> Result<(), RepositoryError> {
        self.create_convergence(convergence).await
    }
    async fn update(&self, convergence: &Convergence) -> Result<(), RepositoryError> {
        self.update_convergence(convergence).await
    }
    async fn find_active_for_revision(
        &self,
        revision_id: ItemRevisionId,
    ) -> Result<Option<Convergence>, RepositoryError> {
        self.find_active_convergence_for_revision(revision_id).await
    }
    async fn find_prepared_for_revision(
        &self,
        revision_id: ItemRevisionId,
    ) -> Result<Option<Convergence>, RepositoryError> {
        self.find_prepared_convergence_for_revision(revision_id)
            .await
    }
    async fn list_by_item(&self, item_id: ItemId) -> Result<Vec<Convergence>, RepositoryError> {
        self.list_convergences_by_item(item_id).await
    }
    async fn list_active(&self) -> Result<Vec<Convergence>, RepositoryError> {
        self.list_active_convergences().await
    }
}

fn required_convergence_field<T>(
    field: &'static str,
    status: &str,
    value: Option<T>,
) -> Result<T, RepositoryError> {
    value.ok_or_else(|| {
        RepositoryError::Database(
            format!("convergence {field} is required for status {status}").into(),
        )
    })
}

fn map_convergence(row: &SqliteRow) -> Result<Convergence, RepositoryError> {
    let status: ConvergenceStatus = parse_enum(row.try_get("status").map_err(db_err)?)?;
    let status_str = row.try_get::<String, _>("status").map_err(db_err)?;

    let integration_workspace_id: Option<ingot_domain::ids::WorkspaceId> = row
        .try_get::<Option<String>, _>("integration_workspace_id")
        .map_err(db_err)?
        .map(parse_id)
        .transpose()?;
    let input_target_commit_oid: Option<String> =
        row.try_get("input_target_commit_oid").map_err(db_err)?;
    let prepared_commit_oid: Option<String> = row.try_get("prepared_commit_oid").map_err(db_err)?;
    let final_target_commit_oid: Option<String> =
        row.try_get("final_target_commit_oid").map_err(db_err)?;
    let conflict_summary: Option<String> = row.try_get("conflict_summary").map_err(db_err)?;
    let completed_at: Option<chrono::DateTime<chrono::Utc>> =
        row.try_get("completed_at").map_err(db_err)?;

    let state = match status {
        ConvergenceStatus::Queued => ConvergenceState::Queued,
        ConvergenceStatus::Running => ConvergenceState::Running {
            integration_workspace_id: required_convergence_field(
                "integration_workspace_id",
                &status_str,
                integration_workspace_id,
            )?,
            input_target_commit_oid: required_convergence_field(
                "input_target_commit_oid",
                &status_str,
                input_target_commit_oid,
            )?,
        },
        ConvergenceStatus::Conflicted => ConvergenceState::Conflicted {
            integration_workspace_id: required_convergence_field(
                "integration_workspace_id",
                &status_str,
                integration_workspace_id,
            )?,
            input_target_commit_oid: required_convergence_field(
                "input_target_commit_oid",
                &status_str,
                input_target_commit_oid,
            )?,
            conflict_summary: required_convergence_field(
                "conflict_summary",
                &status_str,
                conflict_summary,
            )?,
            completed_at: required_convergence_field("completed_at", &status_str, completed_at)?,
        },
        ConvergenceStatus::Prepared => ConvergenceState::Prepared {
            integration_workspace_id: required_convergence_field(
                "integration_workspace_id",
                &status_str,
                integration_workspace_id,
            )?,
            input_target_commit_oid: required_convergence_field(
                "input_target_commit_oid",
                &status_str,
                input_target_commit_oid,
            )?,
            prepared_commit_oid: required_convergence_field(
                "prepared_commit_oid",
                &status_str,
                prepared_commit_oid,
            )?,
            completed_at,
        },
        ConvergenceStatus::Finalized => ConvergenceState::Finalized {
            integration_workspace_id,
            input_target_commit_oid: required_convergence_field(
                "input_target_commit_oid",
                &status_str,
                input_target_commit_oid,
            )?,
            prepared_commit_oid: required_convergence_field(
                "prepared_commit_oid",
                &status_str,
                prepared_commit_oid,
            )?,
            final_target_commit_oid: required_convergence_field(
                "final_target_commit_oid",
                &status_str,
                final_target_commit_oid,
            )?,
            completed_at: required_convergence_field("completed_at", &status_str, completed_at)?,
        },
        ConvergenceStatus::Failed => ConvergenceState::Failed {
            integration_workspace_id,
            input_target_commit_oid,
            conflict_summary,
            completed_at: required_convergence_field("completed_at", &status_str, completed_at)?,
        },
        ConvergenceStatus::Cancelled => ConvergenceState::Cancelled {
            integration_workspace_id,
            input_target_commit_oid,
            completed_at: required_convergence_field("completed_at", &status_str, completed_at)?,
        },
    };

    Ok(Convergence {
        id: parse_id(row.try_get("id").map_err(db_err)?)?,
        project_id: parse_id(row.try_get("project_id").map_err(db_err)?)?,
        item_id: parse_id(row.try_get("item_id").map_err(db_err)?)?,
        item_revision_id: parse_id(row.try_get("item_revision_id").map_err(db_err)?)?,
        source_workspace_id: parse_id(row.try_get("source_workspace_id").map_err(db_err)?)?,
        source_head_commit_oid: row.try_get("source_head_commit_oid").map_err(db_err)?,
        target_ref: row.try_get("target_ref").map_err(db_err)?,
        strategy: parse_enum(row.try_get("strategy").map_err(db_err)?)?,
        target_head_valid: None,
        created_at: row.try_get("created_at").map_err(db_err)?,
        state,
    })
}

#[cfg(test)]
mod tests {
    use ingot_domain::ids::{ConvergenceId, ItemId, ItemRevisionId, ProjectId, WorkspaceId};
    use ingot_domain::workspace::WorkspaceKind;
    use ingot_test_support::fixtures::{
        ItemBuilder, ProjectBuilder, RevisionBuilder, WorkspaceBuilder,
    };
    use ingot_test_support::sqlite::temp_db_path;

    use crate::Database;
    use crate::store::test_fixtures::PersistFixture;

    struct ConvergenceTestContext {
        db: Database,
        project_id: ProjectId,
        item_id: ItemId,
        revision_id: ItemRevisionId,
        source_workspace_id: WorkspaceId,
    }

    async fn migrated_test_db(prefix: &str) -> Database {
        let path = temp_db_path(prefix);
        let db = Database::connect(&path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        db
    }

    async fn prepare_test_context(prefix: &str) -> ConvergenceTestContext {
        let db = migrated_test_db(prefix).await;

        let project = ProjectBuilder::new("/tmp/test")
            .name("Test")
            .build()
            .persist(&db)
            .await
            .expect("create project");
        let revision = RevisionBuilder::new(ItemId::new()).build();
        let item = ItemBuilder::new(project.id, revision.id)
            .id(revision.item_id)
            .build();
        let (item, revision) = (item, revision)
            .persist(&db)
            .await
            .expect("create item with revision");
        let source_workspace = WorkspaceBuilder::new(project.id, WorkspaceKind::Integration)
            .created_for_revision_id(revision.id)
            .build()
            .persist(&db)
            .await
            .expect("create source workspace");

        ConvergenceTestContext {
            db,
            project_id: project.id,
            item_id: item.id,
            revision_id: revision.id,
            source_workspace_id: source_workspace.id,
        }
    }

    #[tokio::test]
    async fn prepared_convergence_requires_integration_workspace_in_schema() {
        let ConvergenceTestContext {
            db,
            project_id,
            item_id,
            revision_id,
            source_workspace_id,
        } = prepare_test_context("ingot-store-convergence").await;

        let error = sqlx::query(
            "INSERT INTO convergences (
                id, project_id, item_id, item_revision_id, source_workspace_id, integration_workspace_id,
                source_head_commit_oid, target_ref, strategy, status, input_target_commit_oid,
                prepared_commit_oid
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(ConvergenceId::new().to_string())
        .bind(project_id.to_string())
        .bind(item_id.to_string())
        .bind(revision_id.to_string())
        .bind(source_workspace_id.to_string())
        .bind(Option::<String>::None)
        .bind("head")
        .bind("refs/heads/main")
        .bind("rebase_then_fast_forward")
        .bind("prepared")
        .bind("base")
        .bind("prepared")
        .execute(&db.pool)
        .await
        .expect_err("prepared convergence without integration workspace should fail");

        let message = error.to_string();
        assert!(
            message.contains("CHECK constraint failed"),
            "unexpected error: {message}"
        );
    }

    #[tokio::test]
    async fn queued_convergence_allows_missing_integration_workspace_in_schema() {
        let ConvergenceTestContext {
            db,
            project_id,
            item_id,
            revision_id,
            source_workspace_id,
        } = prepare_test_context("ingot-store-convergence").await;
        let convergence_id = ConvergenceId::new();

        sqlx::query(
            "INSERT INTO convergences (
                id, project_id, item_id, item_revision_id, source_workspace_id, integration_workspace_id,
                source_head_commit_oid, target_ref, strategy, status
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(convergence_id.to_string())
        .bind(project_id.to_string())
        .bind(item_id.to_string())
        .bind(revision_id.to_string())
        .bind(source_workspace_id.to_string())
        .bind(Option::<String>::None)
        .bind("head")
        .bind("refs/heads/main")
        .bind("rebase_then_fast_forward")
        .bind("queued")
        .execute(&db.pool)
        .await
        .expect("queued convergence without integration workspace should persist");

        let convergence = db
            .get_convergence(convergence_id)
            .await
            .expect("load queued convergence");
        assert_eq!(convergence.state.integration_workspace_id(), None);
    }
}
