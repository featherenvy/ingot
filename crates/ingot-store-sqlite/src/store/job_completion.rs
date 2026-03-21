use chrono::Utc;
use ingot_domain::commit_oid::CommitOid;
use ingot_domain::git_ref::GitRef;
use ingot_domain::ids::JobId;
use ingot_domain::item::Escalation;
use ingot_domain::job::JobStatus;
use ingot_domain::ports::{
    CompletedJobCompletion, JobCompletionContext, JobCompletionMutation, JobCompletionRepository,
    RepositoryError,
};
use sqlx::Row;
use sqlx::{Sqlite, Transaction};

use super::finding::upsert_finding;
use super::helpers::{db_err, encode_enum, item_revision_is_stale, serialize_optional_json};
use crate::db::Database;

impl Database {
    pub async fn load_job_completion_context(
        &self,
        job_id: JobId,
    ) -> Result<JobCompletionContext, RepositoryError> {
        let job = self.get_job(job_id).await?;
        let item = self.get_item(job.item_id).await?;
        let project = self.get_project(item.project_id).await?;
        let revision = self.get_revision(item.current_revision_id).await?;
        let convergences = self.list_convergences_by_item(item.id).await?;

        Ok(JobCompletionContext {
            job,
            item,
            project,
            revision,
            convergences,
        })
    }

    pub async fn load_completed_job_completion(
        &self,
        job_id: JobId,
    ) -> Result<Option<CompletedJobCompletion>, RepositoryError> {
        let job = self.get_job(job_id).await?;
        if job.state.status() != JobStatus::Completed {
            return Ok(None);
        }

        let finding_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM findings WHERE source_job_id = ?")
                .bind(job_id.to_string())
                .fetch_one(&self.pool)
                .await
                .map_err(db_err)?;

        Ok(Some(CompletedJobCompletion {
            job,
            finding_count: finding_count
                .try_into()
                .expect("finding count should fit into usize"),
        }))
    }

    pub async fn apply_job_completion(
        &self,
        mutation: JobCompletionMutation,
    ) -> Result<(), RepositoryError> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let serialized_result_payload = serialize_optional_json(mutation.result_payload.as_ref())?;

