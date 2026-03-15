use std::sync::Arc;

use chrono::{Duration as ChronoDuration, Utc};
use ingot_agent_runtime::{DispatcherConfig, JobDispatcher};
use ingot_domain::activity::ActivityEventType;
use ingot_domain::convergence::{Convergence, ConvergenceStatus, ConvergenceStrategy};
use ingot_domain::convergence_queue::{ConvergenceQueueEntry, ConvergenceQueueEntryStatus};
use ingot_domain::finding::FindingTriageState;
use ingot_domain::git_operation::{GitEntityType, GitOperation, GitOperationStatus, OperationKind};
use ingot_domain::ids::{GitOperationId, WorkspaceId};
use ingot_domain::item::{
    ApprovalState, DoneReason, EscalationReason, LifecycleState,
    ResolutionSource,
};
use ingot_domain::job::{
    ContextPolicy, ExecutionPermission, Job, JobInput, JobStatus, OutcomeClass,
    OutputArtifactKind, PhaseKind,
};
use ingot_domain::project::Project;
use ingot_domain::revision::ItemRevision;
use ingot_domain::workspace::{
    RetentionPolicy, Workspace, WorkspaceKind, WorkspaceStatus, WorkspaceStrategy,
};
use ingot_git::commands::{compare_and_swap_ref, delete_ref, head_oid};
use ingot_store_sqlite::Database;
use ingot_usecases::ProjectLocks;
use ingot_workflow::step;
use uuid::Uuid;

use super::helpers::*;

#[tokio::test]
async fn reconcile_startup_expires_stale_running_jobs_and_marks_workspace_stale() {
    let h = TestHarness::new(Arc::new(FakeRunner)).await;

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let seed_commit = head_oid(&h.repo_path).await.expect("seed head");

    let item = ingot_domain::item::Item {
        id: item_id,
        current_revision_id: revision_id,
        ..test_item(h.project.id, revision_id)
    };
    let revision = ItemRevision {
        id: revision_id,
        item_id,
        ..test_revision(item_id, &seed_commit)
    };
    h.db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let created_at = Utc::now();
    let workspace = Workspace {
        id: WorkspaceId::new(),
        project_id: h.project.id,
        kind: WorkspaceKind::Authoring,
        strategy: WorkspaceStrategy::Worktree,
        path: std::env::temp_dir()
            .join(format!("ingot-runtime-stale-workspace-{}", Uuid::now_v7()))
            .display()
            .to_string(),
        created_for_revision_id: Some(revision.id),
        parent_workspace_id: None,
        target_ref: Some("refs/heads/main".into()),
        workspace_ref: Some("refs/ingot/workspaces/reconcile".into()),
        base_commit_oid: Some(seed_commit.clone()),
        head_commit_oid: Some(seed_commit.clone()),
        retention_policy: RetentionPolicy::Persistent,
        status: WorkspaceStatus::Busy,
        current_job_id: None,
        created_at,
        updated_at: created_at,
    };
    h.db.create_workspace(&workspace)
        .await
        .expect("create workspace");

    let stale_job = Job {
        status: JobStatus::Running,
        workspace_id: Some(workspace.id),
        lease_owner_id: Some("old-daemon".into()),
        heartbeat_at: Some(created_at),
        lease_expires_at: Some(created_at - ChronoDuration::minutes(1)),
        started_at: Some(created_at),
        ..test_authoring_job(h.project.id, item_id, revision_id, &seed_commit)
    };
    h.db.create_job(&stale_job).await.expect("create stale job");

    h.dispatcher
        .reconcile_startup()
        .await
        .expect("reconcile startup");

    let updated_job = h.db.get_job(stale_job.id).await.expect("updated job");
    assert_eq!(updated_job.status, JobStatus::Expired);
    assert_eq!(
        updated_job.outcome_class,
        Some(OutcomeClass::TransientFailure)
    );
    assert_eq!(updated_job.error_code.as_deref(), Some("heartbeat_expired"));

    let updated_workspace = h.db.get_workspace(workspace.id).await.expect("workspace");
    assert_eq!(updated_workspace.status, WorkspaceStatus::Stale);
    assert_eq!(updated_workspace.current_job_id, None);
}

#[tokio::test]
async fn reconcile_active_jobs_reports_progress_when_it_expires_a_running_job() {
    let h = TestHarness::new(Arc::new(FakeRunner)).await;

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let seed_commit = head_oid(&h.repo_path).await.expect("seed head");

    let item = ingot_domain::item::Item {
        id: item_id,
        current_revision_id: revision_id,
        ..test_item(h.project.id, revision_id)
    };
    let revision = ItemRevision {
        id: revision_id,
        item_id,
        ..test_revision(item_id, &seed_commit)
    };
    h.db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let created_at = Utc::now();
    let workspace = Workspace {
        id: WorkspaceId::new(),
        project_id: h.project.id,
        kind: WorkspaceKind::Authoring,
        strategy: WorkspaceStrategy::Worktree,
        path: std::env::temp_dir()
            .join(format!(
                "ingot-runtime-progress-workspace-{}",
                Uuid::now_v7()
            ))
            .display()
            .to_string(),
        created_for_revision_id: Some(revision.id),
        parent_workspace_id: None,
        target_ref: Some("refs/heads/main".into()),
        workspace_ref: Some("refs/ingot/workspaces/progress".into()),
        base_commit_oid: Some(seed_commit.clone()),
        head_commit_oid: Some(seed_commit.clone()),
        retention_policy: RetentionPolicy::Persistent,
        status: WorkspaceStatus::Busy,
        current_job_id: None,
        created_at,
        updated_at: created_at,
    };
    h.db.create_workspace(&workspace)
        .await
        .expect("create workspace");

    let stale_job = Job {
        status: JobStatus::Running,
        workspace_id: Some(workspace.id),
        lease_owner_id: Some("old-daemon".into()),
        heartbeat_at: Some(created_at),
        lease_expires_at: Some(created_at - ChronoDuration::minutes(1)),
        started_at: Some(created_at),
        ..test_authoring_job(h.project.id, item_id, revision_id, &seed_commit)
    };
    h.db.create_job(&stale_job).await.expect("create stale job");

    let made_progress = h
        .dispatcher
        .reconcile_active_jobs()
        .await
        .expect("reconcile active jobs");

    assert!(made_progress);
    let updated_job = h.db.get_job(stale_job.id).await.expect("updated job");
    assert_eq!(updated_job.status, JobStatus::Expired);
}

#[tokio::test]
async fn reconcile_startup_fails_inflight_convergences_and_marks_workspace_stale() {
    let h = TestHarness::new(Arc::new(FakeRunner)).await;

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let seed_commit = head_oid(&h.repo_path).await.expect("seed head");

    let item = ingot_domain::item::Item {
        id: item_id,
        current_revision_id: revision_id,
        ..test_item(h.project.id, revision_id)
    };
    let revision = ItemRevision {
        id: revision_id,
        item_id,
        ..test_revision(item_id, &seed_commit)
    };
    h.db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let created_at = Utc::now();
    let workspace = Workspace {
        id: WorkspaceId::new(),
        project_id: h.project.id,
        kind: WorkspaceKind::Integration,
        strategy: WorkspaceStrategy::Worktree,
        path: std::env::temp_dir()
            .join(format!("ingot-runtime-conv-workspace-{}", Uuid::now_v7()))
            .display()
            .to_string(),
        created_for_revision_id: Some(revision.id),
        parent_workspace_id: None,
        target_ref: Some("refs/heads/main".into()),
        workspace_ref: Some("refs/ingot/workspaces/reconcile-conv".into()),
        base_commit_oid: Some(seed_commit.clone()),
        head_commit_oid: Some(seed_commit.clone()),
        retention_policy: RetentionPolicy::Persistent,
        status: WorkspaceStatus::Busy,
        current_job_id: None,
        created_at,
        updated_at: created_at,
    };
    h.db.create_workspace(&workspace)
        .await
        .expect("create workspace");

    let convergence = Convergence {
        id: ingot_domain::ids::ConvergenceId::new(),
        project_id: h.project.id,
        item_id,
        item_revision_id: revision_id,
        source_workspace_id: workspace.id,
        integration_workspace_id: Some(workspace.id),
        source_head_commit_oid: seed_commit.clone(),
        target_ref: "refs/heads/main".into(),
        strategy: ConvergenceStrategy::RebaseThenFastForward,
        status: ConvergenceStatus::Running,
        input_target_commit_oid: Some(seed_commit.clone()),
        prepared_commit_oid: None,
        final_target_commit_oid: None,
        target_head_valid: None,
        conflict_summary: None,
        created_at,
        completed_at: None,
    };
    h.db.create_convergence(&convergence)
        .await
        .expect("create convergence");

    h.dispatcher
        .reconcile_startup()
        .await
        .expect("reconcile startup");

    let updated_convergence = h
        .db
        .list_convergences_by_item(item.id)
        .await
        .expect("list convergences")
        .into_iter()
        .next()
        .expect("convergence");
    assert_eq!(updated_convergence.status, ConvergenceStatus::Failed);
    assert_eq!(
        updated_convergence.conflict_summary.as_deref(),
        Some("startup_recovery_required")
    );

    let updated_workspace = h.db.get_workspace(workspace.id).await.expect("workspace");
    assert_eq!(updated_workspace.status, WorkspaceStatus::Stale);
}

