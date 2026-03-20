use ingot_domain::commit_oid::CommitOid;
use std::sync::Arc;

use chrono::Duration as ChronoDuration;
use ingot_agent_runtime::{DispatcherConfig, JobDispatcher};
use ingot_domain::activity::ActivityEventType;
use ingot_domain::convergence::ConvergenceStatus;
use ingot_domain::convergence_queue::ConvergenceQueueEntryStatus;
use ingot_domain::git_operation::{GitEntityType, GitOperationStatus, OperationKind};
use ingot_domain::item::{
    ApprovalState, DoneReason, Escalation, EscalationReason, ResolutionSource,
};
use ingot_domain::job::{
    ContextPolicy, ExecutionPermission, JobInput, JobStatus, OutcomeClass, OutputArtifactKind,
    PhaseKind,
};
use ingot_domain::workspace::{RetentionPolicy, WorkspaceKind, WorkspaceStatus};
use ingot_git::commands::{compare_and_swap_ref, delete_ref, head_oid};
use ingot_test_support::fixtures::GitOperationBuilder;
use ingot_test_support::git::unique_temp_path;
use ingot_usecases::{DispatchNotify, ProjectLocks};
use ingot_workflow::step;

mod common;
use common::*;

#[tokio::test]
async fn reconcile_startup_expires_stale_running_jobs_and_marks_workspace_stale() {
    let h = TestHarness::new(Arc::new(FakeRunner)).await;

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let seed_commit = head_oid(&h.repo_path).await.expect("seed head").into_inner();

    let item = ItemBuilder::new(h.project.id, revision_id)
        .id(item_id)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .explicit_seed(seed_commit.as_str())
        .build();
    h.db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let created_at = default_timestamp();
    let job_id = ingot_domain::ids::JobId::new();
    let workspace = WorkspaceBuilder::new(h.project.id, WorkspaceKind::Authoring)
        .path(
            unique_temp_path("ingot-runtime-stale-workspace")
                .display()
                .to_string(),
        )
        .created_for_revision_id(revision.id)
        .status(WorkspaceStatus::Busy)
        .current_job_id(job_id)
        .base_commit_oid(&seed_commit)
        .head_commit_oid(&seed_commit)
        .workspace_ref("refs/ingot/workspaces/reconcile")
        .created_at(created_at)
        .build();
    h.db.create_workspace(&workspace)
        .await
        .expect("create workspace");

    let stale_job = JobBuilder::new(h.project.id, item_id, revision_id, "author_initial")
        .id(job_id)
        .status(JobStatus::Running)
        .workspace_id(workspace.id)
        .workspace_kind(WorkspaceKind::Authoring)
        .execution_permission(ExecutionPermission::MayMutate)
        .phase_template_slug("author-initial")
        .job_input(JobInput::authoring_head(CommitOid::new(seed_commit.clone())))
        .output_artifact_kind(OutputArtifactKind::Commit)
        .lease_owner_id("old-daemon")
        .heartbeat_at(created_at)
        .lease_expires_at(created_at - ChronoDuration::minutes(1))
        .started_at(created_at)
        .build();
    h.db.create_job(&stale_job).await.expect("create stale job");

    h.dispatcher
        .reconcile_startup()
        .await
        .expect("reconcile startup");

    let updated_job = h.db.get_job(stale_job.id).await.expect("updated job");
    assert_eq!(updated_job.state.status(), JobStatus::Expired);
    assert_eq!(
        updated_job.state.outcome_class(),
        Some(OutcomeClass::TransientFailure)
    );
    assert_eq!(updated_job.state.error_code(), Some("heartbeat_expired"));

    let updated_workspace = h.db.get_workspace(workspace.id).await.expect("workspace");
    assert_eq!(updated_workspace.state.status(), WorkspaceStatus::Stale);
    assert_eq!(updated_workspace.state.current_job_id(), None);
}

#[tokio::test]
async fn reconcile_active_jobs_reports_progress_when_it_expires_a_running_job() {
    let h = TestHarness::new(Arc::new(FakeRunner)).await;

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let seed_commit = head_oid(&h.repo_path).await.expect("seed head").into_inner();

    let item = ItemBuilder::new(h.project.id, revision_id)
        .id(item_id)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .explicit_seed(seed_commit.as_str())
        .build();
    h.db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let created_at = default_timestamp();
    let job_id = ingot_domain::ids::JobId::new();
    let workspace = WorkspaceBuilder::new(h.project.id, WorkspaceKind::Authoring)
        .path(
            unique_temp_path("ingot-runtime-progress-workspace")
                .display()
                .to_string(),
        )
        .created_for_revision_id(revision.id)
        .status(WorkspaceStatus::Busy)
        .current_job_id(job_id)
        .base_commit_oid(&seed_commit)
        .head_commit_oid(&seed_commit)
        .workspace_ref("refs/ingot/workspaces/progress")
        .created_at(created_at)
        .build();
    h.db.create_workspace(&workspace)
        .await
        .expect("create workspace");

    let stale_job = JobBuilder::new(h.project.id, item_id, revision_id, "author_initial")
        .id(job_id)
        .status(JobStatus::Running)
        .workspace_id(workspace.id)
        .workspace_kind(WorkspaceKind::Authoring)
        .execution_permission(ExecutionPermission::MayMutate)
        .phase_template_slug("author-initial")
        .job_input(JobInput::authoring_head(CommitOid::new(seed_commit.clone())))
        .output_artifact_kind(OutputArtifactKind::Commit)
        .lease_owner_id("old-daemon")
        .heartbeat_at(created_at)
        .lease_expires_at(created_at - ChronoDuration::minutes(1))
        .started_at(created_at)
        .build();
    h.db.create_job(&stale_job).await.expect("create stale job");

    let made_progress = h
        .dispatcher
        .reconcile_active_jobs()
        .await
        .expect("reconcile active jobs");

    assert!(made_progress);
    let updated_job = h.db.get_job(stale_job.id).await.expect("updated job");
    assert_eq!(updated_job.state.status(), JobStatus::Expired);
}

#[tokio::test]
async fn reconcile_active_jobs_leaves_assigned_rows_for_startup_recovery() {
    let h = TestHarness::new(Arc::new(FakeRunner)).await;

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let seed_commit = head_oid(&h.repo_path).await.expect("seed head").into_inner();

    let item = ItemBuilder::new(h.project.id, revision_id)
        .id(item_id)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .explicit_seed(seed_commit.as_str())
        .build();
    h.db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let created_at = default_timestamp();
    let job_id = ingot_domain::ids::JobId::new();
    let workspace = WorkspaceBuilder::new(h.project.id, WorkspaceKind::Authoring)
        .path(
            unique_temp_path("ingot-runtime-assigned-maintenance")
                .display()
                .to_string(),
        )
        .created_for_revision_id(revision.id)
        .status(WorkspaceStatus::Busy)
        .current_job_id(job_id)
        .base_commit_oid(&seed_commit)
        .head_commit_oid(&seed_commit)
        .workspace_ref("refs/ingot/workspaces/assigned-maintenance")
        .created_at(created_at)
        .build();
    h.db.create_workspace(&workspace)
        .await
        .expect("create workspace");

    let assigned_job = JobBuilder::new(h.project.id, item_id, revision_id, "author_initial")
        .id(job_id)
        .status(JobStatus::Assigned)
        .workspace_id(workspace.id)
        .workspace_kind(WorkspaceKind::Authoring)
        .execution_permission(ExecutionPermission::MayMutate)
        .phase_template_slug("author-initial")
        .job_input(JobInput::authoring_head(CommitOid::new(seed_commit.clone())))
        .output_artifact_kind(OutputArtifactKind::Commit)
        .created_at(created_at)
        .build();
    h.db.create_job(&assigned_job)
        .await
        .expect("create assigned job");

    let made_progress = h
        .dispatcher
        .reconcile_active_jobs()
        .await
        .expect("reconcile active jobs");

    assert!(!made_progress);
    let updated_job = h.db.get_job(assigned_job.id).await.expect("updated job");
    assert_eq!(updated_job.state.status(), JobStatus::Assigned);
    assert_eq!(updated_job.state.workspace_id(), Some(workspace.id));

    let updated_workspace = h.db.get_workspace(workspace.id).await.expect("workspace");
    assert_eq!(updated_workspace.state.status(), WorkspaceStatus::Busy);
    assert_eq!(
        updated_workspace.state.current_job_id(),
        Some(assigned_job.id)
    );
}

