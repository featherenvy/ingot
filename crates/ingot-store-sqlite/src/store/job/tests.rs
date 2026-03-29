use chrono::Utc;
use ingot_domain::ids::{ItemId, JobId};
use ingot_domain::ports::RepositoryError;
use ingot_domain::test_support::{ItemBuilder, ProjectBuilder, RevisionBuilder};
use ingot_test_support::sqlite::temp_db_path;

use crate::db::Database;
use crate::store::PersistFixture;

#[tokio::test]
async fn get_job_rejects_assigned_rows_without_workspace_id() {
    let path = temp_db_path("ingot-store-job");
    let db = Database::connect(&path).await.expect("connect db");
    db.migrate().await.expect("migrate db");

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

    let job_id = JobId::new();
    sqlx::query(
        "INSERT INTO jobs (
            id, project_id, item_id, item_revision_id, step_id, semantic_attempt_no, retry_no,
            supersedes_job_id, status, outcome_class, phase_kind, workspace_id, workspace_kind,
            execution_permission, context_policy, phase_template_slug, phase_template_digest,
            prompt_snapshot, job_input_kind, input_base_commit_oid, input_head_commit_oid,
            output_artifact_kind, output_commit_oid, result_schema_version, result_payload,
            agent_id, process_pid, lease_owner_id, heartbeat_at, lease_expires_at, error_code,
            error_message, created_at, started_at, ended_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(job_id)
    .bind(project.id)
    .bind(item.id)
    .bind(revision.id)
    .bind("author_initial")
    .bind(1_i64)
    .bind(0_i64)
    .bind(Option::<String>::None)
    .bind("assigned")
    .bind(Option::<String>::None)
    .bind("author")
    .bind(Option::<String>::None)
    .bind("authoring")
    .bind("may_mutate")
    .bind("fresh")
    .bind("template")
    .bind(Option::<String>::None)
    .bind(Option::<String>::None)
    .bind("none")
    .bind(Option::<String>::None)
    .bind(Option::<String>::None)
    .bind("none")
    .bind(Option::<String>::None)
    .bind(Option::<String>::None)
    .bind(Option::<String>::None)
    .bind(Option::<String>::None)
    .bind(Option::<i64>::None)
    .bind(Option::<String>::None)
    .bind(Option::<chrono::DateTime<Utc>>::None)
    .bind(Option::<chrono::DateTime<Utc>>::None)
    .bind(Option::<String>::None)
    .bind(Option::<String>::None)
    .bind(Utc::now())
    .bind(Option::<chrono::DateTime<Utc>>::None)
    .bind(Option::<chrono::DateTime<Utc>>::None)
    .execute(&db.pool)
    .await
    .expect("insert malformed assigned job");

    let error = db.get_job(job_id).await.expect_err("missing workspace_id");
    assert!(matches!(error, RepositoryError::Database(_)));
}