#[tokio::test]
async fn reconcile_startup_marks_finalized_target_ref_git_operation_reconciled() {
    let repo = temp_git_repo();
    let base_commit = head_oid(&repo).await.expect("base head");
    std::fs::write(repo.join("tracked.txt"), "next").expect("write file");
    git_sync(&repo, &["add", "tracked.txt"]);
    git_sync(&repo, &["commit", "-m", "next"]);
    let new_head = head_oid(&repo).await.expect("new head");

    let db_path = std::env::temp_dir().join(format!("ingot-runtime-gop-{}.db", Uuid::now_v7()));
    let db = Database::connect(&db_path).await.expect("connect db");
    db.migrate().await.expect("migrate db");
    let dispatcher = JobDispatcher::with_runner(
        db.clone(),
        ProjectLocks::default(),
        DispatcherConfig::new(
            std::env::temp_dir().join(format!("ingot-runtime-gop-state-{}", Uuid::now_v7())),
        ),
        Arc::new(FakeRunner),
    );

    let created_at = Utc::now();
    let project = Project {
        id: ingot_domain::ids::ProjectId::new(),
        name: "repo".into(),
        path: repo.display().to_string(),
        default_branch: "main".into(),
        color: "#000".into(),
        created_at,
        updated_at: created_at,
    };
    db.create_project(&project).await.expect("create project");

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let item = ingot_domain::item::Item {
        id: item_id,
        current_revision_id: revision_id,
        ..test_item(project.id, revision_id)
    };
    let revision = ItemRevision {
        id: revision_id,
        item_id,
        seed_commit_oid: Some(base_commit.clone()),
        seed_target_commit_oid: Some(base_commit.clone()),
        ..test_revision(item_id, &base_commit)
    };
    db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let workspace = Workspace {
        id: WorkspaceId::new(),
        project_id: project.id,
        kind: WorkspaceKind::Integration,
        strategy: WorkspaceStrategy::Worktree,
        path: repo.display().to_string(),
        created_for_revision_id: Some(revision.id),
        parent_workspace_id: None,
        target_ref: Some("refs/heads/main".into()),
        workspace_ref: Some("refs/ingot/workspaces/finalize-adopt".into()),
        base_commit_oid: Some(base_commit.clone()),
        head_commit_oid: Some(new_head.clone()),
        retention_policy: RetentionPolicy::Persistent,
        status: WorkspaceStatus::Ready,
        current_job_id: None,
        created_at,
        updated_at: created_at,
    };
    db.create_workspace(&workspace)
        .await
        .expect("create workspace");

    let convergence = Convergence {
        id: ingot_domain::ids::ConvergenceId::new(),
        project_id: project.id,
        item_id,
        item_revision_id: revision_id,
        source_workspace_id: workspace.id,
        integration_workspace_id: None,
        source_head_commit_oid: new_head.clone(),
        target_ref: "refs/heads/main".into(),
        strategy: ConvergenceStrategy::RebaseThenFastForward,
        status: ConvergenceStatus::Prepared,
        input_target_commit_oid: Some(base_commit.clone()),
        prepared_commit_oid: Some(new_head.clone()),
        final_target_commit_oid: None,
        target_head_valid: Some(true),
        conflict_summary: None,
        created_at,
        completed_at: None,
    };
    db.create_convergence(&convergence)
        .await
        .expect("create convergence");

    let operation = GitOperation {
        id: GitOperationId::new(),
        project_id: project.id,
        operation_kind: OperationKind::FinalizeTargetRef,
        entity_type: GitEntityType::Convergence,
        entity_id: convergence.id.to_string(),
        workspace_id: None,
        ref_name: Some("refs/heads/main".into()),
        expected_old_oid: Some(base_commit.clone()),
        new_oid: Some(new_head.clone()),
        commit_oid: Some(new_head),
        status: GitOperationStatus::Applied,
        metadata: None,
        created_at,
        completed_at: None,
    };
    db.create_git_operation(&operation)
        .await
        .expect("create git operation");

    dispatcher
        .reconcile_startup()
        .await
        .expect("reconcile startup");

    let operations = db
        .list_unresolved_git_operations()
        .await
        .expect("list unresolved");
    assert!(operations.is_empty(), "operation should be reconciled");

    let updated_convergence = db
        .get_convergence(convergence.id)
        .await
        .expect("convergence");
    assert_eq!(updated_convergence.status, ConvergenceStatus::Finalized);
    assert_eq!(
        updated_convergence.final_target_commit_oid.as_deref(),
        Some(
            operation
                .commit_oid
                .as_deref()
                .expect("operation commit oid")
        )
    );

    let updated_item = db.get_item(item.id).await.expect("item");
    assert_eq!(updated_item.lifecycle_state, LifecycleState::Done);
    assert_eq!(updated_item.done_reason, Some(DoneReason::Completed));
    assert_eq!(updated_item.approval_state, ApprovalState::Approved);
    assert_eq!(
        updated_item.resolution_source,
        Some(ResolutionSource::ApprovalCommand)
    );

    let activity = db
        .list_activity_by_project(project.id, 10, 0)
        .await
        .expect("list activity");
    assert!(
        activity
            .iter()
            .any(|row| row.event_type == ActivityEventType::GitOperationReconciled)
    );
}

#[tokio::test]
async fn reconcile_startup_syncs_checkout_before_adopting_finalize() {
    let repo = temp_git_repo();
    let base_commit = head_oid(&repo).await.expect("base head");
    std::fs::write(repo.join("tracked.txt"), "prepared").expect("write prepared");
    git_sync(&repo, &["add", "tracked.txt"]);
    git_sync(&repo, &["commit", "-m", "prepared"]);
    let prepared_commit = head_oid(&repo).await.expect("prepared head");
    git_sync(
        &repo,
        &[
            "update-ref",
            "refs/ingot/tests/finalize-prepared",
            &prepared_commit,
        ],
    );
    git_sync(&repo, &["reset", "--hard", &base_commit]);

    let db_path =
        std::env::temp_dir().join(format!("ingot-runtime-finalize-sync-{}.db", Uuid::now_v7()));
    let db = Database::connect(&db_path).await.expect("connect db");
    db.migrate().await.expect("migrate db");
    let dispatcher = JobDispatcher::with_runner(
        db.clone(),
        ProjectLocks::default(),
        DispatcherConfig::new(std::env::temp_dir().join(format!(
            "ingot-runtime-finalize-sync-state-{}",
            Uuid::now_v7()
        ))),
        Arc::new(FakeRunner),
    );

    let created_at = Utc::now();
    let project = Project {
        id: ingot_domain::ids::ProjectId::new(),
        name: "repo".into(),
        path: repo.display().to_string(),
        default_branch: "main".into(),
        color: "#000".into(),
        created_at,
        updated_at: created_at,
    };
    db.create_project(&project).await.expect("create project");

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let item = ingot_domain::item::Item {
        id: item_id,
        current_revision_id: revision_id,
        approval_state: ApprovalState::Granted,
        ..test_item(project.id, revision_id)
    };
    let revision = ItemRevision {
        id: revision_id,
        item_id,
        seed_commit_oid: Some(base_commit.clone()),
        seed_target_commit_oid: Some(base_commit.clone()),
        ..test_revision(item_id, &base_commit)
    };
    db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");
    let source_workspace = Workspace {
        id: WorkspaceId::new(),
        project_id: project.id,
        kind: WorkspaceKind::Authoring,
        strategy: WorkspaceStrategy::Worktree,
        path: std::env::temp_dir()
            .join(format!("ingot-runtime-finalize-source-{}", Uuid::now_v7()))
            .display()
            .to_string(),
        created_for_revision_id: Some(revision.id),
        parent_workspace_id: None,
        target_ref: Some("refs/heads/main".into()),
        workspace_ref: Some("refs/ingot/workspaces/finalize-source".into()),
        base_commit_oid: Some(base_commit.clone()),
        head_commit_oid: Some(prepared_commit.clone()),
        retention_policy: RetentionPolicy::Persistent,
        status: WorkspaceStatus::Ready,
        current_job_id: None,
        created_at,
        updated_at: created_at,
    };
    db.create_workspace(&source_workspace)
        .await
        .expect("create source workspace");

    let convergence = Convergence {
        id: ingot_domain::ids::ConvergenceId::new(),
        project_id: project.id,
        item_id,
        item_revision_id: revision_id,
        source_workspace_id: source_workspace.id,
        integration_workspace_id: None,
        source_head_commit_oid: prepared_commit.clone(),
        target_ref: "refs/heads/main".into(),
        strategy: ConvergenceStrategy::RebaseThenFastForward,
        status: ConvergenceStatus::Prepared,
        input_target_commit_oid: Some(base_commit.clone()),
        prepared_commit_oid: Some(prepared_commit.clone()),
        final_target_commit_oid: None,
        target_head_valid: Some(true),
        conflict_summary: None,
        created_at,
        completed_at: None,
    };
    db.create_convergence(&convergence)
        .await
        .expect("create convergence");
    db.create_queue_entry(&ConvergenceQueueEntry {
        id: ingot_domain::ids::ConvergenceQueueEntryId::new(),
        project_id: project.id,
        item_id,
        item_revision_id: revision_id,
        target_ref: "refs/heads/main".into(),
        status: ConvergenceQueueEntryStatus::Head,
        head_acquired_at: Some(created_at),
        created_at,
        updated_at: created_at,
        released_at: None,
    })
    .await
    .expect("insert queue entry");

    let paths = dispatcher
        .refresh_project_mirror(&project)
        .await
        .expect("refresh mirror");
    compare_and_swap_ref(
        paths.mirror_git_dir.as_path(),
        "refs/heads/main",
        &prepared_commit,
        &base_commit,
    )
    .await
    .expect("move mirror ref");

    let operation = GitOperation {
        id: GitOperationId::new(),
        project_id: project.id,
        operation_kind: OperationKind::FinalizeTargetRef,
        entity_type: GitEntityType::Convergence,
        entity_id: convergence.id.to_string(),
        workspace_id: None,
        ref_name: Some("refs/heads/main".into()),
        expected_old_oid: Some(base_commit.clone()),
        new_oid: Some(prepared_commit.clone()),
        commit_oid: Some(prepared_commit.clone()),
        status: GitOperationStatus::Applied,
        metadata: None,
        created_at,
        completed_at: Some(created_at),
    };
    db.create_git_operation(&operation)
        .await
        .expect("create git operation");

    assert_eq!(head_oid(&repo).await.expect("checkout head"), base_commit);

    dispatcher
        .reconcile_startup()
        .await
        .expect("reconcile startup");

    assert_eq!(head_oid(&repo).await.expect("synced head"), prepared_commit);
    let unresolved = db
        .list_unresolved_git_operations()
        .await
        .expect("list unresolved");
    assert!(unresolved.is_empty(), "finalize op should reconcile");
    let updated_item = db.get_item(item.id).await.expect("item");
    assert_eq!(updated_item.lifecycle_state, LifecycleState::Done);
    let queue_entries = db
        .list_queue_entries_by_item(item.id)
        .await
        .expect("list queue entries");
    assert_eq!(
        queue_entries[0].status,
        ConvergenceQueueEntryStatus::Released
    );
}

