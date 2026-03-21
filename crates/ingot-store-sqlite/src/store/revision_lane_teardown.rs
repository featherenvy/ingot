use chrono::Utc;
use ingot_domain::git_operation::GitOperationWire;
use ingot_domain::ports::{
    RepositoryError, RevisionLaneTeardownMutation, RevisionLaneTeardownRepository,
};

use super::helpers::{db_err, db_write_err, encode_enum, item_revision_is_stale, json_err};
use crate::db::Database;

impl Database {
    pub async fn apply_revision_lane_teardown(
        &self,
        mutation: RevisionLaneTeardownMutation,
    ) -> Result<(), RepositoryError> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;

        // 1. Job cancellations
        for cancellation in &mutation.job_cancellations {
            let params = &cancellation.params;
            let result = sqlx::query(
                "UPDATE jobs
                 SET status = ?,
                     outcome_class = ?,
                     result_schema_version = NULL,
                     result_payload = NULL,
                     output_commit_oid = NULL,
                     error_code = ?,
                     error_message = ?,
                     ended_at = ?
                 WHERE id = ?
                   AND status IN ('queued', 'assigned', 'running')
                   AND EXISTS (
                       SELECT 1
                       FROM items
                       WHERE id = ?
                         AND current_revision_id = ?
                   )",
            )
            .bind(encode_enum(&params.status)?)
            .bind(params.outcome_class.as_ref().map(encode_enum).transpose()?)
            .bind(params.error_code.as_deref())
            .bind(params.error_message.as_deref())
            .bind(Utc::now())
            .bind(params.job_id.to_string())
            .bind(params.item_id.to_string())
            .bind(params.expected_item_revision_id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;

            if result.rows_affected() != 1 {
                if item_revision_is_stale(&mut tx, params.item_id, params.expected_item_revision_id)
                    .await?
                {
                    return Err(RepositoryError::Conflict("job_revision_stale".into()));
                }

                let job_is_active: Option<String> = sqlx::query_scalar(
                    "SELECT id FROM jobs WHERE id = ? AND status IN ('queued', 'assigned', 'running')",
                )
                .bind(params.job_id.to_string())
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_err)?;

                if job_is_active.is_none() {
                    return Err(RepositoryError::Conflict("job_not_active".into()));
                }

                return Err(RepositoryError::Conflict("job_update_conflict".into()));
            }

            // Update workspace if present
            if let Some(workspace) = &cancellation.workspace_update {
                sqlx::query(
                    "UPDATE workspaces
                     SET path = ?, target_ref = ?, workspace_ref = ?, base_commit_oid = ?,
                         head_commit_oid = ?, retention_policy = ?, status = ?,
                         current_job_id = ?, updated_at = ?
                     WHERE id = ?",
                )
                .bind(&workspace.path)
                .bind(workspace.target_ref.clone())
                .bind(workspace.workspace_ref.clone())
                .bind(workspace.state.base_commit_oid().cloned())
                .bind(workspace.state.head_commit_oid().cloned())
                .bind(encode_enum(&workspace.retention_policy)?)
                .bind(encode_enum(&workspace.state.status())?)
                .bind(workspace.state.current_job_id().map(|id| id.to_string()))
                .bind(workspace.updated_at)
                .bind(workspace.id.to_string())
                .execute(&mut *tx)
                .await
                .map_err(db_write_err)?;
            }

            // Insert activity
            let activity = &cancellation.activity;
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
        }

        // 2. Convergence updates
        for convergence in &mutation.convergence_updates {
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
            .bind(convergence.source_head_commit_oid.clone())
            .bind(&convergence.target_ref)
            .bind(encode_enum(&convergence.strategy)?)
            .bind(encode_enum(&state.status())?)
            .bind(state.input_target_commit_oid().cloned())
            .bind(state.prepared_commit_oid().cloned())
            .bind(state.final_target_commit_oid().cloned())
            .bind(state.conflict_summary())
            .bind(state.completed_at())
            .bind(convergence.id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_write_err)?;

            if result.rows_affected() == 0 {
                return Err(RepositoryError::NotFound);
            }
        }

