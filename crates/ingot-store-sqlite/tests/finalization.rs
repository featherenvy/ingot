mod common;

use chrono::Utc;
use ingot_domain::activity::ActivityEventType;
use ingot_domain::convergence::{CheckoutAdoptionState, ConvergenceStatus};
use ingot_domain::git_operation::{GitOperationEntityRef, GitOperationStatus, OperationKind};
use ingot_domain::item::{ApprovalState, EscalationReason, ResolutionSource};
use ingot_domain::ports::{
    ConflictKind, FinalizationCheckoutAdoptionSucceededMutation, FinalizationMutation,
    FinalizationTargetRefAdvancedMutation, RepositoryError,
};
use ingot_domain::test_support::{
    ConvergenceBuilder, ConvergenceQueueEntryBuilder, GitOperationBuilder, ItemBuilder,
    ProjectBuilder, RevisionBuilder, WorkspaceBuilder,
};
use ingot_domain::workspace::{WorkspaceKind, WorkspaceStatus};
use ingot_store_sqlite::Database;
use ingot_test_support::sqlite::PersistFixture;

struct FinalizationFixture {
    db: Database,
    project: ingot_domain::project::Project,
    item: ingot_domain::item::Item,
    revision: ingot_domain::revision::ItemRevision,
    workspace: ingot_domain::workspace::Workspace,
}

async fn prepare_fixture(prefix: &str) -> FinalizationFixture {
    let db = common::migrated_test_db(prefix).await;
    let project = ProjectBuilder::new("/tmp/finalization-store")
        .build()
        .persist(&db)
        .await
        .expect("create project");
    let revision = RevisionBuilder::new(ingot_domain::ids::ItemId::new()).build();
    let item = ItemBuilder::new(project.id, revision.id)
        .id(revision.item_id)
        .approval_state(ApprovalState::Pending)
        .build();
    let (item, revision) = (item, revision).persist(&db).await.expect("create item");
    let workspace = WorkspaceBuilder::new(project.id, WorkspaceKind::Integration)
        .created_for_revision_id(revision.id)
        .status(WorkspaceStatus::Ready)
        .build()
        .persist(&db)
        .await
        .expect("create workspace");

    FinalizationFixture {
        db,
        project,
        item,
        revision,
        workspace,
    }
}

fn assert_conflict(error: RepositoryError, expected: &str) {
    match error {
        RepositoryError::Conflict(ConflictKind::Other(code)) => assert_eq!(code, expected),
        other => panic!("expected conflict {expected}, got {other:?}"),
    }
}

#[tokio::test]
async fn target_ref_advanced_rejects_failed_convergence_state() {
    let fixture = prepare_fixture("ingot-store-finalization").await;
    let convergence =
        ConvergenceBuilder::new(fixture.project.id, fixture.item.id, fixture.revision.id)
            .source_workspace_id(fixture.workspace.id)
            .integration_workspace_id(fixture.workspace.id)
            .status(ConvergenceStatus::Failed)
            .final_target_commit_oid("final")
            .completed_at(Utc::now())
            .build()
            .persist(&fixture.db)
            .await
            .expect("persist convergence");
    let operation = GitOperationBuilder::new(
        fixture.project.id,
        OperationKind::FinalizeTargetRef,
        GitOperationEntityRef::Convergence(convergence.id),
    )
    .workspace_id(fixture.workspace.id)
    .ref_name("refs/heads/main")
    .expected_old_oid("base")
    .new_oid("final")
    .commit_oid("final")
    .status(GitOperationStatus::Applied)
    .build();
    fixture
        .db
        .create_git_operation(&operation)
        .await
        .expect("persist git operation");
    fixture
        .db
        .create_queue_entry(
            &ConvergenceQueueEntryBuilder::new(
                fixture.project.id,
                fixture.item.id,
                fixture.revision.id,
            )
            .build(),
        )
        .await
        .expect("persist queue entry");

    let error = fixture
        .db
        .apply_finalization_mutation(FinalizationMutation::TargetRefAdvanced(
            FinalizationTargetRefAdvancedMutation {
                project_id: fixture.project.id,
                item_id: fixture.item.id,
                expected_item_revision_id: fixture.revision.id,
                convergence_id: convergence.id,
                git_operation_id: operation.id,
                final_target_commit_oid: "final".into(),
                checkout_adoption: ingot_domain::convergence::FinalizedCheckoutAdoption::pending(
                    Utc::now(),
                ),
            },
        ))
        .await
        .expect_err("failed convergence should be rejected");

    assert_conflict(
        error,
        "finalization_requires_prepared_or_finalized_convergence",
    );

    let loaded_convergence = fixture
        .db
        .get_convergence(convergence.id)
        .await
        .expect("reload convergence");
    assert_eq!(loaded_convergence.state.status(), ConvergenceStatus::Failed);
    let loaded_operation = fixture
        .db
        .find_unresolved_finalize_for_convergence(convergence.id)
        .await
        .expect("reload finalize op")
        .expect("finalize op remains unresolved");
    assert_eq!(loaded_operation.status, GitOperationStatus::Applied);
    let queue_entries = fixture
        .db
        .list_queue_entries_by_item(fixture.item.id)
        .await
        .expect("list queue entries");
    assert_eq!(
        queue_entries[0].status,
        ingot_domain::convergence_queue::ConvergenceQueueEntryStatus::Head
    );
    let workspace = fixture
        .db
        .get_workspace(fixture.workspace.id)
        .await
        .expect("reload workspace");
    assert_eq!(workspace.state.status(), WorkspaceStatus::Ready);
}