#[tokio::test]
async fn reconcile_startup_leaves_finalize_open_when_checkout_sync_is_blocked() {
    let repo = temp_git_repo();
    let base_commit = head_oid(&repo).await.expect("base head");
    std::fs::write(repo.join("tracked.txt"), "prepared").expect("write prepared");
    git_sync(&repo, &["add", "tracked.txt"]);
    git_sync(&repo, &["commit", "-m", "prepared"]);
    let prepared_commit = head_oid(&repo).await.expect("prepared head");
    git_sync(
        &repo,
        &[
            "update-ref",
            "refs/ingot/tests/finalize-prepared",
            &prepared_commit,
        ],
    );
    git_sync(&repo, &["reset", "--hard", &base_commit]);
    git_sync(&repo, &["checkout", "-b", "feature"]);

    let db_path = std::env::temp_dir().join(format!(
        "ingot-runtime-finalize-blocked-{}.db",
        Uuid::now_v7()
    ));
    let db = Database::connect(&db_path).await.expect("connect db");
    db.migrate().await.expect("migrate db");
    let dispatcher = JobDispatcher::with_runner(
        db.clone(),
        ProjectLocks::default(),
        DispatcherConfig::new(std::env::temp_dir().join(format!(
            "ingot-runtime-finalize-blocked-state-{}",
            Uuid::now_v7()
        ))),
        Arc::new(FakeRunner),
    );

    let created_at = Utc::now();
    let project = Project {
        id: ingot_domain::ids::ProjectId::new(),
        name: "repo".into(),
        path: repo.display().to_string(),
        default_branch: "main".into(),
        color: "#000".into(),
        created_at,
        updated_at: created_at,
    };
    db.create_project(&project).await.expect("create project");

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let item = ingot_domain::item::Item {
        id: item_id,
        current_revision_id: revision_id,
        approval_state: ApprovalState::Granted,
        ..test_item(project.id, revision_id)
    };
    let revision = ItemRevision {
        id: revision_id,
        item_id,
        seed_commit_oid: Some(base_commit.clone()),
        seed_target_commit_oid: Some(base_commit.clone()),
        ..test_revision(item_id, &base_commit)
    };
    db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");
    let source_workspace = Workspace {
        id: WorkspaceId::new(),
        project_id: project.id,
        kind: WorkspaceKind::Authoring,
        strategy: WorkspaceStrategy::Worktree,
        path: std::env::temp_dir()
            .join(format!(
                "ingot-runtime-finalize-blocked-source-{}",
                Uuid::now_v7()
            ))
            .display()
            .to_string(),
        created_for_revision_id: Some(revision.id),
        parent_workspace_id: None,
        target_ref: Some("refs/heads/main".into()),
        workspace_ref: Some("refs/ingot/workspaces/finalize-blocked-source".into()),
        base_commit_oid: Some(base_commit.clone()),
        head_commit_oid: Some(prepared_commit.clone()),
        retention_policy: RetentionPolicy::Persistent,
        status: WorkspaceStatus::Ready,
        current_job_id: None,
        created_at,
        updated_at: created_at,
    };
    db.create_workspace(&source_workspace)
        .await
        .expect("create source workspace");

    let convergence = Convergence {
        id: ingot_domain::ids::ConvergenceId::new(),
        project_id: project.id,
        item_id,
        item_revision_id: revision_id,
        source_workspace_id: source_workspace.id,
        integration_workspace_id: None,
        source_head_commit_oid: prepared_commit.clone(),
        target_ref: "refs/heads/main".into(),
        strategy: ConvergenceStrategy::RebaseThenFastForward,
        status: ConvergenceStatus::Prepared,
        input_target_commit_oid: Some(base_commit.clone()),
        prepared_commit_oid: Some(prepared_commit.clone()),
        final_target_commit_oid: None,
        target_head_valid: Some(true),
        conflict_summary: None,
        created_at,
        completed_at: None,
    };
    db.create_convergence(&convergence)
        .await
        .expect("create convergence");
    db.create_queue_entry(&ConvergenceQueueEntry {
        id: ingot_domain::ids::ConvergenceQueueEntryId::new(),
        project_id: project.id,
        item_id,
        item_revision_id: revision_id,
        target_ref: "refs/heads/main".into(),
        status: ConvergenceQueueEntryStatus::Head,
        head_acquired_at: Some(created_at),
        created_at,
        updated_at: created_at,
        released_at: None,
    })
    .await
    .expect("insert queue entry");

    let paths = dispatcher
        .refresh_project_mirror(&project)
        .await
        .expect("refresh mirror");
    compare_and_swap_ref(
        paths.mirror_git_dir.as_path(),
        "refs/heads/main",
        &prepared_commit,
        &base_commit,
    )
    .await
    .expect("move mirror ref");

    let operation = GitOperation {
        id: GitOperationId::new(),
        project_id: project.id,
        operation_kind: OperationKind::FinalizeTargetRef,
        entity_type: GitEntityType::Convergence,
        entity_id: convergence.id.to_string(),
        workspace_id: None,
        ref_name: Some("refs/heads/main".into()),
        expected_old_oid: Some(base_commit),
        new_oid: Some(prepared_commit),
        commit_oid: Some(
            convergence
                .prepared_commit_oid
                .clone()
                .expect("prepared oid"),
        ),
        status: GitOperationStatus::Applied,
        metadata: None,
        created_at,
        completed_at: Some(created_at),
    };
    db.create_git_operation(&operation)
        .await
        .expect("create git operation");

    dispatcher
        .reconcile_startup()
        .await
        .expect("reconcile startup");

    let unresolved = db
        .list_unresolved_git_operations()
        .await
        .expect("list unresolved");
    assert_eq!(
        unresolved.len(),
        1,
        "blocked finalize should stay unresolved"
    );
    let updated_item = db.get_item(item.id).await.expect("item");
    assert_eq!(updated_item.lifecycle_state, LifecycleState::Open);
    assert_eq!(
        updated_item.escalation_reason,
        Some(EscalationReason::CheckoutSyncBlocked)
    );
    let updated_convergence = db
        .get_convergence(convergence.id)
        .await
        .expect("convergence");
    assert_eq!(updated_convergence.status, ConvergenceStatus::Prepared);
    let queue_entries = db
        .list_queue_entries_by_item(item.id)
        .await
        .expect("list queue entries");
    assert_eq!(queue_entries[0].status, ConvergenceQueueEntryStatus::Head);
}