        // 3. Workspace abandonments
        for workspace in &mutation.workspace_abandonments {
            let result = sqlx::query(
                "UPDATE workspaces
                 SET path = ?, target_ref = ?, workspace_ref = ?, base_commit_oid = ?,
                     head_commit_oid = ?, retention_policy = ?, status = ?,
                     current_job_id = ?, updated_at = ?
                 WHERE id = ?",
            )
            .bind(&workspace.path)
            .bind(workspace.target_ref.clone())
            .bind(workspace.workspace_ref.clone())
            .bind(workspace.state.base_commit_oid().cloned())
            .bind(workspace.state.head_commit_oid().cloned())
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

        // 4. Queue entry update
        if let Some(queue_entry) = &mutation.queue_entry_update {
            let result = sqlx::query(
                "UPDATE convergence_queue_entries
                 SET status = ?, head_acquired_at = ?, updated_at = ?, released_at = ?
                 WHERE id = ?",
            )
            .bind(encode_enum(&queue_entry.status)?)
            .bind(queue_entry.head_acquired_at)
            .bind(queue_entry.updated_at)
            .bind(queue_entry.released_at)
            .bind(queue_entry.id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_write_err)?;

            if result.rows_affected() == 0 {
                return Err(RepositoryError::NotFound);
            }
        }

        // 5. Git operation updates
        for operation in &mutation.git_operation_updates {
            let wire = GitOperationWire::from(operation);
            let result = sqlx::query(
                "UPDATE git_operations
                 SET workspace_id = ?, ref_name = ?, expected_old_oid = ?, new_oid = ?,
                     commit_oid = ?, status = ?, metadata = ?, completed_at = ?
                 WHERE id = ?",
            )
            .bind(wire.workspace_id.map(|id| id.to_string()))
            .bind(wire.ref_name.clone())
            .bind(wire.expected_old_oid.clone())
            .bind(wire.new_oid.clone())
            .bind(wire.commit_oid.clone())
            .bind(encode_enum(&wire.status)?)
            .bind(
                wire.metadata
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()
                    .map_err(json_err)?,
            )
            .bind(wire.completed_at)
            .bind(wire.id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_write_err)?;

            if result.rows_affected() == 0 {
                return Err(RepositoryError::NotFound);
            }
        }

        // 6. Git operation activities
        for activity in &mutation.git_operation_activities {
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
        }

        tx.commit().await.map_err(db_err)?;
        Ok(())
    }
}

impl RevisionLaneTeardownRepository for Database {
    async fn apply_revision_lane_teardown(
        &self,
        mutation: RevisionLaneTeardownMutation,
    ) -> Result<(), RepositoryError> {
        Database::apply_revision_lane_teardown(self, mutation).await
    }
}

