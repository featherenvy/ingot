use std::sync::Arc;
use std::time::Duration;

use ingot_agent_runtime::{DispatcherConfig, JobDispatcher};
use ingot_domain::item::{ApprovalState, Escalation, EscalationReason, ResolutionSource};
use ingot_domain::job::{
    ContextPolicy, ExecutionPermission, JobInput, JobStatus, OutcomeClass, OutputArtifactKind,
    PhaseKind,
};
use ingot_domain::revision::ApprovalPolicy;
use ingot_domain::workspace::{WorkspaceKind, WorkspaceStatus};
use ingot_git::commands::{head_oid, resolve_ref_oid};
use ingot_usecases::ProjectLocks;

mod common;
use common::*;
use ingot_domain::activity::ActivityEventType;
use ingot_domain::convergence::ConvergenceStatus;
use ingot_domain::convergence_queue::ConvergenceQueueEntryStatus;
use ingot_domain::git_operation::{GitEntityType, GitOperationStatus, OperationKind};
use ingot_test_support::fixtures::{
    GitOperationBuilder, JobBuilder, ProjectBuilder, RevisionBuilder, WorkspaceBuilder,
};
use ingot_test_support::git::unique_temp_path;
use tokio::time::timeout;
use ingot_usecases::job::{DispatchJobCommand, dispatch_job};
use ingot_workflow::{Evaluator, RecommendedAction, step};

struct BlockedAutoFinalizeFixture {
    db: ingot_store_sqlite::Database,
    dispatcher: JobDispatcher,
    item_id: ingot_domain::ids::ItemId,
    convergence_id: ingot_domain::ids::ConvergenceId,
    integration_workspace_path: std::path::PathBuf,
}

async fn blocked_auto_finalize_fixture() -> BlockedAutoFinalizeFixture {
    let repo = temp_git_repo("ingot-runtime-repo");
    let base_commit = head_oid(&repo).await.expect("base head");
    std::fs::write(repo.join("tracked.txt"), "prepared").expect("write file");
    git_sync(&repo, &["add", "tracked.txt"]);
    git_sync(&repo, &["commit", "-m", "prepared"]);
    let prepared_commit = head_oid(&repo).await.expect("prepared head");
    git_sync(&repo, &["reset", "--hard", &base_commit]);
    git_sync(&repo, &["checkout", "-b", "feature"]);
    let integration_workspace_path = unique_temp_path("ingot-runtime-integration-blocked");

    let db = migrated_test_db("ingot-runtime-finalize-blocked-auto").await;
    let state_root = unique_temp_path("ingot-runtime-finalize-blocked-auto-state");
    let dispatcher = JobDispatcher::with_runner(
        db.clone(),
        ProjectLocks::default(),
        DispatcherConfig::new(state_root.clone()),
        Arc::new(FakeRunner),
    );

    let created_at = default_timestamp();
    let project = ProjectBuilder::new(&repo).created_at(created_at).build();
    db.create_project(&project).await.expect("create project");
    let paths = ensure_test_mirror(state_root.as_path(), &project).await;
    git_sync(
        &paths.mirror_git_dir,
        &[
            "update-ref",
            "refs/ingot/workspaces/wrk_integration_blocked",
            &prepared_commit,
        ],
    );
    git_sync(
        &paths.mirror_git_dir,
        &[
            "worktree",
            "add",
            "--detach",
            integration_workspace_path.to_str().expect("workspace path"),
            "refs/ingot/workspaces/wrk_integration_blocked",
        ],
    );

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let item = ItemBuilder::new(project.id, revision_id)
        .id(item_id)
        .approval_state(ApprovalState::NotRequired)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .approval_policy(ApprovalPolicy::NotRequired)
        .explicit_seed(&base_commit)
        .build();
    db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let integration_workspace = WorkspaceBuilder::new(project.id, WorkspaceKind::Integration)
        .path(integration_workspace_path.display().to_string())
        .workspace_ref("refs/ingot/workspaces/wrk_integration_blocked")
        .created_for_revision_id(revision.id)
        .base_commit_oid(base_commit.clone())
        .head_commit_oid(prepared_commit.clone())
        .created_at(created_at)
        .build();
    db.create_workspace(&integration_workspace)
        .await
        .expect("create workspace");
    let source_workspace = WorkspaceBuilder::new(project.id, WorkspaceKind::Authoring)
        .created_for_revision_id(revision.id)
        .base_commit_oid(base_commit.clone())
        .head_commit_oid(prepared_commit.clone())
        .created_at(created_at)
        .build();
    db.create_workspace(&source_workspace)
        .await
        .expect("create source workspace");

    let validate_job = JobBuilder::new(project.id, item_id, revision_id, "validate_integrated")
        .status(JobStatus::Completed)
        .outcome_class(OutcomeClass::Clean)
        .phase_kind(PhaseKind::Validate)
        .workspace_id(integration_workspace.id)
        .workspace_kind(WorkspaceKind::Integration)
        .execution_permission(ExecutionPermission::MustNotMutate)
        .context_policy(ContextPolicy::ResumeContext)
        .phase_template_slug("validate-integrated")
        .job_input(JobInput::integrated_subject(
            base_commit.clone(),
            prepared_commit.clone(),
        ))
        .output_artifact_kind(OutputArtifactKind::ValidationReport)
        .result_schema_version("validation_report:v1")
        .result_payload(serde_json::json!({
            "outcome": "clean",
            "summary": "integrated clean",
            "checks": [],
            "findings": []
        }))
        .created_at(created_at)
        .started_at(created_at)
        .ended_at(created_at)
        .build();
    db.create_job(&validate_job).await.expect("create job");

    let convergence = ConvergenceBuilder::new(project.id, item_id, revision_id)
        .source_workspace_id(source_workspace.id)
        .integration_workspace_id(integration_workspace.id)
        .source_head_commit_oid(prepared_commit.clone())
        .input_target_commit_oid(base_commit.clone())
        .prepared_commit_oid(prepared_commit.clone())
        .target_head_valid(true)
        .created_at(created_at)
        .build();
    db.create_convergence(&convergence)
        .await
        .expect("create convergence");
    db.create_queue_entry(
        &ConvergenceQueueEntryBuilder::new(project.id, item.id, revision.id)
            .created_at(created_at)
            .build(),
    )
    .await
    .expect("insert queue entry");

    BlockedAutoFinalizeFixture {
        db,
        dispatcher,
        item_id,
        convergence_id: convergence.id,
        integration_workspace_path,
    }
}