#[tokio::test]
async fn reconcile_startup_adopts_prepared_convergence_from_git_operation() {
    let repo = temp_git_repo();
    let base_commit = head_oid(&repo).await.expect("base head");
    std::fs::write(repo.join("tracked.txt"), "prepared").expect("write file");
    git_sync(&repo, &["add", "tracked.txt"]);
    git_sync(&repo, &["commit", "-m", "prepared"]);
    let prepared_head = head_oid(&repo).await.expect("prepared head");
    git_sync(
        &repo,
        &[
            "update-ref",
            "refs/ingot/workspaces/prepare-adopt",
            &prepared_head,
        ],
    );
    git_sync(&repo, &["reset", "--hard", &base_commit]);

    let db_path =
        std::env::temp_dir().join(format!("ingot-runtime-prepare-adopt-{}.db", Uuid::now_v7()));
    let db = Database::connect(&db_path).await.expect("connect db");
    db.migrate().await.expect("migrate db");
    let dispatcher = JobDispatcher::with_runner(
        db.clone(),
        ProjectLocks::default(),
        DispatcherConfig::new(std::env::temp_dir().join(format!(
            "ingot-runtime-prepare-adopt-state-{}",
            Uuid::now_v7()
        ))),
        Arc::new(FakeRunner),
    );

    let created_at = Utc::now();
    let project = Project {
        id: ingot_domain::ids::ProjectId::new(),
        name: "repo".into(),
        path: repo.display().to_string(),
        default_branch: "main".into(),
        color: "#000".into(),
        created_at,
        updated_at: created_at,
    };
    db.create_project(&project).await.expect("create project");

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let item = ingot_domain::item::Item {
        id: item_id,
        current_revision_id: revision_id,
        ..test_item(project.id, revision_id)
    };
    let revision = ItemRevision {
        id: revision_id,
        item_id,
        seed_commit_oid: Some(base_commit.clone()),
        seed_target_commit_oid: Some(base_commit.clone()),
        ..test_revision(item_id, &base_commit)
    };
    db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let workspace = Workspace {
        id: WorkspaceId::new(),
        project_id: project.id,
        kind: WorkspaceKind::Integration,
        strategy: WorkspaceStrategy::Worktree,
        path: repo.display().to_string(),
        created_for_revision_id: Some(revision.id),
        parent_workspace_id: None,
        target_ref: Some("refs/heads/main".into()),
        workspace_ref: Some("refs/ingot/workspaces/prepare-adopt".into()),
        base_commit_oid: Some(base_commit.clone()),
        head_commit_oid: Some(base_commit.clone()),
        retention_policy: RetentionPolicy::Persistent,
        status: WorkspaceStatus::Busy,
        current_job_id: None,
        created_at,
        updated_at: created_at,
    };
    db.create_workspace(&workspace)
        .await
        .expect("create workspace");

    let convergence = Convergence {
        id: ingot_domain::ids::ConvergenceId::new(),
        project_id: project.id,
        item_id,
        item_revision_id: revision_id,
        source_workspace_id: workspace.id,
        integration_workspace_id: Some(workspace.id),
        source_head_commit_oid: prepared_head.clone(),
        target_ref: "refs/heads/main".into(),
        strategy: ConvergenceStrategy::RebaseThenFastForward,
        status: ConvergenceStatus::Running,
        input_target_commit_oid: Some(base_commit.clone()),
        prepared_commit_oid: None,
        final_target_commit_oid: None,
        target_head_valid: None,
        conflict_summary: None,
        created_at,
        completed_at: None,
    };
    db.create_convergence(&convergence)
        .await
        .expect("create convergence");

    let operation = GitOperation {
        id: GitOperationId::new(),
        project_id: project.id,
        operation_kind: OperationKind::PrepareConvergenceCommit,
        entity_type: GitEntityType::Convergence,
        entity_id: convergence.id.to_string(),
        workspace_id: Some(workspace.id),
        ref_name: workspace.workspace_ref.clone(),
        expected_old_oid: workspace.base_commit_oid.clone(),
        new_oid: Some(prepared_head.clone()),
        commit_oid: Some(prepared_head.clone()),
        status: GitOperationStatus::Applied,
        metadata: Some(serde_json::json!({
            "source_commit_oids": [prepared_head],
            "prepared_commit_oids": [prepared_head]
        })),
        created_at,
        completed_at: None,
    };
    db.create_git_operation(&operation)
        .await
        .expect("create operation");

    dispatcher
        .reconcile_startup()
        .await
        .expect("reconcile startup");

    let updated_convergence = db
        .get_convergence(convergence.id)
        .await
        .expect("convergence");
    assert_eq!(updated_convergence.status, ConvergenceStatus::Prepared);
    assert!(updated_convergence.prepared_commit_oid.is_some());
    let updated_workspace = db.get_workspace(workspace.id).await.expect("workspace");
    assert_eq!(updated_workspace.status, WorkspaceStatus::Ready);
    assert_eq!(
        updated_workspace.head_commit_oid,
        updated_convergence.prepared_commit_oid
    );
    let unresolved = db
        .list_unresolved_git_operations()
        .await
        .expect("list unresolved");
    assert!(unresolved.is_empty(), "prepare op should reconcile");
}

#[tokio::test]
async fn reconcile_startup_does_not_resurrect_cancelled_convergence_from_prepare_git_operation()
{
    let repo = temp_git_repo();
    let base_commit = head_oid(&repo).await.expect("base head");
    std::fs::write(repo.join("tracked.txt"), "prepared").expect("write file");
    git_sync(&repo, &["add", "tracked.txt"]);
    git_sync(&repo, &["commit", "-m", "prepared"]);
    let prepared_head = head_oid(&repo).await.expect("prepared head");
    git_sync(
        &repo,
        &[
            "update-ref",
            "refs/ingot/workspaces/prepare-cancelled",
            &prepared_head,
        ],
    );
    git_sync(&repo, &["reset", "--hard", &base_commit]);

    let db_path = std::env::temp_dir().join(format!(
        "ingot-runtime-prepare-cancelled-{}.db",
        Uuid::now_v7()
    ));
    let db = Database::connect(&db_path).await.expect("connect db");
    db.migrate().await.expect("migrate db");
    let dispatcher = JobDispatcher::with_runner(
        db.clone(),
        ProjectLocks::default(),
        DispatcherConfig::new(std::env::temp_dir().join(format!(
            "ingot-runtime-prepare-cancelled-state-{}",
            Uuid::now_v7()
        ))),
        Arc::new(FakeRunner),
    );

    let created_at = Utc::now();
    let project = Project {
        id: ingot_domain::ids::ProjectId::new(),
        name: "repo".into(),
        path: repo.display().to_string(),
        default_branch: "main".into(),
        color: "#000".into(),
        created_at,
        updated_at: created_at,
    };
    db.create_project(&project).await.expect("create project");

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let item = ingot_domain::item::Item {
        id: item_id,
        current_revision_id: revision_id,
        ..test_item(project.id, revision_id)
    };
    let revision = ItemRevision {
        id: revision_id,
        item_id,
        seed_commit_oid: Some(base_commit.clone()),
        seed_target_commit_oid: Some(base_commit.clone()),
        ..test_revision(item_id, &base_commit)
    };
    db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let workspace = Workspace {
        id: WorkspaceId::new(),
        project_id: project.id,
        kind: WorkspaceKind::Integration,
        strategy: WorkspaceStrategy::Worktree,
        path: repo.display().to_string(),
        created_for_revision_id: Some(revision.id),
        parent_workspace_id: None,
        target_ref: Some("refs/heads/main".into()),
        workspace_ref: Some("refs/ingot/workspaces/prepare-cancelled".into()),
        base_commit_oid: Some(base_commit.clone()),
        head_commit_oid: Some(prepared_head.clone()),
        retention_policy: RetentionPolicy::Persistent,
        status: WorkspaceStatus::Abandoned,
        current_job_id: None,
        created_at,
        updated_at: created_at,
    };
    db.create_workspace(&workspace)
        .await
        .expect("create workspace");

    let convergence = Convergence {
        id: ingot_domain::ids::ConvergenceId::new(),
        project_id: project.id,
        item_id,
        item_revision_id: revision_id,
        source_workspace_id: workspace.id,
        integration_workspace_id: Some(workspace.id),
        source_head_commit_oid: prepared_head.clone(),
        target_ref: "refs/heads/main".into(),
        strategy: ConvergenceStrategy::RebaseThenFastForward,
        status: ConvergenceStatus::Cancelled,
        input_target_commit_oid: Some(base_commit),
        prepared_commit_oid: Some(prepared_head.clone()),
        final_target_commit_oid: None,
        target_head_valid: None,
        conflict_summary: None,
        created_at,
        completed_at: Some(created_at),
    };
    db.create_convergence(&convergence)
        .await
        .expect("create convergence");

    let operation = GitOperation {
        id: GitOperationId::new(),
        project_id: project.id,
        operation_kind: OperationKind::PrepareConvergenceCommit,
        entity_type: GitEntityType::Convergence,
        entity_id: convergence.id.to_string(),
        workspace_id: Some(workspace.id),
        ref_name: workspace.workspace_ref.clone(),
        expected_old_oid: workspace.base_commit_oid.clone(),
        new_oid: Some(prepared_head.clone()),
        commit_oid: Some(prepared_head),
        status: GitOperationStatus::Applied,
        metadata: Some(serde_json::json!({
            "source_commit_oids": [],
            "prepared_commit_oids": []
        })),
        created_at,
        completed_at: None,
    };
    db.create_git_operation(&operation)
        .await
        .expect("create operation");

    dispatcher
        .reconcile_startup()
        .await
        .expect("reconcile startup");

    let updated_convergence = db
        .get_convergence(convergence.id)
        .await
        .expect("convergence");
    assert_eq!(updated_convergence.status, ConvergenceStatus::Cancelled);
    let updated_workspace = db.get_workspace(workspace.id).await.expect("workspace");
    assert_eq!(updated_workspace.status, WorkspaceStatus::Abandoned);
    let unresolved = db
        .list_unresolved_git_operations()
        .await
        .expect("list unresolved");
    assert!(unresolved.is_empty(), "cancelled prepare op should resolve");
}