#[tokio::test]
async fn reconcile_active_jobs_repairs_inert_assigned_authoring_dispatch_residue() {
    let h = TestHarness::new(Arc::new(FakeRunner)).await;

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let seed_commit = head_oid(&h.repo_path).await.expect("seed head").into_inner();

    let item = ItemBuilder::new(h.project.id, revision_id)
        .id(item_id)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .explicit_seed(seed_commit.as_str())
        .build();
    h.db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let created_at = default_timestamp();
    let job_id = ingot_domain::ids::JobId::new();
    let workspace = WorkspaceBuilder::new(h.project.id, WorkspaceKind::Authoring)
        .path(
            unique_temp_path("ingot-runtime-assigned-dispatch-residue")
                .display()
                .to_string(),
        )
        .created_for_revision_id(revision.id)
        .status(WorkspaceStatus::Ready)
        .base_commit_oid(&seed_commit)
        .head_commit_oid(&seed_commit)
        .workspace_ref("refs/ingot/workspaces/assigned-dispatch-residue")
        .created_at(created_at)
        .build();
    h.db.create_workspace(&workspace)
        .await
        .expect("create workspace");

    let assigned_job = JobBuilder::new(h.project.id, item_id, revision_id, "author_initial")
        .id(job_id)
        .status(JobStatus::Assigned)
        .workspace_id(workspace.id)
        .workspace_kind(WorkspaceKind::Authoring)
        .execution_permission(ExecutionPermission::MayMutate)
        .phase_kind(PhaseKind::Author)
        .phase_template_slug("author-initial")
        .job_input(JobInput::authoring_head(CommitOid::new(seed_commit.clone())))
        .output_artifact_kind(OutputArtifactKind::Commit)
        .created_at(created_at)
        .build();
    h.db.create_job(&assigned_job)
        .await
        .expect("create assigned job");

    let made_progress = h
        .dispatcher
        .reconcile_active_jobs()
        .await
        .expect("reconcile active jobs");

    assert!(made_progress);
    let updated_job = h.db.get_job(assigned_job.id).await.expect("updated job");
    assert_eq!(updated_job.state.status(), JobStatus::Queued);
    assert_eq!(updated_job.state.workspace_id(), None);

    let updated_workspace = h.db.get_workspace(workspace.id).await.expect("workspace");
    assert_eq!(updated_workspace.state.status(), WorkspaceStatus::Ready);
    assert_eq!(updated_workspace.state.current_job_id(), None);
}

#[tokio::test]
async fn reconcile_active_jobs_does_not_repair_daemon_validation_assigned_handoff() {
    let h = TestHarness::new(Arc::new(FakeRunner)).await;

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let seed_commit = head_oid(&h.repo_path).await.expect("seed head").into_inner();

    let item = ItemBuilder::new(h.project.id, revision_id)
        .id(item_id)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .explicit_seed(seed_commit.as_str())
        .build();
    h.db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let created_at = default_timestamp();
    let job_id = ingot_domain::ids::JobId::new();
    let workspace = WorkspaceBuilder::new(h.project.id, WorkspaceKind::Authoring)
        .path(
            unique_temp_path("ingot-runtime-daemon-validation-assigned")
                .display()
                .to_string(),
        )
        .created_for_revision_id(revision.id)
        .status(WorkspaceStatus::Busy)
        .current_job_id(job_id)
        .base_commit_oid(&seed_commit)
        .head_commit_oid(&seed_commit)
        .workspace_ref("refs/ingot/workspaces/daemon-validation-assigned")
        .created_at(created_at)
        .build();
    h.db.create_workspace(&workspace)
        .await
        .expect("create workspace");

    let assigned_job = JobBuilder::new(
        h.project.id,
        item_id,
        revision_id,
        step::VALIDATE_CANDIDATE_INITIAL,
    )
    .id(job_id)
    .status(JobStatus::Assigned)
    .workspace_id(workspace.id)
    .workspace_kind(WorkspaceKind::Authoring)
    .execution_permission(ExecutionPermission::DaemonOnly)
    .context_policy(ContextPolicy::None)
    .phase_kind(PhaseKind::Validate)
    .phase_template_slug("")
    .job_input(JobInput::candidate_subject(
        CommitOid::new(seed_commit.clone()),
        CommitOid::new(seed_commit.clone()),
    ))
    .output_artifact_kind(OutputArtifactKind::ValidationReport)
    .created_at(created_at)
    .build();
    h.db.create_job(&assigned_job)
        .await
        .expect("create assigned job");

    let made_progress = h
        .dispatcher
        .reconcile_active_jobs()
        .await
        .expect("reconcile active jobs");

    assert!(!made_progress);
    let updated_job = h.db.get_job(assigned_job.id).await.expect("updated job");
    assert_eq!(updated_job.state.status(), JobStatus::Assigned);
    assert_eq!(updated_job.state.workspace_id(), Some(workspace.id));

    let updated_workspace = h.db.get_workspace(workspace.id).await.expect("workspace");
    assert_eq!(updated_workspace.state.status(), WorkspaceStatus::Busy);
    assert_eq!(
        updated_workspace.state.current_job_id(),
        Some(assigned_job.id)
    );
}

#[tokio::test]
async fn reconcile_startup_fails_inflight_convergences_and_marks_workspace_stale() {
    let h = TestHarness::new(Arc::new(FakeRunner)).await;

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let seed_commit = head_oid(&h.repo_path).await.expect("seed head").into_inner();

    let item = ItemBuilder::new(h.project.id, revision_id)
        .id(item_id)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .explicit_seed(seed_commit.as_str())
        .build();
    h.db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let created_at = default_timestamp();
    let workspace = WorkspaceBuilder::new(h.project.id, WorkspaceKind::Integration)
        .path(
            unique_temp_path("ingot-runtime-conv-workspace")
                .display()
                .to_string(),
        )
        .created_for_revision_id(revision.id)
        .status(WorkspaceStatus::Provisioning)
        .base_commit_oid(&seed_commit)
        .head_commit_oid(&seed_commit)
        .workspace_ref("refs/ingot/workspaces/reconcile-conv")
        .created_at(created_at)
        .build();
    h.db.create_workspace(&workspace)
        .await
        .expect("create workspace");

    let convergence = ConvergenceBuilder::new(h.project.id, item_id, revision_id)
        .source_workspace_id(workspace.id)
        .integration_workspace_id(workspace.id)
        .source_head_commit_oid(seed_commit.clone())
        .status(ConvergenceStatus::Running)
        .input_target_commit_oid(seed_commit.clone())
        .no_prepared_commit_oid()
        .created_at(created_at)
        .build();
    h.db.create_convergence(&convergence)
        .await
        .expect("create convergence");

    h.dispatcher
        .reconcile_startup()
        .await
        .expect("reconcile startup");

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
        Some("startup_recovery_required")
    );

    let updated_workspace = h.db.get_workspace(workspace.id).await.expect("workspace");
    assert_eq!(updated_workspace.state.status(), WorkspaceStatus::Stale);
}

