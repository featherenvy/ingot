use ingot_domain::commit_oid::CommitOid;
use ingot_domain::ports::{
    InvalidatePreparedConvergenceMutation, InvalidatePreparedConvergenceRepository, RepositoryError,
};

use super::helpers::{db_err, db_write_err, encode_enum, json_err};
use crate::db::Database;

impl Database {
    pub async fn apply_invalidate_prepared_convergence(
        &self,
        mutation: InvalidatePreparedConvergenceMutation,
    ) -> Result<(), RepositoryError> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;

        // 1. Update convergence (mark as failed)
        let convergence = &mutation.convergence;
        let state = &convergence.state;
        let result = sqlx::query(
            "UPDATE convergences
             SET integration_workspace_id = ?, source_head_commit_oid = ?, target_ref = ?,
                 strategy = ?, status = ?, input_target_commit_oid = ?,
                 prepared_commit_oid = ?, final_target_commit_oid = ?,
                 conflict_summary = ?, completed_at = ?
             WHERE id = ?",
        )
        .bind(state.integration_workspace_id().map(|id| id.to_string()))
        .bind(convergence.source_head_commit_oid.as_str())
        .bind(&convergence.target_ref)
        .bind(encode_enum(&convergence.strategy)?)
        .bind(encode_enum(&state.status())?)
        .bind(state.input_target_commit_oid().map(CommitOid::as_str))
        .bind(state.prepared_commit_oid().map(CommitOid::as_str))
        .bind(state.final_target_commit_oid().map(CommitOid::as_str))
        .bind(state.conflict_summary())
        .bind(state.completed_at())
        .bind(convergence.id.to_string())
        .execute(&mut *tx)
        .await
        .map_err(db_write_err)?;

        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound);
        }

        // 2. Update workspace (mark as stale) if present
        if let Some(workspace) = &mutation.workspace_update {
            let result = sqlx::query(
                "UPDATE workspaces
                 SET path = ?, target_ref = ?, workspace_ref = ?, base_commit_oid = ?,
                     head_commit_oid = ?, retention_policy = ?, status = ?,
                     current_job_id = ?, updated_at = ?
                 WHERE id = ?",
            )
            .bind(&workspace.path)
            .bind(workspace.target_ref.as_deref())
            .bind(workspace.workspace_ref.as_deref())
            .bind(workspace.state.base_commit_oid().map(CommitOid::as_str))
            .bind(workspace.state.head_commit_oid().map(CommitOid::as_str))
            .bind(encode_enum(&workspace.retention_policy)?)
            .bind(encode_enum(&workspace.state.status())?)
            .bind(workspace.state.current_job_id().map(|id| id.to_string()))
            .bind(workspace.updated_at)
            .bind(workspace.id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_write_err)?;

            if result.rows_affected() == 0 {
                return Err(RepositoryError::NotFound);
            }
        }

        // 3. Update item (reset approval state)
        let item = &mutation.item;
        let result = sqlx::query(
            "UPDATE items
             SET approval_state = ?, updated_at = ?
             WHERE id = ?",
        )
        .bind(encode_enum(&item.approval_state)?)
        .bind(item.updated_at)
        .bind(item.id.to_string())
        .execute(&mut *tx)
        .await
        .map_err(db_write_err)?;

        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound);
        }

        // 4. Append activity
        let activity = &mutation.activity;
        sqlx::query(
            "INSERT INTO activity (
                id, project_id, event_type, entity_type, entity_id, payload, created_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(activity.id.to_string())
        .bind(activity.project_id.to_string())
        .bind(encode_enum(&activity.event_type)?)
        .bind(encode_enum(&activity.entity_type)?)
        .bind(&activity.entity_id)
        .bind(serde_json::to_string(&activity.payload).map_err(json_err)?)
        .bind(activity.created_at)
        .execute(&mut *tx)
        .await
        .map_err(db_write_err)?;

        tx.commit().await.map_err(db_err)?;
        Ok(())
    }
}

impl InvalidatePreparedConvergenceRepository for Database {
    async fn apply_invalidate_prepared_convergence(
        &self,
        mutation: InvalidatePreparedConvergenceMutation,
    ) -> Result<(), RepositoryError> {
        Database::apply_invalidate_prepared_convergence(self, mutation).await
    }
}

#[cfg(test)]
mod tests {
    use ingot_domain::convergence::ConvergenceStatus;
    use ingot_domain::ids::{ActivityId, ItemId};
    use ingot_domain::item::ApprovalState;
    use ingot_domain::ports::{InvalidatePreparedConvergenceMutation, RepositoryError};
    use ingot_domain::workspace::{WorkspaceKind, WorkspaceStatus};
    use ingot_test_support::fixtures::{
        ConvergenceBuilder, ItemBuilder, ProjectBuilder, RevisionBuilder, WorkspaceBuilder,
    };
    use ingot_test_support::sqlite::temp_db_path;

    use crate::Database;
    use crate::store::test_fixtures::PersistFixture;

