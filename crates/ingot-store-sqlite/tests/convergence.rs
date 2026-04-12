mod common;

use ingot_domain::ids::{ConvergenceId, ItemId, ItemRevisionId, ProjectId, WorkspaceId};
use ingot_domain::test_support::{
    ConvergenceBuilder, ItemBuilder, ProjectBuilder, RevisionBuilder, WorkspaceBuilder,
};
use ingot_domain::workspace::WorkspaceKind;
use ingot_store_sqlite::Database;
use ingot_test_support::sqlite::PersistFixture;
use sqlx::SqlitePool;

struct ConvergenceTestContext {
    db: Database,
    raw_pool: SqlitePool,
    project_id: ProjectId,
    item_id: ItemId,
    revision_id: ItemRevisionId,
    source_workspace_id: WorkspaceId,
}

async fn prepare_test_context(prefix: &str) -> ConvergenceTestContext {
    let (db, path) = common::migrated_test_db_with_path(prefix).await;
    let raw_pool = common::raw_sqlite_pool(&path).await;

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
        raw_pool,
        project_id: project.id,
        item_id: item.id,
        revision_id: revision.id,
        source_workspace_id: source_workspace.id,
    }
}

#[tokio::test]
async fn prepared_convergence_requires_integration_workspace_in_schema() {
    let ConvergenceTestContext {
        db: _,
        raw_pool,
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
    .bind(ConvergenceId::new())
    .bind(project_id)
    .bind(item_id)
    .bind(revision_id)
    .bind(source_workspace_id)
    .bind(Option::<String>::None)
    .bind("head")
    .bind("refs/heads/main")
    .bind("rebase_then_fast_forward")
    .bind("prepared")
    .bind("base")
    .bind("prepared")
    .execute(&raw_pool)
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
        raw_pool,
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
    .bind(convergence_id)
    .bind(project_id)
    .bind(item_id)
    .bind(revision_id)
    .bind(source_workspace_id)
    .bind(Option::<String>::None)
    .bind("head")
    .bind("refs/heads/main")
    .bind("rebase_then_fast_forward")
    .bind("queued")
    .execute(&raw_pool)
    .await
    .expect("queued convergence without integration workspace should persist");

    let convergence = db
        .get_convergence(convergence_id)
        .await
        .expect("load queued convergence");
    assert_eq!(convergence.state.integration_workspace_id(), None);
}

#[tokio::test]
async fn finalized_convergence_round_trips_checkout_adoption_state() {
    let ConvergenceTestContext {
        db,
        project_id,
        item_id,
        revision_id,
        source_workspace_id,
        ..
    } = prepare_test_context("ingot-store-convergence").await;

    let convergence = ConvergenceBuilder::new(project_id, item_id, revision_id)
        .id(ConvergenceId::new())
        .source_workspace_id(source_workspace_id)
        .integration_workspace_id(source_workspace_id)
        .status(ingot_domain::convergence::ConvergenceStatus::Finalized)
        .final_target_commit_oid("final")
        .checkout_adoption_blocked_at(
            "registered checkout has tracked changes",
            chrono::Utc::now(),
        )
        .build();
    db.create_convergence(&convergence)
        .await
        .expect("persist finalized convergence");

    let loaded = db
        .get_convergence(convergence.id)
        .await
        .expect("load finalized convergence");
    assert_eq!(
        loaded.state.checkout_adoption_state(),
        Some(ingot_domain::convergence::CheckoutAdoptionState::Blocked)
    );
    assert_eq!(
        loaded.state.checkout_adoption_message(),
        Some("registered checkout has tracked changes")
    );
    assert_eq!(
        loaded
            .state
            .final_target_commit_oid()
            .map(ToString::to_string),
        Some("final".into())
    );
}