#[tokio::test]
async fn reconcile_startup_marks_finalized_target_ref_git_operation_reconciled() {
    let repo = temp_git_repo("ingot-runtime-repo");
    let base_commit = head_oid(&repo).await.expect("base head").into_inner();
    std::fs::write(repo.join("tracked.txt"), "next").expect("write file");
    git_sync(&repo, &["add", "tracked.txt"]);
    git_sync(&repo, &["commit", "-m", "next"]);
    let new_head = head_oid(&repo).await.expect("new head").into_inner();

    let db = migrated_test_db("ingot-runtime").await;
    let dispatcher = JobDispatcher::with_runner(
        db.clone(),
        ProjectLocks::default(),
        DispatcherConfig::new(unique_temp_path("ingot-runtime-gop-state")),
        Arc::new(FakeRunner),
        DispatchNotify::default(),
    );

    let created_at = default_timestamp();
    let project = ProjectBuilder::new(&repo).created_at(created_at).build();
    db.create_project(&project).await.expect("create project");

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let item = ItemBuilder::new(project.id, revision_id)
        .id(item_id)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .seed_commit_oid(Some(base_commit.clone()))
        .seed_target_commit_oid(Some(base_commit.clone()))
        .explicit_seed(base_commit.as_str())
        .build();
    db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let workspace = WorkspaceBuilder::new(project.id, WorkspaceKind::Integration)
        .path(repo.display().to_string())
        .created_for_revision_id(revision.id)
        .status(WorkspaceStatus::Ready)
        .base_commit_oid(&base_commit)
        .head_commit_oid(&new_head)
        .workspace_ref("refs/ingot/workspaces/finalize-adopt")
        .created_at(created_at)
        .build();
    db.create_workspace(&workspace)
        .await
        .expect("create workspace");

    let integration_workspace = WorkspaceBuilder::new(project.id, WorkspaceKind::Integration)
        .created_for_revision_id(revision.id)
        .base_commit_oid(&base_commit)
        .head_commit_oid(&new_head)
        .created_at(created_at)
        .build();
    db.create_workspace(&integration_workspace)
        .await
        .expect("create integration workspace");

    let convergence = ConvergenceBuilder::new(project.id, item_id, revision_id)
        .source_workspace_id(workspace.id)
        .integration_workspace_id(integration_workspace.id)
        .source_head_commit_oid(new_head.clone())
        .input_target_commit_oid(base_commit.clone())
        .prepared_commit_oid(new_head.clone())
        .target_head_valid(true)
        .created_at(created_at)
        .build();
    db.create_convergence(&convergence)
        .await
        .expect("create convergence");

    let operation = GitOperationBuilder::new(
        project.id,
        OperationKind::FinalizeTargetRef,
        GitEntityType::Convergence,
        convergence.id.to_string(),
    )
    .ref_name("refs/heads/main")
    .expected_old_oid(base_commit.clone())
    .new_oid(new_head.clone())
    .commit_oid(new_head)
    .status(GitOperationStatus::Applied)
    .created_at(created_at)
    .build();
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
    assert_eq!(
        updated_convergence.state.status(),
        ConvergenceStatus::Finalized
    );
    assert_eq!(
        updated_convergence.state.final_target_commit_oid(),
        Some(operation.commit_oid().expect("operation commit oid"))
    );

    let updated_item = db.get_item(item.id).await.expect("item");
    assert!(updated_item.lifecycle.is_done());
    assert_eq!(
        updated_item.lifecycle.done_reason(),
        Some(DoneReason::Completed)
    );
    assert_eq!(updated_item.approval_state, ApprovalState::Approved);
    assert_eq!(
        updated_item.lifecycle.resolution_source(),
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
    let repo = temp_git_repo("ingot-runtime-repo");
    let base_commit = head_oid(&repo).await.expect("base head").into_inner();
    std::fs::write(repo.join("tracked.txt"), "prepared").expect("write prepared");
    git_sync(&repo, &["add", "tracked.txt"]);
    git_sync(&repo, &["commit", "-m", "prepared"]);
    let prepared_commit = head_oid(&repo).await.expect("prepared head").into_inner();
    git_sync(
        &repo,
        &[
            "update-ref",
            "refs/ingot/tests/finalize-prepared",
            &prepared_commit,
        ],
    );
    git_sync(&repo, &["reset", "--hard", &base_commit]);

    let db = migrated_test_db("ingot-runtime").await;
    let dispatcher = JobDispatcher::with_runner(
        db.clone(),
        ProjectLocks::default(),
        DispatcherConfig::new(unique_temp_path("ingot-runtime-finalize-sync-state")),
        Arc::new(FakeRunner),
        DispatchNotify::default(),
    );

    let created_at = default_timestamp();
    let project = ProjectBuilder::new(&repo).created_at(created_at).build();
    db.create_project(&project).await.expect("create project");

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let item = ItemBuilder::new(project.id, revision_id)
        .id(item_id)
        .approval_state(ApprovalState::NotRequired)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .approval_policy(ingot_domain::revision::ApprovalPolicy::NotRequired)
        .seed_commit_oid(Some(base_commit.clone()))
        .seed_target_commit_oid(Some(base_commit.clone()))
        .explicit_seed(base_commit.as_str())
        .build();
    db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");
    let source_workspace = WorkspaceBuilder::new(project.id, WorkspaceKind::Authoring)
        .path(
            unique_temp_path("ingot-runtime-finalize-source")
                .display()
                .to_string(),
        )
        .created_for_revision_id(revision.id)
        .status(WorkspaceStatus::Ready)
        .base_commit_oid(&base_commit)
        .head_commit_oid(&prepared_commit)
        .workspace_ref("refs/ingot/workspaces/finalize-source")
        .created_at(created_at)
        .build();
    db.create_workspace(&source_workspace)
        .await
        .expect("create source workspace");

    let integration_workspace = WorkspaceBuilder::new(project.id, WorkspaceKind::Integration)
        .created_for_revision_id(revision.id)
        .base_commit_oid(&base_commit)
        .head_commit_oid(&prepared_commit)
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
        &ConvergenceQueueEntryBuilder::new(project.id, item_id, revision_id)
            .created_at(created_at)
            .build(),
    )
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

    let operation = GitOperationBuilder::new(
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
    .build();
    db.create_git_operation(&operation)
        .await
        .expect("create git operation");

    assert_eq!(head_oid(&repo).await.expect("checkout head").into_inner(), base_commit);

    dispatcher
        .reconcile_startup()
        .await
        .expect("reconcile startup");

    assert_eq!(head_oid(&repo).await.expect("synced head").into_inner(), prepared_commit);
    let unresolved = db
        .list_unresolved_git_operations()
        .await
        .expect("list unresolved");
    assert!(unresolved.is_empty(), "finalize op should reconcile");
    let updated_item = db.get_item(item.id).await.expect("item");
    assert!(updated_item.lifecycle.is_done());
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
    let repo = temp_git_repo("ingot-runtime-repo");
    let base_commit = head_oid(&repo).await.expect("base head").into_inner();
    std::fs::write(repo.join("tracked.txt"), "prepared").expect("write prepared");
    git_sync(&repo, &["add", "tracked.txt"]);
    git_sync(&repo, &["commit", "-m", "prepared"]);
    let prepared_commit = head_oid(&repo).await.expect("prepared head").into_inner();
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

    let db = migrated_test_db("ingot-runtime").await;
    let dispatcher = JobDispatcher::with_runner(
        db.clone(),
        ProjectLocks::default(),
        DispatcherConfig::new(unique_temp_path("ingot-runtime-finalize-blocked-state")),
        Arc::new(FakeRunner),
        DispatchNotify::default(),
    );

    let created_at = default_timestamp();
    let project = ProjectBuilder::new(&repo).created_at(created_at).build();
    db.create_project(&project).await.expect("create project");

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let item = ItemBuilder::new(project.id, revision_id)
        .id(item_id)
        .approval_state(ApprovalState::NotRequired)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .approval_policy(ingot_domain::revision::ApprovalPolicy::NotRequired)
        .seed_commit_oid(Some(base_commit.clone()))
        .seed_target_commit_oid(Some(base_commit.clone()))
        .explicit_seed(base_commit.as_str())
        .build();
    db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");
    let source_workspace = WorkspaceBuilder::new(project.id, WorkspaceKind::Authoring)
        .path(
            unique_temp_path("ingot-runtime-finalize-blocked-source")
                .display()
                .to_string(),
        )
        .created_for_revision_id(revision.id)
        .status(WorkspaceStatus::Ready)
        .base_commit_oid(&base_commit)
        .head_commit_oid(&prepared_commit)
        .workspace_ref("refs/ingot/workspaces/finalize-blocked-source")
        .created_at(created_at)
        .build();
    db.create_workspace(&source_workspace)
        .await
        .expect("create source workspace");

    let integration_workspace = WorkspaceBuilder::new(project.id, WorkspaceKind::Integration)
        .created_for_revision_id(revision.id)
        .base_commit_oid(&base_commit)
        .head_commit_oid(&prepared_commit)
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
        &ConvergenceQueueEntryBuilder::new(project.id, item_id, revision_id)
            .created_at(created_at)
            .build(),
    )
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

    let operation = GitOperationBuilder::new(
        project.id,
        OperationKind::FinalizeTargetRef,
        GitEntityType::Convergence,
        convergence.id.to_string(),
    )
    .ref_name("refs/heads/main")
    .expected_old_oid(base_commit)
    .new_oid(prepared_commit)
    .commit_oid(
        convergence
            .state
            .prepared_commit_oid()
            .expect("prepared oid")
            .clone(),
    )
    .status(GitOperationStatus::Applied)
    .created_at(created_at)
    .completed_at(created_at)
    .build();
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
    assert!(updated_item.lifecycle.is_open());
    assert!(matches!(
        updated_item.escalation,
        Escalation::OperatorRequired {
            reason: EscalationReason::CheckoutSyncBlocked
        }
    ));
    let updated_convergence = db
        .get_convergence(convergence.id)
        .await
        .expect("convergence");
    assert_eq!(
        updated_convergence.state.status(),
        ConvergenceStatus::Prepared
    );
    let queue_entries = db
        .list_queue_entries_by_item(item.id)
        .await
        .expect("list queue entries");
    assert_eq!(queue_entries[0].status, ConvergenceQueueEntryStatus::Head);
}