#[cfg(test)]
mod tests {
    use ingot_domain::ids::{ActivityId, ItemId, ItemRevisionId};
    use ingot_domain::job::{JobStatus, OutcomeClass};
    use ingot_domain::ports::{
        FinishJobNonSuccessParams, RepositoryError, RevisionLaneTeardownMutation,
        TeardownJobCancellation,
    };
    use ingot_domain::workspace::{WorkspaceKind, WorkspaceStatus};
    use ingot_test_support::fixtures::{
        ItemBuilder, JobBuilder, ProjectBuilder, RevisionBuilder, WorkspaceBuilder,
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
    async fn apply_teardown_cancels_job_and_updates_workspace_atomically() {
        let db = migrated_test_db("teardown-atomic").await;

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
            .build();
        let (item, revision) = (item, revision)
            .persist(&db)
            .await
            .expect("create item with revision");

        let job_id = ingot_domain::ids::JobId::new();
        let workspace = WorkspaceBuilder::new(project.id, WorkspaceKind::Authoring)
            .created_for_revision_id(revision.id)
            .status(WorkspaceStatus::Busy)
            .current_job_id(job_id)
            .build()
            .persist(&db)
            .await
            .expect("create workspace");

        let job = JobBuilder::new(project.id, item.id, revision.id, "author_initial")
            .id(job_id)
            .status(JobStatus::Running)
            .workspace_id(workspace.id)
            .build()
            .persist(&db)
            .await
            .expect("create job");

        let mut released_workspace = db.get_workspace(workspace.id).await.expect("get workspace");
        released_workspace.release_to(WorkspaceStatus::Ready, chrono::Utc::now());

        let activity = ingot_domain::activity::Activity {
            id: ActivityId::new(),
            project_id: project.id,
            event_type: ingot_domain::activity::ActivityEventType::JobCancelled,
            entity_type: ingot_domain::activity::ActivityEntityType::Job,
            entity_id: job.id.to_string(),
            payload: serde_json::json!({ "item_id": item.id }),
            created_at: chrono::Utc::now(),
        };

        let mutation = RevisionLaneTeardownMutation {
            job_cancellations: vec![TeardownJobCancellation {
                params: FinishJobNonSuccessParams {
                    job_id: job.id,
                    item_id: item.id,
                    expected_item_revision_id: revision.id,
                    status: JobStatus::Cancelled,
                    outcome_class: Some(OutcomeClass::Cancelled),
                    error_code: Some("item_mutation_cancelled".into()),
                    error_message: None,
                    escalation_reason: None,
                },
                workspace_update: Some(released_workspace),
                activity,
            }],
            ..Default::default()
        };

        db.apply_revision_lane_teardown(mutation)
            .await
            .expect("apply teardown");

        let persisted_job = db.get_job(job.id).await.expect("load job");
        assert_eq!(persisted_job.state.status(), JobStatus::Cancelled);

        let persisted_workspace = db
            .get_workspace(workspace.id)
            .await
            .expect("load workspace");
        assert_eq!(persisted_workspace.state.status(), WorkspaceStatus::Ready);
        assert_eq!(persisted_workspace.state.current_job_id(), None);
    }

    #[tokio::test]
    async fn apply_teardown_rolls_back_on_stale_revision() {
        let db = migrated_test_db("teardown-rollback").await;

        let project = ProjectBuilder::new("/tmp/test")
            .name("Test")
            .build()
            .persist(&db)
            .await
            .expect("create project");

        let item_id = ItemId::new();
        let revision = RevisionBuilder::new(item_id)
            .seed_commit_oid(Some("abc"))
            .seed_target_commit_oid(Some("def"))
            .build();
        let mut next_revision = RevisionBuilder::new(item_id)
            .id(ItemRevisionId::new())
            .revision_no(2)
            .seed_commit_oid(Some("ghi"))
            .seed_target_commit_oid(Some("jkl"))
            .build();
        next_revision.supersedes_revision_id = Some(revision.id);

        // Item points to next_revision but job points to original revision
        let item = ItemBuilder::new(project.id, next_revision.id)
            .id(item_id)
            .build();
        let (item, revision) = (item, revision)
            .persist(&db)
            .await
            .expect("create item with source revision");
        let _next_revision = next_revision
            .persist(&db)
            .await
            .expect("create next revision");

        let job_id = ingot_domain::ids::JobId::new();
        let workspace = WorkspaceBuilder::new(project.id, WorkspaceKind::Authoring)
            .created_for_revision_id(revision.id)
            .status(WorkspaceStatus::Busy)
            .current_job_id(job_id)
            .build()
            .persist(&db)
            .await
            .expect("create workspace");

        let job = JobBuilder::new(project.id, item.id, revision.id, "author_initial")
            .id(job_id)
            .status(JobStatus::Running)
            .workspace_id(workspace.id)
            .build()
            .persist(&db)
            .await
            .expect("create job");

        let mut released_workspace = db.get_workspace(workspace.id).await.expect("get workspace");
        released_workspace.release_to(WorkspaceStatus::Ready, chrono::Utc::now());

        let mutation = RevisionLaneTeardownMutation {
            job_cancellations: vec![TeardownJobCancellation {
                params: FinishJobNonSuccessParams {
                    job_id: job.id,
                    item_id: item.id,
                    expected_item_revision_id: revision.id, // stale!
                    status: JobStatus::Cancelled,
                    outcome_class: Some(OutcomeClass::Cancelled),
                    error_code: Some("item_mutation_cancelled".into()),
                    error_message: None,
                    escalation_reason: None,
                },
                workspace_update: Some(released_workspace),
                activity: ingot_domain::activity::Activity {
                    id: ActivityId::new(),
                    project_id: project.id,
                    event_type: ingot_domain::activity::ActivityEventType::JobCancelled,
                    entity_type: ingot_domain::activity::ActivityEntityType::Job,
                    entity_id: job.id.to_string(),
                    payload: serde_json::json!({}),
                    created_at: chrono::Utc::now(),
                },
            }],
            ..Default::default()
        };

        let error = db
            .apply_revision_lane_teardown(mutation)
            .await
            .expect_err("stale revision should fail");

        assert!(matches!(
            error,
            RepositoryError::Conflict(message) if message == "job_revision_stale"
        ));

        // Verify rollback: workspace should still be Busy
        let persisted_workspace = db
            .get_workspace(workspace.id)
            .await
            .expect("load workspace");
        assert_eq!(persisted_workspace.state.status(), WorkspaceStatus::Busy);

        // Job should still be Running
        let persisted_job = db.get_job(job.id).await.expect("load job");
        assert_eq!(persisted_job.state.status(), JobStatus::Running);
    }
}