#[tokio::test]
async fn target_ref_advanced_rejects_terminal_finalize_operation() {
    let fixture = prepare_fixture("ingot-store-finalization").await;
    let convergence =
        ConvergenceBuilder::new(fixture.project.id, fixture.item.id, fixture.revision.id)
            .source_workspace_id(fixture.workspace.id)
            .integration_workspace_id(fixture.workspace.id)
            .build()
            .persist(&fixture.db)
            .await
            .expect("persist convergence");
    let operation = GitOperationBuilder::new(
        fixture.project.id,
        OperationKind::FinalizeTargetRef,
        GitOperationEntityRef::Convergence(convergence.id),
    )
    .workspace_id(fixture.workspace.id)
    .ref_name("refs/heads/main")
    .expected_old_oid("base")
    .new_oid("final")
    .commit_oid("final")
    .status(GitOperationStatus::Reconciled)
    .build();
    fixture
        .db
        .create_git_operation(&operation)
        .await
        .expect("persist git operation");

    let error = fixture
        .db
        .apply_finalization_mutation(FinalizationMutation::TargetRefAdvanced(
            FinalizationTargetRefAdvancedMutation {
                project_id: fixture.project.id,
                item_id: fixture.item.id,
                expected_item_revision_id: fixture.revision.id,
                convergence_id: convergence.id,
                git_operation_id: operation.id,
                final_target_commit_oid: "final".into(),
                checkout_adoption: ingot_domain::convergence::FinalizedCheckoutAdoption::pending(
                    Utc::now(),
                ),
            },
        ))
        .await
        .expect_err("terminal finalize op should be rejected");

    assert_conflict(error, "finalization_requires_unresolved_finalize_operation");

    let loaded_convergence = fixture
        .db
        .get_convergence(convergence.id)
        .await
        .expect("reload convergence");
    assert_eq!(
        loaded_convergence.state.status(),
        ConvergenceStatus::Prepared
    );
}

#[tokio::test]
async fn checkout_adoption_succeeded_rejects_terminal_finalize_operation() {
    let fixture = prepare_fixture("ingot-store-finalization").await;
    let item = ItemBuilder::new(fixture.project.id, fixture.revision.id)
        .id(fixture.item.id)
        .approval_state(ApprovalState::Pending)
        .escalated(EscalationReason::CheckoutSyncBlocked)
        .build();
    fixture.db.update_item(&item).await.expect("escalate item");
    let convergence =
        ConvergenceBuilder::new(fixture.project.id, fixture.item.id, fixture.revision.id)
            .source_workspace_id(fixture.workspace.id)
            .integration_workspace_id(fixture.workspace.id)
            .status(ConvergenceStatus::Finalized)
            .final_target_commit_oid("final")
            .checkout_adoption_blocked_at("registered checkout blocked", Utc::now())
            .completed_at(Utc::now())
            .build()
            .persist(&fixture.db)
            .await
            .expect("persist convergence");
    let operation = GitOperationBuilder::new(
        fixture.project.id,
        OperationKind::FinalizeTargetRef,
        GitOperationEntityRef::Convergence(convergence.id),
    )
    .workspace_id(fixture.workspace.id)
    .ref_name("refs/heads/main")
    .expected_old_oid("base")
    .new_oid("final")
    .commit_oid("final")
    .status(GitOperationStatus::Failed)
    .build();
    fixture
        .db
        .create_git_operation(&operation)
        .await
        .expect("persist git operation");

    let error = fixture
        .db
        .apply_finalization_mutation(FinalizationMutation::CheckoutAdoptionSucceeded(
            FinalizationCheckoutAdoptionSucceededMutation {
                project_id: fixture.project.id,
                item_id: fixture.item.id,
                expected_item_revision_id: fixture.revision.id,
                convergence_id: convergence.id,
                git_operation_id: operation.id,
                resolution_source: ResolutionSource::ApprovalCommand,
                approval_state: ApprovalState::Approved,
                synced_at: Utc::now(),
            },
        ))
        .await
        .expect_err("terminal finalize op should be rejected");

    assert_conflict(error, "finalization_requires_unresolved_finalize_operation");

    let loaded_item = fixture
        .db
        .get_item(fixture.item.id)
        .await
        .expect("reload item");
    assert!(loaded_item.lifecycle.is_open());
    assert_eq!(
        loaded_item.escalation.reason(),
        Some(EscalationReason::CheckoutSyncBlocked)
    );
    let loaded_convergence = fixture
        .db
        .get_convergence(convergence.id)
        .await
        .expect("reload convergence");
    assert_eq!(
        loaded_convergence.state.checkout_adoption_state(),
        Some(CheckoutAdoptionState::Blocked)
    );

    let activities = fixture
        .db
        .list_activity_by_project(fixture.project.id, 50, 0)
        .await
        .expect("list activity");
    assert!(
        !activities
            .iter()
            .any(|activity| activity.event_type == ActivityEventType::GitOperationReconciled)
    );
}