#[tokio::test]
async fn reconcile_startup_adopts_prepared_convergence_from_git_operation() {
    let repo = temp_git_repo("ingot-runtime-repo");
    let base_commit = head_oid(&repo).await.expect("base head").into_inner();
    std::fs::write(repo.join("tracked.txt"), "prepared").expect("write file");
    git_sync(&repo, &["add", "tracked.txt"]);
    git_sync(&repo, &["commit", "-m", "prepared"]);
    let prepared_head = head_oid(&repo).await.expect("prepared head").into_inner();
    git_sync(
        &repo,
        &[
            "update-ref",
            "refs/ingot/workspaces/prepare-adopt",
            &prepared_head,
        ],
    );
    git_sync(&repo, &["reset", "--hard", &base_commit]);

    let db = migrated_test_db("ingot-runtime").await;
    let dispatcher = JobDispatcher::with_runner(
        db.clone(),
        ProjectLocks::default(),
        DispatcherConfig::new(unique_temp_path("ingot-runtime-prepare-adopt-state")),
        Arc::new(FakeRunner),
        DispatchNotify::default(),
    );

    let created_at = default_timestamp();
    let project = ProjectBuilder::new(&repo).created_at(created_at).build();
    db.create_project(&project).await.expect("create project");

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let item = ItemBuilder::new(project.id, revision_id)
        .id(item_id)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .seed_commit_oid(Some(base_commit.clone()))
        .seed_target_commit_oid(Some(base_commit.clone()))
        .explicit_seed(base_commit.as_str())
        .build();
    db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let workspace = WorkspaceBuilder::new(project.id, WorkspaceKind::Integration)
        .path(repo.display().to_string())
        .created_for_revision_id(revision.id)
        .status(WorkspaceStatus::Provisioning)
        .base_commit_oid(&base_commit)
        .head_commit_oid(&base_commit)
        .workspace_ref("refs/ingot/workspaces/prepare-adopt")
        .created_at(created_at)
        .build();
    db.create_workspace(&workspace)
        .await
        .expect("create workspace");

    let convergence = ConvergenceBuilder::new(project.id, item_id, revision_id)
        .source_workspace_id(workspace.id)
        .integration_workspace_id(workspace.id)
        .source_head_commit_oid(prepared_head.clone())
        .status(ConvergenceStatus::Running)
        .input_target_commit_oid(base_commit.clone())
        .no_prepared_commit_oid()
        .created_at(created_at)
        .build();
    db.create_convergence(&convergence)
        .await
        .expect("create convergence");

    let operation = GitOperationBuilder::new(
        project.id,
        OperationKind::PrepareConvergenceCommit,
        GitEntityType::Convergence,
        convergence.id.to_string(),
    )
    .workspace_id(workspace.id)
    .ref_name(workspace.workspace_ref.clone().expect("workspace ref"))
    .expected_old_oid(
        workspace
            .state
            .base_commit_oid()
            .expect("workspace base commit")
            .to_owned(),
    )
    .new_oid(prepared_head.clone())
    .commit_oid(prepared_head.clone())
    .status(GitOperationStatus::Applied)
    .metadata(serde_json::json!({
        "source_commit_oids": [prepared_head],
        "prepared_commit_oids": [prepared_head]
    }))
    .created_at(created_at)
    .build();
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
    assert_eq!(
        updated_convergence.state.status(),
        ConvergenceStatus::Prepared
    );
    assert!(updated_convergence.state.prepared_commit_oid().is_some());
    let updated_workspace = db.get_workspace(workspace.id).await.expect("workspace");
    assert_eq!(updated_workspace.state.status(), WorkspaceStatus::Ready);
    assert_eq!(
        updated_workspace.state.head_commit_oid(),
        updated_convergence.state.prepared_commit_oid()
    );
    let unresolved = db
        .list_unresolved_git_operations()
        .await
        .expect("list unresolved");
    assert!(unresolved.is_empty(), "prepare op should reconcile");
}