#[tokio::test]
async fn reconcile_startup_adopts_create_job_commit_into_completed_job() {
    let repo = temp_git_repo();
    let base_commit = head_oid(&repo).await.expect("base head");
    std::fs::write(repo.join("tracked.txt"), "authored").expect("write file");
    git_sync(&repo, &["add", "tracked.txt"]);
    git_sync(&repo, &["commit", "-m", "authored"]);
    let authored_commit = head_oid(&repo).await.expect("authored head");
    git_sync(
        &repo,
        &[
            "update-ref",
            "refs/ingot/workspaces/adopt-job",
            &authored_commit,
        ],
    );
    git_sync(&repo, &["reset", "--hard", &base_commit]);

    let db_path =
        std::env::temp_dir().join(format!("ingot-runtime-adopt-job-{}.db", Uuid::now_v7()));
    let db = Database::connect(&db_path).await.expect("connect db");
    db.migrate().await.expect("migrate db");
    let dispatcher = JobDispatcher::with_runner(
        db.clone(),
        ProjectLocks::default(),
        DispatcherConfig::new(
            std::env::temp_dir()
                .join(format!("ingot-runtime-adopt-job-state-{}", Uuid::now_v7())),
        ),
        Arc::new(FakeRunner),
    );

    let created_at = Utc::now();
    let project = Project {
        id: ingot_domain::ids::ProjectId::new(),
        name: "repo".into(),
        path: repo.display().to_string(),
        default_branch: "main".into(),
        color: "#000".into(),
        created_at,
        updated_at: created_at,
    };
    db.create_project(&project).await.expect("create project");

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let item = ingot_domain::item::Item {
        id: item_id,
        current_revision_id: revision_id,
        ..test_item(project.id, revision_id)
    };
    let revision = ItemRevision {
        id: revision_id,
        item_id,
        seed_commit_oid: Some(base_commit.clone()),
        seed_target_commit_oid: Some(base_commit.clone()),
        ..test_revision(item_id, &base_commit)
    };
    db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let workspace = Workspace {
        id: WorkspaceId::new(),
        project_id: project.id,
        kind: WorkspaceKind::Authoring,
        strategy: WorkspaceStrategy::Worktree,
        path: repo.display().to_string(),
        created_for_revision_id: Some(revision.id),
        parent_workspace_id: None,
        target_ref: Some("refs/heads/main".into()),
        workspace_ref: Some("refs/ingot/workspaces/adopt-job".into()),
        base_commit_oid: Some(base_commit.clone()),
        head_commit_oid: Some(base_commit.clone()),
        retention_policy: RetentionPolicy::Persistent,
        status: WorkspaceStatus::Busy,
        current_job_id: None,
        created_at,
        updated_at: created_at,
    };
    db.create_workspace(&workspace)
        .await
        .expect("create workspace");

    let job = Job {
        status: JobStatus::Running,
        workspace_id: Some(workspace.id),
        lease_owner_id: Some("old-daemon".into()),
        heartbeat_at: Some(created_at),
        lease_expires_at: Some(created_at + ChronoDuration::minutes(5)),
        started_at: Some(created_at),
        job_input: JobInput::authoring_head(base_commit.clone()),
        ..test_authoring_job(project.id, item_id, revision_id, &base_commit)
    };
    db.create_job(&job).await.expect("create job");

    let operation = GitOperation {
        id: GitOperationId::new(),
        project_id: project.id,
        operation_kind: OperationKind::CreateJobCommit,
        entity_type: GitEntityType::Job,
        entity_id: job.id.to_string(),
        workspace_id: Some(workspace.id),
        ref_name: workspace.workspace_ref.clone(),
        expected_old_oid: Some(base_commit.clone()),
        new_oid: Some(authored_commit.clone()),
        commit_oid: Some(authored_commit.clone()),
        status: GitOperationStatus::Applied,
        metadata: None,
        created_at,
        completed_at: None,
    };
    db.create_git_operation(&operation)
        .await
        .expect("create operation");

    dispatcher
        .reconcile_startup()
        .await
        .expect("reconcile startup");

    let updated_job = db.get_job(job.id).await.expect("updated job");
    assert_eq!(updated_job.status, JobStatus::Completed);
    assert_eq!(updated_job.outcome_class, Some(OutcomeClass::Clean));
    assert_eq!(
        updated_job.output_commit_oid.as_deref(),
        Some(authored_commit.as_str())
    );

    let updated_workspace = db.get_workspace(workspace.id).await.expect("workspace");
    assert_eq!(updated_workspace.status, WorkspaceStatus::Ready);
    assert_eq!(
        updated_workspace.head_commit_oid.as_deref(),
        Some(authored_commit.as_str())
    );

    let jobs = db.list_jobs_by_item(item.id).await.expect("jobs");
    let review_job = jobs
        .iter()
        .find(|job| job.step_id == step::REVIEW_INCREMENTAL_INITIAL)
        .expect("auto-dispatched review job after startup adoption");
    assert_eq!(review_job.status, JobStatus::Queued);
    assert_eq!(
        review_job.job_input.base_commit_oid(),
        Some(base_commit.as_str())
    );
    assert_eq!(
        review_job.job_input.head_commit_oid(),
        Some(authored_commit.as_str())
    );
}

#[tokio::test]
async fn reconcile_startup_continues_review_recovery_past_broken_project() {
    let healthy_repo = temp_git_repo();
    let healthy_seed_commit = head_oid(&healthy_repo).await.expect("healthy seed head");
    std::fs::write(healthy_repo.join("feature.txt"), "authored").expect("write file");
    git_sync(&healthy_repo, &["add", "feature.txt"]);
    git_sync(&healthy_repo, &["commit", "-m", "authored"]);
    let healthy_authored_commit = head_oid(&healthy_repo)
        .await
        .expect("healthy authored head");

    let db_path = std::env::temp_dir().join(format!(
        "ingot-runtime-startup-review-recovery-{}.db",
        Uuid::now_v7()
    ));
    let db = Database::connect(&db_path).await.expect("connect db");
    db.migrate().await.expect("migrate db");
    let dispatcher = JobDispatcher::with_runner(
        db.clone(),
        ProjectLocks::default(),
        DispatcherConfig::new(std::env::temp_dir().join(format!(
            "ingot-runtime-startup-review-recovery-state-{}",
            Uuid::now_v7()
        ))),
        Arc::new(FakeRunner),
    );

    let created_at = Utc::now();

    // Broken project
    let broken_project = Project {
        id: ingot_domain::ids::ProjectId::new(),
        name: "broken".into(),
        path: std::env::temp_dir()
            .join(format!("ingot-missing-project-{}", Uuid::now_v7()))
            .display()
            .to_string(),
        default_branch: "main".into(),
        color: "#111".into(),
        created_at,
        updated_at: created_at,
    };
    db.create_project(&broken_project)
        .await
        .expect("create broken project");

    let broken_item_id = ingot_domain::ids::ItemId::new();
    let broken_revision_id = ingot_domain::ids::ItemRevisionId::new();
    let broken_item = ingot_domain::item::Item {
        id: broken_item_id,
        project_id: broken_project.id,
        current_revision_id: broken_revision_id,
        ..test_item(broken_project.id, broken_revision_id)
    };
    let broken_revision = ItemRevision {
        id: broken_revision_id,
        item_id: broken_item_id,
        seed_commit_oid: Some("missing-seed".into()),
        seed_target_commit_oid: Some("missing-seed".into()),
        ..test_revision(broken_item_id, "missing-seed")
    };
    db.create_item_with_revision(&broken_item, &broken_revision)
        .await
        .expect("create broken item");
    let broken_workspace = Workspace {
        id: WorkspaceId::new(),
        project_id: broken_project.id,
        kind: WorkspaceKind::Authoring,
        strategy: WorkspaceStrategy::Worktree,
        path: broken_project.path.clone(),
        created_for_revision_id: Some(broken_revision_id),
        parent_workspace_id: None,
        target_ref: None,
        workspace_ref: None,
        base_commit_oid: Some("missing-seed".into()),
        head_commit_oid: Some("missing-prepared".into()),
        retention_policy: RetentionPolicy::Persistent,
        status: WorkspaceStatus::Ready,
        current_job_id: None,
        created_at,
        updated_at: created_at,
    };
    db.create_workspace(&broken_workspace)
        .await
        .expect("create broken workspace");
    db.create_convergence(&Convergence {
        id: ingot_domain::ids::ConvergenceId::new(),
        project_id: broken_project.id,
        item_id: broken_item_id,
        item_revision_id: broken_revision_id,
        source_workspace_id: broken_workspace.id,
        integration_workspace_id: Some(broken_workspace.id),
        source_head_commit_oid: "missing-prepared".into(),
        target_ref: "refs/heads/main".into(),
        strategy: ConvergenceStrategy::RebaseThenFastForward,
        status: ConvergenceStatus::Prepared,
        input_target_commit_oid: Some("missing-seed".into()),
        prepared_commit_oid: Some("missing-prepared".into()),
        final_target_commit_oid: None,
        target_head_valid: None,
        conflict_summary: None,
        created_at,
        completed_at: None,
    })
    .await
    .expect("create broken convergence");

    // Healthy project
    let healthy_project = Project {
        id: ingot_domain::ids::ProjectId::new(),
        name: "healthy".into(),
        path: healthy_repo.display().to_string(),
        default_branch: "main".into(),
        color: "#000".into(),
        created_at,
        updated_at: created_at,
    };
    db.create_project(&healthy_project)
        .await
        .expect("create healthy project");

    let healthy_item_id = ingot_domain::ids::ItemId::new();
    let healthy_revision_id = ingot_domain::ids::ItemRevisionId::new();
    let healthy_item = ingot_domain::item::Item {
        id: healthy_item_id,
        project_id: healthy_project.id,
        current_revision_id: healthy_revision_id,
        ..test_item(healthy_project.id, healthy_revision_id)
    };
    let healthy_revision = ItemRevision {
        id: healthy_revision_id,
        item_id: healthy_item_id,
        ..test_revision(healthy_item_id, &healthy_seed_commit)
    };
    db.create_item_with_revision(&healthy_item, &healthy_revision)
        .await
        .expect("create healthy item");
    db.create_job(&Job {
        status: JobStatus::Completed,
        outcome_class: Some(OutcomeClass::Clean),
        output_commit_oid: Some(healthy_authored_commit.clone()),
        started_at: Some(created_at),
        ended_at: Some(created_at),
        ..test_authoring_job(healthy_project.id, healthy_item_id, healthy_revision_id, &healthy_seed_commit)
    })
    .await
    .expect("create healthy author job");

    dispatcher
        .reconcile_startup()
        .await
        .expect("reconcile startup");

    let healthy_jobs = db
        .list_jobs_by_item(healthy_item_id)
        .await
        .expect("healthy jobs");
    let review_job = healthy_jobs
        .iter()
        .find(|job| job.step_id == step::REVIEW_INCREMENTAL_INITIAL)
        .expect("startup queued review for healthy project");
    assert_eq!(review_job.status, JobStatus::Queued);
    assert_eq!(
        review_job.job_input.base_commit_oid(),
        Some(healthy_seed_commit.as_str())
    );
    assert_eq!(
        review_job.job_input.head_commit_oid(),
        Some(healthy_authored_commit.as_str())
    );
}

