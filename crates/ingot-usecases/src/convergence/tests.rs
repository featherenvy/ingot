use chrono::Utc;
use ingot_domain::ids::{ItemId, ItemRevisionId, ProjectId};
use ingot_test_support::fixtures::{ItemBuilder, ProjectBuilder, RevisionBuilder};
use ingot_test_support::git::unique_temp_path;
use uuid::Uuid;

use crate::UseCaseError;

use super::test_support::{FakePort, fake_completed_validate_job, project_state};
use super::{
    ApprovalFinalizeReadiness, ConvergenceQueuePrepareContext, ConvergenceService,
    FinalizeTargetRefResult,
};

#[tokio::test]
async fn queue_prepare_creates_lane_head_when_lane_is_empty() {
    let now = Utc::now();
    let project_id = ProjectId::from_uuid(Uuid::nil());
    let item_id = ItemId::from_uuid(Uuid::nil());
    let revision_id = ItemRevisionId::from_uuid(Uuid::nil());
    let project = ProjectBuilder::new(unique_temp_path("ingot-convergence"))
        .id(project_id)
        .created_at(now)
        .build();
    let item = ItemBuilder::new(project_id, revision_id)
        .id(item_id)
        .created_at(now)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .explicit_seed("seed")
        .created_at(now)
        .build();
    let port = FakePort::with_queue_prepare_context(ConvergenceQueuePrepareContext {
        project,
        item,
        revision,
        jobs: vec![fake_completed_validate_job("prepare_convergence")],
        findings: vec![],
        convergences: vec![],
        active_queue_entry: None,
        lane_head: None,
    });
    let service = ConvergenceService::new(port.clone());

    service
        .queue_prepare(project_id, item_id)
        .await
        .expect("queue prepare");

    let calls = port.calls();
    assert!(calls.iter().any(|call| call.starts_with("create_queue:")));
    assert!(
        calls
            .iter()
            .any(|call| call == "activity:ConvergenceQueued")
    );
}

#[tokio::test]
async fn invalidation_wins_first() {
    let port = FakePort::with_projects(vec![project_state("invalidate_prepared_convergence")]);
    let service = ConvergenceService::new(port.clone());

    let made_progress = service
        .tick_system_actions()
        .await
        .expect("tick system actions");

    assert!(made_progress);
    assert!(
        port.calls()
            .iter()
            .any(|call| call.starts_with("invalidate:"))
    );
}

#[tokio::test]
async fn prepare_runs_for_queue_head() {
    let port = FakePort::with_projects(vec![project_state("prepare_convergence")]);
    let service = ConvergenceService::new(port.clone());

    let made_progress = service
        .tick_system_actions()
        .await
        .expect("tick system actions");

    assert!(made_progress);
    assert!(port.calls().iter().any(|call| call.starts_with("prepare:")));
}

#[tokio::test]
async fn blocked_auto_finalize_does_not_count_as_progress() {
    let port = FakePort {
        auto_finalize_progress: false,
        ..FakePort::with_projects(vec![project_state("finalize_prepared_convergence")])
    };
    let service = ConvergenceService::new(port.clone());

    let made_progress = service
        .tick_system_actions()
        .await
        .expect("tick system actions");

    assert!(!made_progress);
    let calls = port.calls();
    assert!(calls.iter().any(|call| call.starts_with("finalize:")));
    assert!(!calls.iter().any(|call| call.starts_with("prepare:")));
}

#[tokio::test]
async fn blocked_auto_finalize_allows_later_system_action_to_run() {
    let port = FakePort {
        auto_finalize_progress: false,
        ..FakePort::with_projects(vec![
            project_state("finalize_prepared_convergence"),
            project_state("prepare_convergence"),
        ])
    };
    let service = ConvergenceService::new(port.clone());

    let made_progress = service
        .tick_system_actions()
        .await
        .expect("tick system actions");

    assert!(made_progress);
    let calls = port.calls();
    let finalize_index = calls
        .iter()
        .position(|call| call.starts_with("finalize:"))
        .expect("finalize call");
    let prepare_index = calls
        .iter()
        .position(|call| call.starts_with("prepare:"))
        .expect("prepare call");
    assert!(finalize_index < prepare_index);
}

#[tokio::test]
async fn approve_item_returns_stale_when_readiness_is_stale() {
    let mut context = FakePort::default_approval_context();
    context.finalize_readiness = ApprovalFinalizeReadiness::PreparedConvergenceStale;
    let port = FakePort::with_approval_context(context);
    let service = ConvergenceService::new(port);

    let error = service
        .approve_item(
            ProjectId::from_uuid(Uuid::nil()),
            ItemId::from_uuid(Uuid::nil()),
        )
        .await
        .expect_err("approval should reject stale convergence");

    assert!(matches!(error, UseCaseError::PreparedConvergenceStale));
}

#[tokio::test]
async fn approve_item_uses_shared_finalizer_for_already_finalized_target() {
    let port = FakePort {
        finalize_target_ref_result: FinalizeTargetRefResult::AlreadyFinalized,
        ..FakePort::with_approval_context(FakePort::default_approval_context())
    };
    let service = ConvergenceService::new(port.clone());

    service
        .approve_item(
            ProjectId::from_uuid(Uuid::nil()),
            ItemId::from_uuid(Uuid::nil()),
        )
        .await
        .expect("approval should finalize");

    let calls = port.calls();
    assert!(
        calls
            .iter()
            .any(|call| call == "update_op:Applied" || call == "update_op:Reconciled")
    );
    assert!(
        calls
            .iter()
            .any(|call| call.starts_with("finalize_target_ref:"))
    );
    assert!(
        calls
            .iter()
            .any(|call| call.starts_with("apply_successful_finalization:ApprovalCommand:"))
    );
}

#[tokio::test]
async fn approve_item_keeps_finalize_operation_unresolved_when_success_persistence_fails() {
    let port = FakePort {
        apply_successful_finalization_should_fail: true,
        ..FakePort::with_approval_context(FakePort::default_approval_context())
    };
    let service = ConvergenceService::new(port.clone());

    let error = service
        .approve_item(
            ProjectId::from_uuid(Uuid::nil()),
            ItemId::from_uuid(Uuid::nil()),
        )
        .await
        .expect_err("approval should surface persistence failure");

    assert!(matches!(error, UseCaseError::Internal(message) if message == "boom"));
    let calls = port.calls();
    assert!(calls.iter().any(|call| call == "update_op:Applied"));
    assert!(!calls.iter().any(|call| call == "update_op:Reconciled"));
}