#[tokio::test]
async fn reconcile_startup_does_not_resurrect_cancelled_convergence_from_prepare_git_operation() {
    let repo = temp_git_repo("ingot-runtime-repo");
    let base_commit = head_oid(&repo).await.expect("base head").into_inner();
    std::fs::write(repo.join("tracked.txt"), "prepared").expect("write file");
    git_sync(&repo, &["add", "tracked.txt"]);
    git_sync(&repo, &["commit", "-m", "prepared"]);
    let prepared_head = head_oid(&repo).await.expect("prepared head").into_inner();
    git_sync(
        &repo,
        &[
            "update-ref",
            "refs/ingot/workspaces/prepare-cancelled",
            &prepared_head,
        ],
    );
    git_sync(&repo, &["reset", "--hard", &base_commit]);

    let db = migrated_test_db("ingot-runtime").await;
    let dispatcher = JobDispatcher::with_runner(
        db.clone(),
        ProjectLocks::default(),
        DispatcherConfig::new(unique_temp_path("ingot-runtime-prepare-cancelled-state")),
        Arc::new(FakeRunner),
        DispatchNotify::default(),
    );

    let created_at = default_timestamp();
    let project = ProjectBuilder::new(&repo).created_at(created_at).build();
    db.create_project(&project).await.expect("create project");

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let item = ItemBuilder::new(project.id, revision_id)
        .id(item_id)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .seed_commit_oid(Some(base_commit.clone()))
        .seed_target_commit_oid(Some(base_commit.clone()))
        .explicit_seed(base_commit.as_str())
        .build();
    db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let workspace = WorkspaceBuilder::new(project.id, WorkspaceKind::Integration)
        .path(repo.display().to_string())
        .created_for_revision_id(revision.id)
        .status(WorkspaceStatus::Abandoned)
        .base_commit_oid(&base_commit)
        .head_commit_oid(&prepared_head)
        .workspace_ref("refs/ingot/workspaces/prepare-cancelled")
        .created_at(created_at)
        .build();
    db.create_workspace(&workspace)
        .await
        .expect("create workspace");

    let convergence = ConvergenceBuilder::new(project.id, item_id, revision_id)
        .source_workspace_id(workspace.id)
        .integration_workspace_id(workspace.id)
        .source_head_commit_oid(prepared_head.clone())
        .status(ConvergenceStatus::Cancelled)
        .input_target_commit_oid(base_commit.clone())
        .prepared_commit_oid(prepared_head.clone())
        .completed_at(created_at)
        .created_at(created_at)
        .build();
    db.create_convergence(&convergence)
        .await
        .expect("create convergence");

    let operation = GitOperationBuilder::new(
        project.id,
        OperationKind::PrepareConvergenceCommit,
        GitEntityType::Convergence,
        convergence.id.to_string(),
    )
    .workspace_id(workspace.id)
    .ref_name(workspace.workspace_ref.clone().expect("workspace ref"))
    .expected_old_oid(base_commit)
    .new_oid(prepared_head.clone())
    .commit_oid(prepared_head)
    .status(GitOperationStatus::Applied)
    .metadata(serde_json::json!({
        "source_commit_oids": [],
        "prepared_commit_oids": []
    }))
    .created_at(created_at)
    .build();
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
    assert_eq!(
        updated_convergence.state.status(),
        ConvergenceStatus::Cancelled
    );
    let updated_workspace = db.get_workspace(workspace.id).await.expect("workspace");
    assert_eq!(updated_workspace.state.status(), WorkspaceStatus::Abandoned);
    let unresolved = db
        .list_unresolved_git_operations()
        .await
        .expect("list unresolved");
    assert!(unresolved.is_empty(), "cancelled prepare op should resolve");
}

#[tokio::test]
async fn reconcile_startup_adopts_create_job_commit_into_completed_job() {
    let repo = temp_git_repo("ingot-runtime-repo");
    let base_commit = head_oid(&repo).await.expect("base head").into_inner();
    std::fs::write(repo.join("tracked.txt"), "authored").expect("write file");
    git_sync(&repo, &["add", "tracked.txt"]);
    git_sync(&repo, &["commit", "-m", "authored"]);
    let authored_commit = head_oid(&repo).await.expect("authored head").into_inner();
    git_sync(
        &repo,
        &[
            "update-ref",
            "refs/ingot/workspaces/adopt-job",
            &authored_commit,
        ],
    );
    git_sync(&repo, &["reset", "--hard", &base_commit]);

    let db = migrated_test_db("ingot-runtime").await;
    let dispatcher = JobDispatcher::with_runner(
        db.clone(),
        ProjectLocks::default(),
        DispatcherConfig::new(unique_temp_path("ingot-runtime-adopt-job-state")),
        Arc::new(FakeRunner),
        DispatchNotify::default(),
    );

    let created_at = default_timestamp();
    let project = ProjectBuilder::new(&repo).created_at(created_at).build();
    db.create_project(&project).await.expect("create project");

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let item = ItemBuilder::new(project.id, revision_id)
        .id(item_id)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .seed_commit_oid(Some(base_commit.clone()))
        .seed_target_commit_oid(Some(base_commit.clone()))
        .explicit_seed(base_commit.as_str())
        .build();
    db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let job_id = ingot_domain::ids::JobId::new();
    let workspace = WorkspaceBuilder::new(project.id, WorkspaceKind::Authoring)
        .path(repo.display().to_string())
        .created_for_revision_id(revision.id)
        .status(WorkspaceStatus::Busy)
        .current_job_id(job_id)
        .base_commit_oid(&base_commit)
        .head_commit_oid(&base_commit)
        .workspace_ref("refs/ingot/workspaces/adopt-job")
        .created_at(created_at)
        .build();
    db.create_workspace(&workspace)
        .await
        .expect("create workspace");

    let job = JobBuilder::new(project.id, item_id, revision_id, "author_initial")
        .id(job_id)
        .status(JobStatus::Running)
        .workspace_id(workspace.id)
        .workspace_kind(WorkspaceKind::Authoring)
        .execution_permission(ExecutionPermission::MayMutate)
        .phase_template_slug("author-initial")
        .job_input(JobInput::authoring_head(CommitOid::new(base_commit.clone())))
        .output_artifact_kind(OutputArtifactKind::Commit)
        .lease_owner_id("old-daemon")
        .heartbeat_at(created_at)
        .lease_expires_at(created_at + ChronoDuration::minutes(5))
        .started_at(created_at)
        .build();
    db.create_job(&job).await.expect("create job");

    let operation = GitOperationBuilder::new(
        project.id,
        OperationKind::CreateJobCommit,
        GitEntityType::Job,
        job.id.to_string(),
    )
    .workspace_id(workspace.id)
    .ref_name(workspace.workspace_ref.clone().expect("workspace ref"))
    .expected_old_oid(base_commit.clone())
    .new_oid(authored_commit.clone())
    .commit_oid(authored_commit.clone())
    .status(GitOperationStatus::Applied)
    .created_at(created_at)
    .build();
    db.create_git_operation(&operation)
        .await
        .expect("create operation");

    dispatcher
        .reconcile_startup()
        .await
        .expect("reconcile startup");

    let updated_job = db.get_job(job.id).await.expect("updated job");
    assert_eq!(updated_job.state.status(), JobStatus::Completed);
    assert_eq!(updated_job.state.outcome_class(), Some(OutcomeClass::Clean));
    assert_eq!(
        updated_job.state.output_commit_oid().map(|c| c.as_str()),
        Some(authored_commit.as_str())
    );

    let updated_workspace = db.get_workspace(workspace.id).await.expect("workspace");
    assert_eq!(updated_workspace.state.status(), WorkspaceStatus::Ready);
    assert_eq!(
        updated_workspace.state.head_commit_oid().map(|c| c.as_str()),
        Some(authored_commit.as_str())
    );

    let jobs = db.list_jobs_by_item(item.id).await.expect("jobs");
    let review_job = jobs
        .iter()
        .find(|job| job.step_id == step::REVIEW_INCREMENTAL_INITIAL)
        .expect("auto-dispatched review job after startup adoption");
    assert_eq!(review_job.state.status(), JobStatus::Queued);
    assert_eq!(
        review_job.job_input.base_commit_oid().map(|c| c.as_str()),
        Some(base_commit.as_str())
    );
    assert_eq!(
        review_job.job_input.head_commit_oid().map(|c| c.as_str()),
        Some(authored_commit.as_str())
    );
}

