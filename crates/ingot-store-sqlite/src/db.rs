use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::path::Path;

#[derive(Clone)]
pub struct Database {
    pub pool: SqlitePool,
}

impl Database {
    pub async fn connect(path: &Path) -> Result<Self, sqlx::Error> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .foreign_keys(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;

        Ok(Self { pool })
    }

    pub async fn migrate(&self) -> Result<(), sqlx::migrate::MigrateError> {
        sqlx::migrate!("./migrations").run(&self.pool).await
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ingot_domain::ids::{ConvergenceId, ItemId, ItemRevisionId, JobId};
    use ingot_domain::item::{ApprovalState, EscalationReason};
    use ingot_domain::job::{JobStatus, OutcomeClass};
    use ingot_domain::ports::{JobCompletionMutation, PreparedConvergenceGuard, RepositoryError};
    use uuid::Uuid;

    use super::Database;
    use crate::FinishJobNonSuccessParams;

    #[tokio::test]
    async fn migrate_supports_finding_promotion_relationships() {
        let db_path = temp_db_path();
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("run migrations");

        sqlx::query(
            "INSERT INTO projects (id, name, path, default_branch, color)
             VALUES ('prj_00000000000000000000000000000000', 'Test', '/tmp/test', 'main', '#000')",
        )
        .execute(&db.pool)
        .await
        .expect("insert project");

        sqlx::query(
            "INSERT INTO items (
                id, project_id, classification, workflow_version, lifecycle_state, parking_state,
                approval_state, escalation_state, current_revision_id, origin_kind, origin_finding_id,
                priority, labels, created_at, updated_at
             ) VALUES (
                'itm_00000000000000000000000000000000',
                'prj_00000000000000000000000000000000',
                'change',
                'delivery:v1',
                'open',
                'active',
                'not_requested',
                'none',
                'rev_00000000000000000000000000000000',
                'manual',
                NULL,
                'major',
                '[]',
                '2026-03-12T00:00:00Z',
                '2026-03-12T00:00:00Z'
             )",
        )
        .execute(&db.pool)
        .await
        .expect("insert source item");

        sqlx::query(
            "INSERT INTO item_revisions (
                id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
                approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid, seed_target_commit_oid,
                supersedes_revision_id, created_at
             ) VALUES (
                'rev_00000000000000000000000000000000',
                'itm_00000000000000000000000000000000',
                1,
                'Title',
                'Desc',
                'AC',
                'refs/heads/main',
                'required',
                '{}',
                '{}',
                'abc',
                'def',
                NULL,
                '2026-03-12T00:00:00Z'
             )",
        )
        .execute(&db.pool)
        .await
        .expect("insert source revision");