    async fn migrated_test_db(prefix: &str) -> Database {
        let path = temp_db_path(prefix);
        let db = Database::connect(&path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        db
    }

    #[tokio::test]
    async fn apply_invalidation_fails_convergence_and_stales_workspace_atomically() {
        let db = migrated_test_db("invalidate-atomic").await;

        let project = ProjectBuilder::new("/tmp/test")
            .name("Test")
            .build()
            .persist(&db)
            .await
            .expect("create project");
        let revision = RevisionBuilder::new(ItemId::new())
            .seed_commit_oid(Some("abc"))
            .seed_target_commit_oid(Some("def"))
            .build();
        let item = ItemBuilder::new(project.id, revision.id)
            .id(revision.item_id)
            .approval_state(ApprovalState::Pending)
            .build();
        let (item, revision) = (item, revision)
            .persist(&db)
            .await
            .expect("create item with revision");

        let workspace = WorkspaceBuilder::new(project.id, WorkspaceKind::Integration)
            .created_for_revision_id(revision.id)
            .status(WorkspaceStatus::Ready)
            .build()
            .persist(&db)
            .await
            .expect("create workspace");

        let convergence = ConvergenceBuilder::new(project.id, item.id, revision.id)
            .source_workspace_id(workspace.id)
            .integration_workspace_id(workspace.id)
            .source_head_commit_oid("head")
            .status(ConvergenceStatus::Prepared)
            .input_target_commit_oid("target")
            .prepared_commit_oid("prepared")
            .build()
            .persist(&db)
            .await
            .expect("create convergence");

        // Build the mutation in memory
        let mut failed_convergence = convergence.clone();
        failed_convergence
            .transition_to_failed(Some("target_ref_moved".into()), chrono::Utc::now());

        let mut stale_workspace = db.get_workspace(workspace.id).await.expect("get workspace");
        stale_workspace.mark_stale(chrono::Utc::now());

        let mut updated_item = db.get_item(item.id).await.expect("get item");
        updated_item.approval_state = ApprovalState::NotRequested;
        updated_item.updated_at = chrono::Utc::now();

        let mutation = InvalidatePreparedConvergenceMutation {
            convergence: failed_convergence,
            workspace_update: Some(stale_workspace),
            item: updated_item,
            activity: ingot_domain::activity::Activity {
                id: ActivityId::new(),
                project_id: project.id,
                event_type: ingot_domain::activity::ActivityEventType::ConvergenceFailed,
                entity_type: ingot_domain::activity::ActivityEntityType::Convergence,
                entity_id: convergence.id.to_string(),
                payload: serde_json::json!({ "item_id": item.id, "reason": "target_ref_moved" }),
                created_at: chrono::Utc::now(),
            },
        };

        db.apply_invalidate_prepared_convergence(mutation)
            .await
            .expect("apply invalidation");

        let persisted_convergence = db
            .get_convergence(convergence.id)
            .await
            .expect("load convergence");
        assert_eq!(
            persisted_convergence.state.status(),
            ConvergenceStatus::Failed
        );

        let persisted_workspace = db
            .get_workspace(workspace.id)
            .await
            .expect("load workspace");
        assert_eq!(persisted_workspace.state.status(), WorkspaceStatus::Stale);

        let persisted_item = db.get_item(item.id).await.expect("load item");
        assert_eq!(persisted_item.approval_state, ApprovalState::NotRequested);
    }

    #[tokio::test]
    async fn apply_invalidation_rolls_back_on_missing_convergence() {
        let db = migrated_test_db("invalidate-rollback").await;

        let project = ProjectBuilder::new("/tmp/test")
            .name("Test")
            .build()
            .persist(&db)
            .await
            .expect("create project");
        let revision = RevisionBuilder::new(ItemId::new())
            .seed_commit_oid(Some("abc"))
            .seed_target_commit_oid(Some("def"))
            .build();
        let item = ItemBuilder::new(project.id, revision.id)
            .id(revision.item_id)
            .approval_state(ApprovalState::Pending)
            .build();
        let (item, _revision) = (item, revision)
            .persist(&db)
            .await
            .expect("create item with revision");

        // Build a mutation with a non-existent convergence ID
        let fake_convergence = ConvergenceBuilder::new(
            project.id,
            item.id,
            ingot_domain::ids::ItemRevisionId::new(),
        )
        .status(ConvergenceStatus::Failed)
        .build();

        let mut updated_item = db.get_item(item.id).await.expect("get item");
        updated_item.approval_state = ApprovalState::NotRequested;
        updated_item.updated_at = chrono::Utc::now();

        let mutation = InvalidatePreparedConvergenceMutation {
            convergence: fake_convergence,
            workspace_update: None,
            item: updated_item,
            activity: ingot_domain::activity::Activity {
                id: ActivityId::new(),
                project_id: project.id,
                event_type: ingot_domain::activity::ActivityEventType::ConvergenceFailed,
                entity_type: ingot_domain::activity::ActivityEntityType::Convergence,
                entity_id: "fake".into(),
                payload: serde_json::json!({}),
                created_at: chrono::Utc::now(),
            },
        };

        let error = db
            .apply_invalidate_prepared_convergence(mutation)
            .await
            .expect_err("missing convergence should fail");

        assert!(matches!(error, RepositoryError::NotFound));

        // Item should still have Pending approval (rollback)
        let persisted_item = db.get_item(item.id).await.expect("load item");
        assert_eq!(persisted_item.approval_state, ApprovalState::Pending);
    }
}