#[tokio::test]
async fn reconcile_startup_continues_review_recovery_past_broken_project() {
    let healthy_repo = temp_git_repo("ingot-runtime-repo");
    let healthy_seed_commit = head_oid(&healthy_repo).await.expect("healthy seed head").into_inner();
    std::fs::write(healthy_repo.join("feature.txt"), "authored").expect("write file");
    git_sync(&healthy_repo, &["add", "feature.txt"]);
    git_sync(&healthy_repo, &["commit", "-m", "authored"]);
    let healthy_authored_commit = head_oid(&healthy_repo)
        .await
        .expect("healthy authored head");

    let db = migrated_test_db("ingot-runtime").await;
    let dispatcher = JobDispatcher::with_runner(
        db.clone(),
        ProjectLocks::default(),
        DispatcherConfig::new(unique_temp_path(
            "ingot-runtime-startup-review-recovery-state",
        )),
        Arc::new(FakeRunner),
        DispatchNotify::default(),
    );

    let created_at = default_timestamp();

    // Broken project
    let broken_project = ProjectBuilder::new(unique_temp_path("ingot-missing-project"))
        .name("broken")
        .created_at(created_at)
        .build();
    db.create_project(&broken_project)
        .await
        .expect("create broken project");

    let broken_item_id = ingot_domain::ids::ItemId::new();
    let broken_revision_id = ingot_domain::ids::ItemRevisionId::new();
    let broken_item = ItemBuilder::new(broken_project.id, broken_revision_id)
        .id(broken_item_id)
        .build();
    let broken_revision = RevisionBuilder::new(broken_item_id)
        .id(broken_revision_id)
        .seed_commit_oid(Some("missing-seed"))
        .seed_target_commit_oid(Some("missing-seed"))
        .explicit_seed("missing-seed")
        .build();
    db.create_item_with_revision(&broken_item, &broken_revision)
        .await
        .expect("create broken item");
    let broken_workspace = WorkspaceBuilder::new(broken_project.id, WorkspaceKind::Authoring)
        .path(broken_project.path.clone())
        .created_for_revision_id(broken_revision_id)
        .status(WorkspaceStatus::Ready)
        .base_commit_oid("missing-seed")
        .head_commit_oid("missing-prepared")
        .no_target_ref()
        .no_workspace_ref()
        .created_at(created_at)
        .build();
    db.create_workspace(&broken_workspace)
        .await
        .expect("create broken workspace");
    db.create_convergence(
        &ConvergenceBuilder::new(broken_project.id, broken_item_id, broken_revision_id)
            .source_workspace_id(broken_workspace.id)
            .integration_workspace_id(broken_workspace.id)
            .source_head_commit_oid("missing-prepared")
            .input_target_commit_oid("missing-seed")
            .prepared_commit_oid("missing-prepared")
            .created_at(created_at)
            .build(),
    )
    .await
    .expect("create broken convergence");

    // Healthy project
    let healthy_project = ProjectBuilder::new(&healthy_repo)
        .name("healthy")
        .created_at(created_at)
        .build();
    db.create_project(&healthy_project)
        .await
        .expect("create healthy project");

    let healthy_item_id = ingot_domain::ids::ItemId::new();
    let healthy_revision_id = ingot_domain::ids::ItemRevisionId::new();
    let healthy_item = ItemBuilder::new(healthy_project.id, healthy_revision_id)
        .id(healthy_item_id)
        .build();
    let healthy_revision = RevisionBuilder::new(healthy_item_id)
        .id(healthy_revision_id)
        .explicit_seed(healthy_seed_commit.as_str())
        .build();
    db.create_item_with_revision(&healthy_item, &healthy_revision)
        .await
        .expect("create healthy item");
    db.create_job(
        &JobBuilder::new(
            healthy_project.id,
            healthy_item_id,
            healthy_revision_id,
            "author_initial",
        )
        .status(JobStatus::Completed)
        .outcome_class(OutcomeClass::Clean)
        .workspace_kind(WorkspaceKind::Authoring)
        .execution_permission(ExecutionPermission::MayMutate)
        .phase_template_slug("author-initial")
        .job_input(JobInput::authoring_head(CommitOid::new(healthy_seed_commit.clone())))
        .output_artifact_kind(OutputArtifactKind::Commit)
        .output_commit_oid(healthy_authored_commit.clone())
        .started_at(created_at)
        .ended_at(created_at)
        .build(),
    )
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
    assert_eq!(review_job.state.status(), JobStatus::Queued);
    assert_eq!(
        review_job.job_input.base_commit_oid().map(|c| c.as_str()),
        Some(healthy_seed_commit.as_str())
    );
    assert_eq!(
        review_job.job_input.head_commit_oid().map(|c| c.as_str()),
        Some(healthy_authored_commit.as_str())
    );
}

#[tokio::test]
async fn reconcile_startup_adopts_reset_workspace_operation() {
    let h = TestHarness::new(Arc::new(FakeRunner)).await;
    let head = head_oid(&h.repo_path).await.expect("head").into_inner();
    let created_at = default_timestamp();

    let workspace = WorkspaceBuilder::new(h.project.id, WorkspaceKind::Authoring)
        .path(h.repo_path.display().to_string())
        .status(WorkspaceStatus::Busy)
        .base_commit_oid(&head)
        .head_commit_oid(&head)
        .workspace_ref("refs/ingot/workspaces/reset-adopt")
        .current_job_id(ingot_domain::ids::JobId::new())
        .created_at(created_at)
        .build();
    h.db.create_workspace(&workspace)
        .await
        .expect("create workspace");
    let operation = GitOperationBuilder::new(
        h.project.id,
        OperationKind::ResetWorkspace,
        GitEntityType::Workspace,
        workspace.id.to_string(),
    )
    .workspace_id(workspace.id)
    .ref_name(workspace.workspace_ref.clone().expect("workspace ref"))
    .expected_old_oid(
        workspace
            .state
            .head_commit_oid()
            .expect("workspace head commit")
            .to_owned(),
    )
    .new_oid(head)
    .status(GitOperationStatus::Applied)
    .created_at(created_at)
    .build();
    h.db.create_git_operation(&operation)
        .await
        .expect("create operation");

    h.dispatcher
        .reconcile_startup()
        .await
        .expect("reconcile startup");

    let updated_workspace = h.db.get_workspace(workspace.id).await.expect("workspace");
    assert_eq!(updated_workspace.state.status(), WorkspaceStatus::Ready);
    assert_eq!(updated_workspace.state.current_job_id(), None);
}