        sqlx::query(
            "INSERT INTO jobs (
                id, project_id, item_id, item_revision_id, step_id, semantic_attempt_no, retry_no,
                status, phase_kind, workspace_kind, execution_permission, context_policy,
                phase_template_slug, output_artifact_kind, created_at
             ) VALUES (
                'job_00000000000000000000000000000000',
                'prj_00000000000000000000000000000000',
                'itm_00000000000000000000000000000000',
                'rev_00000000000000000000000000000000',
                'investigate_item',
                1,
                0,
                'completed',
                'investigate',
                'review',
                'must_not_mutate',
                'fresh',
                'investigate-item',
                'finding_report',
                '2026-03-12T00:00:00Z'
             )",
        )
        .execute(&db.pool)
        .await
        .expect("insert job");

        sqlx::query(
            "INSERT INTO findings (
                id, project_id, source_item_id, source_item_revision_id, source_job_id, source_step_id,
                source_report_schema_version, source_finding_key, source_subject_kind,
                source_subject_base_commit_oid, source_subject_head_commit_oid, code, severity,
                summary, paths, evidence, triage_state, created_at
             ) VALUES (
                'fnd_00000000000000000000000000000000',
                'prj_00000000000000000000000000000000',
                'itm_00000000000000000000000000000000',
                'rev_00000000000000000000000000000000',
                'job_00000000000000000000000000000000',
                'investigate_item',
                'finding_report:v1',
                'finding-1',
                'candidate',
                NULL,
                'head',
                'BUG001',
                'high',
                'Summary',
                '[]',
                '[]',
                'untriaged',
                '2026-03-12T00:00:00Z'
             )",
        )
        .execute(&db.pool)
        .await
        .expect("insert finding");

        sqlx::query(
            "INSERT INTO items (
                id, project_id, classification, workflow_version, lifecycle_state, parking_state,
                approval_state, escalation_state, current_revision_id, origin_kind, origin_finding_id,
                priority, labels, created_at, updated_at
             ) VALUES (
                'itm_11111111111111111111111111111111',
                'prj_00000000000000000000000000000000',
                'bug',
                'delivery:v1',
                'open',
                'active',
                'not_requested',
                'none',
                'rev_11111111111111111111111111111111',
                'promoted_finding',
                'fnd_00000000000000000000000000000000',
                'major',
                '[]',
                '2026-03-12T00:00:00Z',
                '2026-03-12T00:00:00Z'
             )",
        )
        .execute(&db.pool)
        .await
        .expect("insert promoted item");

        sqlx::query(
            "INSERT INTO item_revisions (
                id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
                approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid, seed_target_commit_oid,
                supersedes_revision_id, created_at
             ) VALUES (
                'rev_11111111111111111111111111111111',
                'itm_11111111111111111111111111111111',
                1,
                'Promoted',
                'Desc',
                'AC',
                'refs/heads/main',
                'required',
                '{}',
                '{}',
                'head',
                'def',
                NULL,
                '2026-03-12T00:00:00Z'
             )",
        )
        .execute(&db.pool)
        .await
        .expect("insert promoted revision");

        sqlx::query(
            "UPDATE findings
             SET triage_state = 'backlog',
                 linked_item_id = 'itm_11111111111111111111111111111111',
                 triaged_at = '2026-03-12T00:00:00Z'
             WHERE id = 'fnd_00000000000000000000000000000000'",
        )
        .execute(&db.pool)
        .await
        .expect("promote finding");

        let fk_violations: Vec<(String, String, String, i64)> =
            sqlx::query_as("PRAGMA foreign_key_check")
                .fetch_all(&db.pool)
                .await
                .expect("foreign key check");
        let completed = db
            .load_completed_job_completion(JobId::from_uuid(Uuid::nil()))
            .await
            .expect("load completed job completion")
            .expect("completed job completion should exist");

        assert!(
            fk_violations.is_empty(),
            "foreign key violations: {fk_violations:?}"
        );
        assert_eq!(completed.job.status, JobStatus::Completed);
        assert_eq!(completed.finding_count, 1);
    }

    #[tokio::test]
    async fn complete_job_rejects_duplicate_terminal_updates() {
        let db_path = temp_db_path();
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("run migrations");

        sqlx::query(
            "INSERT INTO projects (id, name, path, default_branch, color)
             VALUES ('prj_00000000000000000000000000000000', 'Test', '/tmp/test', 'main', '#000')",
        )
        .execute(&db.pool)
        .await
        .expect("insert project");

        sqlx::query(
            "INSERT INTO items (
                id, project_id, classification, workflow_version, lifecycle_state, parking_state,
                approval_state, escalation_state, current_revision_id, origin_kind, origin_finding_id,
                priority, labels, created_at, updated_at
             ) VALUES (
                'itm_00000000000000000000000000000000',
                'prj_00000000000000000000000000000000',
                'change',
                'delivery:v1',
                'open',
                'active',
                'not_requested',
                'none',
                'rev_00000000000000000000000000000000',
                'manual',
                NULL,
                'major',
                '[]',
                '2026-03-12T00:00:00Z',
                '2026-03-12T00:00:00Z'
             )",
        )
        .execute(&db.pool)
        .await
        .expect("insert item");

        sqlx::query(
            "INSERT INTO item_revisions (
                id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
                approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid, seed_target_commit_oid,
                supersedes_revision_id, created_at
             ) VALUES (
                'rev_00000000000000000000000000000000',
                'itm_00000000000000000000000000000000',
                1,
                'Title',
                'Desc',
                'AC',
                'refs/heads/main',
                'required',
                '{}',
                '{}',
                'abc',
                'def',
                NULL,
                '2026-03-12T00:00:00Z'
             )",
        )
        .execute(&db.pool)
        .await
        .expect("insert revision");

        sqlx::query(
            "INSERT INTO jobs (
                id, project_id, item_id, item_revision_id, step_id, semantic_attempt_no, retry_no,
                status, phase_kind, workspace_kind, execution_permission, context_policy,
                phase_template_slug, output_artifact_kind, created_at
             ) VALUES (
                'job_00000000000000000000000000000000',
                'prj_00000000000000000000000000000000',
                'itm_00000000000000000000000000000000',
                'rev_00000000000000000000000000000000',
                'investigate_item',
                1,
                0,
                'running',
                'investigate',
                'review',
                'must_not_mutate',
                'fresh',
                'investigate-item',
                'finding_report',
                '2026-03-12T00:00:00Z'
             )",
        )
        .execute(&db.pool)
        .await
        .expect("insert job");

        db.apply_job_completion(JobCompletionMutation {
            job_id: JobId::from_uuid(Uuid::nil()),
            item_id: ItemId::from_uuid(Uuid::nil()),
            expected_item_revision_id: ItemRevisionId::from_uuid(Uuid::nil()),
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
                job_id: JobId::from_uuid(Uuid::nil()),
                item_id: ItemId::from_uuid(Uuid::nil()),
                expected_item_revision_id: ItemRevisionId::from_uuid(Uuid::nil()),
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
        let db_path = temp_db_path();
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("run migrations");

        sqlx::query(
            "INSERT INTO projects (id, name, path, default_branch, color)
             VALUES ('prj_00000000000000000000000000000000', 'Test', '/tmp/test', 'main', '#000')",
        )
        .execute(&db.pool)
        .await
        .expect("insert project");

        sqlx::query(
            "INSERT INTO items (
                id, project_id, classification, workflow_version, lifecycle_state, parking_state,
                approval_state, escalation_state, current_revision_id, origin_kind, origin_finding_id,
                priority, labels, created_at, updated_at
             ) VALUES (
                'itm_00000000000000000000000000000000',
                'prj_00000000000000000000000000000000',
                'change',
                'delivery:v1',
                'open',
                'active',
                'not_requested',
                'none',
                'rev_00000000000000000000000000000000',
                'manual',
                NULL,
                'major',
                '[]',
                '2026-03-12T00:00:00Z',
                '2026-03-12T00:00:00Z'
             )",
        )
        .execute(&db.pool)
        .await
        .expect("insert item");

        sqlx::query(
            "INSERT INTO item_revisions (
                id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
                approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid, seed_target_commit_oid,
                supersedes_revision_id, created_at
             ) VALUES (
                'rev_00000000000000000000000000000000',
                'itm_00000000000000000000000000000000',
                1,
                'Title',
                'Desc',
                'AC',
                'refs/heads/main',
                'required',
                '{}',
                '{}',
                'abc',
                'def',
                NULL,
                '2026-03-12T00:00:00Z'
             )",
        )
        .execute(&db.pool)
        .await
        .expect("insert revision");

        sqlx::query(
            "INSERT INTO workspaces (
                id, project_id, kind, strategy, path, created_for_revision_id, retention_policy,
                status, created_at, updated_at
             ) VALUES (
                'wrk_00000000000000000000000000000000',
                'prj_00000000000000000000000000000000',
                'integration',
                'worktree',
                '/tmp/workspace',
                'rev_00000000000000000000000000000000',
                'ephemeral',
                'ready',
                '2026-03-12T00:00:00Z',
                '2026-03-12T00:00:00Z'
             )",
        )
        .execute(&db.pool)
        .await
        .expect("insert workspace");

        sqlx::query(
            "INSERT INTO jobs (
                id, project_id, item_id, item_revision_id, step_id, semantic_attempt_no, retry_no,
                status, phase_kind, workspace_kind, execution_permission, context_policy,
                phase_template_slug, output_artifact_kind, job_input_kind, input_base_commit_oid, input_head_commit_oid, created_at
             ) VALUES (
                'job_00000000000000000000000000000000',
                'prj_00000000000000000000000000000000',
                'itm_00000000000000000000000000000000',
                'rev_00000000000000000000000000000000',
                'validate_integrated',
                1,
                0,
                'running',
                'validate',
                'integration',
                'must_not_mutate',
                'resume_context',
                'validate-integrated',
                'validation_report',
                'integrated_subject',
                'expected-target',
                'prepared-head',
                '2026-03-12T00:00:00Z'
             )",
        )
        .execute(&db.pool)
        .await
        .expect("insert job");

        sqlx::query(
            "INSERT INTO convergences (
                id, project_id, item_id, item_revision_id, source_workspace_id, integration_workspace_id,
                source_head_commit_oid, target_ref, strategy, status, input_target_commit_oid,
                prepared_commit_oid, final_target_commit_oid, conflict_summary, created_at, completed_at
             ) VALUES (
                'conv_00000000000000000000000000000000',
                'prj_00000000000000000000000000000000',
                'itm_00000000000000000000000000000000',
                'rev_00000000000000000000000000000000',
                'wrk_00000000000000000000000000000000',
                NULL,
                'prepared-head',
                'refs/heads/main',
                'rebase_then_fast_forward',
                'prepared',
                'expected-target',
                'prepared-head',
                NULL,
                NULL,
                '2026-03-12T00:00:00Z',
                NULL
             )",
        )
        .execute(&db.pool)
        .await
        .expect("insert convergence");

        let error = db
            .apply_job_completion(JobCompletionMutation {
                job_id: JobId::from_uuid(Uuid::nil()),
                item_id: ItemId::from_uuid(Uuid::nil()),
                expected_item_revision_id: ItemRevisionId::from_uuid(Uuid::nil()),
                outcome_class: OutcomeClass::Clean,
                clear_item_escalation: false,
                result_schema_version: Some("validation_report:v1".into()),
                result_payload: Some(serde_json::json!({
                    "outcome": "clean",
                    "summary": "ok",
                    "checks": [{
                        "name": "lint",
                        "status": "pass",
                        "summary": "ok"
                    }],
                    "findings": []
                })),
                output_commit_oid: None,
                findings: vec![],
                prepared_convergence_guard: Some(PreparedConvergenceGuard {
                    convergence_id: ConvergenceId::from_uuid(Uuid::nil()),
                    item_revision_id: ItemRevisionId::from_uuid(Uuid::nil()),
                    target_ref: "refs/heads/main".into(),
                    expected_target_head_oid: "moved-target".into(),
                    next_approval_state: Some(ApprovalState::Pending),
                }),
            })
            .await
            .expect_err("stale prepared convergence should fail");

        assert!(matches!(
            error,
            RepositoryError::Conflict(message) if message == "prepared_convergence_stale"
        ));

        let job_status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id = ?")
            .bind("job_00000000000000000000000000000000")
            .fetch_one(&db.pool)
            .await
            .expect("job status");
        let approval_state: String =
            sqlx::query_scalar("SELECT approval_state FROM items WHERE id = ?")
                .bind("itm_00000000000000000000000000000000")
                .fetch_one(&db.pool)
                .await
                .expect("approval state");

        assert_eq!(job_status, "running");
        assert_eq!(approval_state, "not_requested");
    }

    #[tokio::test]
    async fn complete_job_rolls_back_when_prepared_convergence_target_ref_changes() {
        let db_path = temp_db_path();
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("run migrations");

        sqlx::query(
            "INSERT INTO projects (id, name, path, default_branch, color)
             VALUES ('prj_00000000000000000000000000000000', 'Test', '/tmp/test', 'main', '#000')",
        )
        .execute(&db.pool)
        .await
        .expect("insert project");

        sqlx::query(
            "INSERT INTO items (
                id, project_id, classification, workflow_version, lifecycle_state, parking_state,
                approval_state, escalation_state, current_revision_id, origin_kind, origin_finding_id,
                priority, labels, created_at, updated_at
             ) VALUES (
                'itm_00000000000000000000000000000000',
                'prj_00000000000000000000000000000000',
                'change',
                'delivery:v1',
                'open',
                'active',
                'not_requested',
                'none',
                'rev_00000000000000000000000000000000',
                'manual',
                NULL,
                'major',
                '[]',
                '2026-03-12T00:00:00Z',
                '2026-03-12T00:00:00Z'
             )",
        )
        .execute(&db.pool)
        .await
        .expect("insert item");

        sqlx::query(
            "INSERT INTO item_revisions (
                id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
                approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid, seed_target_commit_oid,
                supersedes_revision_id, created_at
             ) VALUES (
                'rev_00000000000000000000000000000000',
                'itm_00000000000000000000000000000000',
                1,
                'Title',
                'Desc',
                'AC',
                'refs/heads/release',
                'required',
                '{}',
                '{}',
                'abc',
                'def',
                NULL,
                '2026-03-12T00:00:00Z'
             )",
        )
        .execute(&db.pool)
        .await
        .expect("insert revision");

        sqlx::query(
            "INSERT INTO workspaces (
                id, project_id, kind, strategy, path, created_for_revision_id, retention_policy,
                status, created_at, updated_at
             ) VALUES (
                'wrk_00000000000000000000000000000000',
                'prj_00000000000000000000000000000000',
                'integration',
                'worktree',
                '/tmp/workspace',
                'rev_00000000000000000000000000000000',
                'ephemeral',
                'ready',
                '2026-03-12T00:00:00Z',
                '2026-03-12T00:00:00Z'
             )",
        )
        .execute(&db.pool)
        .await
        .expect("insert workspace");

        sqlx::query(
            "INSERT INTO jobs (
                id, project_id, item_id, item_revision_id, step_id, semantic_attempt_no, retry_no,
                status, phase_kind, workspace_kind, execution_permission, context_policy,
                phase_template_slug, output_artifact_kind, job_input_kind, input_base_commit_oid, input_head_commit_oid, created_at
             ) VALUES (
                'job_00000000000000000000000000000000',
                'prj_00000000000000000000000000000000',
                'itm_00000000000000000000000000000000',
                'rev_00000000000000000000000000000000',
                'validate_integrated',
                1,
                0,
                'running',
                'validate',
                'integration',
                'must_not_mutate',
                'resume_context',
                'validate-integrated',
                'validation_report',
                'integrated_subject',
                'expected-target',
                'prepared-head',
                '2026-03-12T00:00:00Z'
             )",
        )
        .execute(&db.pool)
        .await
        .expect("insert job");

        sqlx::query(
            "INSERT INTO convergences (
                id, project_id, item_id, item_revision_id, source_workspace_id, integration_workspace_id,
                source_head_commit_oid, target_ref, strategy, status, input_target_commit_oid,
                prepared_commit_oid, final_target_commit_oid, conflict_summary, created_at, completed_at
             ) VALUES (
                'conv_00000000000000000000000000000000',
                'prj_00000000000000000000000000000000',
                'itm_00000000000000000000000000000000',
                'rev_00000000000000000000000000000000',
                'wrk_00000000000000000000000000000000',
                NULL,
                'prepared-head',
                'refs/heads/main',
                'rebase_then_fast_forward',
                'prepared',
                'expected-target',
                'prepared-head',
                NULL,
                NULL,
                '2026-03-12T00:00:00Z',
                NULL
             )",
        )
        .execute(&db.pool)
        .await
        .expect("insert convergence");

        let error = db
            .apply_job_completion(JobCompletionMutation {
                job_id: JobId::from_uuid(Uuid::nil()),
                item_id: ItemId::from_uuid(Uuid::nil()),
                expected_item_revision_id: ItemRevisionId::from_uuid(Uuid::nil()),
                outcome_class: OutcomeClass::Clean,
                clear_item_escalation: false,
                result_schema_version: Some("validation_report:v1".into()),
                result_payload: Some(serde_json::json!({
                    "outcome": "clean",
                    "summary": "ok",
                    "checks": [{
                        "name": "lint",
                        "status": "pass",
                        "summary": "ok"
                    }],
                    "findings": []
                })),
                output_commit_oid: None,
                findings: vec![],
                prepared_convergence_guard: Some(PreparedConvergenceGuard {
                    convergence_id: ConvergenceId::from_uuid(Uuid::nil()),
                    item_revision_id: ItemRevisionId::from_uuid(Uuid::nil()),
                    target_ref: "refs/heads/release".into(),
                    expected_target_head_oid: "expected-target".into(),
                    next_approval_state: Some(ApprovalState::Pending),
                }),
            })
            .await
            .expect_err("mismatched target_ref should fail");

        assert!(matches!(
            error,
            RepositoryError::Conflict(message) if message == "prepared_convergence_stale"
        ));

        let job_status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id = ?")
            .bind("job_00000000000000000000000000000000")
            .fetch_one(&db.pool)
            .await
            .expect("job status");
        let approval_state: String =
            sqlx::query_scalar("SELECT approval_state FROM items WHERE id = ?")
                .bind("itm_00000000000000000000000000000000")
                .fetch_one(&db.pool)
                .await
                .expect("approval state");

        assert_eq!(job_status, "running");
        assert_eq!(approval_state, "not_requested");
    }

    #[tokio::test]
    async fn complete_job_rolls_back_when_item_revision_changes_before_commit() {
        let db_path = temp_db_path();
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("run migrations");

        sqlx::query(
            "INSERT INTO projects (id, name, path, default_branch, color)
             VALUES ('prj_00000000000000000000000000000000', 'Test', '/tmp/test', 'main', '#000')",
        )
        .execute(&db.pool)
        .await
        .expect("insert project");

        sqlx::query(
            "INSERT INTO items (
                id, project_id, classification, workflow_version, lifecycle_state, parking_state,
                approval_state, escalation_state, current_revision_id, origin_kind, origin_finding_id,
                priority, labels, created_at, updated_at
             ) VALUES (
                'itm_00000000000000000000000000000000',
                'prj_00000000000000000000000000000000',
                'change',
                'delivery:v1',
                'open',
                'active',
                'not_requested',
                'none',
                'rev_11111111111111111111111111111111',
                'manual',
                NULL,
                'major',
                '[]',
                '2026-03-12T00:00:00Z',
                '2026-03-12T00:00:00Z'
             )",
        )
        .execute(&db.pool)
        .await
        .expect("insert item");

        sqlx::query(
            "INSERT INTO item_revisions (
                id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
                approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid, seed_target_commit_oid,
                supersedes_revision_id, created_at
             ) VALUES
             (
                'rev_00000000000000000000000000000000',
                'itm_00000000000000000000000000000000',
                1,
                'Title',
                'Desc',
                'AC',
                'refs/heads/main',
                'required',
                '{}',
                '{}',
                'abc',
                'def',
                NULL,
                '2026-03-12T00:00:00Z'
             ),
             (
                'rev_11111111111111111111111111111111',
                'itm_00000000000000000000000000000000',
                2,
                'Title 2',
                'Desc 2',
                'AC 2',
                'refs/heads/main',
                'required',
                '{}',
                '{}',
                'ghi',
                'jkl',
                'rev_00000000000000000000000000000000',
                '2026-03-13T00:00:00Z'
             )",
        )
        .execute(&db.pool)
        .await
        .expect("insert revisions");

        sqlx::query(
            "INSERT INTO jobs (
                id, project_id, item_id, item_revision_id, step_id, semantic_attempt_no, retry_no,
                status, phase_kind, workspace_kind, execution_permission, context_policy,
                phase_template_slug, output_artifact_kind, created_at
             ) VALUES (
                'job_00000000000000000000000000000000',
                'prj_00000000000000000000000000000000',
                'itm_00000000000000000000000000000000',
                'rev_00000000000000000000000000000000',
                'investigate_item',
                1,
                0,
                'running',
                'investigate',
                'review',
                'must_not_mutate',
                'fresh',
                'investigate-item',
                'finding_report',
                '2026-03-12T00:00:00Z'
             )",
        )
        .execute(&db.pool)
        .await
        .expect("insert job");

        let error = db
            .apply_job_completion(JobCompletionMutation {
                job_id: JobId::from_uuid(Uuid::nil()),
                item_id: ItemId::from_uuid(Uuid::nil()),
                expected_item_revision_id: ItemRevisionId::from_uuid(Uuid::nil()),
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

        let job_status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id = ?")
            .bind("job_00000000000000000000000000000000")
            .fetch_one(&db.pool)
            .await
            .expect("job status");

        assert_eq!(job_status, "running");
    }

    #[tokio::test]
    async fn finish_job_non_success_rolls_back_when_item_revision_changes_before_commit() {
        let db_path = temp_db_path();
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("run migrations");

        sqlx::query(
            "INSERT INTO projects (id, name, path, default_branch, color)
             VALUES ('prj_00000000000000000000000000000000', 'Test', '/tmp/test', 'main', '#000')",
        )
        .execute(&db.pool)
        .await
        .expect("insert project");

        sqlx::query(
            "INSERT INTO items (
                id, project_id, classification, workflow_version, lifecycle_state, parking_state,
                approval_state, escalation_state, current_revision_id, origin_kind, origin_finding_id,
                priority, labels, created_at, updated_at
             ) VALUES (
                'itm_00000000000000000000000000000000',
                'prj_00000000000000000000000000000000',
                'change',
                'delivery:v1',
                'open',
                'active',
                'not_requested',
                'none',
                'rev_11111111111111111111111111111111',
                'manual',
                NULL,
                'major',
                '[]',
                '2026-03-12T00:00:00Z',
                '2026-03-12T00:00:00Z'
             )",
        )
        .execute(&db.pool)
        .await
        .expect("insert item");

        sqlx::query(
            "INSERT INTO item_revisions (
                id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
                approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid, seed_target_commit_oid,
                supersedes_revision_id, created_at
             ) VALUES
             (
                'rev_00000000000000000000000000000000',
                'itm_00000000000000000000000000000000',
                1,
                'Title',
                'Desc',
                'AC',
                'refs/heads/main',
                'required',
                '{}',
                '{}',
                'abc',
                'def',
                NULL,
                '2026-03-12T00:00:00Z'
             ),
             (
                'rev_11111111111111111111111111111111',
                'itm_00000000000000000000000000000000',
                2,
                'Title 2',
                'Desc 2',
                'AC 2',
                'refs/heads/main',
                'required',
                '{}',
                '{}',
                'ghi',
                'jkl',
                'rev_00000000000000000000000000000000',
                '2026-03-13T00:00:00Z'
             )",
        )
        .execute(&db.pool)
        .await
        .expect("insert revisions");

        sqlx::query(
            "INSERT INTO jobs (
                id, project_id, item_id, item_revision_id, step_id, semantic_attempt_no, retry_no,
                status, phase_kind, workspace_kind, execution_permission, context_policy,
                phase_template_slug, output_artifact_kind, created_at
             ) VALUES (
                'job_00000000000000000000000000000000',
                'prj_00000000000000000000000000000000',
                'itm_00000000000000000000000000000000',
                'rev_00000000000000000000000000000000',
                'repair_candidate',
                1,
                0,
                'running',
                'author',
                'authoring',
                'may_mutate',
                'resume_context',
                'repair-candidate',
                'commit',
                '2026-03-12T00:00:00Z'
             )",
        )
        .execute(&db.pool)
        .await
        .expect("insert job");

        let error = db
            .finish_job_non_success(FinishJobNonSuccessParams {
                job_id: JobId::from_uuid(Uuid::nil()),
                item_id: ItemId::from_uuid(Uuid::nil()),
                expected_item_revision_id: ItemRevisionId::from_uuid(Uuid::nil()),
                status: JobStatus::Failed,
                outcome_class: Some(OutcomeClass::TerminalFailure),
                error_code: Some("worker_failed"),
                error_message: Some("boom"),
                escalation_reason: Some(EscalationReason::StepFailed),
            })
            .await
            .expect_err("revision drift should fail");

        assert!(matches!(
            error,
            RepositoryError::Conflict(message) if message == "job_revision_stale"
        ));

        let job_status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id = ?")
            .bind("job_00000000000000000000000000000000")
            .fetch_one(&db.pool)
            .await
            .expect("job status");
        let escalation_state: String =
            sqlx::query_scalar("SELECT escalation_state FROM items WHERE id = ?")
                .bind("itm_00000000000000000000000000000000")
                .fetch_one(&db.pool)
                .await
                .expect("escalation state");

        assert_eq!(job_status, "running");
        assert_eq!(escalation_state, "none");
    }

    fn temp_db_path() -> PathBuf {
        std::env::temp_dir().join(format!("ingot-store-{}.db", Uuid::now_v7()))
    }
}
