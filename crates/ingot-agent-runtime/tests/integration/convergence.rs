use std::sync::Arc;

use chrono::Utc;
use ingot_agent_runtime::{DispatcherConfig, JobDispatcher};
use ingot_domain::ids::WorkspaceId;
use ingot_domain::item::{ApprovalState, EscalationReason, EscalationState, ResolutionSource};
use ingot_domain::job::{
    ContextPolicy, ExecutionPermission, Job, JobInput, JobStatus, OutcomeClass,
    OutputArtifactKind, PhaseKind,
};
use ingot_domain::revision::{ApprovalPolicy, ItemRevision};
use ingot_domain::workspace::{
    RetentionPolicy, Workspace, WorkspaceKind, WorkspaceStatus, WorkspaceStrategy,
};
use ingot_git::commands::{head_oid, resolve_ref_oid};
use ingot_store_sqlite::Database;
use ingot_usecases::ProjectLocks;
use uuid::Uuid;

use super::helpers::*;
use chrono::Duration as ChronoDuration;
use ingot_domain::activity::ActivityEventType;
use ingot_domain::convergence::{Convergence, ConvergenceStatus, ConvergenceStrategy};
use ingot_domain::convergence_queue::{ConvergenceQueueEntry, ConvergenceQueueEntryStatus};
use ingot_domain::git_operation::{
    GitEntityType, GitOperation, GitOperationStatus, OperationKind,
};
use ingot_domain::ids::GitOperationId;
use ingot_domain::item::LifecycleState;
use ingot_usecases::job::{DispatchJobCommand, dispatch_job};
use ingot_workflow::{Evaluator, step};