#[tokio::test]
async fn reconcile_startup_adopts_reset_workspace_operation() {
    let h = TestHarness::new(Arc::new(FakeRunner)).await;
    let head = head_oid(&h.repo_path).await.expect("head");
    let created_at = Utc::now();

    let workspace = Workspace {
        id: WorkspaceId::new(),
        project_id: h.project.id,
        kind: WorkspaceKind::Authoring,
        strategy: WorkspaceStrategy::Worktree,
        path: h.repo_path.display().to_string(),
        created_for_revision_id: None,
        parent_workspace_id: None,
        target_ref: Some("refs/heads/main".into()),
        workspace_ref: Some("refs/ingot/workspaces/reset-adopt".into()),
        base_commit_oid: Some(head.clone()),
        head_commit_oid: Some(head.clone()),
        retention_policy: RetentionPolicy::Persistent,
        status: WorkspaceStatus::Busy,
        current_job_id: Some(ingot_domain::ids::JobId::new()),
        created_at,
        updated_at: created_at,
    };
    h.db.create_workspace(&workspace)
        .await
        .expect("create workspace");
    let operation = GitOperation {
        id: GitOperationId::new(),
        project_id: h.project.id,
        operation_kind: OperationKind::ResetWorkspace,
        entity_type: GitEntityType::Workspace,
        entity_id: workspace.id.to_string(),
        workspace_id: Some(workspace.id),
        ref_name: workspace.workspace_ref.clone(),
        expected_old_oid: workspace.head_commit_oid.clone(),
        new_oid: Some(head),
        commit_oid: None,
        status: GitOperationStatus::Applied,
        metadata: None,
        created_at,
        completed_at: None,
    };
    h.db.create_git_operation(&operation)
        .await
        .expect("create operation");

    h.dispatcher
        .reconcile_startup()
        .await
        .expect("reconcile startup");

    let updated_workspace = h.db.get_workspace(workspace.id).await.expect("workspace");
    assert_eq!(updated_workspace.status, WorkspaceStatus::Ready);
    assert_eq!(updated_workspace.current_job_id, None);
}

#[tokio::test]
async fn reconcile_startup_adopts_remove_workspace_ref_operation() {
    let repo = temp_git_repo();
    let head = head_oid(&repo).await.expect("head");
    let workspace_path =
        std::env::temp_dir().join(format!("ingot-runtime-remove-adopt-{}", Uuid::now_v7()));

    let db_path =
        std::env::temp_dir().join(format!("ingot-runtime-remove-adopt-{}.db", Uuid::now_v7()));
    let db = Database::connect(&db_path).await.expect("connect db");
    db.migrate().await.expect("migrate db");
    let state_root = std::env::temp_dir().join(format!(
        "ingot-runtime-remove-adopt-state-{}",
        Uuid::now_v7()
    ));
    let dispatcher = JobDispatcher::with_runner(
        db.clone(),
        ProjectLocks::default(),
        DispatcherConfig::new(state_root.clone()),
        Arc::new(FakeRunner),
    );
    let created_at = Utc::now();
    let project = Project {
        id: ingot_domain::ids::ProjectId::new(),
        name: "repo".into(),
        path: repo.display().to_string(),
        default_branch: "main".into(),
        color: "#000".into(),
        created_at,
        updated_at: created_at,
    };
    db.create_project(&project).await.expect("create project");
    let paths = ensure_test_mirror(state_root.as_path(), &project).await;
    git_sync(
        &paths.mirror_git_dir,
        &["update-ref", "refs/ingot/workspaces/remove-adopt", &head],
    );
    git_sync(
        &paths.mirror_git_dir,
        &[
            "worktree",
            "add",
            "--detach",
            workspace_path.to_str().expect("workspace path"),
            "refs/ingot/workspaces/remove-adopt",
        ],
    );
    delete_ref(&paths.mirror_git_dir, "refs/ingot/workspaces/remove-adopt")
        .await
        .expect("delete ref");
    let workspace = Workspace {
        id: WorkspaceId::new(),
        project_id: project.id,
        kind: WorkspaceKind::Review,
        strategy: WorkspaceStrategy::Worktree,
        path: workspace_path.display().to_string(),
        created_for_revision_id: None,
        parent_workspace_id: None,
        target_ref: None,
        workspace_ref: Some("refs/ingot/workspaces/remove-adopt".into()),
        base_commit_oid: Some(head.clone()),
        head_commit_oid: Some(head),
        retention_policy: RetentionPolicy::Ephemeral,
        status: WorkspaceStatus::Removing,
        current_job_id: Some(ingot_domain::ids::JobId::new()),
        created_at,
        updated_at: created_at,
    };
    db.create_workspace(&workspace)
        .await
        .expect("create workspace");
    let operation = GitOperation {
        id: GitOperationId::new(),
        project_id: project.id,
        operation_kind: OperationKind::RemoveWorkspaceRef,
        entity_type: GitEntityType::Workspace,
        entity_id: workspace.id.to_string(),
        workspace_id: Some(workspace.id),
        ref_name: workspace.workspace_ref.clone(),
        expected_old_oid: workspace.head_commit_oid.clone(),
        new_oid: None,
        commit_oid: None,
        status: GitOperationStatus::Applied,
        metadata: None,
        created_at,
        completed_at: None,
    };
    db.create_git_operation(&operation)
        .await
        .expect("create operation");

    dispatcher
        .reconcile_startup()
        .await
        .expect("reconcile startup");

    let updated_workspace = db.get_workspace(workspace.id).await.expect("workspace");
    assert_eq!(updated_workspace.status, WorkspaceStatus::Abandoned);
    assert_eq!(updated_workspace.current_job_id, None);
    assert_eq!(updated_workspace.workspace_ref, None);
}

#[tokio::test]
async fn reconcile_startup_removes_abandoned_review_workspace_when_safe() {
    let repo = temp_git_repo();
    let workspace_path =
        std::env::temp_dir().join(format!("ingot-runtime-review-cleanup-{}", Uuid::now_v7()));

    let db_path =
        std::env::temp_dir().join(format!("ingot-runtime-cleanup-{}.db", Uuid::now_v7()));
    let db = Database::connect(&db_path).await.expect("connect db");
    db.migrate().await.expect("migrate db");
    let state_root =
        std::env::temp_dir().join(format!("ingot-runtime-cleanup-state-{}", Uuid::now_v7()));
    let dispatcher = JobDispatcher::with_runner(
        db.clone(),
        ProjectLocks::default(),
        DispatcherConfig::new(state_root.clone()),
        Arc::new(FakeRunner),
    );

    let created_at = Utc::now();
    let project = Project {
        id: ingot_domain::ids::ProjectId::new(),
        name: "repo".into(),
        path: repo.display().to_string(),
        default_branch: "main".into(),
        color: "#000".into(),
        created_at,
        updated_at: created_at,
    };
    let seed_commit = head_oid(&repo).await.expect("seed head");
    db.create_project(&project).await.expect("create project");
    let paths = ensure_test_mirror(state_root.as_path(), &project).await;
    git_sync(
        &paths.mirror_git_dir,
        &[
            "worktree",
            "add",
            "--detach",
            workspace_path.to_str().expect("workspace path"),
            "HEAD",
        ],
    );

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let item = ingot_domain::item::Item {
        id: item_id,
        current_revision_id: revision_id,
        lifecycle_state: LifecycleState::Done,
        done_reason: Some(DoneReason::Completed),
        resolution_source: Some(ResolutionSource::ManualCommand),
        closed_at: Some(created_at),
        ..test_item(project.id, revision_id)
    };
    let revision = ItemRevision {
        id: revision_id,
        item_id,
        ..test_revision(item_id, &seed_commit)
    };
    db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let workspace = Workspace {
        id: WorkspaceId::new(),
        project_id: project.id,
        kind: WorkspaceKind::Review,
        strategy: WorkspaceStrategy::Worktree,
        path: workspace_path.display().to_string(),
        created_for_revision_id: Some(revision.id),
        parent_workspace_id: None,
        target_ref: None,
        workspace_ref: None,
        base_commit_oid: Some(seed_commit.clone()),
        head_commit_oid: Some(seed_commit),
        retention_policy: RetentionPolicy::Ephemeral,
        status: WorkspaceStatus::Abandoned,
        current_job_id: None,
        created_at,
        updated_at: created_at,
    };
    db.create_workspace(&workspace)
        .await
        .expect("create workspace");

    dispatcher
        .reconcile_startup()
        .await
        .expect("reconcile startup");

    assert!(
        !workspace_path.exists(),
        "abandoned review workspace should be removed"
    );
}