        let result = if let Some(prepared_convergence_guard) =
            mutation.prepared_convergence_guard.as_ref()
        {
            sqlx::query(
                "UPDATE jobs
                 SET status = 'completed',
                     outcome_class = ?,
                     result_schema_version = ?,
                     result_payload = ?,
                     output_commit_oid = ?,
                     ended_at = ?
                 WHERE id = ?
                   AND status IN ('queued', 'assigned', 'running')
                   AND EXISTS (
                       SELECT 1
                       FROM items
                       WHERE id = ?
                         AND current_revision_id = ?
                   )
                   AND EXISTS (
                       SELECT 1
                       FROM convergences
                       WHERE id = ?
                         AND item_revision_id = ?
                         AND status = 'prepared'
                         AND target_ref = ?
                         AND input_target_commit_oid = ?
                   )",
            )
            .bind(encode_enum(&mutation.outcome_class)?)
            .bind(mutation.result_schema_version.as_deref())
            .bind(&serialized_result_payload)
            .bind(mutation.output_commit_oid.clone())
            .bind(Utc::now())
            .bind(mutation.job_id.to_string())
            .bind(mutation.item_id.to_string())
            .bind(mutation.expected_item_revision_id.to_string())
            .bind(prepared_convergence_guard.convergence_id.to_string())
            .bind(prepared_convergence_guard.item_revision_id.to_string())
            .bind(&prepared_convergence_guard.target_ref)
            .bind(prepared_convergence_guard.expected_target_head_oid.clone())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?
        } else {
            sqlx::query(
                "UPDATE jobs
                 SET status = 'completed',
                     outcome_class = ?,
                     result_schema_version = ?,
                     result_payload = ?,
                     output_commit_oid = ?,
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
            .bind(encode_enum(&mutation.outcome_class)?)
            .bind(mutation.result_schema_version.as_deref())
            .bind(&serialized_result_payload)
            .bind(mutation.output_commit_oid.clone())
            .bind(Utc::now())
            .bind(mutation.job_id.to_string())
            .bind(mutation.item_id.to_string())
            .bind(mutation.expected_item_revision_id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?
        };

        if result.rows_affected() != 1 {
            return Err(classify_job_completion_conflict(&mut tx, &mutation).await?);
        }

        for finding in &mutation.findings {
            upsert_finding(&mut tx, finding).await?;
        }

        if mutation.clear_item_escalation {
            let escalation = sqlx::query(
                "UPDATE items
                 SET escalation_state = ?, escalation_reason = NULL, updated_at = ?
                 WHERE id = ?
                   AND current_revision_id = ?",
            )
            .bind(Escalation::None.as_db_str())
            .bind(Utc::now())
            .bind(mutation.item_id.to_string())
            .bind(mutation.expected_item_revision_id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;

            if escalation.rows_affected() != 1 {
                return Err(RepositoryError::Conflict("job_revision_stale".into()));
            }
        }

        if let Some(prepared_convergence_guard) = mutation.prepared_convergence_guard.as_ref() {
            if let Some(approval_state) = prepared_convergence_guard.next_approval_state.as_ref() {
                let approval = sqlx::query(
                    "UPDATE items
                     SET approval_state = ?, updated_at = ?
                     WHERE id = ?
                       AND current_revision_id = ?",
                )
                .bind(encode_enum(approval_state)?)
                .bind(Utc::now())
                .bind(mutation.item_id.to_string())
                .bind(mutation.expected_item_revision_id.to_string())
                .execute(&mut *tx)
                .await
                .map_err(db_err)?;

                if approval.rows_affected() != 1 {
                    return Err(RepositoryError::Conflict("job_revision_stale".into()));
                }
            }
        }

        tx.commit().await.map_err(db_err)?;
        Ok(())
    }
}

impl JobCompletionRepository for Database {
    async fn load_job_completion_context(
        &self,
        job_id: JobId,
    ) -> Result<JobCompletionContext, RepositoryError> {
        Database::load_job_completion_context(self, job_id).await
    }

    async fn load_completed_job_completion(
        &self,
        job_id: JobId,
    ) -> Result<Option<CompletedJobCompletion>, RepositoryError> {
        Database::load_completed_job_completion(self, job_id).await
    }

    async fn apply_job_completion(
        &self,
        mutation: JobCompletionMutation,
    ) -> Result<(), RepositoryError> {
        Database::apply_job_completion(self, mutation).await
    }
}

async fn classify_job_completion_conflict(
    tx: &mut Transaction<'_, Sqlite>,
    mutation: &JobCompletionMutation,
) -> Result<RepositoryError, RepositoryError> {
    if item_revision_is_stale(tx, mutation.item_id, mutation.expected_item_revision_id).await? {
        return Ok(RepositoryError::Conflict("job_revision_stale".into()));
    }

    if let Some(prepared_convergence_guard) = mutation.prepared_convergence_guard.as_ref() {
        let prepared_convergence = sqlx::query(
            "SELECT id, target_ref, input_target_commit_oid
             FROM convergences
             WHERE item_revision_id = ?
               AND status = 'prepared'
             ORDER BY created_at DESC
             LIMIT 1",
        )
        .bind(prepared_convergence_guard.item_revision_id.to_string())
        .fetch_optional(&mut **tx)
        .await
        .map_err(db_err)?;

        let Some(prepared_convergence) = prepared_convergence else {
            return Ok(RepositoryError::Conflict(
                "prepared_convergence_missing".into(),
            ));
        };

        let prepared_convergence_id: String = prepared_convergence.try_get("id").map_err(db_err)?;
        let prepared_target_ref: GitRef = prepared_convergence.try_get("target_ref").map_err(db_err)?;
        let input_target_commit_oid: Option<CommitOid> = prepared_convergence
            .try_get("input_target_commit_oid")
            .map_err(db_err)?;
        if prepared_convergence_id != prepared_convergence_guard.convergence_id.to_string()
            || prepared_target_ref != prepared_convergence_guard.target_ref
            || input_target_commit_oid.as_ref()
                != Some(&prepared_convergence_guard.expected_target_head_oid)
        {
            return Ok(RepositoryError::Conflict(
                "prepared_convergence_stale".into(),
            ));
        }
    }

    Ok(RepositoryError::Conflict("job_not_active".into()))
}

#[cfg(test)]
mod tests {
    use ingot_domain::commit_oid::CommitOid;
    use ingot_domain::convergence::{Convergence, ConvergenceStatus};
    use ingot_domain::ids::{ItemId, ItemRevisionId, ProjectId, WorkspaceId};
    use ingot_domain::item::{ApprovalState, Classification, Item, Origin};
    use ingot_domain::job::{
        ContextPolicy, ExecutionPermission, Job, JobInput, JobStatus, OutcomeClass,
        OutputArtifactKind, PhaseKind,
    };
    use ingot_domain::ports::{JobCompletionMutation, PreparedConvergenceGuard, RepositoryError};
    use ingot_domain::project::Project;
    use ingot_domain::revision::ItemRevision;
    use ingot_domain::workspace::{RetentionPolicy, Workspace, WorkspaceKind};
    use ingot_test_support::fixtures::{
        ConvergenceBuilder, FindingBuilder, ItemBuilder, JobBuilder, ProjectBuilder,
        RevisionBuilder, WorkspaceBuilder, default_timestamp, parse_timestamp,
    };
    use ingot_test_support::reports::clean_validation_report;
    use ingot_test_support::sqlite::temp_db_path;

    use crate::Database;
    use crate::store::test_fixtures::PersistFixture;

    async fn migrated_test_db(prefix: &str) -> Database {
        let path = temp_db_path(prefix);
        let db = Database::connect(&path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        db
    }

    async fn persist_project(db: &Database) -> Project {
        ProjectBuilder::new("/tmp/test")
            .name("Test")
            .build()
            .persist(db)
            .await
            .expect("create project")
    }

    async fn persist_item_with_revision(
        db: &Database,
        project_id: ProjectId,
        target_ref: &str,
    ) -> (Item, ItemRevision) {
        let item_id = ItemId::new();
        let mut revision = RevisionBuilder::new(item_id)
            .seed_commit_oid(Some("abc"))
            .seed_target_commit_oid(Some("def"))
            .build();
        revision.target_ref = target_ref.into();

        let item = ItemBuilder::new(project_id, revision.id)
            .id(item_id)
            .build();
        (item, revision)
            .persist(db)
            .await
            .expect("create item with revision")
    }

    async fn persist_promoted_finding_item(
        db: &Database,
        project_id: ProjectId,
        finding_id: ingot_domain::ids::FindingId,
    ) -> (Item, ItemRevision) {
        let item_id = ItemId::new();
        let revision = RevisionBuilder::new(item_id)
            .seed_commit_oid(Some("head"))
            .seed_target_commit_oid(Some("def"))
            .build();

        let mut item = ItemBuilder::new(project_id, revision.id)
            .id(item_id)
            .build();
        item.classification = Classification::Bug;
        item.origin = Origin::PromotedFinding { finding_id };

        (item, revision)
            .persist(db)
            .await
            .expect("create promoted item")
    }

    async fn persist_investigate_job(
        db: &Database,
        project_id: ProjectId,
        item_id: ItemId,
        revision_id: ItemRevisionId,
        status: JobStatus,
    ) -> Job {
        JobBuilder::new(project_id, item_id, revision_id, "investigate_item")
            .status(status)
            .phase_kind(PhaseKind::Investigate)
            .workspace_kind(WorkspaceKind::Review)
            .execution_permission(ExecutionPermission::MustNotMutate)
            .context_policy(ContextPolicy::Fresh)
            .phase_template_slug("investigate-item")
            .output_artifact_kind(OutputArtifactKind::FindingReport)
            .build()
            .persist(db)
            .await
            .expect("create investigate job")
    }

    async fn persist_integration_workspace(
        db: &Database,
        project_id: ProjectId,
        revision_id: ItemRevisionId,
    ) -> Workspace {
        let mut workspace = WorkspaceBuilder::new(project_id, WorkspaceKind::Integration)
            .created_for_revision_id(revision_id)
            .path("/tmp/workspace")
            .build();
        workspace.retention_policy = RetentionPolicy::Ephemeral;
        workspace.target_ref = None;
        workspace.workspace_ref = None;

        workspace
            .persist(db)
            .await
            .expect("create integration workspace")
    }

    async fn persist_validate_job(
        db: &Database,
        project_id: ProjectId,
        item_id: ItemId,
        revision_id: ItemRevisionId,
        expected_target_head_oid: &str,
        prepared_head_oid: &str,
    ) -> Job {
        JobBuilder::new(project_id, item_id, revision_id, "validate_integrated")
            .status(JobStatus::Running)
            .phase_kind(PhaseKind::Validate)
            .workspace_kind(WorkspaceKind::Integration)
            .execution_permission(ExecutionPermission::MustNotMutate)
            .context_policy(ContextPolicy::ResumeContext)
            .phase_template_slug("validate-integrated")
            .output_artifact_kind(OutputArtifactKind::ValidationReport)
            .job_input(JobInput::integrated_subject(
                expected_target_head_oid.into(),
                prepared_head_oid.into(),
            ))
            .build()
            .persist(db)
            .await
            .expect("create validate job")
    }

    #[allow(clippy::too_many_arguments)]
    async fn persist_prepared_convergence(
        db: &Database,
        project_id: ProjectId,
        item_id: ItemId,
        revision_id: ItemRevisionId,
        source_workspace_id: WorkspaceId,
        integration_workspace_id: WorkspaceId,
        expected_target_head_oid: &str,
        prepared_head_oid: &str,
    ) -> Convergence {
        ConvergenceBuilder::new(project_id, item_id, revision_id)
            .source_workspace_id(source_workspace_id)
            .integration_workspace_id(integration_workspace_id)
            .source_head_commit_oid(prepared_head_oid)
            .status(ConvergenceStatus::Prepared)
            .input_target_commit_oid(expected_target_head_oid)
            .prepared_commit_oid(prepared_head_oid)
            .build()
            .persist(db)
            .await
            .expect("create convergence")
    }

    #[tokio::test]
    async fn migrate_supports_finding_promotion_relationships() {
        let db = migrated_test_db("ingot-store").await;
        let project = persist_project(&db).await;
        let (source_item, source_revision) =
            persist_item_with_revision(&db, project.id, "refs/heads/main").await;
        let job = persist_investigate_job(
            &db,
            project.id,
            source_item.id,
            source_revision.id,
            JobStatus::Completed,
        )
        .await;

        let mut finding =
            FindingBuilder::new(project.id, source_item.id, source_revision.id, job.id)
                .source_step_id("investigate_item")
                .source_report_schema_version("finding_report:v1")
                .source_finding_key("finding-1")
                .source_subject_base_commit_oid(None::<String>)
                .source_subject_head_commit_oid("head")
                .summary("Summary")
                .paths(Vec::new())
                .evidence(serde_json::json!([]))
                .build()
                .persist(&db)
                .await
                .expect("create finding");

        let (promoted_item, _) = persist_promoted_finding_item(&db, project.id, finding.id).await;

        finding.triage = ingot_domain::finding::FindingTriage::Backlog {
            linked_item_id: promoted_item.id,
            triage_note: None,
            triaged_at: default_timestamp(),
        };
        db.triage_finding(&finding).await.expect("promote finding");

        let fk_violations: Vec<(String, String, String, i64)> =
            sqlx::query_as("PRAGMA foreign_key_check")
                .fetch_all(&db.pool)
                .await
                .expect("foreign key check");
        let completed = db
            .load_completed_job_completion(job.id)
            .await
            .expect("load completed job completion")
            .expect("completed job completion should exist");

        assert!(
            fk_violations.is_empty(),
            "foreign key violations: {fk_violations:?}"
        );
        assert_eq!(completed.job.state.status(), JobStatus::Completed);
        assert_eq!(completed.finding_count, 1);
    }

    #[tokio::test]
    async fn complete_job_rejects_duplicate_terminal_updates() {
        let db = migrated_test_db("ingot-store").await;
        let project = persist_project(&db).await;
        let (item, revision) = persist_item_with_revision(&db, project.id, "refs/heads/main").await;
        let job =
            persist_investigate_job(&db, project.id, item.id, revision.id, JobStatus::Running)
                .await;

        db.apply_job_completion(JobCompletionMutation {
            job_id: job.id,
            item_id: item.id,
            expected_item_revision_id: revision.id,
            outcome_class: OutcomeClass::Clean,
            clear_item_escalation: false,
            result_schema_version: Some("finding_report:v1".into()),
            result_payload: Some(serde_json::json!({
                "outcome": "clean",
                "summary": "ok",
                "findings": []
            })),
            output_commit_oid: None,
            findings: vec![],
            prepared_convergence_guard: None,
        })
        .await
        .expect("first completion succeeds");

        let error = db
            .apply_job_completion(JobCompletionMutation {
                job_id: job.id,
                item_id: item.id,
                expected_item_revision_id: revision.id,
                outcome_class: OutcomeClass::Clean,
                clear_item_escalation: false,
                result_schema_version: Some("finding_report:v1".into()),
                result_payload: Some(serde_json::json!({
                    "outcome": "clean",
                    "summary": "ok",
                    "findings": []
                })),
                output_commit_oid: None,
                findings: vec![],
                prepared_convergence_guard: None,
            })
            .await
            .expect_err("duplicate completion should fail");

        assert!(matches!(error, RepositoryError::Conflict(_)));
    }

    #[tokio::test]
    async fn complete_job_rolls_back_when_prepared_convergence_becomes_stale() {
        let db = migrated_test_db("ingot-store").await;
        let project = persist_project(&db).await;
        let (item, revision) = persist_item_with_revision(&db, project.id, "refs/heads/main").await;
        let workspace = persist_integration_workspace(&db, project.id, revision.id).await;
        let job = persist_validate_job(
            &db,
            project.id,
            item.id,
            revision.id,
            "expected-target",
            "prepared-head",
        )
        .await;
        let convergence = persist_prepared_convergence(
            &db,
            project.id,
            item.id,
            revision.id,
            workspace.id,
            workspace.id,
            "expected-target",
            "prepared-head",
        )
        .await;

        let error = db
            .apply_job_completion(JobCompletionMutation {
                job_id: job.id,
                item_id: item.id,
                expected_item_revision_id: revision.id,
                outcome_class: OutcomeClass::Clean,
                clear_item_escalation: false,
                result_schema_version: Some("validation_report:v1".into()),
                result_payload: Some(clean_validation_report("ok")),
                output_commit_oid: None,
                findings: vec![],
                prepared_convergence_guard: Some(PreparedConvergenceGuard {
                    convergence_id: convergence.id,
                    item_revision_id: revision.id,
                    target_ref: "refs/heads/main".into(),
                    expected_target_head_oid: CommitOid::new("moved-target"),
                    next_approval_state: Some(ApprovalState::Pending),
                }),
            })
            .await
            .expect_err("stale prepared convergence should fail");

        assert!(matches!(
            error,
            RepositoryError::Conflict(message) if message == "prepared_convergence_stale"
        ));

        let persisted_job = db.get_job(job.id).await.expect("load job after rollback");
        let persisted_item = db
            .get_item(item.id)
            .await
            .expect("load item after rollback");

        assert_eq!(persisted_job.state.status(), JobStatus::Running);
        assert_eq!(persisted_item.approval_state, ApprovalState::NotRequested);
    }

    #[tokio::test]
    async fn complete_job_rolls_back_when_prepared_convergence_target_ref_changes() {
        let db = migrated_test_db("ingot-store").await;
        let project = persist_project(&db).await;
        let (item, revision) =
            persist_item_with_revision(&db, project.id, "refs/heads/release").await;
        let workspace = persist_integration_workspace(&db, project.id, revision.id).await;
        let job = persist_validate_job(
            &db,
            project.id,
            item.id,
            revision.id,
            "expected-target",
            "prepared-head",
        )
        .await;
        let convergence = persist_prepared_convergence(
            &db,
            project.id,
            item.id,
            revision.id,
            workspace.id,
            workspace.id,
            "expected-target",
            "prepared-head",
        )
        .await;

        let error = db
            .apply_job_completion(JobCompletionMutation {
                job_id: job.id,
                item_id: item.id,
                expected_item_revision_id: revision.id,
                outcome_class: OutcomeClass::Clean,
                clear_item_escalation: false,
                result_schema_version: Some("validation_report:v1".into()),
                result_payload: Some(clean_validation_report("ok")),
                output_commit_oid: None,
                findings: vec![],
                prepared_convergence_guard: Some(PreparedConvergenceGuard {
                    convergence_id: convergence.id,
                    item_revision_id: revision.id,
                    target_ref: "refs/heads/release".into(),
                    expected_target_head_oid: CommitOid::new("expected-target"),
                    next_approval_state: Some(ApprovalState::Pending),
                }),
            })
            .await
            .expect_err("mismatched target_ref should fail");

        assert!(matches!(
            error,
            RepositoryError::Conflict(message) if message == "prepared_convergence_stale"
        ));

        let persisted_job = db.get_job(job.id).await.expect("load job after rollback");
        let persisted_item = db
            .get_item(item.id)
            .await
            .expect("load item after rollback");

        assert_eq!(persisted_job.state.status(), JobStatus::Running);
        assert_eq!(persisted_item.approval_state, ApprovalState::NotRequested);
    }

    #[tokio::test]
    async fn complete_job_rolls_back_when_item_revision_changes_before_commit() {
        let db = migrated_test_db("ingot-store").await;
        let project = persist_project(&db).await;
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
            .created_at(parse_timestamp("2026-03-13T00:00:00Z"))
            .build();
        next_revision.supersedes_revision_id = Some(revision.id);

        let item = ItemBuilder::new(project.id, next_revision.id)
            .id(item_id)
            .build();
        let (item, revision) = (item, revision)
            .persist(&db)
            .await
            .expect("create item with source revision");
        let next_revision = next_revision
            .persist(&db)
            .await
            .expect("create next revision");

        let job =
            persist_investigate_job(&db, project.id, item.id, revision.id, JobStatus::Running)
                .await;

        let error = db
            .apply_job_completion(JobCompletionMutation {
                job_id: job.id,
                item_id: item.id,
                expected_item_revision_id: revision.id,
                outcome_class: OutcomeClass::Clean,
                clear_item_escalation: false,
                result_schema_version: Some("finding_report:v1".into()),
                result_payload: Some(serde_json::json!({
                    "outcome": "clean",
                    "summary": "ok",
                    "findings": []
                })),
                output_commit_oid: None,
                findings: vec![],
                prepared_convergence_guard: None,
            })
            .await
            .expect_err("revision drift should fail");

        assert!(matches!(
            error,
            RepositoryError::Conflict(message) if message == "job_revision_stale"
        ));

        let persisted_job = db.get_job(job.id).await.expect("load job after rollback");
        let persisted_item = db
            .get_item(item.id)
            .await
            .expect("load item after rollback");

        assert_eq!(persisted_job.state.status(), JobStatus::Running);
        assert_eq!(persisted_item.current_revision_id, next_revision.id);
    }
}
