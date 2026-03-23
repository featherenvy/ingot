mod common;

use ingot_domain::convergence::ConvergenceStatus;
use ingot_domain::ids::{ActivityId, ItemId};
use ingot_domain::item::ApprovalState;
use ingot_domain::ports::{InvalidatePreparedConvergenceMutation, RepositoryError};
use ingot_domain::workspace::{WorkspaceKind, WorkspaceStatus};
use ingot_store_sqlite::PersistFixture;
use ingot_test_support::fixtures::{
    ConvergenceBuilder, ItemBuilder, ProjectBuilder, RevisionBuilder, WorkspaceBuilder,
};

#[tokio::test]
async fn apply_invalidation_fails_convergence_and_stales_workspace_atomically() {
    let db = common::migrated_test_db("invalidate-atomic").await;

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
    failed_convergence.transition_to_failed(Some("target_ref_moved".into()), chrono::Utc::now());

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
            subject: ingot_domain::activity::ActivitySubject::Convergence(convergence.id),
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
    let db = common::migrated_test_db("invalidate-rollback").await;

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
            subject: ingot_domain::activity::ActivitySubject::Convergence(
                ingot_domain::ids::ConvergenceId::from_uuid(uuid::Uuid::nil()),
            ),
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