#[tokio::test]
async fn reconcile_startup_removes_abandoned_authoring_workspace_when_item_is_done_and_safe() {
    let repo = temp_git_repo();
    let workspace_path =
        std::env::temp_dir().join(format!("ingot-runtime-author-cleanup-{}", Uuid::now_v7()));

    let db_path = std::env::temp_dir().join(format!(
        "ingot-runtime-author-cleanup-{}.db",
        Uuid::now_v7()
    ));
    let db = Database::connect(&db_path).await.expect("connect db");
    db.migrate().await.expect("migrate db");
    let state_root = std::env::temp_dir().join(format!(
        "ingot-runtime-author-cleanup-state-{}",
        Uuid::now_v7()
    ));
    let dispatcher = JobDispatcher::with_runner(
        db.clone(),
        ProjectLocks::default(),
        DispatcherConfig::new(state_root.clone()),
        Arc::new(FakeRunner),
    );

    let created_at = Utc::now();
    let project = Project {
        id: ingot_domain::ids::ProjectId::new(),
        name: "repo".into(),
        path: repo.display().to_string(),
        default_branch: "main".into(),
        color: "#000".into(),
        created_at,
        updated_at: created_at,
    };
    let seed_commit = head_oid(&repo).await.expect("seed head");
    db.create_project(&project).await.expect("create project");
    let paths = ensure_test_mirror(state_root.as_path(), &project).await;
    git_sync(
        &paths.mirror_git_dir,
        &[
            "update-ref",
            "refs/ingot/workspaces/author-cleanup",
            &seed_commit,
        ],
    );
    git_sync(
        &paths.mirror_git_dir,
        &[
            "worktree",
            "add",
            "--detach",
            workspace_path.to_str().expect("workspace path"),
            "refs/ingot/workspaces/author-cleanup",
        ],
    );

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let item = ingot_domain::item::Item {
        id: item_id,
        current_revision_id: revision_id,
        lifecycle_state: LifecycleState::Done,
        done_reason: Some(DoneReason::Completed),
        resolution_source: Some(ResolutionSource::ManualCommand),
        closed_at: Some(created_at),
        ..test_item(project.id, revision_id)
    };
    let revision = ItemRevision {
        id: revision_id,
        item_id,
        ..test_revision(item_id, &seed_commit)
    };
    db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let workspace = Workspace {
        id: WorkspaceId::new(),
        project_id: project.id,
        kind: WorkspaceKind::Authoring,
        strategy: WorkspaceStrategy::Worktree,
        path: workspace_path.display().to_string(),
        created_for_revision_id: Some(revision.id),
        parent_workspace_id: None,
        target_ref: Some("refs/heads/main".into()),
        workspace_ref: Some("refs/ingot/workspaces/author-cleanup".into()),
        base_commit_oid: Some(seed_commit.clone()),
        head_commit_oid: Some(seed_commit),
        retention_policy: RetentionPolicy::Persistent,
        status: WorkspaceStatus::Abandoned,
        current_job_id: None,
        created_at,
        updated_at: created_at,
    };
    db.create_workspace(&workspace)
        .await
        .expect("create workspace");

    dispatcher
        .reconcile_startup()
        .await
        .expect("reconcile startup");

    assert!(
        !workspace_path.exists(),
        "abandoned authoring workspace should be removed when safe"
    );
}

#[tokio::test]
async fn reconcile_startup_retains_abandoned_authoring_workspace_with_untriaged_candidate_finding()
{
    let repo = temp_git_repo();
    let seed_commit = head_oid(&repo).await.expect("seed head");
    let workspace_path =
        std::env::temp_dir().join(format!("ingot-runtime-author-retain-{}", Uuid::now_v7()));
    git_sync(
        &repo,
        &[
            "worktree",
            "add",
            "--detach",
            workspace_path.to_str().expect("workspace path"),
            "HEAD",
        ],
    );

    let db_path =
        std::env::temp_dir().join(format!("ingot-runtime-retain-{}.db", Uuid::now_v7()));
    let db = Database::connect(&db_path).await.expect("connect db");
    db.migrate().await.expect("migrate db");
    let dispatcher = JobDispatcher::with_runner(
        db.clone(),
        ProjectLocks::default(),
        DispatcherConfig::new(
            std::env::temp_dir().join(format!("ingot-runtime-retain-state-{}", Uuid::now_v7())),
        ),
        Arc::new(FakeRunner),
    );

    let created_at = Utc::now();
    let project = Project {
        id: ingot_domain::ids::ProjectId::new(),
        name: "repo".into(),
        path: repo.display().to_string(),
        default_branch: "main".into(),
        color: "#000".into(),
        created_at,
        updated_at: created_at,
    };
    db.create_project(&project).await.expect("create project");

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let item = ingot_domain::item::Item {
        id: item_id,
        current_revision_id: revision_id,
        lifecycle_state: LifecycleState::Done,
        done_reason: Some(DoneReason::Dismissed),
        resolution_source: Some(ResolutionSource::ManualCommand),
        closed_at: Some(created_at),
        ..test_item(project.id, revision_id)
    };
    let revision = ItemRevision {
        id: revision_id,
        item_id,
        ..test_revision(item_id, &seed_commit)
    };
    db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let workspace = Workspace {
        id: WorkspaceId::new(),
        project_id: project.id,
        kind: WorkspaceKind::Authoring,
        strategy: WorkspaceStrategy::Worktree,
        path: workspace_path.display().to_string(),
        created_for_revision_id: Some(revision.id),
        parent_workspace_id: None,
        target_ref: Some("refs/heads/main".into()),
        workspace_ref: Some("refs/ingot/workspaces/retain".into()),
        base_commit_oid: Some(seed_commit.clone()),
        head_commit_oid: Some(seed_commit.clone()),
        retention_policy: RetentionPolicy::Ephemeral,
        status: WorkspaceStatus::Abandoned,
        current_job_id: None,
        created_at,
        updated_at: created_at,
    };
    db.create_workspace(&workspace)
        .await
        .expect("create workspace");

    let source_job = Job {
        id: ingot_domain::ids::JobId::new(),
        project_id: project.id,
        item_id,
        item_revision_id: revision_id,
        step_id: "review_candidate_initial".into(),
        semantic_attempt_no: 1,
        retry_no: 0,
        supersedes_job_id: None,
        status: JobStatus::Completed,
        outcome_class: Some(OutcomeClass::Findings),
        phase_kind: PhaseKind::Review,
        workspace_id: Some(workspace.id),
        workspace_kind: WorkspaceKind::Review,
        execution_permission: ExecutionPermission::MustNotMutate,
        context_policy: ContextPolicy::Fresh,
        phase_template_slug: "review-candidate".into(),
        phase_template_digest: None,
        prompt_snapshot: None,
        job_input: JobInput::candidate_subject(seed_commit.clone(), seed_commit.clone()),
        output_artifact_kind: OutputArtifactKind::ReviewReport,
        output_commit_oid: None,
        result_schema_version: Some("review_report:v1".into()),
        result_payload: Some(serde_json::json!({
            "outcome": "findings",
            "summary": "finding",
            "review_subject": {
                "base_commit_oid": seed_commit.clone(),
                "head_commit_oid": seed_commit.clone()
            },
            "overall_risk": "medium",
            "findings": []
        })),
        agent_id: None,
        process_pid: None,
        lease_owner_id: None,
        heartbeat_at: None,
        lease_expires_at: None,
        error_code: None,
        error_message: None,
        created_at,
        started_at: Some(created_at),
        ended_at: Some(created_at),
    };
    db.create_job(&source_job).await.expect("create source job");

    let finding = ingot_domain::finding::Finding {
        id: ingot_domain::ids::FindingId::new(),
        project_id: project.id,
        source_item_id: item.id,
        source_item_revision_id: revision.id,
        source_job_id: source_job.id,
        source_step_id: "review_candidate_initial".into(),
        source_report_schema_version: "review_report:v1".into(),
        source_finding_key: "fnd".into(),
        source_subject_kind: ingot_domain::finding::FindingSubjectKind::Candidate,
        source_subject_base_commit_oid: Some(seed_commit.clone()),
        source_subject_head_commit_oid: seed_commit.clone(),
        code: "CODE".into(),
        severity: ingot_domain::finding::FindingSeverity::Medium,
        summary: "retain me".into(),
        paths: vec!["tracked.txt".into()],
        evidence: serde_json::json!(["evidence"]),
        triage_state: FindingTriageState::Untriaged,
        linked_item_id: None,
        triage_note: None,
        created_at,
        triaged_at: None,
    };
    db.create_finding(&finding).await.expect("create finding");

    dispatcher
        .reconcile_startup()
        .await
        .expect("reconcile startup");

    assert!(workspace_path.exists(), "workspace should be retained");
}