#[tokio::test]
async fn reconcile_startup_adopts_remove_workspace_ref_operation() {
    let repo = temp_git_repo("ingot-runtime-repo");
    let head = head_oid(&repo).await.expect("head").into_inner();
    let workspace_path = unique_temp_path("ingot-runtime-remove-adopt");

    let db = migrated_test_db("ingot-runtime").await;
    let state_root = unique_temp_path("ingot-runtime-remove-adopt-state");
    let dispatcher = JobDispatcher::with_runner(
        db.clone(),
        ProjectLocks::default(),
        DispatcherConfig::new(state_root.clone()),
        Arc::new(FakeRunner),
        DispatchNotify::default(),
    );
    let created_at = default_timestamp();
    let project = ProjectBuilder::new(&repo).created_at(created_at).build();
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
    let workspace = WorkspaceBuilder::new(project.id, WorkspaceKind::Review)
        .path(workspace_path.display().to_string())
        .status(WorkspaceStatus::Removing)
        .base_commit_oid(&head)
        .head_commit_oid(&head)
        .no_target_ref()
        .workspace_ref("refs/ingot/workspaces/remove-adopt")
        .retention_policy(RetentionPolicy::Ephemeral)
        .current_job_id(ingot_domain::ids::JobId::new())
        .created_at(created_at)
        .build();
    db.create_workspace(&workspace)
        .await
        .expect("create workspace");
    let operation = GitOperationBuilder::new(
        project.id,
        OperationKind::RemoveWorkspaceRef,
        GitEntityType::Workspace,
        workspace.id.to_string(),
    )
    .workspace_id(workspace.id)
    .ref_name(workspace.workspace_ref.clone().expect("workspace ref"))
    .expected_old_oid(
        workspace
            .state
            .head_commit_oid()
            .expect("workspace head commit")
            .to_owned(),
    )
    .status(GitOperationStatus::Applied)
    .created_at(created_at)
    .build();
    db.create_git_operation(&operation)
        .await
        .expect("create operation");

    dispatcher
        .reconcile_startup()
        .await
        .expect("reconcile startup");

    let updated_workspace = db.get_workspace(workspace.id).await.expect("workspace");
    assert_eq!(updated_workspace.state.status(), WorkspaceStatus::Abandoned);
    assert_eq!(updated_workspace.state.current_job_id(), None);
    assert_eq!(updated_workspace.workspace_ref, None);
}