#[tokio::test]
async fn tick_auto_finalizes_prepared_convergence_for_not_required_approval() {
    let repo = temp_git_repo();
    let base_commit = head_oid(&repo).await.expect("base head");
    std::fs::write(repo.join("tracked.txt"), "prepared").expect("write file");
    git_sync(&repo, &["add", "tracked.txt"]);
    git_sync(&repo, &["commit", "-m", "prepared"]);
    let prepared_commit = head_oid(&repo).await.expect("prepared head");
    git_sync(&repo, &["reset", "--hard", &base_commit]);
    let integration_workspace_path =
        std::env::temp_dir().join(format!("ingot-runtime-integration-{}", Uuid::now_v7()));

    let db_path =
        std::env::temp_dir().join(format!("ingot-runtime-finalize-{}.db", Uuid::now_v7()));
    let db = Database::connect(&db_path).await.expect("connect db");
    db.migrate().await.expect("migrate db");
    let state_root =
        std::env::temp_dir().join(format!("ingot-runtime-finalize-state-{}", Uuid::now_v7()));
    let dispatcher = JobDispatcher::with_runner(
        db.clone(),
        ProjectLocks::default(),
        DispatcherConfig::new(state_root.clone()),
        Arc::new(FakeRunner),
    );

    let created_at = Utc::now();
    let project = ingot_domain::project::Project {
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
            "refs/ingot/workspaces/wrk_integration_test",
            &prepared_commit,
        ],
    );
    git_sync(&repo, &["reset", "--hard", &base_commit]);
    git_sync(
        &paths.mirror_git_dir,
        &[
            "worktree",
            "add",
            "--detach",
            integration_workspace_path.to_str().expect("workspace path"),
            "refs/ingot/workspaces/wrk_integration_test",
        ],
    );

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let seed_commit = head_oid(&repo).await.expect("seed head");
    let item = ingot_domain::item::Item {
        id: item_id,
        current_revision_id: revision_id,
        approval_state: ApprovalState::NotRequired,
        ..test_item(project.id, revision_id)
    };
    let revision = ItemRevision {
        id: revision_id,
        item_id,
        approval_policy: ApprovalPolicy::NotRequired,
        seed_commit_oid: Some(base_commit.clone()),
        seed_target_commit_oid: Some(base_commit.clone()),
        ..test_revision(item_id, &base_commit)
    };
    db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let integration_workspace = Workspace {
        id: WorkspaceId::new(),
        project_id: project.id,
        kind: WorkspaceKind::Integration,
        strategy: WorkspaceStrategy::Worktree,
        path: integration_workspace_path.display().to_string(),
        created_for_revision_id: Some(revision.id),
        parent_workspace_id: None,
        target_ref: Some("refs/heads/main".into()),
        workspace_ref: Some("refs/ingot/workspaces/wrk_integration_test".into()),
        base_commit_oid: Some(base_commit.clone()),
        head_commit_oid: Some(prepared_commit.clone()),
        retention_policy: RetentionPolicy::Persistent,
        status: WorkspaceStatus::Ready,
        current_job_id: None,
        created_at,
        updated_at: created_at,
    };
    db.create_workspace(&integration_workspace)
        .await
        .expect("create workspace");
    let source_workspace = Workspace {
        id: WorkspaceId::new(),
        project_id: project.id,
        kind: WorkspaceKind::Authoring,
        strategy: WorkspaceStrategy::Worktree,
        path: std::env::temp_dir()
            .join(format!("ingot-runtime-source-{}", Uuid::now_v7()))
            .display()
            .to_string(),
        created_for_revision_id: Some(revision.id),
        parent_workspace_id: None,
        target_ref: Some("refs/heads/main".into()),
        workspace_ref: Some("refs/ingot/workspaces/wrk_source_test".into()),
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

    let validate_job = Job {
        id: ingot_domain::ids::JobId::new(),
        project_id: project.id,
        item_id,
        item_revision_id: revision_id,
        step_id: "validate_integrated".into(),
        semantic_attempt_no: 1,
        retry_no: 0,
        supersedes_job_id: None,
        status: JobStatus::Completed,
        outcome_class: Some(OutcomeClass::Clean),
        phase_kind: PhaseKind::Validate,
        workspace_id: Some(integration_workspace.id),
        workspace_kind: WorkspaceKind::Integration,
        execution_permission: ExecutionPermission::MustNotMutate,
        context_policy: ContextPolicy::ResumeContext,
        phase_template_slug: "validate-integrated".into(),
        phase_template_digest: None,
        prompt_snapshot: None,
        job_input: JobInput::integrated_subject(base_commit.clone(), prepared_commit.clone()),
        output_artifact_kind: OutputArtifactKind::ValidationReport,
        output_commit_oid: None,
        result_schema_version: Some("validation_report:v1".into()),
        result_payload: Some(serde_json::json!({
            "outcome": "clean",
            "summary": "integrated clean",
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
    db.create_job(&validate_job).await.expect("create job");

    let convergence = Convergence {
        id: ingot_domain::ids::ConvergenceId::new(),
        project_id: project.id,
        item_id,
        item_revision_id: revision_id,
        source_workspace_id: source_workspace.id,
        integration_workspace_id: Some(integration_workspace.id),
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
        item_id: item.id,
        item_revision_id: revision.id,
        target_ref: "refs/heads/main".into(),
        status: ConvergenceQueueEntryStatus::Head,
        head_acquired_at: Some(created_at),
        created_at,
        updated_at: created_at,
        released_at: None,
    })
    .await
    .expect("insert queue entry");

    assert!(dispatcher.tick().await.expect("tick should finalize"));

    let updated_item = db.get_item(item.id).await.expect("updated item");
    assert_eq!(updated_item.lifecycle_state, LifecycleState::Done);
    assert_eq!(
        updated_item.resolution_source,
        Some(ResolutionSource::SystemCommand)
    );
    let updated_convergence = db
        .list_convergences_by_item(item.id)
        .await
        .expect("list convergences")
        .into_iter()
        .next()
        .expect("convergence");
    assert_eq!(updated_convergence.status, ConvergenceStatus::Finalized);
    assert_eq!(
        git_output(&repo, &["rev-parse", "refs/heads/main"]),
        prepared_commit
    );
    assert!(!integration_workspace_path.exists());
    let unresolved = db
        .list_unresolved_git_operations()
        .await
        .expect("list unresolved");
    assert!(
        unresolved.is_empty(),
        "auto-finalize should resolve git ops"
    );

    std::fs::write(repo.join("tracked.txt"), "post-finalize refresh")
        .expect("write post-finalize change");
    git_sync(&repo, &["add", "tracked.txt"]);
    git_sync(&repo, &["commit", "-m", "post-finalize refresh"]);
    let refreshed_head = head_oid(&repo).await.expect("refreshed head");
    let refreshed_paths = dispatcher
        .refresh_project_mirror(&project)
        .await
        .expect("refresh mirror");
    assert_eq!(
        resolve_ref_oid(refreshed_paths.mirror_git_dir.as_path(), "refs/heads/main")
            .await
            .expect("resolve mirror head"),
        Some(refreshed_head)
    );
}

#[tokio::test]
async fn tick_auto_finalizes_granted_prepared_convergence_even_when_commit_exists_only_in_mirror()
{
    let repo = temp_git_repo();
    let base_commit = head_oid(&repo).await.expect("base head");
    let db_path = std::env::temp_dir().join(format!(
        "ingot-runtime-finalize-mirror-only-{}.db",
        Uuid::now_v7()
    ));
    let db = Database::connect(&db_path).await.expect("connect db");
    db.migrate().await.expect("migrate db");
    let state_root = std::env::temp_dir().join(format!(
        "ingot-runtime-finalize-mirror-only-state-{}",
        Uuid::now_v7()
    ));
    let dispatcher = JobDispatcher::with_runner(
        db.clone(),
        ProjectLocks::default(),
        DispatcherConfig::new(state_root.clone()),
        Arc::new(FakeRunner),
    );

    let created_at = Utc::now();
    let project = ingot_domain::project::Project {
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
    let workspace_ref = "refs/ingot/workspaces/mirror-only-finalize";
    let (integration_workspace_path, prepared_commit) = create_mirror_only_commit(
        paths.mirror_git_dir.as_path(),
        &base_commit,
        workspace_ref,
        "mirror-only prepared",
    )
    .await;

    let checkout_has_commit = std::process::Command::new("git")
        .args(["cat-file", "-e", &format!("{prepared_commit}^{{commit}}")])
        .current_dir(&repo)
        .status()
        .expect("check checkout object");
    assert!(
        !checkout_has_commit.success(),
        "test setup requires the prepared commit to be absent from the registered checkout"
    );

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

    let integration_workspace = Workspace {
        id: WorkspaceId::new(),
        project_id: project.id,
        kind: WorkspaceKind::Integration,
        strategy: WorkspaceStrategy::Worktree,
        path: integration_workspace_path.display().to_string(),
        created_for_revision_id: Some(revision.id),
        parent_workspace_id: None,
        target_ref: Some("refs/heads/main".into()),
        workspace_ref: Some(workspace_ref.into()),
        base_commit_oid: Some(base_commit.clone()),
        head_commit_oid: Some(prepared_commit.clone()),
        retention_policy: RetentionPolicy::Persistent,
        status: WorkspaceStatus::Ready,
        current_job_id: None,
        created_at,
        updated_at: created_at,
    };
    db.create_workspace(&integration_workspace)
        .await
        .expect("create integration workspace");
    let source_workspace = Workspace {
        id: WorkspaceId::new(),
        project_id: project.id,
        kind: WorkspaceKind::Authoring,
        strategy: WorkspaceStrategy::Worktree,
        path: std::env::temp_dir()
            .join(format!(
                "ingot-runtime-mirror-only-source-{}",
                Uuid::now_v7()
            ))
            .display()
            .to_string(),
        created_for_revision_id: Some(revision.id),
        parent_workspace_id: None,
        target_ref: Some("refs/heads/main".into()),
        workspace_ref: Some("refs/ingot/workspaces/mirror-only-source".into()),
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

    let validate_job = Job {
        id: ingot_domain::ids::JobId::new(),
        project_id: project.id,
        item_id,
        item_revision_id: revision_id,
        step_id: step::VALIDATE_INTEGRATED.into(),
        semantic_attempt_no: 1,
        retry_no: 0,
        supersedes_job_id: None,
        status: JobStatus::Completed,
        outcome_class: Some(OutcomeClass::Clean),
        phase_kind: PhaseKind::Validate,
        workspace_id: Some(integration_workspace.id),
        workspace_kind: WorkspaceKind::Integration,
        execution_permission: ExecutionPermission::MustNotMutate,
        context_policy: ContextPolicy::ResumeContext,
        phase_template_slug: "validate-integrated".into(),
        phase_template_digest: None,
        prompt_snapshot: None,
        job_input: JobInput::integrated_subject(base_commit.clone(), prepared_commit.clone()),
        output_artifact_kind: OutputArtifactKind::ValidationReport,
        output_commit_oid: None,
        result_schema_version: Some("validation_report:v1".into()),
        result_payload: Some(serde_json::json!({
            "outcome": "clean",
            "summary": "integrated clean",
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
    db.create_job(&validate_job)
        .await
        .expect("create validation");

    let convergence = Convergence {
        id: ingot_domain::ids::ConvergenceId::new(),
        project_id: project.id,
        item_id,
        item_revision_id: revision_id,
        source_workspace_id: source_workspace.id,
        integration_workspace_id: Some(integration_workspace.id),
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
        item_revision_id: revision.id,
        target_ref: "refs/heads/main".into(),
        status: ConvergenceQueueEntryStatus::Head,
        head_acquired_at: Some(created_at),
        created_at,
        updated_at: created_at,
        released_at: None,
    })
    .await
    .expect("insert queue entry");

    assert!(dispatcher.tick().await.expect("tick should finalize"));

    assert_eq!(
        head_oid(&repo).await.expect("checkout head"),
        prepared_commit
    );
    let updated_convergence = db
        .get_convergence(convergence.id)
        .await
        .expect("convergence");
    assert_eq!(updated_convergence.status, ConvergenceStatus::Finalized);
    let updated_item = db.get_item(item.id).await.expect("item");
    assert_eq!(updated_item.lifecycle_state, LifecycleState::Done);
    assert_eq!(updated_item.approval_state, ApprovalState::Approved);
    let queue_entries = db
        .list_queue_entries_by_item(item.id)
        .await
        .expect("queue entries");
    assert_eq!(
        queue_entries[0].status,
        ConvergenceQueueEntryStatus::Released
    );
    let unresolved = db
        .list_unresolved_git_operations()
        .await
        .expect("list unresolved");
    assert!(unresolved.is_empty(), "finalize op should reconcile");
}

#[tokio::test]
async fn tick_invalidates_stale_prepared_convergence() {
    let repo = temp_git_repo();
    let base_commit = head_oid(&repo).await.expect("base head");
    std::fs::write(repo.join("tracked.txt"), "prepared").expect("write file");
    git_sync(&repo, &["add", "tracked.txt"]);
    git_sync(&repo, &["commit", "-m", "prepared"]);
    let prepared_commit = head_oid(&repo).await.expect("prepared head");
    git_sync(&repo, &["reset", "--hard", &base_commit]);
    std::fs::write(repo.join("tracked.txt"), "moved target").expect("write moved target");
    git_sync(&repo, &["add", "tracked.txt"]);
    git_sync(&repo, &["commit", "-m", "moved target"]);

    let h = TestHarness::new(Arc::new(FakeRunner)).await;
    // Override the project to use our custom repo
    let created_at = Utc::now();
    let project = ingot_domain::project::Project {
        id: ingot_domain::ids::ProjectId::new(),
        name: "repo".into(),
        path: repo.display().to_string(),
        default_branch: "main".into(),
        color: "#000".into(),
        created_at,
        updated_at: created_at,
    };
    h.db.create_project(&project).await.expect("create project");

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let seed_commit = head_oid(&repo).await.expect("seed head");
    let item = ingot_domain::item::Item {
        id: item_id,
        current_revision_id: revision_id,
        approval_state: ApprovalState::Pending,
        ..test_item(project.id, revision_id)
    };
    let revision = ItemRevision {
        id: revision_id,
        item_id,
        seed_commit_oid: Some(base_commit.clone()),
        seed_target_commit_oid: Some(base_commit.clone()),
        ..test_revision(item_id, &base_commit)
    };
    h.db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let integration_workspace = Workspace {
        id: WorkspaceId::new(),
        project_id: project.id,
        kind: WorkspaceKind::Integration,
        strategy: WorkspaceStrategy::Worktree,
        path: std::env::temp_dir()
            .join(format!("ingot-runtime-stale-{}", Uuid::now_v7()))
            .display()
            .to_string(),
        created_for_revision_id: Some(revision.id),
        parent_workspace_id: None,
        target_ref: Some("refs/heads/main".into()),
        workspace_ref: Some("refs/ingot/workspaces/stale".into()),
        base_commit_oid: Some(base_commit.clone()),
        head_commit_oid: Some(prepared_commit.clone()),
        retention_policy: RetentionPolicy::Persistent,
        status: WorkspaceStatus::Ready,
        current_job_id: None,
        created_at,
        updated_at: created_at,
    };
    h.db.create_workspace(&integration_workspace)
        .await
        .expect("create workspace");
    let source_workspace = Workspace {
        id: WorkspaceId::new(),
        project_id: project.id,
        kind: WorkspaceKind::Authoring,
        strategy: WorkspaceStrategy::Worktree,
        path: std::env::temp_dir()
            .join(format!("ingot-runtime-source-{}", Uuid::now_v7()))
            .display()
            .to_string(),
        created_for_revision_id: Some(revision.id),
        parent_workspace_id: None,
        target_ref: Some("refs/heads/main".into()),
        workspace_ref: Some("refs/ingot/workspaces/stale-source".into()),
        base_commit_oid: Some(base_commit.clone()),
        head_commit_oid: Some(prepared_commit.clone()),
        retention_policy: RetentionPolicy::Persistent,
        status: WorkspaceStatus::Ready,
        current_job_id: None,
        created_at,
        updated_at: created_at,
    };
    h.db.create_workspace(&source_workspace)
        .await
        .expect("create source workspace");

    let validate_job = Job {
        id: ingot_domain::ids::JobId::new(),
        project_id: project.id,
        item_id,
        item_revision_id: revision_id,
        step_id: "validate_integrated".into(),
        semantic_attempt_no: 1,
        retry_no: 0,
        supersedes_job_id: None,
        status: JobStatus::Completed,
        outcome_class: Some(OutcomeClass::Clean),
        phase_kind: PhaseKind::Validate,
        workspace_id: Some(integration_workspace.id),
        workspace_kind: WorkspaceKind::Integration,
        execution_permission: ExecutionPermission::MustNotMutate,
        context_policy: ContextPolicy::ResumeContext,
        phase_template_slug: "validate-integrated".into(),
        phase_template_digest: None,
        prompt_snapshot: None,
        job_input: JobInput::integrated_subject(base_commit.clone(), prepared_commit.clone()),
        output_artifact_kind: OutputArtifactKind::ValidationReport,
        output_commit_oid: None,
        result_schema_version: Some("validation_report:v1".into()),
        result_payload: Some(serde_json::json!({
            "outcome": "clean",
            "summary": "integrated clean",
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
    h.db.create_job(&validate_job).await.expect("create job");

    let convergence = Convergence {
        id: ingot_domain::ids::ConvergenceId::new(),
        project_id: project.id,
        item_id,
        item_revision_id: revision_id,
        source_workspace_id: source_workspace.id,
        integration_workspace_id: Some(integration_workspace.id),
        source_head_commit_oid: prepared_commit.clone(),
        target_ref: "refs/heads/main".into(),
        strategy: ConvergenceStrategy::RebaseThenFastForward,
        status: ConvergenceStatus::Prepared,
        input_target_commit_oid: Some(base_commit.clone()),
        prepared_commit_oid: Some(prepared_commit.clone()),
        final_target_commit_oid: None,
        target_head_valid: Some(false),
        conflict_summary: None,
        created_at,
        completed_at: None,
    };
    h.db.create_convergence(&convergence)
        .await
        .expect("create convergence");

    assert!(h.dispatcher.tick().await.expect("tick should invalidate"));

    let updated_item = h.db.get_item(item.id).await.expect("updated item");
    assert_eq!(updated_item.approval_state, ApprovalState::NotRequested);
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
        Some("target_ref_moved")
    );
    let updated_workspace = h
        .db
        .get_workspace(integration_workspace.id)
        .await
        .expect("workspace");
    assert_eq!(updated_workspace.status, WorkspaceStatus::Stale);
}

#[tokio::test]
async fn tick_reconciles_applied_finalize_operation_instead_of_invalidating_prepared_convergence()
{
    let repo = temp_git_repo();
    let base_commit = head_oid(&repo).await.expect("base head");
    std::fs::write(repo.join("tracked.txt"), "prepared").expect("write prepared");
    git_sync(&repo, &["add", "tracked.txt"]);
    git_sync(&repo, &["commit", "-m", "prepared"]);
    let prepared_commit = head_oid(&repo).await.expect("prepared head");

    let db_path = std::env::temp_dir().join(format!(
        "ingot-runtime-finalize-reconcile-{}.db",
        Uuid::now_v7()
    ));
    let db = Database::connect(&db_path).await.expect("connect db");
    db.migrate().await.expect("migrate db");
    let state_root = std::env::temp_dir().join(format!(
        "ingot-runtime-finalize-reconcile-state-{}",
        Uuid::now_v7()
    ));
    let dispatcher = JobDispatcher::with_runner(
        db.clone(),
        ProjectLocks::default(),
        DispatcherConfig::new(state_root.clone()),
        Arc::new(FakeRunner),
    );

    let created_at = Utc::now();
    let project = ingot_domain::project::Project {
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
    assert_eq!(
        resolve_ref_oid(paths.mirror_git_dir.as_path(), "refs/heads/main")
            .await
            .expect("mirror head"),
        Some(prepared_commit.clone())
    );

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
                "ingot-runtime-finalize-reconcile-source-{}",
                Uuid::now_v7()
            ))
            .display()
            .to_string(),
        created_for_revision_id: Some(revision.id),
        parent_workspace_id: None,
        target_ref: Some("refs/heads/main".into()),
        workspace_ref: Some("refs/ingot/workspaces/finalize-reconcile-source".into()),
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
        item_revision_id: revision.id,
        target_ref: "refs/heads/main".into(),
        status: ConvergenceQueueEntryStatus::Head,
        head_acquired_at: Some(created_at),
        created_at,
        updated_at: created_at,
        released_at: None,
    })
    .await
    .expect("insert queue entry");
    db.create_git_operation(&GitOperation {
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
    })
    .await
    .expect("create finalize operation");

    assert!(
        dispatcher
            .tick()
            .await
            .expect("tick should reconcile finalize")
    );

    let updated_convergence = db
        .get_convergence(convergence.id)
        .await
        .expect("convergence");
    assert_eq!(updated_convergence.status, ConvergenceStatus::Finalized);
    let updated_item = db.get_item(item.id).await.expect("item");
    assert_eq!(updated_item.lifecycle_state, LifecycleState::Done);
    let queue_entries = db
        .list_queue_entries_by_item(item.id)
        .await
        .expect("queue entries");
    assert_eq!(
        queue_entries[0].status,
        ConvergenceQueueEntryStatus::Released
    );
    let unresolved = db
        .list_unresolved_git_operations()
        .await
        .expect("list unresolved");
    assert!(
        unresolved.is_empty(),
        "applied finalize op should reconcile"
    );
}

#[tokio::test]
async fn tick_reprepares_granted_lane_head_without_prepared_convergence() {
    let h = TestHarness::new(Arc::new(FakeRunner)).await;

    let seed_commit = head_oid(&h.repo_path).await.expect("seed head");
    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let item = ingot_domain::item::Item {
        id: item_id,
        current_revision_id: revision_id,
        approval_state: ApprovalState::Granted,
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
    let authoring_workspace = Workspace {
        id: WorkspaceId::new(),
        project_id: h.project.id,
        kind: WorkspaceKind::Authoring,
        strategy: WorkspaceStrategy::Worktree,
        path: std::env::temp_dir()
            .join(format!(
                "ingot-runtime-granted-authoring-{}",
                Uuid::now_v7()
            ))
            .display()
            .to_string(),
        created_for_revision_id: Some(revision.id),
        parent_workspace_id: None,
        target_ref: Some("refs/heads/main".into()),
        workspace_ref: Some("refs/ingot/workspaces/granted-head".into()),
        base_commit_oid: Some(seed_commit.clone()),
        head_commit_oid: Some(seed_commit.clone()),
        retention_policy: RetentionPolicy::Persistent,
        status: WorkspaceStatus::Ready,
        current_job_id: None,
        created_at,
        updated_at: created_at,
    };
    h.db.create_workspace(&authoring_workspace)
        .await
        .expect("create workspace");

    let candidate_validate_job = Job {
        id: ingot_domain::ids::JobId::new(),
        project_id: h.project.id,
        item_id,
        item_revision_id: revision_id,
        step_id: step::VALIDATE_CANDIDATE_INITIAL.into(),
        semantic_attempt_no: 1,
        retry_no: 0,
        supersedes_job_id: None,
        status: JobStatus::Completed,
        outcome_class: Some(OutcomeClass::Clean),
        phase_kind: PhaseKind::Validate,
        workspace_id: None,
        workspace_kind: WorkspaceKind::Authoring,
        execution_permission: ExecutionPermission::MustNotMutate,
        context_policy: ContextPolicy::ResumeContext,
        phase_template_slug: "validate-candidate".into(),
        phase_template_digest: None,
        prompt_snapshot: None,
        job_input: JobInput::candidate_subject(seed_commit.clone(), seed_commit.clone()),
        output_artifact_kind: OutputArtifactKind::ValidationReport,
        output_commit_oid: None,
        result_schema_version: Some("validation_report:v1".into()),
        result_payload: Some(serde_json::json!({
            "outcome": "clean",
            "summary": "integrated clean",
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
    h.db.create_job(&candidate_validate_job)
        .await
        .expect("create candidate validation");
    let stale_validate_job = Job {
        id: ingot_domain::ids::JobId::new(),
        project_id: h.project.id,
        item_id,
        item_revision_id: revision_id,
        step_id: step::VALIDATE_INTEGRATED.into(),
        semantic_attempt_no: 1,
        retry_no: 0,
        supersedes_job_id: None,
        status: JobStatus::Completed,
        outcome_class: Some(OutcomeClass::Clean),
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
            "outcome": "clean",
            "summary": "integrated clean",
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
        ended_at: Some(created_at + ChronoDuration::seconds(1)),
    };
    h.db.create_job(&stale_validate_job)
        .await
        .expect("create stale validation");
    h.db.create_queue_entry(&ConvergenceQueueEntry {
        id: ingot_domain::ids::ConvergenceQueueEntryId::new(),
        project_id: h.project.id,
        item_id,
        item_revision_id: revision.id,
        target_ref: "refs/heads/main".into(),
        status: ConvergenceQueueEntryStatus::Head,
        head_acquired_at: Some(created_at),
        created_at,
        updated_at: created_at,
        released_at: None,
    })
    .await
    .expect("insert queue entry");

    assert!(h.dispatcher.tick().await.expect("tick should reprepare"));

    let convergences = h
        .db
        .list_convergences_by_item(item.id)
        .await
        .expect("list convergences");
    assert!(
        convergences
            .iter()
            .any(|convergence| convergence.status == ConvergenceStatus::Prepared)
    );
    let jobs = h.db.list_jobs_by_item(item.id).await.expect("jobs");
    assert!(
        jobs.iter().any(|job| {
            job.step_id == step::VALIDATE_INTEGRATED
                && job.status == JobStatus::Queued
                && job.item_revision_id == revision.id
        }),
        "reprepare should dispatch a fresh integrated validation job"
    );
    let unresolved = h
        .db
        .list_unresolved_git_operations()
        .await
        .expect("list unresolved");
    assert!(
        unresolved.is_empty(),
        "successful reprepare should reconcile its git operation"
    );
}

#[tokio::test]
async fn tick_reprepare_of_already_integrated_patch_does_not_leave_running_busy_planned_state()
{
    let repo = temp_git_repo();
    let seed_commit = head_oid(&repo).await.expect("seed head");
    std::fs::write(repo.join("tracked.txt"), "already integrated").expect("write change");
    git_sync(&repo, &["add", "tracked.txt"]);
    git_sync(&repo, &["commit", "-m", "already integrated"]);
    let source_commit = head_oid(&repo).await.expect("source commit");

    let db_path = std::env::temp_dir().join(format!(
        "ingot-runtime-empty-reprepare-{}.db",
        Uuid::now_v7()
    ));
    let db = Database::connect(&db_path).await.expect("connect db");
    db.migrate().await.expect("migrate db");
    let dispatcher = JobDispatcher::with_runner(
        db.clone(),
        ProjectLocks::default(),
        DispatcherConfig::new(std::env::temp_dir().join(format!(
            "ingot-runtime-empty-reprepare-state-{}",
            Uuid::now_v7()
        ))),
        Arc::new(FakeRunner),
    );

    let created_at = Utc::now();
    let project = ingot_domain::project::Project {
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
        ..test_revision(item_id, &seed_commit)
    };
    db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let authoring_workspace = Workspace {
        id: WorkspaceId::new(),
        project_id: project.id,
        kind: WorkspaceKind::Authoring,
        strategy: WorkspaceStrategy::Worktree,
        path: std::env::temp_dir()
            .join(format!(
                "ingot-runtime-empty-reprepare-authoring-{}",
                Uuid::now_v7()
            ))
            .display()
            .to_string(),
        created_for_revision_id: Some(revision.id),
        parent_workspace_id: None,
        target_ref: Some("refs/heads/main".into()),
        workspace_ref: Some("refs/ingot/workspaces/empty-reprepare-head".into()),
        base_commit_oid: Some(seed_commit.clone()),
        head_commit_oid: Some(source_commit.clone()),
        retention_policy: RetentionPolicy::Persistent,
        status: WorkspaceStatus::Ready,
        current_job_id: None,
        created_at,
        updated_at: created_at,
    };
    db.create_workspace(&authoring_workspace)
        .await
        .expect("create workspace");

    let author_job = Job {
        id: ingot_domain::ids::JobId::new(),
        project_id: project.id,
        item_id,
        item_revision_id: revision_id,
        step_id: step::AUTHOR_INITIAL.into(),
        semantic_attempt_no: 1,
        retry_no: 0,
        supersedes_job_id: None,
        status: JobStatus::Completed,
        outcome_class: Some(OutcomeClass::Clean),
        phase_kind: PhaseKind::Author,
        workspace_id: Some(authoring_workspace.id),
        workspace_kind: WorkspaceKind::Authoring,
        execution_permission: ExecutionPermission::MayMutate,
        context_policy: ContextPolicy::Fresh,
        phase_template_slug: "author-initial".into(),
        phase_template_digest: None,
        prompt_snapshot: None,
        job_input: JobInput::authoring_head(seed_commit.clone()),
        output_artifact_kind: OutputArtifactKind::Commit,
        output_commit_oid: Some(source_commit.clone()),
        result_schema_version: Some("commit_summary:v1".into()),
        result_payload: Some(serde_json::json!({
            "summary": "already integrated",
            "validation": null
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
    db.create_job(&author_job).await.expect("create author job");

    let candidate_validate_job = Job {
        id: ingot_domain::ids::JobId::new(),
        project_id: project.id,
        item_id,
        item_revision_id: revision_id,
        step_id: step::VALIDATE_CANDIDATE_INITIAL.into(),
        semantic_attempt_no: 1,
        retry_no: 0,
        supersedes_job_id: None,
        status: JobStatus::Completed,
        outcome_class: Some(OutcomeClass::Clean),
        phase_kind: PhaseKind::Validate,
        workspace_id: Some(authoring_workspace.id),
        workspace_kind: WorkspaceKind::Authoring,
        execution_permission: ExecutionPermission::MustNotMutate,
        context_policy: ContextPolicy::ResumeContext,
        phase_template_slug: "validate-candidate".into(),
        phase_template_digest: None,
        prompt_snapshot: None,
        job_input: JobInput::candidate_subject(seed_commit.clone(), source_commit.clone()),
        output_artifact_kind: OutputArtifactKind::ValidationReport,
        output_commit_oid: None,
        result_schema_version: Some("validation_report:v1".into()),
        result_payload: Some(serde_json::json!({
            "outcome": "clean",
            "summary": "candidate clean",
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
    db.create_job(&candidate_validate_job)
        .await
        .expect("create candidate validation");
    let stale_validate_job = Job {
        id: ingot_domain::ids::JobId::new(),
        project_id: project.id,
        item_id,
        item_revision_id: revision_id,
        step_id: step::VALIDATE_INTEGRATED.into(),
        semantic_attempt_no: 1,
        retry_no: 0,
        supersedes_job_id: None,
        status: JobStatus::Completed,
        outcome_class: Some(OutcomeClass::Clean),
        phase_kind: PhaseKind::Validate,
        workspace_id: None,
        workspace_kind: WorkspaceKind::Integration,
        execution_permission: ExecutionPermission::MustNotMutate,
        context_policy: ContextPolicy::ResumeContext,
        phase_template_slug: "validate-integrated".into(),
        phase_template_digest: None,
        prompt_snapshot: None,
        job_input: JobInput::integrated_subject(seed_commit.clone(), source_commit.clone()),
        output_artifact_kind: OutputArtifactKind::ValidationReport,
        output_commit_oid: None,
        result_schema_version: Some("validation_report:v1".into()),
        result_payload: Some(serde_json::json!({
            "outcome": "clean",
            "summary": "integrated clean",
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
        ended_at: Some(created_at + ChronoDuration::seconds(1)),
    };
    db.create_job(&stale_validate_job)
        .await
        .expect("create stale validation");
    db.create_queue_entry(&ConvergenceQueueEntry {
        id: ingot_domain::ids::ConvergenceQueueEntryId::new(),
        project_id: project.id,
        item_id,
        item_revision_id: revision.id,
        target_ref: "refs/heads/main".into(),
        status: ConvergenceQueueEntryStatus::Head,
        head_acquired_at: Some(created_at),
        created_at,
        updated_at: created_at,
        released_at: None,
    })
    .await
    .expect("insert queue entry");

    assert!(
        dispatcher
            .tick()
            .await
            .expect("tick should reprepare cleanly")
    );

    let convergences = db
        .list_convergences_by_item(item.id)
        .await
        .expect("list convergences");
    assert!(
        convergences
            .iter()
            .any(|convergence| convergence.status == ConvergenceStatus::Prepared),
        "reprepare should leave a prepared convergence rather than a running one"
    );
    assert!(
        !convergences
            .iter()
            .any(|convergence| convergence.status == ConvergenceStatus::Running),
        "empty replay must not strand a running convergence"
    );
    let workspaces = db
        .list_workspaces_by_item(item.id)
        .await
        .expect("list workspaces");
    assert!(
        !workspaces.iter().any(|workspace| {
            workspace.kind == WorkspaceKind::Integration
                && workspace.status == WorkspaceStatus::Busy
        }),
        "empty replay must not strand a busy integration workspace"
    );
    let unresolved = db
        .list_unresolved_git_operations()
        .await
        .expect("list unresolved");
    assert!(
        unresolved.is_empty(),
        "empty replay should not leave a planned convergence git operation behind"
    );
    let jobs = db.list_jobs_by_item(item.id).await.expect("jobs");
    assert!(
        jobs.iter().any(|job| {
            job.step_id == step::VALIDATE_INTEGRATED
                && job.status == JobStatus::Queued
                && job.item_revision_id == revision.id
        }),
        "reprepare should queue a fresh integrated validation job"
    );
}

#[tokio::test]
async fn fail_prepare_convergence_attempt_marks_non_conflict_failures_as_step_failed() {
    let h = TestHarness::new(Arc::new(FakeRunner)).await;

    let seed_commit = head_oid(&h.repo_path).await.expect("seed head");
    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let item = ingot_domain::item::Item {
        id: item_id,
        current_revision_id: revision_id,
        approval_state: ApprovalState::Granted,
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
    let mut integration_workspace = Workspace {
        id: WorkspaceId::new(),
        project_id: h.project.id,
        kind: WorkspaceKind::Integration,
        strategy: WorkspaceStrategy::Worktree,
        path: std::env::temp_dir()
            .join(format!(
                "ingot-runtime-prepare-failure-workspace-{}",
                Uuid::now_v7()
            ))
            .display()
            .to_string(),
        created_for_revision_id: Some(revision.id),
        parent_workspace_id: None,
        target_ref: Some("refs/heads/main".into()),
        workspace_ref: Some("refs/ingot/workspaces/prepare-failure".into()),
        base_commit_oid: Some(seed_commit.clone()),
        head_commit_oid: Some(seed_commit.clone()),
        retention_policy: RetentionPolicy::Persistent,
        status: WorkspaceStatus::Busy,
        current_job_id: None,
        created_at,
        updated_at: created_at,
    };
    h.db.create_workspace(&integration_workspace)
        .await
        .expect("create integration workspace");
    let source_workspace = Workspace {
        id: WorkspaceId::new(),
        project_id: h.project.id,
        kind: WorkspaceKind::Authoring,
        strategy: WorkspaceStrategy::Worktree,
        path: std::env::temp_dir()
            .join(format!(
                "ingot-runtime-prepare-failure-source-{}",
                Uuid::now_v7()
            ))
            .display()
            .to_string(),
        created_for_revision_id: Some(revision.id),
        parent_workspace_id: None,
        target_ref: Some("refs/heads/main".into()),
        workspace_ref: Some("refs/ingot/workspaces/prepare-failure-source".into()),
        base_commit_oid: Some(seed_commit.clone()),
        head_commit_oid: Some(seed_commit.clone()),
        retention_policy: RetentionPolicy::Persistent,
        status: WorkspaceStatus::Ready,
        current_job_id: None,
        created_at,
        updated_at: created_at,
    };
    h.db.create_workspace(&source_workspace)
        .await
        .expect("create source workspace");

    let mut convergence = Convergence {
        id: ingot_domain::ids::ConvergenceId::new(),
        project_id: h.project.id,
        item_id,
        item_revision_id: revision_id,
        source_workspace_id: source_workspace.id,
        integration_workspace_id: Some(integration_workspace.id),
        source_head_commit_oid: seed_commit.clone(),
        target_ref: "refs/heads/main".into(),
        strategy: ConvergenceStrategy::RebaseThenFastForward,
        status: ConvergenceStatus::Running,
        input_target_commit_oid: Some(seed_commit.clone()),
        prepared_commit_oid: None,
        final_target_commit_oid: None,
        target_head_valid: Some(true),
        conflict_summary: None,
        created_at,
        completed_at: None,
    };
    h.db.create_convergence(&convergence)
        .await
        .expect("create convergence");
    let queue_entry = ConvergenceQueueEntry {
        id: ingot_domain::ids::ConvergenceQueueEntryId::new(),
        project_id: h.project.id,
        item_id,
        item_revision_id: revision.id,
        target_ref: "refs/heads/main".into(),
        status: ConvergenceQueueEntryStatus::Head,
        head_acquired_at: Some(created_at),
        created_at,
        updated_at: created_at,
        released_at: None,
    };
    h.db.create_queue_entry(&queue_entry)
        .await
        .expect("create queue entry");
    let mut operation = GitOperation {
        id: GitOperationId::new(),
        project_id: h.project.id,
        operation_kind: OperationKind::PrepareConvergenceCommit,
        entity_type: GitEntityType::Convergence,
        entity_id: convergence.id.to_string(),
        workspace_id: Some(integration_workspace.id),
        ref_name: integration_workspace.workspace_ref.clone(),
        expected_old_oid: Some(seed_commit.clone()),
        new_oid: None,
        commit_oid: None,
        status: GitOperationStatus::Planned,
        metadata: Some(serde_json::json!({
            "source_commit_oids": [seed_commit.clone()],
            "prepared_commit_oids": [],
        })),
        created_at,
        completed_at: None,
    };
    h.db.create_git_operation(&operation)
        .await
        .expect("create git operation");

    h.dispatcher
        .fail_prepare_convergence_attempt(
            &h.project,
            &item,
            &revision,
            &queue_entry,
            &mut integration_workspace,
            &mut convergence,
            &mut operation,
            std::slice::from_ref(&seed_commit),
            &[],
            "non-conflict failure".into(),
            ConvergenceStatus::Failed,
        )
        .await
        .expect("fail prepare attempt");

    let updated_item = h.db.get_item(item.id).await.expect("item");
    assert_eq!(
        updated_item.escalation_state,
        EscalationState::OperatorRequired
    );
    assert_eq!(
        updated_item.escalation_reason,
        Some(EscalationReason::StepFailed)
    );
    let activity = h
        .db
        .list_activity_by_project(h.project.id, 20, 0)
        .await
        .expect("activity");
    assert!(
        activity.iter().any(|row| {
            row.event_type == ActivityEventType::ItemEscalated
                && row.payload.get("reason").and_then(|value| value.as_str())
                    == Some("step_failed")
        }),
        "item escalation activity should carry the step_failed reason"
    );
}

#[tokio::test]
async fn candidate_repair_loop_advances_to_prepare_convergence() {
    let h = TestHarness::new(Arc::new(ScriptedLoopRunner)).await;
    h.register_full_agent().await;

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

    let author_job = dispatch_job(
        &item,
        &revision,
        &[],
        &[],
        &[],
        DispatchJobCommand { step_id: None },
    )
    .expect("dispatch author initial");
    h.db.create_job(&author_job).await.expect("create author job");
    h.dispatcher.tick().await.expect("author tick");

    let mut jobs = h.db.list_jobs_by_item(item.id).await.expect("jobs");
    let review_initial = jobs
        .iter()
        .find(|job| job.step_id == step::REVIEW_INCREMENTAL_INITIAL)
        .cloned()
        .expect("auto-dispatched review initial");
    assert_eq!(review_initial.status, JobStatus::Queued);
    h.dispatcher.tick().await.expect("review initial tick");

    jobs = h.db.list_jobs_by_item(item.id).await.expect("jobs");
    let repair_job = dispatch_job(
        &item,
        &revision,
        &jobs,
        &[],
        &[],
        DispatchJobCommand { step_id: None },
    )
    .expect("dispatch repair candidate");
    h.db.create_job(&repair_job).await.expect("create repair");
    h.dispatcher.tick().await.expect("repair tick");

    jobs = h.db.list_jobs_by_item(item.id).await.expect("jobs");
    let review_incremental_repair = jobs
        .iter()
        .find(|job| job.step_id == step::REVIEW_INCREMENTAL_REPAIR)
        .cloned()
        .expect("auto-dispatched review incremental repair");
    assert_eq!(review_incremental_repair.status, JobStatus::Queued);
    h.dispatcher
        .tick()
        .await
        .expect("review incremental repair tick");

    jobs = h.db.list_jobs_by_item(item.id).await.expect("jobs");
    let review_candidate_repair = jobs
        .iter()
        .find(|job| job.step_id == step::REVIEW_CANDIDATE_REPAIR)
        .cloned()
        .expect("auto-dispatched review candidate repair");
    assert_eq!(review_candidate_repair.status, JobStatus::Queued);
    h.dispatcher
        .tick()
        .await
        .expect("review candidate repair tick");

    jobs = h.db.list_jobs_by_item(item.id).await.expect("jobs");
    let validate_candidate_repair = dispatch_job(
        &item,
        &revision,
        &jobs,
        &[],
        &[],
        DispatchJobCommand { step_id: None },
    )
    .expect("dispatch validate candidate repair");
    h.db.create_job(&validate_candidate_repair)
        .await
        .expect("create validate candidate repair");
    h.dispatcher
        .tick()
        .await
        .expect("validate candidate repair tick");

    let jobs = h.db.list_jobs_by_item(item.id).await.expect("jobs");
    let evaluation = Evaluator::new().evaluate(&item, &revision, &jobs, &[], &[]);
    assert_eq!(evaluation.next_recommended_action, "prepare_convergence");
}