async fn assert_blocked_auto_finalize_state(fixture: &BlockedAutoFinalizeFixture) {
    let updated_item = fixture.db.get_item(fixture.item_id).await.expect("item");
    assert!(updated_item.lifecycle.is_open());
    assert!(matches!(
        updated_item.escalation,
        Escalation::OperatorRequired {
            reason: EscalationReason::CheckoutSyncBlocked
        }
    ));
    let updated_convergence = fixture
        .db
        .get_convergence(fixture.convergence_id)
        .await
        .expect("convergence");
    assert_eq!(
        updated_convergence.state.status(),
        ConvergenceStatus::Prepared
    );
    let queue_entries = fixture
        .db
        .list_queue_entries_by_item(fixture.item_id)
        .await
        .expect("list queue entries");
    assert_eq!(queue_entries[0].status, ConvergenceQueueEntryStatus::Head);
    let unresolved = fixture
        .db
        .list_unresolved_git_operations()
        .await
        .expect("list unresolved");
    assert_eq!(unresolved.len(), 1, "blocked finalize should stay unresolved");
    assert_eq!(
        unresolved[0].operation_kind(),
        OperationKind::FinalizeTargetRef
    );
    assert!(fixture.integration_workspace_path.exists());
}

#[tokio::test]
async fn tick_auto_finalizes_prepared_convergence_for_not_required_approval() {
    let repo = temp_git_repo("ingot-runtime-repo");
    let base_commit = head_oid(&repo).await.expect("base head");
    std::fs::write(repo.join("tracked.txt"), "prepared").expect("write file");
    git_sync(&repo, &["add", "tracked.txt"]);
    git_sync(&repo, &["commit", "-m", "prepared"]);
    let prepared_commit = head_oid(&repo).await.expect("prepared head");
    git_sync(&repo, &["reset", "--hard", &base_commit]);
    let integration_workspace_path = unique_temp_path("ingot-runtime-integration");

    let db = migrated_test_db("ingot-runtime-finalize").await;
    let state_root = unique_temp_path("ingot-runtime-finalize-state");
    let dispatcher = JobDispatcher::with_runner(
        db.clone(),
        ProjectLocks::default(),
        DispatcherConfig::new(state_root.clone()),
        Arc::new(FakeRunner),
    );

    let created_at = default_timestamp();
    let project = ProjectBuilder::new(&repo).created_at(created_at).build();
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
    let item = ItemBuilder::new(project.id, revision_id)
        .id(item_id)
        .approval_state(ApprovalState::NotRequired)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .approval_policy(ApprovalPolicy::NotRequired)
        .explicit_seed(&base_commit)
        .build();
    db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let integration_workspace = WorkspaceBuilder::new(project.id, WorkspaceKind::Integration)
        .path(integration_workspace_path.display().to_string())
        .workspace_ref("refs/ingot/workspaces/wrk_integration_test")
        .created_for_revision_id(revision.id)
        .base_commit_oid(base_commit.clone())
        .head_commit_oid(prepared_commit.clone())
        .created_at(created_at)
        .build();
    db.create_workspace(&integration_workspace)
        .await
        .expect("create workspace");
    let source_workspace = WorkspaceBuilder::new(project.id, WorkspaceKind::Authoring)
        .created_for_revision_id(revision.id)
        .base_commit_oid(base_commit.clone())
        .head_commit_oid(prepared_commit.clone())
        .created_at(created_at)
        .build();
    db.create_workspace(&source_workspace)
        .await
        .expect("create source workspace");

    let validate_job = JobBuilder::new(project.id, item_id, revision_id, "validate_integrated")
        .status(JobStatus::Completed)
        .outcome_class(OutcomeClass::Clean)
        .phase_kind(PhaseKind::Validate)
        .workspace_id(integration_workspace.id)
        .workspace_kind(WorkspaceKind::Integration)
        .execution_permission(ExecutionPermission::MustNotMutate)
        .context_policy(ContextPolicy::ResumeContext)
        .phase_template_slug("validate-integrated")
        .job_input(JobInput::integrated_subject(
            base_commit.clone(),
            prepared_commit.clone(),
        ))
        .output_artifact_kind(OutputArtifactKind::ValidationReport)
        .result_schema_version("validation_report:v1")
        .result_payload(serde_json::json!({
            "outcome": "clean",
            "summary": "integrated clean",
            "checks": [],
            "findings": []
        }))
        .created_at(created_at)
        .started_at(created_at)
        .ended_at(created_at)
        .build();
    db.create_job(&validate_job).await.expect("create job");

    let convergence = ConvergenceBuilder::new(project.id, item_id, revision_id)
        .source_workspace_id(source_workspace.id)
        .integration_workspace_id(integration_workspace.id)
        .source_head_commit_oid(prepared_commit.clone())
        .input_target_commit_oid(base_commit.clone())
        .prepared_commit_oid(prepared_commit.clone())
        .target_head_valid(true)
        .created_at(created_at)
        .build();
    db.create_convergence(&convergence)
        .await
        .expect("create convergence");
    db.create_queue_entry(
        &ConvergenceQueueEntryBuilder::new(project.id, item.id, revision.id)
            .created_at(created_at)
            .build(),
    )
    .await
    .expect("insert queue entry");

    assert!(dispatcher.tick().await.expect("tick should finalize"));

    let updated_item = db.get_item(item.id).await.expect("updated item");
    assert!(updated_item.lifecycle.is_done());
    assert_eq!(
        updated_item.lifecycle.resolution_source(),
        Some(ResolutionSource::SystemCommand)
    );
    let updated_convergence = db
        .list_convergences_by_item(item.id)
        .await
        .expect("list convergences")
        .into_iter()
        .next()
        .expect("convergence");
    assert_eq!(
        updated_convergence.state.status(),
        ConvergenceStatus::Finalized
    );
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
async fn reconcile_startup_does_not_spin_when_auto_finalize_is_blocked() {
    let fixture = blocked_auto_finalize_fixture().await;

    timeout(Duration::from_secs(10), fixture.dispatcher.reconcile_startup())
        .await
        .expect("startup should not hang")
        .expect("reconcile startup");

    assert_blocked_auto_finalize_state(&fixture).await;
}

#[tokio::test]
async fn tick_reports_no_progress_when_auto_finalize_is_blocked() {
    let fixture = blocked_auto_finalize_fixture().await;

    assert!(
        !fixture
            .dispatcher
            .tick()
            .await
            .expect("blocked auto-finalize should not report progress")
    );

    assert_blocked_auto_finalize_state(&fixture).await;
}

#[tokio::test]
async fn tick_auto_finalizes_not_required_prepared_convergence_even_when_commit_exists_only_in_mirror()
 {
    let repo = temp_git_repo("ingot-runtime-repo");
    let base_commit = head_oid(&repo).await.expect("base head");
    let db = migrated_test_db("ingot-runtime-finalize-mirror-only").await;
    let state_root = unique_temp_path("ingot-runtime-finalize-mirror-only-state");
    let dispatcher = JobDispatcher::with_runner(
        db.clone(),
        ProjectLocks::default(),
        DispatcherConfig::new(state_root.clone()),
        Arc::new(FakeRunner),
    );

    let created_at = default_timestamp();
    let project = ProjectBuilder::new(&repo).created_at(created_at).build();
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
    let item = ItemBuilder::new(project.id, revision_id)
        .id(item_id)
        .approval_state(ApprovalState::NotRequired)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .approval_policy(ingot_domain::revision::ApprovalPolicy::NotRequired)
        .explicit_seed(&base_commit)
        .build();
    db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let integration_workspace = WorkspaceBuilder::new(project.id, WorkspaceKind::Integration)
        .path(integration_workspace_path.display().to_string())
        .workspace_ref(workspace_ref)
        .created_for_revision_id(revision.id)
        .base_commit_oid(base_commit.clone())
        .head_commit_oid(prepared_commit.clone())
        .created_at(created_at)
        .build();
    db.create_workspace(&integration_workspace)
        .await
        .expect("create integration workspace");
    let source_workspace = WorkspaceBuilder::new(project.id, WorkspaceKind::Authoring)
        .created_for_revision_id(revision.id)
        .base_commit_oid(base_commit.clone())
        .head_commit_oid(prepared_commit.clone())
        .created_at(created_at)
        .build();
    db.create_workspace(&source_workspace)
        .await
        .expect("create source workspace");

    let validate_job = JobBuilder::new(project.id, item_id, revision_id, step::VALIDATE_INTEGRATED)
        .status(JobStatus::Completed)
        .outcome_class(OutcomeClass::Clean)
        .phase_kind(PhaseKind::Validate)
        .workspace_id(integration_workspace.id)
        .workspace_kind(WorkspaceKind::Integration)
        .execution_permission(ExecutionPermission::MustNotMutate)
        .context_policy(ContextPolicy::ResumeContext)
        .phase_template_slug("validate-integrated")
        .job_input(JobInput::integrated_subject(
            base_commit.clone(),
            prepared_commit.clone(),
        ))
        .output_artifact_kind(OutputArtifactKind::ValidationReport)
        .result_schema_version("validation_report:v1")
        .result_payload(serde_json::json!({
            "outcome": "clean",
            "summary": "integrated clean",
            "checks": [],
            "findings": []
        }))
        .created_at(created_at)
        .started_at(created_at)
        .ended_at(created_at)
        .build();
    db.create_job(&validate_job)
        .await
        .expect("create validation");

    let convergence = ConvergenceBuilder::new(project.id, item_id, revision_id)
        .source_workspace_id(source_workspace.id)
        .integration_workspace_id(integration_workspace.id)
        .source_head_commit_oid(prepared_commit.clone())
        .input_target_commit_oid(base_commit.clone())
        .prepared_commit_oid(prepared_commit.clone())
        .target_head_valid(true)
        .created_at(created_at)
        .build();
    db.create_convergence(&convergence)
        .await
        .expect("create convergence");
    db.create_queue_entry(
        &ConvergenceQueueEntryBuilder::new(project.id, item_id, revision.id)
            .created_at(created_at)
            .build(),
    )
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
    assert_eq!(
        updated_convergence.state.status(),
        ConvergenceStatus::Finalized
    );
    let updated_item = db.get_item(item.id).await.expect("item");
    assert!(updated_item.lifecycle.is_done());
    assert_eq!(updated_item.approval_state, ApprovalState::NotRequired);
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
    let repo = temp_git_repo("ingot-runtime-repo");
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
    let created_at = default_timestamp();
    let project = ProjectBuilder::new(&repo).created_at(created_at).build();
    h.db.create_project(&project).await.expect("create project");

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let item = ItemBuilder::new(project.id, revision_id)
        .id(item_id)
        .approval_state(ApprovalState::Pending)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .explicit_seed(&base_commit)
        .build();
    h.db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let integration_workspace = WorkspaceBuilder::new(project.id, WorkspaceKind::Integration)
        .created_for_revision_id(revision.id)
        .base_commit_oid(base_commit.clone())
        .head_commit_oid(prepared_commit.clone())
        .created_at(created_at)
        .build();
    h.db.create_workspace(&integration_workspace)
        .await
        .expect("create workspace");
    let source_workspace = WorkspaceBuilder::new(project.id, WorkspaceKind::Authoring)
        .created_for_revision_id(revision.id)
        .base_commit_oid(base_commit.clone())
        .head_commit_oid(prepared_commit.clone())
        .created_at(created_at)
        .build();
    h.db.create_workspace(&source_workspace)
        .await
        .expect("create source workspace");

    let validate_job = JobBuilder::new(project.id, item_id, revision_id, "validate_integrated")
        .status(JobStatus::Completed)
        .outcome_class(OutcomeClass::Clean)
        .phase_kind(PhaseKind::Validate)
        .workspace_id(integration_workspace.id)
        .workspace_kind(WorkspaceKind::Integration)
        .execution_permission(ExecutionPermission::MustNotMutate)
        .context_policy(ContextPolicy::ResumeContext)
        .phase_template_slug("validate-integrated")
        .job_input(JobInput::integrated_subject(
            base_commit.clone(),
            prepared_commit.clone(),
        ))
        .output_artifact_kind(OutputArtifactKind::ValidationReport)
        .result_schema_version("validation_report:v1")
        .result_payload(serde_json::json!({
            "outcome": "clean",
            "summary": "integrated clean",
            "checks": [],
            "findings": []
        }))
        .created_at(created_at)
        .started_at(created_at)
        .ended_at(created_at)
        .build();
    h.db.create_job(&validate_job).await.expect("create job");

    let convergence = ConvergenceBuilder::new(project.id, item_id, revision_id)
        .source_workspace_id(source_workspace.id)
        .integration_workspace_id(integration_workspace.id)
        .source_head_commit_oid(prepared_commit.clone())
        .input_target_commit_oid(base_commit.clone())
        .prepared_commit_oid(prepared_commit.clone())
        .target_head_valid(false)
        .created_at(created_at)
        .build();
    h.db.create_convergence(&convergence)
        .await
        .expect("create convergence");

    assert!(h.dispatcher.tick().await.expect("tick should invalidate"));

    let updated_item = h.db.get_item(item.id).await.expect("updated item");
    assert_eq!(updated_item.approval_state, ApprovalState::NotRequested);
    let updated_convergence =
        h.db.list_convergences_by_item(item.id)
            .await
            .expect("list convergences")
            .into_iter()
            .next()
            .expect("convergence");
    assert_eq!(
        updated_convergence.state.status(),
        ConvergenceStatus::Failed
    );
    assert_eq!(
        updated_convergence.state.conflict_summary(),
        Some("target_ref_moved")
    );
    let updated_workspace =
        h.db.get_workspace(integration_workspace.id)
            .await
            .expect("workspace");
    assert_eq!(updated_workspace.state.status(), WorkspaceStatus::Stale);
}

#[tokio::test]
async fn tick_reconciles_applied_finalize_operation_instead_of_invalidating_prepared_convergence() {
    let repo = temp_git_repo("ingot-runtime-repo");
    let base_commit = head_oid(&repo).await.expect("base head");
    std::fs::write(repo.join("tracked.txt"), "prepared").expect("write prepared");
    git_sync(&repo, &["add", "tracked.txt"]);
    git_sync(&repo, &["commit", "-m", "prepared"]);
    let prepared_commit = head_oid(&repo).await.expect("prepared head");

    let db = migrated_test_db("ingot-runtime-finalize-reconcile").await;
    let state_root = unique_temp_path("ingot-runtime-finalize-reconcile-state");
    let dispatcher = JobDispatcher::with_runner(
        db.clone(),
        ProjectLocks::default(),
        DispatcherConfig::new(state_root.clone()),
        Arc::new(FakeRunner),
    );

    let created_at = default_timestamp();
    let project = ProjectBuilder::new(&repo).created_at(created_at).build();
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
    let item = ItemBuilder::new(project.id, revision_id)
        .id(item_id)
        .approval_state(ApprovalState::NotRequired)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .approval_policy(ingot_domain::revision::ApprovalPolicy::NotRequired)
        .explicit_seed(&base_commit)
        .build();
    db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let source_workspace = WorkspaceBuilder::new(project.id, WorkspaceKind::Authoring)
        .created_for_revision_id(revision.id)
        .base_commit_oid(base_commit.clone())
        .head_commit_oid(prepared_commit.clone())
        .created_at(created_at)
        .build();
    db.create_workspace(&source_workspace)
        .await
        .expect("create source workspace");

    let integration_workspace = WorkspaceBuilder::new(project.id, WorkspaceKind::Integration)
        .created_for_revision_id(revision.id)
        .base_commit_oid(base_commit.clone())
        .head_commit_oid(prepared_commit.clone())
        .created_at(created_at)
        .build();
    db.create_workspace(&integration_workspace)
        .await
        .expect("create integration workspace");

    let convergence = ConvergenceBuilder::new(project.id, item_id, revision_id)
        .source_workspace_id(source_workspace.id)
        .integration_workspace_id(integration_workspace.id)
        .source_head_commit_oid(prepared_commit.clone())
        .input_target_commit_oid(base_commit.clone())
        .prepared_commit_oid(prepared_commit.clone())
        .target_head_valid(true)
        .created_at(created_at)
        .build();
    db.create_convergence(&convergence)
        .await
        .expect("create convergence");
    db.create_queue_entry(
        &ConvergenceQueueEntryBuilder::new(project.id, item_id, revision.id)
            .created_at(created_at)
            .build(),
    )
    .await
    .expect("insert queue entry");
    db.create_git_operation(
        &GitOperationBuilder::new(
            project.id,
            OperationKind::FinalizeTargetRef,
            GitEntityType::Convergence,
            convergence.id.to_string(),
        )
        .ref_name("refs/heads/main")
        .expected_old_oid(base_commit.clone())
        .new_oid(prepared_commit.clone())
        .commit_oid(prepared_commit.clone())
        .status(GitOperationStatus::Applied)
        .created_at(created_at)
        .completed_at(created_at)
        .build(),
    )
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
    assert_eq!(
        updated_convergence.state.status(),
        ConvergenceStatus::Finalized
    );
    let updated_item = db.get_item(item.id).await.expect("item");
    assert!(updated_item.lifecycle.is_done());
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
async fn fail_prepare_convergence_attempt_marks_non_conflict_failures_as_step_failed() {
    let h = TestHarness::new(Arc::new(FakeRunner)).await;

    let seed_commit = head_oid(&h.repo_path).await.expect("seed head");
    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let item = ItemBuilder::new(h.project.id, revision_id)
        .id(item_id)
        .approval_state(ApprovalState::NotRequired)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .approval_policy(ingot_domain::revision::ApprovalPolicy::NotRequired)
        .explicit_seed(&seed_commit)
        .build();
    h.db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let created_at = default_timestamp();
    let mut integration_workspace = WorkspaceBuilder::new(h.project.id, WorkspaceKind::Integration)
        .created_for_revision_id(revision.id)
        .base_commit_oid(seed_commit.clone())
        .head_commit_oid(seed_commit.clone())
        .status(WorkspaceStatus::Provisioning)
        .created_at(created_at)
        .build();
    h.db.create_workspace(&integration_workspace)
        .await
        .expect("create integration workspace");
    let source_workspace = WorkspaceBuilder::new(h.project.id, WorkspaceKind::Authoring)
        .created_for_revision_id(revision.id)
        .base_commit_oid(seed_commit.clone())
        .head_commit_oid(seed_commit.clone())
        .created_at(created_at)
        .build();
    h.db.create_workspace(&source_workspace)
        .await
        .expect("create source workspace");

    let mut convergence = ConvergenceBuilder::new(h.project.id, item_id, revision_id)
        .source_workspace_id(source_workspace.id)
        .integration_workspace_id(integration_workspace.id)
        .source_head_commit_oid(seed_commit.clone())
        .status(ConvergenceStatus::Running)
        .input_target_commit_oid(seed_commit.clone())
        .no_prepared_commit_oid()
        .target_head_valid(true)
        .created_at(created_at)
        .build();
    h.db.create_convergence(&convergence)
        .await
        .expect("create convergence");
    let queue_entry = ConvergenceQueueEntryBuilder::new(h.project.id, item_id, revision.id)
        .created_at(created_at)
        .build();
    h.db.create_queue_entry(&queue_entry)
        .await
        .expect("create queue entry");
    let mut operation = GitOperationBuilder::new(
        h.project.id,
        OperationKind::PrepareConvergenceCommit,
        GitEntityType::Convergence,
        convergence.id.to_string(),
    )
    .workspace_id(integration_workspace.id)
    .ref_name(
        integration_workspace
            .workspace_ref
            .clone()
            .expect("workspace ref"),
    )
    .expected_old_oid(seed_commit.clone())
    .status(GitOperationStatus::Planned)
    .metadata(serde_json::json!({
        "source_commit_oids": [seed_commit.clone()],
        "prepared_commit_oids": [],
    }))
    .created_at(created_at)
    .build();
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
            ingot_domain::convergence::PrepareFailureKind::Failed,
        )
        .await
        .expect("fail prepare attempt");

    let updated_item = h.db.get_item(item.id).await.expect("item");
    assert!(matches!(
        updated_item.escalation,
        Escalation::OperatorRequired {
            reason: EscalationReason::StepFailed
        }
    ));
    let activity =
        h.db.list_activity_by_project(h.project.id, 20, 0)
            .await
            .expect("activity");
    assert!(
        activity.iter().any(|row| {
            row.event_type == ActivityEventType::ItemEscalated
                && row.payload.get("reason").and_then(|value| value.as_str()) == Some("step_failed")
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

    let item = ItemBuilder::new(h.project.id, revision_id)
        .id(item_id)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .explicit_seed(&seed_commit)
        .build();
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
    h.db.create_job(&author_job)
        .await
        .expect("create author job");
    h.dispatcher.tick().await.expect("author tick");

    let mut jobs = h.db.list_jobs_by_item(item.id).await.expect("jobs");
    let review_initial = jobs
        .iter()
        .find(|job| job.step_id == step::REVIEW_INCREMENTAL_INITIAL)
        .cloned()
        .expect("auto-dispatched review initial");
    assert_eq!(review_initial.state.status(), JobStatus::Queued);
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
    assert_eq!(review_incremental_repair.state.status(), JobStatus::Queued);
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
    assert_eq!(review_candidate_repair.state.status(), JobStatus::Queued);
    h.dispatcher
        .tick()
        .await
        .expect("review candidate repair tick");

    jobs = h.db.list_jobs_by_item(item.id).await.expect("jobs");
    let validate_candidate_repair = jobs
        .iter()
        .find(|job| job.step_id == step::VALIDATE_CANDIDATE_REPAIR)
        .cloned()
        .expect("auto-dispatched validate candidate repair");
    assert_eq!(validate_candidate_repair.state.status(), JobStatus::Queued);
    h.dispatcher
        .tick()
        .await
        .expect("validate candidate repair tick");

    let jobs = h.db.list_jobs_by_item(item.id).await.expect("jobs");
    let evaluation = Evaluator::new().evaluate(&item, &revision, &jobs, &[], &[]);
    assert_eq!(
        evaluation.next_recommended_action,
        RecommendedAction::PrepareConvergence
    );
}