#[tokio::test]
async fn reconcile_startup_removes_abandoned_review_workspace_when_safe() {
    let repo = temp_git_repo("ingot-runtime-repo");
    let workspace_path = unique_temp_path("ingot-runtime-review-cleanup");

    let db = migrated_test_db("ingot-runtime").await;
    let state_root = unique_temp_path("ingot-runtime-cleanup-state");
    let dispatcher = JobDispatcher::with_runner(
        db.clone(),
        ProjectLocks::default(),
        DispatcherConfig::new(state_root.clone()),
        Arc::new(FakeRunner),
        DispatchNotify::default(),
    );

    let created_at = default_timestamp();
    let project = ProjectBuilder::new(&repo).created_at(created_at).build();
    let seed_commit = head_oid(&repo).await.expect("seed head").into_inner();
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
    let item = ItemBuilder::new(project.id, revision_id)
        .id(item_id)
        .done(DoneReason::Completed, ResolutionSource::ManualCommand)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .explicit_seed(seed_commit.as_str())
        .build();
    db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let workspace = WorkspaceBuilder::new(project.id, WorkspaceKind::Review)
        .path(workspace_path.display().to_string())
        .created_for_revision_id(revision.id)
        .status(WorkspaceStatus::Abandoned)
        .base_commit_oid(&seed_commit)
        .head_commit_oid(&seed_commit)
        .no_target_ref()
        .no_workspace_ref()
        .retention_policy(RetentionPolicy::Ephemeral)
        .created_at(created_at)
        .build();
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
    let repo = temp_git_repo("ingot-runtime-repo");
    let workspace_path = unique_temp_path("ingot-runtime-author-cleanup");

    let db = migrated_test_db("ingot-runtime").await;
    let state_root = unique_temp_path("ingot-runtime-author-cleanup-state");
    let dispatcher = JobDispatcher::with_runner(
        db.clone(),
        ProjectLocks::default(),
        DispatcherConfig::new(state_root.clone()),
        Arc::new(FakeRunner),
        DispatchNotify::default(),
    );

    let created_at = default_timestamp();
    let project = ProjectBuilder::new(&repo).created_at(created_at).build();
    let seed_commit = head_oid(&repo).await.expect("seed head").into_inner();
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
    let item = ItemBuilder::new(project.id, revision_id)
        .id(item_id)
        .done(DoneReason::Completed, ResolutionSource::ManualCommand)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .explicit_seed(seed_commit.as_str())
        .build();
    db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let workspace = WorkspaceBuilder::new(project.id, WorkspaceKind::Authoring)
        .path(workspace_path.display().to_string())
        .created_for_revision_id(revision.id)
        .status(WorkspaceStatus::Abandoned)
        .base_commit_oid(&seed_commit)
        .head_commit_oid(&seed_commit)
        .workspace_ref("refs/ingot/workspaces/author-cleanup")
        .created_at(created_at)
        .build();
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
    let repo = temp_git_repo("ingot-runtime-repo");
    let seed_commit = head_oid(&repo).await.expect("seed head").into_inner();
    let workspace_path = unique_temp_path("ingot-runtime-author-retain");
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

    let db = migrated_test_db("ingot-runtime").await;
    let dispatcher = JobDispatcher::with_runner(
        db.clone(),
        ProjectLocks::default(),
        DispatcherConfig::new(unique_temp_path("ingot-runtime-retain-state")),
        Arc::new(FakeRunner),
        DispatchNotify::default(),
    );

    let created_at = default_timestamp();
    let project = ProjectBuilder::new(&repo).created_at(created_at).build();
    db.create_project(&project).await.expect("create project");

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let item = ItemBuilder::new(project.id, revision_id)
        .id(item_id)
        .done(DoneReason::Dismissed, ResolutionSource::ManualCommand)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .explicit_seed(seed_commit.as_str())
        .build();
    db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let workspace = WorkspaceBuilder::new(project.id, WorkspaceKind::Authoring)
        .path(workspace_path.display().to_string())
        .created_for_revision_id(revision.id)
        .status(WorkspaceStatus::Abandoned)
        .base_commit_oid(&seed_commit)
        .head_commit_oid(&seed_commit)
        .workspace_ref("refs/ingot/workspaces/retain")
        .retention_policy(RetentionPolicy::Ephemeral)
        .created_at(created_at)
        .build();
    db.create_workspace(&workspace)
        .await
        .expect("create workspace");

    let source_job = JobBuilder::new(project.id, item_id, revision_id, "review_candidate_initial")
        .status(JobStatus::Completed)
        .outcome_class(OutcomeClass::Findings)
        .phase_kind(PhaseKind::Review)
        .workspace_id(workspace.id)
        .workspace_kind(WorkspaceKind::Review)
        .execution_permission(ExecutionPermission::MustNotMutate)
        .phase_template_slug("review-candidate")
        .job_input(JobInput::candidate_subject(
        CommitOid::new(seed_commit.clone()),
        CommitOid::new(seed_commit.clone()),
    ))
        .output_artifact_kind(OutputArtifactKind::ReviewReport)
        .result_schema_version("review_report:v1")
        .result_payload(serde_json::json!({
            "outcome": "findings",
            "summary": "finding",
            "review_subject": {
                "base_commit_oid": seed_commit.clone(),
                "head_commit_oid": seed_commit.clone()
            },
            "overall_risk": "medium",
            "findings": []
        }))
        .created_at(created_at)
        .started_at(created_at)
        .ended_at(created_at)
        .build();
    db.create_job(&source_job).await.expect("create source job");

    let finding = FindingBuilder::new(project.id, item.id, revision.id, source_job.id)
        .source_finding_key("fnd")
        .source_subject_base_commit_oid(Some(seed_commit.clone()))
        .source_subject_head_commit_oid(seed_commit.clone())
        .code("CODE")
        .severity(ingot_domain::finding::FindingSeverity::Medium)
        .summary("retain me")
        .paths(vec!["tracked.txt".into()])
        .created_at(created_at)
        .build();
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
    let repo = temp_git_repo("ingot-runtime-repo");
    let seed_commit = head_oid(&repo).await.expect("seed head").into_inner();
    let workspace_path = unique_temp_path("ingot-runtime-integration-retain");
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

    let db = migrated_test_db("ingot-runtime").await;
    let dispatcher = JobDispatcher::with_runner(
        db.clone(),
        ProjectLocks::default(),
        DispatcherConfig::new(unique_temp_path("ingot-runtime-integration-retain-state")),
        Arc::new(FakeRunner),
        DispatchNotify::default(),
    );

    let created_at = default_timestamp();
    let project = ProjectBuilder::new(&repo).created_at(created_at).build();
    db.create_project(&project).await.expect("create project");

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let item = ItemBuilder::new(project.id, revision_id)
        .id(item_id)
        .done(DoneReason::Completed, ResolutionSource::ManualCommand)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .explicit_seed(seed_commit.as_str())
        .build();
    db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let source_job = JobBuilder::new(project.id, item_id, revision_id, "validate_integrated")
        .status(JobStatus::Completed)
        .outcome_class(OutcomeClass::Findings)
        .phase_kind(PhaseKind::Validate)
        .workspace_kind(WorkspaceKind::Integration)
        .execution_permission(ExecutionPermission::MustNotMutate)
        .context_policy(ContextPolicy::ResumeContext)
        .phase_template_slug("validate-integrated")
        .job_input(JobInput::integrated_subject(
        CommitOid::new(seed_commit.clone()),
        CommitOid::new(seed_commit.clone()),
    ))
        .output_artifact_kind(OutputArtifactKind::ValidationReport)
        .result_schema_version("validation_report:v1")
        .result_payload(serde_json::json!({
            "outcome": "findings",
            "summary": "finding",
            "checks": [],
            "findings": []
        }))
        .created_at(created_at)
        .started_at(created_at)
        .ended_at(created_at)
        .build();
    db.create_job(&source_job).await.expect("create source job");

    let workspace = WorkspaceBuilder::new(project.id, WorkspaceKind::Integration)
        .path(workspace_path.display().to_string())
        .created_for_revision_id(revision.id)
        .status(WorkspaceStatus::Abandoned)
        .base_commit_oid(&seed_commit)
        .head_commit_oid(&seed_commit)
        .workspace_ref("refs/ingot/workspaces/integration-retain")
        .created_at(created_at)
        .build();
    db.create_workspace(&workspace)
        .await
        .expect("create workspace");

    let finding = FindingBuilder::new(project.id, item.id, revision.id, source_job.id)
        .source_step_id("validate_integrated")
        .source_report_schema_version("validation_report:v1")
        .source_finding_key("fnd")
        .source_subject_kind(ingot_domain::finding::FindingSubjectKind::Integrated)
        .source_subject_base_commit_oid(Some(seed_commit.clone()))
        .source_subject_head_commit_oid(seed_commit.clone())
        .code("CODE")
        .summary("retain integration")
        .paths(vec!["tracked.txt".into()])
        .created_at(created_at)
        .build();
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

    let seed_commit = head_oid(&h.repo_path).await.expect("seed head").into_inner();
    let created_at = default_timestamp();

    let item_a_id = ingot_domain::ids::ItemId::new();
    let rev_a_id = ingot_domain::ids::ItemRevisionId::new();
    let rev_a = RevisionBuilder::new(item_a_id)
        .id(rev_a_id)
        .revision_no(1)
        .explicit_seed(seed_commit.as_str())
        .created_at(created_at)
        .build();
    let item_a = ItemBuilder::new(h.project.id, rev_a.id)
        .id(rev_a.item_id)
        .created_at(created_at)
        .build();
    h.db.create_item_with_revision(&item_a, &rev_a)
        .await
        .expect("create item a");
    let assigned_job_id = ingot_domain::ids::JobId::new();
    let workspace_a = WorkspaceBuilder::new(h.project.id, WorkspaceKind::Authoring)
        .path(h.repo_path.display().to_string())
        .created_for_revision_id(rev_a.id)
        .status(WorkspaceStatus::Busy)
        .current_job_id(assigned_job_id)
        .base_commit_oid(&seed_commit)
        .head_commit_oid(&seed_commit)
        .created_at(created_at)
        .build();
    h.db.create_workspace(&workspace_a)
        .await
        .expect("workspace a");
    let assigned_job = JobBuilder::new(h.project.id, item_a.id, rev_a.id, "author_initial")
        .id(assigned_job_id)
        .workspace_kind(WorkspaceKind::Authoring)
        .job_input(JobInput::authoring_head(CommitOid::new(seed_commit.clone())))
        .output_artifact_kind(OutputArtifactKind::Commit)
        .status(JobStatus::Assigned)
        .workspace_id(workspace_a.id)
        .created_at(created_at)
        .build();
    h.db.create_job(&assigned_job).await.expect("assigned job");

    let item_b_id = ingot_domain::ids::ItemId::new();
    let rev_b_id = ingot_domain::ids::ItemRevisionId::new();
    let rev_b = RevisionBuilder::new(item_b_id)
        .id(rev_b_id)
        .revision_no(1)
        .explicit_seed(seed_commit.as_str())
        .created_at(created_at)
        .build();
    let item_b = ItemBuilder::new(h.project.id, rev_b.id)
        .id(rev_b.item_id)
        .created_at(created_at)
        .build();
    h.db.create_item_with_revision(&item_b, &rev_b)
        .await
        .expect("create item b");
    let running_job_id = ingot_domain::ids::JobId::new();
    let workspace_b = WorkspaceBuilder::new(h.project.id, WorkspaceKind::Authoring)
        .path(h.repo_path.display().to_string())
        .created_for_revision_id(rev_b.id)
        .status(WorkspaceStatus::Busy)
        .current_job_id(running_job_id)
        .base_commit_oid(&seed_commit)
        .head_commit_oid(&seed_commit)
        .created_at(created_at)
        .build();
    h.db.create_workspace(&workspace_b)
        .await
        .expect("workspace b");
    let running_job = JobBuilder::new(h.project.id, item_b.id, rev_b.id, "author_initial")
        .id(running_job_id)
        .workspace_kind(WorkspaceKind::Authoring)
        .job_input(JobInput::authoring_head(CommitOid::new(seed_commit.clone())))
        .output_artifact_kind(OutputArtifactKind::Commit)
        .status(JobStatus::Running)
        .workspace_id(workspace_b.id)
        .lease_owner_id("old-daemon")
        .lease_expires_at(created_at - ChronoDuration::minutes(1))
        .started_at(created_at)
        .created_at(created_at)
        .build();
    h.db.create_job(&running_job).await.expect("running job");

    h.dispatcher
        .reconcile_startup()
        .await
        .expect("reconcile startup");

    let updated_assigned = h.db.get_job(assigned_job.id).await.expect("assigned");
    assert_eq!(updated_assigned.state.status(), JobStatus::Queued);
    assert_eq!(updated_assigned.state.workspace_id(), None);

    let updated_running = h.db.get_job(running_job.id).await.expect("running");
    assert_eq!(updated_running.state.status(), JobStatus::Expired);
    assert_eq!(
        updated_running.state.outcome_class(),
        Some(OutcomeClass::TransientFailure)
    );

    let updated_workspace_a =
        h.db.get_workspace(workspace_a.id)
            .await
            .expect("workspace a");
    assert_eq!(updated_workspace_a.state.status(), WorkspaceStatus::Ready);
    let updated_workspace_b =
        h.db.get_workspace(workspace_b.id)
            .await
            .expect("workspace b");
    assert_eq!(updated_workspace_b.state.status(), WorkspaceStatus::Stale);
}