#[tokio::test]
async fn reconcile_startup_retains_abandoned_integration_workspace_with_untriaged_integrated_finding()
{
    let repo = temp_git_repo();
    let seed_commit = head_oid(&repo).await.expect("seed head");
    let workspace_path = std::env::temp_dir().join(format!(
        "ingot-runtime-integration-retain-{}",
        Uuid::now_v7()
    ));
    git_sync(
        &repo,
        &[
            "worktree",
            "add",
            "--detach",
            workspace_path.to_str().expect("workspace path"),
            "HEAD",
        ],
    );

    let db_path = std::env::temp_dir().join(format!(
        "ingot-runtime-integration-retain-{}.db",
        Uuid::now_v7()
    ));
    let db = Database::connect(&db_path).await.expect("connect db");
    db.migrate().await.expect("migrate db");
    let dispatcher = JobDispatcher::with_runner(
        db.clone(),
        ProjectLocks::default(),
        DispatcherConfig::new(std::env::temp_dir().join(format!(
            "ingot-runtime-integration-retain-state-{}",
            Uuid::now_v7()
        ))),
        Arc::new(FakeRunner),
    );

    let created_at = Utc::now();
    let project = Project {
        id: ingot_domain::ids::ProjectId::new(),
        name: "repo".into(),
        path: repo.display().to_string(),
        default_branch: "main".into(),
        color: "#000".into(),
        created_at,
        updated_at: created_at,
    };
    db.create_project(&project).await.expect("create project");

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let item = ingot_domain::item::Item {
        id: item_id,
        current_revision_id: revision_id,
        lifecycle_state: LifecycleState::Done,
        done_reason: Some(DoneReason::Completed),
        resolution_source: Some(ResolutionSource::ManualCommand),
        closed_at: Some(created_at),
        ..test_item(project.id, revision_id)
    };
    let revision = ItemRevision {
        id: revision_id,
        item_id,
        ..test_revision(item_id, &seed_commit)
    };
    db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let source_job = Job {
        id: ingot_domain::ids::JobId::new(),
        project_id: project.id,
        item_id,
        item_revision_id: revision_id,
        step_id: "validate_integrated".into(),
        semantic_attempt_no: 1,
        retry_no: 0,
        supersedes_job_id: None,
        status: JobStatus::Completed,
        outcome_class: Some(OutcomeClass::Findings),
        phase_kind: PhaseKind::Validate,
        workspace_id: None,
        workspace_kind: WorkspaceKind::Integration,
        execution_permission: ExecutionPermission::MustNotMutate,
        context_policy: ContextPolicy::ResumeContext,
        phase_template_slug: "validate-integrated".into(),
        phase_template_digest: None,
        prompt_snapshot: None,
        job_input: JobInput::integrated_subject(seed_commit.clone(), seed_commit.clone()),
        output_artifact_kind: OutputArtifactKind::ValidationReport,
        output_commit_oid: None,
        result_schema_version: Some("validation_report:v1".into()),
        result_payload: Some(serde_json::json!({
            "outcome": "findings",
            "summary": "finding",
            "checks": [],
            "findings": []
        })),
        agent_id: None,
        process_pid: None,
        lease_owner_id: None,
        heartbeat_at: None,
        lease_expires_at: None,
        error_code: None,
        error_message: None,
        created_at,
        started_at: Some(created_at),
        ended_at: Some(created_at),
    };
    db.create_job(&source_job).await.expect("create source job");

    let workspace = Workspace {
        id: WorkspaceId::new(),
        project_id: project.id,
        kind: WorkspaceKind::Integration,
        strategy: WorkspaceStrategy::Worktree,
        path: workspace_path.display().to_string(),
        created_for_revision_id: Some(revision.id),
        parent_workspace_id: None,
        target_ref: Some("refs/heads/main".into()),
        workspace_ref: Some("refs/ingot/workspaces/integration-retain".into()),
        base_commit_oid: Some(seed_commit.clone()),
        head_commit_oid: Some(seed_commit.clone()),
        retention_policy: RetentionPolicy::Persistent,
        status: WorkspaceStatus::Abandoned,
        current_job_id: None,
        created_at,
        updated_at: created_at,
    };
    db.create_workspace(&workspace)
        .await
        .expect("create workspace");

    let finding = ingot_domain::finding::Finding {
        id: ingot_domain::ids::FindingId::new(),
        project_id: project.id,
        source_item_id: item.id,
        source_item_revision_id: revision.id,
        source_job_id: source_job.id,
        source_step_id: "validate_integrated".into(),
        source_report_schema_version: "validation_report:v1".into(),
        source_finding_key: "fnd".into(),
        source_subject_kind: ingot_domain::finding::FindingSubjectKind::Integrated,
        source_subject_base_commit_oid: Some(seed_commit.clone()),
        source_subject_head_commit_oid: seed_commit.clone(),
        code: "CODE".into(),
        severity: ingot_domain::finding::FindingSeverity::High,
        summary: "retain integration".into(),
        paths: vec!["tracked.txt".into()],
        evidence: serde_json::json!(["evidence"]),
        triage_state: FindingTriageState::Untriaged,
        linked_item_id: None,
        triage_note: None,
        created_at,
        triaged_at: None,
    };
    db.create_finding(&finding).await.expect("create finding");

    dispatcher
        .reconcile_startup()
        .await
        .expect("reconcile startup");

    assert!(
        workspace_path.exists(),
        "integration workspace should be retained"
    );
}

#[tokio::test]
async fn reconcile_startup_handles_mixed_inflight_states_conservatively() {
    let h = TestHarness::new(Arc::new(FakeRunner)).await;

    let seed_commit = head_oid(&h.repo_path).await.expect("seed head");
    let created_at = Utc::now();

    let rev_a = make_runtime_revision(
        ingot_domain::ids::ItemId::new(),
        1,
        &seed_commit,
        created_at,
    );
    let item_a = make_runtime_item(h.project.id, rev_a.id, rev_a.item_id, created_at);
    h.db.create_item_with_revision(&item_a, &rev_a)
        .await
        .expect("create item a");
    let workspace_a = make_runtime_workspace(
        h.project.id,
        Some(rev_a.id),
        WorkspaceKind::Authoring,
        WorkspaceStatus::Busy,
        &seed_commit,
        created_at,
    );
    h.db.create_workspace(&workspace_a)
        .await
        .expect("workspace a");
    let mut assigned_job = make_runtime_job(
        h.project.id,
        item_a.id,
        rev_a.id,
        "author_initial",
        WorkspaceKind::Authoring,
        OutputArtifactKind::Commit,
        created_at,
    );
    assigned_job.status = JobStatus::Assigned;
    assigned_job.workspace_id = Some(workspace_a.id);
    h.db.create_job(&assigned_job).await.expect("assigned job");

    let rev_b = make_runtime_revision(
        ingot_domain::ids::ItemId::new(),
        1,
        &seed_commit,
        created_at,
    );
    let item_b = make_runtime_item(h.project.id, rev_b.id, rev_b.item_id, created_at);
    h.db.create_item_with_revision(&item_b, &rev_b)
        .await
        .expect("create item b");
    let workspace_b = make_runtime_workspace(
        h.project.id,
        Some(rev_b.id),
        WorkspaceKind::Authoring,
        WorkspaceStatus::Busy,
        &seed_commit,
        created_at,
    );
    h.db.create_workspace(&workspace_b)
        .await
        .expect("workspace b");
    let mut running_job = make_runtime_job(
        h.project.id,
        item_b.id,
        rev_b.id,
        "author_initial",
        WorkspaceKind::Authoring,
        OutputArtifactKind::Commit,
        created_at,
    );
    running_job.status = JobStatus::Running;
    running_job.workspace_id = Some(workspace_b.id);
    running_job.lease_owner_id = Some("old-daemon".into());
    running_job.lease_expires_at = Some(created_at - ChronoDuration::minutes(1));
    running_job.started_at = Some(created_at);
    h.db.create_job(&running_job).await.expect("running job");

    h.dispatcher
        .reconcile_startup()
        .await
        .expect("reconcile startup");

    let updated_assigned = h.db.get_job(assigned_job.id).await.expect("assigned");
    assert_eq!(updated_assigned.status, JobStatus::Queued);
    assert_eq!(updated_assigned.workspace_id, None);

    let updated_running = h.db.get_job(running_job.id).await.expect("running");
    assert_eq!(updated_running.status, JobStatus::Expired);
    assert_eq!(
        updated_running.outcome_class,
        Some(OutcomeClass::TransientFailure)
    );

    let updated_workspace_a = h.db.get_workspace(workspace_a.id).await.expect("workspace a");
    assert_eq!(updated_workspace_a.status, WorkspaceStatus::Ready);
    let updated_workspace_b = h.db.get_workspace(workspace_b.id).await.expect("workspace b");
    assert_eq!(updated_workspace_b.status, WorkspaceStatus::Stale);
}
