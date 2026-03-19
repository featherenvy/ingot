use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use ingot_agent_runtime::{DispatcherConfig, RuntimeError};
use ingot_domain::job::{
    ContextPolicy, ExecutionPermission, JobInput, JobStatus, OutcomeClass, OutputArtifactKind,
    PhaseKind,
};
use ingot_domain::workspace::{WorkspaceKind, WorkspaceStatus};
use ingot_git::commands::head_oid;
use ingot_test_support::git::unique_temp_path;
use ingot_test_support::reports::{clean_review_report, findings_review_report};
use ingot_usecases::DispatchNotify;
use ingot_workspace::{provision_authoring_workspace, provision_integration_workspace};

mod common;
use common::*;
use ingot_domain::finding::{FindingSeverity, FindingTriageState};
use ingot_git::commands::git;
use ingot_usecases::job::{DispatchJobCommand, dispatch_job};
use ingot_usecases::job_lifecycle;
use ingot_workflow::step;

fn write_harness_toml(repo_path: &Path, contents: &str) {
    let ingot_dir = repo_path.join(".ingot");
    std::fs::create_dir_all(&ingot_dir).expect("create .ingot dir");
    std::fs::write(ingot_dir.join("harness.toml"), contents).expect("write harness.toml");
}

fn make_runtime_workspace(
    project_id: ingot_domain::ids::ProjectId,
    kind: WorkspaceKind,
    workspace_id: ingot_domain::ids::WorkspaceId,
    path: &Path,
    revision_id: ingot_domain::ids::ItemRevisionId,
    workspace_ref: impl Into<String>,
    base_commit_oid: impl Into<String>,
    head_commit_oid: impl Into<String>,
) -> ingot_domain::workspace::Workspace {
    WorkspaceBuilder::new(project_id, kind)
        .id(workspace_id)
        .path(path.display().to_string())
        .created_for_revision_id(revision_id)
        .workspace_ref(workspace_ref)
        .base_commit_oid(base_commit_oid)
        .head_commit_oid(head_commit_oid)
        .created_at(default_timestamp())
        .build()
}

async fn create_authoring_validation_workspace(
    h: &TestHarness,
    revision_id: ingot_domain::ids::ItemRevisionId,
    base_commit_oid: &str,
    head_commit_oid: &str,
) -> ingot_domain::workspace::Workspace {
    let paths = ensure_test_mirror(&h.state_root, &h.project).await;
    let workspace_id = ingot_domain::ids::WorkspaceId::new();
    let workspace_path = paths.worktree_root.join(workspace_id.to_string());
    let workspace_ref = format!("refs/ingot/workspaces/{workspace_id}");
    let provisioned = provision_authoring_workspace(
        paths.mirror_git_dir.as_path(),
        &workspace_path,
        &workspace_ref,
        head_commit_oid,
    )
    .await
    .expect("provision authoring workspace");
    let workspace = make_runtime_workspace(
        h.project.id,
        WorkspaceKind::Authoring,
        workspace_id,
        provisioned.workspace_path.as_path(),
        revision_id,
        provisioned.workspace_ref,
        base_commit_oid,
        provisioned.head_commit_oid,
    );
    h.db.create_workspace(&workspace)
        .await
        .expect("create authoring workspace");
    workspace
}

#[tokio::test]
async fn authoring_success_auto_dispatches_incremental_review() {
    let h = TestHarness::new(Arc::new(FakeRunner)).await;
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

    let jobs = h.db.list_jobs_by_item(item.id).await.expect("jobs");
    assert_eq!(jobs.len(), 2, "author success should auto-queue review");

    let completed_author = jobs
        .iter()
        .find(|job| job.step_id == step::AUTHOR_INITIAL)
        .expect("completed author job");
    assert_eq!(completed_author.state.status(), JobStatus::Completed);
    assert_eq!(
        completed_author.state.outcome_class(),
        Some(OutcomeClass::Clean)
    );

    let review_job = jobs
        .iter()
        .find(|job| job.step_id == step::REVIEW_INCREMENTAL_INITIAL)
        .expect("auto-dispatched incremental review job");
    assert_eq!(review_job.state.status(), JobStatus::Queued);
    assert_eq!(
        review_job.job_input.base_commit_oid(),
        Some(seed_commit.as_str())
    );
    assert_eq!(
        review_job.job_input.head_commit_oid(),
        completed_author.state.output_commit_oid()
    );
}

#[tokio::test]
async fn implicit_revision_auto_dispatches_incremental_review_from_bound_workspace_base() {
    let h = TestHarness::new(Arc::new(FakeRunner)).await;

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let bound_base = head_oid(&h.repo_path).await.expect("bound base");

    let item = ItemBuilder::new(h.project.id, revision_id)
        .id(item_id)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .seed_commit_oid(None::<String>)
        .seed_target_commit_oid(Some(bound_base.clone()))
        .build();
    h.db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    std::fs::write(h.repo_path.join("tracked.txt"), "implicit review change")
        .expect("write tracked file");
    git(&h.repo_path, &["add", "tracked.txt"])
        .await
        .expect("git add");
    git(&h.repo_path, &["commit", "-m", "implicit review change"])
        .await
        .expect("git commit");
    let author_output_commit = head_oid(&h.repo_path).await.expect("author output");

    let created_at = default_timestamp();
    let authoring_workspace = WorkspaceBuilder::new(h.project.id, WorkspaceKind::Authoring)
        .created_for_revision_id(revision.id)
        .base_commit_oid(bound_base.clone())
        .head_commit_oid(author_output_commit.clone())
        .workspace_ref("refs/ingot/workspaces/implicit-auto-review")
        .created_at(created_at)
        .build();
    h.db.create_workspace(&authoring_workspace)
        .await
        .expect("create workspace");

    let author_job = JobBuilder::new(h.project.id, item_id, revision_id, step::AUTHOR_INITIAL)
        .status(JobStatus::Completed)
        .outcome_class(OutcomeClass::Clean)
        .workspace_id(authoring_workspace.id)
        .phase_template_slug("author-initial")
        .job_input(JobInput::authoring_head(bound_base.clone()))
        .output_artifact_kind(OutputArtifactKind::Commit)
        .output_commit_oid(author_output_commit.clone())
        .result_schema_version("commit_summary:v1")
        .result_payload(serde_json::json!({
            "summary": "implicit review change",
            "validation": null
        }))
        .created_at(created_at)
        .started_at(created_at)
        .ended_at(created_at)
        .build();
    h.db.create_job(&author_job)
        .await
        .expect("create author job");

    let dispatched = h
        .dispatcher
        .auto_dispatch_projected_review_locked(&h.project, item_id)
        .await
        .expect("auto-dispatch review");
    assert!(dispatched, "review should be auto-dispatched");

    let jobs = h.db.list_jobs_by_item(item.id).await.expect("jobs");
    assert_eq!(jobs.len(), 2, "author success should auto-queue review");

    let review_job = jobs
        .iter()
        .find(|job| job.step_id == step::REVIEW_INCREMENTAL_INITIAL)
        .expect("auto-dispatched incremental review job");
    assert_eq!(review_job.state.status(), JobStatus::Queued);
    assert_eq!(
        review_job.job_input.base_commit_oid(),
        Some(bound_base.as_str())
    );
    assert_eq!(
        review_job.job_input.head_commit_oid(),
        Some(author_output_commit.as_str())
    );
}

#[tokio::test]
async fn auto_dispatch_projected_review_rejects_missing_candidate_subject() {
    let h = TestHarness::new(Arc::new(FakeRunner)).await;

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let item = ItemBuilder::new(h.project.id, revision_id)
        .id(item_id)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .seed_commit_oid(None::<String>)
        .seed_target_commit_oid(None::<String>)
        .build();
    h.db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let created_at = default_timestamp();
    let mut incomplete_review_report = clean_review_report("missing-base", "missing-head");
    incomplete_review_report
        .as_object_mut()
        .expect("review report object")
        .remove("review_subject");
    let completed_incremental_review = JobBuilder::new(
        h.project.id,
        item_id,
        revision_id,
        step::REVIEW_INCREMENTAL_INITIAL,
    )
    .status(JobStatus::Completed)
    .outcome_class(OutcomeClass::Clean)
    .phase_kind(PhaseKind::Review)
    .workspace_kind(WorkspaceKind::Review)
    .execution_permission(ExecutionPermission::MustNotMutate)
    .context_policy(ContextPolicy::ResumeContext)
    .phase_template_slug("review-incremental")
    .output_artifact_kind(OutputArtifactKind::ReviewReport)
    .result_schema_version("review_report:v1")
    .result_payload(incomplete_review_report)
    .created_at(created_at)
    .started_at(created_at)
    .ended_at(created_at)
    .build();
    h.db.create_job(&completed_incremental_review)
        .await
        .expect("create review job");

    let result = h
        .dispatcher
        .auto_dispatch_projected_review_locked(&h.project, item_id)
        .await;

    assert!(matches!(
        result,
        Err(RuntimeError::InvalidState(message))
            if message.contains("incomplete candidate subject")
    ));
}

#[tokio::test]
async fn tick_recovers_idle_review_work_even_when_processing_other_queued_jobs() {
    let h = TestHarness::new(Arc::new(FakeRunner)).await;
    h.register_full_agent().await;

    let authored_seed = head_oid(&h.repo_path).await.expect("seed head");
    std::fs::write(h.repo_path.join("feature.txt"), "candidate change").expect("write feature");
    git_sync(&h.repo_path, &["add", "feature.txt"]);
    git_sync(&h.repo_path, &["commit", "-m", "candidate change"]);
    let authored_head = head_oid(&h.repo_path).await.expect("authored head");

    // Busy item with a queued authoring job
    let busy_item_id = ingot_domain::ids::ItemId::new();
    let busy_revision_id = ingot_domain::ids::ItemRevisionId::new();
    let busy_item = ItemBuilder::new(h.project.id, busy_revision_id)
        .id(busy_item_id)
        .build();
    let busy_revision = RevisionBuilder::new(busy_item_id)
        .id(busy_revision_id)
        .seed_commit_oid(Some(authored_head.clone()))
        .seed_target_commit_oid(Some(authored_head.clone()))
        .build();
    h.db.create_item_with_revision(&busy_item, &busy_revision)
        .await
        .expect("create busy item");

    let busy_author_job = dispatch_job(
        &busy_item,
        &busy_revision,
        &[],
        &[],
        &[],
        DispatchJobCommand { step_id: None },
    )
    .expect("dispatch busy author job");
    h.db.create_job(&busy_author_job)
        .await
        .expect("create busy author job");

    // Idle item: author completed, review completed with findings, findings triaged
    let idle_item_id = ingot_domain::ids::ItemId::new();
    let idle_revision_id = ingot_domain::ids::ItemRevisionId::new();
    let idle_item = ItemBuilder::new(h.project.id, idle_revision_id)
        .id(idle_item_id)
        .build();
    let idle_revision = RevisionBuilder::new(idle_item_id)
        .id(idle_revision_id)
        .seed_commit_oid(Some(authored_seed.clone()))
        .seed_target_commit_oid(Some(authored_seed.clone()))
        .build();
    h.db.create_item_with_revision(&idle_item, &idle_revision)
        .await
        .expect("create idle item");

    let created_at = default_timestamp();
    h.db.create_job(
        &JobBuilder::new(
            h.project.id,
            idle_item_id,
            idle_revision_id,
            step::AUTHOR_INITIAL,
        )
        .status(JobStatus::Completed)
        .outcome_class(OutcomeClass::Clean)
        .phase_template_slug("author-initial")
        .job_input(JobInput::authoring_head(authored_seed.clone()))
        .output_artifact_kind(OutputArtifactKind::Commit)
        .output_commit_oid(authored_head.clone())
        .created_at(created_at)
        .started_at(created_at)
        .ended_at(created_at)
        .build(),
    )
    .await
    .expect("create idle author job");

    let mut idle_review_payload = findings_review_report(
        &authored_seed,
        &authored_head,
        "non-blocking note",
        "low",
        vec![serde_json::json!({
            "finding_key": "note",
            "code": "NOTE001",
            "severity": "low",
            "summary": "acceptable note",
            "paths": ["feature.txt"],
            "evidence": ["acceptable"]
        })],
    );
    idle_review_payload
        .as_object_mut()
        .expect("review payload object")
        .insert("extensions".into(), serde_json::Value::Null);

    let idle_review_job = JobBuilder::new(
        h.project.id,
        idle_item_id,
        idle_revision_id,
        step::REVIEW_INCREMENTAL_INITIAL,
    )
    .status(JobStatus::Completed)
    .outcome_class(OutcomeClass::Findings)
    .phase_kind(PhaseKind::Review)
    .workspace_kind(WorkspaceKind::Review)
    .execution_permission(ExecutionPermission::MustNotMutate)
    .phase_template_slug("review-incremental")
    .job_input(JobInput::candidate_subject(
        authored_seed.clone(),
        authored_head.clone(),
    ))
    .output_artifact_kind(OutputArtifactKind::ReviewReport)
    .result_schema_version("review_report:v1")
    .result_payload(idle_review_payload)
    .created_at(created_at)
    .started_at(created_at)
    .ended_at(created_at)
    .build();
    h.db.create_job(&idle_review_job)
        .await
        .expect("create idle review job");
    h.db.create_finding(
        &FindingBuilder::new(
            h.project.id,
            idle_item_id,
            idle_revision_id,
            idle_review_job.id,
        )
        .source_step_id(step::REVIEW_INCREMENTAL_INITIAL)
        .source_finding_key("note")
        .source_subject_base_commit_oid(
            idle_review_job
                .job_input
                .base_commit_oid()
                .map(ToOwned::to_owned),
        )
        .source_subject_head_commit_oid(
            idle_review_job
                .job_input
                .head_commit_oid()
                .map(ToOwned::to_owned)
                .expect("idle review head"),
        )
        .code("NOTE001")
        .severity(FindingSeverity::Low)
        .summary("acceptable note")
        .paths(vec!["feature.txt".into()])
        .evidence(serde_json::json!(["acceptable"]))
        .triage_state(FindingTriageState::WontFix)
        .triage_note("accepted for now")
        .created_at(created_at)
        .triaged_at(created_at)
        .build(),
    )
    .await
    .expect("create idle finding");

    assert!(
        h.dispatcher
            .tick()
            .await
            .expect("tick should run and recover")
    );

    let busy_jobs =
        h.db.list_jobs_by_item(busy_item_id)
            .await
            .expect("busy jobs");
    let busy_completed_author = busy_jobs
        .iter()
        .find(|job| job.step_id == step::AUTHOR_INITIAL)
        .expect("completed busy author");
    assert_eq!(busy_completed_author.state.status(), JobStatus::Completed);
    assert!(
        busy_jobs
            .iter()
            .any(|job| job.step_id == step::REVIEW_INCREMENTAL_INITIAL
                && job.state.status() == JobStatus::Queued)
    );

    let idle_jobs =
        h.db.list_jobs_by_item(idle_item_id)
            .await
            .expect("idle jobs");
    let idle_candidate_review = idle_jobs
        .iter()
        .find(|job| job.step_id == step::REVIEW_CANDIDATE_INITIAL)
        .expect("recovered idle candidate review");
    assert_eq!(idle_candidate_review.state.status(), JobStatus::Queued);
}

#[tokio::test]
async fn clean_incremental_review_auto_dispatches_candidate_review() {
    let repo = temp_git_repo("ingot-runtime-repo");
    let seed_commit = head_oid(&repo).await.expect("seed head");
    std::fs::write(repo.join("feature.txt"), "candidate change").expect("write feature");
    git_sync(&repo, &["add", "feature.txt"]);
    git_sync(&repo, &["commit", "-m", "candidate change"]);
    let candidate_head = head_oid(&repo).await.expect("candidate head");

    let db = migrated_test_db("ingot-runtime-auto-candidate-review").await;
    let dispatcher = ingot_agent_runtime::JobDispatcher::with_runner(
        db.clone(),
        ingot_usecases::ProjectLocks::default(),
        ingot_agent_runtime::DispatcherConfig::new(unique_temp_path(
            "ingot-runtime-auto-candidate-review-state",
        )),
        Arc::new(CleanInitialReviewRunner),
        DispatchNotify::default(),
    );

    let created_at = default_timestamp();
    let project = ProjectBuilder::new(&repo).created_at(created_at).build();
    db.create_project(&project).await.expect("create project");

    let agent = AgentBuilder::new(
        "codex",
        vec![
            ingot_domain::agent::AgentCapability::ReadOnlyJobs,
            ingot_domain::agent::AgentCapability::StructuredOutput,
        ],
    )
    .build();
    db.create_agent(&agent).await.expect("create agent");

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let item = ItemBuilder::new(project.id, revision_id)
        .id(item_id)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .seed_commit_oid(Some(seed_commit.clone()))
        .seed_target_commit_oid(Some(seed_commit.clone()))
        .build();
    db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    // Completed author job
    db.create_job(
        &JobBuilder::new(project.id, item_id, revision_id, step::AUTHOR_INITIAL)
            .status(JobStatus::Completed)
            .outcome_class(OutcomeClass::Clean)
            .phase_template_slug("author-initial")
            .job_input(JobInput::authoring_head(seed_commit.clone()))
            .output_artifact_kind(OutputArtifactKind::Commit)
            .output_commit_oid(candidate_head.clone())
            .created_at(created_at)
            .started_at(created_at)
            .ended_at(created_at)
            .build(),
    )
    .await
    .expect("create author job");

    // Queued incremental review job
    db.create_job(
        &JobBuilder::new(
            project.id,
            item_id,
            revision_id,
            step::REVIEW_INCREMENTAL_INITIAL,
        )
        .phase_kind(PhaseKind::Review)
        .workspace_kind(WorkspaceKind::Review)
        .execution_permission(ExecutionPermission::MustNotMutate)
        .phase_template_slug("review-incremental")
        .job_input(JobInput::candidate_subject(
            seed_commit.clone(),
            candidate_head.clone(),
        ))
        .output_artifact_kind(OutputArtifactKind::ReviewReport)
        .created_at(created_at)
        .build(),
    )
    .await
    .expect("create review job");

    assert!(dispatcher.tick().await.expect("review tick"));

    let jobs = db.list_jobs_by_item(item.id).await.expect("jobs");
    let completed_review = jobs
        .iter()
        .find(|job| job.step_id == step::REVIEW_INCREMENTAL_INITIAL)
        .expect("completed incremental review");
    assert_eq!(completed_review.state.status(), JobStatus::Completed);
    assert_eq!(
        completed_review.state.outcome_class(),
        Some(OutcomeClass::Clean)
    );

    let candidate_review = jobs
        .iter()
        .find(|job| job.step_id == step::REVIEW_CANDIDATE_INITIAL)
        .expect("auto-dispatched candidate review");
    assert_eq!(candidate_review.state.status(), JobStatus::Queued);
    assert_eq!(
        candidate_review.job_input.base_commit_oid(),
        Some(seed_commit.as_str())
    );
    assert_eq!(
        candidate_review.job_input.head_commit_oid(),
        Some(candidate_head.as_str())
    );
}

#[tokio::test]
async fn clean_candidate_review_auto_dispatches_candidate_validation() {
    let repo = temp_git_repo("ingot-runtime-repo");
    let seed_commit = head_oid(&repo).await.expect("seed head");
    std::fs::write(repo.join("feature.txt"), "candidate change").expect("write feature");
    git_sync(&repo, &["add", "feature.txt"]);
    git_sync(&repo, &["commit", "-m", "candidate change"]);
    let candidate_head = head_oid(&repo).await.expect("candidate head");

    let db = migrated_test_db("ingot-runtime-auto-candidate-validation").await;
    let dispatcher = ingot_agent_runtime::JobDispatcher::with_runner(
        db.clone(),
        ingot_usecases::ProjectLocks::default(),
        ingot_agent_runtime::DispatcherConfig::new(unique_temp_path(
            "ingot-runtime-auto-candidate-validation-state",
        )),
        Arc::new(CleanCandidateReviewRunner),
        DispatchNotify::default(),
    );

    let created_at = default_timestamp();
    let project = ProjectBuilder::new(&repo).created_at(created_at).build();
    db.create_project(&project).await.expect("create project");

    let agent = AgentBuilder::new(
        "codex",
        vec![
            ingot_domain::agent::AgentCapability::ReadOnlyJobs,
            ingot_domain::agent::AgentCapability::StructuredOutput,
        ],
    )
    .build();
    db.create_agent(&agent).await.expect("create agent");

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let item = ItemBuilder::new(project.id, revision_id)
        .id(item_id)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .seed_commit_oid(Some(seed_commit.clone()))
        .seed_target_commit_oid(Some(seed_commit.clone()))
        .build();
    db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    db.create_job(
        &JobBuilder::new(project.id, item_id, revision_id, step::AUTHOR_INITIAL)
            .status(JobStatus::Completed)
            .outcome_class(OutcomeClass::Clean)
            .phase_template_slug("author-initial")
            .job_input(JobInput::authoring_head(seed_commit.clone()))
            .output_artifact_kind(OutputArtifactKind::Commit)
            .output_commit_oid(candidate_head.clone())
            .created_at(created_at)
            .started_at(created_at)
            .ended_at(created_at)
            .build(),
    )
    .await
    .expect("create author job");

    db.create_job(
        &JobBuilder::new(
            project.id,
            item_id,
            revision_id,
            step::REVIEW_CANDIDATE_INITIAL,
        )
        .phase_kind(PhaseKind::Review)
        .workspace_kind(WorkspaceKind::Review)
        .execution_permission(ExecutionPermission::MustNotMutate)
        .phase_template_slug("review-candidate")
        .job_input(JobInput::candidate_subject(
            seed_commit.clone(),
            candidate_head.clone(),
        ))
        .output_artifact_kind(OutputArtifactKind::ReviewReport)
        .created_at(created_at)
        .build(),
    )
    .await
    .expect("create review candidate job");

    assert!(dispatcher.tick().await.expect("review candidate tick"));

    let jobs = db.list_jobs_by_item(item.id).await.expect("jobs");
    let completed_review = jobs
        .iter()
        .find(|job| job.step_id == step::REVIEW_CANDIDATE_INITIAL)
        .expect("completed candidate review");
    assert_eq!(completed_review.state.status(), JobStatus::Completed);
    assert_eq!(
        completed_review.state.outcome_class(),
        Some(OutcomeClass::Clean)
    );

    let validation_job = jobs
        .iter()
        .find(|job| job.step_id == step::VALIDATE_CANDIDATE_INITIAL)
        .expect("auto-dispatched candidate validation");
    assert_eq!(validation_job.state.status(), JobStatus::Queued);
    assert_eq!(
        validation_job.job_input.base_commit_oid(),
        Some(seed_commit.as_str())
    );
    assert_eq!(
        validation_job.job_input.head_commit_oid(),
        Some(candidate_head.as_str())
    );
}

#[tokio::test]
async fn daemon_only_validation_job_executes_on_tick() {
    // Daemon-only validation jobs use the harness execution path (no agent needed).
    // With no harness profile, validation auto-completes as clean.
    let h = TestHarness::new(Arc::new(FakeRunner)).await;

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let seed_commit = head_oid(&h.repo_path).await.expect("seed head");
    std::fs::write(h.repo_path.join("tracked.txt"), "candidate change").expect("write tracked");
    git_sync(&h.repo_path, &["add", "tracked.txt"]);
    git_sync(&h.repo_path, &["commit", "-m", "candidate change"]);
    let candidate_head = head_oid(&h.repo_path).await.expect("candidate head");

    let item = ItemBuilder::new(h.project.id, revision_id)
        .id(item_id)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .seed_commit_oid(Some(seed_commit.clone()))
        .seed_target_commit_oid(Some(seed_commit.clone()))
        .build();
    h.db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    // Create an authoring workspace so the harness validation can resolve it
    let workspace_id = ingot_domain::ids::WorkspaceId::new();
    let workspace = make_runtime_workspace(
        h.project.id,
        WorkspaceKind::Authoring,
        workspace_id,
        &h.repo_path,
        revision_id,
        format!("refs/ingot/workspaces/{workspace_id}"),
        seed_commit.clone(),
        candidate_head.clone(),
    );
    h.db.create_workspace(&workspace)
        .await
        .expect("create authoring workspace");

    let validation_job = JobBuilder::new(
        h.project.id,
        item_id,
        revision_id,
        step::VALIDATE_CANDIDATE_INITIAL,
    )
    .phase_kind(PhaseKind::Validate)
    .workspace_kind(WorkspaceKind::Authoring)
    .execution_permission(ExecutionPermission::DaemonOnly)
    .context_policy(ContextPolicy::None)
    .phase_template_slug("")
    .job_input(JobInput::candidate_subject(
        seed_commit.clone(),
        candidate_head.clone(),
    ))
    .output_artifact_kind(OutputArtifactKind::ValidationReport)
    .build();
    h.db.create_job(&validation_job)
        .await
        .expect("create validation job");

    assert!(h.dispatcher.tick().await.expect("validation tick"));

    let job =
        h.db.get_job(validation_job.id)
            .await
            .expect("reload validation job");
    assert_eq!(job.state.status(), JobStatus::Completed);
    assert_eq!(job.state.outcome_class(), Some(OutcomeClass::Clean));
    assert_eq!(
        job.state.result_schema_version(),
        Some("validation_report:v1")
    );
}

#[tokio::test]
async fn run_forever_executes_daemon_only_validation_job() {
    let mut config = DispatcherConfig::new(unique_temp_path("ingot-runtime-daemon-validation"));
    config.poll_interval = Duration::from_secs(10);
    config.max_concurrent_jobs = 1;
    let h = TestHarness::with_config(Arc::new(FakeRunner), Some(config)).await;
    h.dispatcher
        .reconcile_startup()
        .await
        .expect("reconcile startup");

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let seed_commit = head_oid(&h.repo_path).await.expect("seed head");
    std::fs::write(h.repo_path.join("tracked.txt"), "candidate change").expect("write tracked");
    git_sync(&h.repo_path, &["add", "tracked.txt"]);
    git_sync(&h.repo_path, &["commit", "-m", "candidate change"]);
    let candidate_head = head_oid(&h.repo_path).await.expect("candidate head");

    let item = ItemBuilder::new(h.project.id, revision_id)
        .id(item_id)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .seed_commit_oid(Some(seed_commit.clone()))
        .seed_target_commit_oid(Some(seed_commit.clone()))
        .build();
    h.db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");
    create_authoring_validation_workspace(&h, revision_id, &seed_commit, &candidate_head).await;

    let validation_job = JobBuilder::new(
        h.project.id,
        item_id,
        revision_id,
        step::VALIDATE_CANDIDATE_INITIAL,
    )
    .phase_kind(PhaseKind::Validate)
    .workspace_kind(WorkspaceKind::Authoring)
    .execution_permission(ExecutionPermission::DaemonOnly)
    .context_policy(ContextPolicy::None)
    .phase_template_slug("")
    .job_input(JobInput::candidate_subject(
        seed_commit.clone(),
        candidate_head.clone(),
    ))
    .output_artifact_kind(OutputArtifactKind::ValidationReport)
    .build();
    h.db.create_job(&validation_job)
        .await
        .expect("create validation job");

    let dispatcher = h.dispatcher.clone();
    let handle = tokio::spawn(async move { dispatcher.run_forever().await });
    h.dispatch_notify.notify();

    let job = h
        .wait_for_job_status(
            validation_job.id,
            JobStatus::Completed,
            Duration::from_secs(5),
        )
        .await;
    assert_eq!(job.state.outcome_class(), Some(OutcomeClass::Clean));
    assert_eq!(
        job.state.result_schema_version(),
        Some("validation_report:v1")
    );

    let artifact_dir = h
        .state_root
        .join("logs")
        .join(validation_job.id.to_string());
    assert!(
        !artifact_dir.join("prompt.txt").exists(),
        "daemon-only validation should not write prompt artifacts"
    );
    assert!(
        !artifact_dir.join("result.json").exists(),
        "daemon-only validation should not write result artifacts"
    );

    handle.abort();
    let _ = handle.await;
}

#[tokio::test]
async fn run_forever_refreshes_heartbeat_for_daemon_only_validation_job() {
    let mut config = DispatcherConfig::new(unique_temp_path(
        "ingot-runtime-daemon-validation-heartbeat",
    ));
    config.poll_interval = Duration::from_secs(10);
    config.heartbeat_interval = Duration::from_millis(20);
    config.max_concurrent_jobs = 1;
    let h = TestHarness::with_config(Arc::new(FakeRunner), Some(config)).await;
    h.dispatcher
        .reconcile_startup()
        .await
        .expect("reconcile startup");

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let seed_commit = head_oid(&h.repo_path).await.expect("seed head");
    std::fs::write(h.repo_path.join("tracked.txt"), "candidate change").expect("write tracked");
    git_sync(&h.repo_path, &["add", "tracked.txt"]);
    git_sync(&h.repo_path, &["commit", "-m", "candidate change"]);
    let candidate_head = head_oid(&h.repo_path).await.expect("candidate head");

    let item = ItemBuilder::new(h.project.id, revision_id)
        .id(item_id)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .seed_commit_oid(Some(seed_commit.clone()))
        .seed_target_commit_oid(Some(seed_commit.clone()))
        .build();
    h.db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");
    create_authoring_validation_workspace(&h, revision_id, &seed_commit, &candidate_head).await;

    write_harness_toml(
        &h.repo_path,
        r#"
[commands.sleepy]
run = "sleep 0.5"
timeout = "30s"
"#,
    );

    let validation_job = JobBuilder::new(
        h.project.id,
        item_id,
        revision_id,
        step::VALIDATE_CANDIDATE_INITIAL,
    )
    .phase_kind(PhaseKind::Validate)
    .workspace_kind(WorkspaceKind::Authoring)
    .execution_permission(ExecutionPermission::DaemonOnly)
    .context_policy(ContextPolicy::None)
    .phase_template_slug("")
    .job_input(JobInput::candidate_subject(
        seed_commit.clone(),
        candidate_head.clone(),
    ))
    .output_artifact_kind(OutputArtifactKind::ValidationReport)
    .build();
    h.db.create_job(&validation_job)
        .await
        .expect("create validation job");

    let dispatcher = h.dispatcher.clone();
    let handle = tokio::spawn(async move { dispatcher.run_forever().await });
    h.dispatch_notify.notify();

    let running_job = h
        .wait_for_job_status(
            validation_job.id,
            JobStatus::Running,
            Duration::from_secs(2),
        )
        .await;
    let initial_heartbeat = running_job
        .state
        .heartbeat_at()
        .expect("initial validation heartbeat");

    let refreshed_job = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let job =
                h.db.get_job(validation_job.id)
                    .await
                    .expect("reload validation job");
            if job
                .state
                .heartbeat_at()
                .is_some_and(|heartbeat| heartbeat > initial_heartbeat)
            {
                return job;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("timed out waiting for validation heartbeat refresh");
    assert!(
        refreshed_job
            .state
            .heartbeat_at()
            .is_some_and(|heartbeat| heartbeat > initial_heartbeat)
    );

    h.wait_for_job_status(
        validation_job.id,
        JobStatus::Completed,
        Duration::from_secs(2),
    )
    .await;
    handle.abort();
    let _ = handle.await;
}

#[tokio::test]
async fn run_forever_cancels_daemon_only_validation_command() {
    let mut config =
        DispatcherConfig::new(unique_temp_path("ingot-runtime-daemon-validation-cancel"));
    config.poll_interval = Duration::from_secs(10);
    config.heartbeat_interval = Duration::from_secs(5);
    config.max_concurrent_jobs = 1;
    let h = TestHarness::with_config(Arc::new(FakeRunner), Some(config)).await;
    h.dispatcher
        .reconcile_startup()
        .await
        .expect("reconcile startup");

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let seed_commit = head_oid(&h.repo_path).await.expect("seed head");
    std::fs::write(h.repo_path.join("tracked.txt"), "candidate change").expect("write tracked");
    git_sync(&h.repo_path, &["add", "tracked.txt"]);
    git_sync(&h.repo_path, &["commit", "-m", "candidate change"]);
    let candidate_head = head_oid(&h.repo_path).await.expect("candidate head");

    let item = ItemBuilder::new(h.project.id, revision_id)
        .id(item_id)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .seed_commit_oid(Some(seed_commit.clone()))
        .seed_target_commit_oid(Some(seed_commit.clone()))
        .build();
    h.db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");
    let workspace =
        create_authoring_validation_workspace(&h, revision_id, &seed_commit, &candidate_head).await;

    write_harness_toml(
        &h.repo_path,
        r#"
[commands.sleepy]
run = "while true; do echo tick >> cancellation-marker.log; sleep 0.05; done"
timeout = "30s"
"#,
    );

    let validation_job = JobBuilder::new(
        h.project.id,
        item_id,
        revision_id,
        step::VALIDATE_CANDIDATE_INITIAL,
    )
    .phase_kind(PhaseKind::Validate)
    .workspace_kind(WorkspaceKind::Authoring)
    .execution_permission(ExecutionPermission::DaemonOnly)
    .context_policy(ContextPolicy::None)
    .phase_template_slug("")
    .job_input(JobInput::candidate_subject(
        seed_commit.clone(),
        candidate_head.clone(),
    ))
    .output_artifact_kind(OutputArtifactKind::ValidationReport)
    .build();
    h.db.create_job(&validation_job)
        .await
        .expect("create validation job");

    let dispatcher = h.dispatcher.clone();
    let handle = tokio::spawn(async move { dispatcher.run_forever().await });
    h.dispatch_notify.notify();

    h.wait_for_job_status(
        validation_job.id,
        JobStatus::Running,
        Duration::from_secs(2),
    )
    .await;
    let marker_path = Path::new(&workspace.path).join("cancellation-marker.log");
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if marker_path.exists() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("marker file should be created before cancellation");

    let active_job =
        h.db.get_job(validation_job.id)
            .await
            .expect("reload active job");
    job_lifecycle::cancel_job(
        &h.db,
        &h.db,
        &h.db,
        &active_job,
        &item,
        "operator_cancelled",
        WorkspaceStatus::Ready,
    )
    .await
    .expect("cancel validation job");
    h.dispatch_notify.notify();

    h.wait_for_job_status(
        validation_job.id,
        JobStatus::Cancelled,
        Duration::from_secs(2),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(250)).await;
    let size_after_settle = std::fs::metadata(&marker_path)
        .expect("marker file after cancellation settles")
        .len();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let size_later = std::fs::metadata(&marker_path)
        .expect("marker file after cancellation remains stable")
        .len();
    assert_eq!(
        size_later, size_after_settle,
        "cancelled daemon-only validation command should stop writing output"
    );

    handle.abort();
    let _ = handle.await;
}

#[tokio::test]
async fn daemon_only_validation_command_completes_even_when_heartbeat_interval_exceeds_command_timeout()
 {
    let mut config = DispatcherConfig::new(unique_temp_path(
        "ingot-runtime-daemon-validation-timeout-race",
    ));
    config.poll_interval = Duration::from_secs(10);
    config.heartbeat_interval = Duration::from_secs(5);
    config.max_concurrent_jobs = 1;
    let h = TestHarness::with_config(Arc::new(FakeRunner), Some(config)).await;

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let seed_commit = head_oid(&h.repo_path).await.expect("seed head");
    std::fs::write(h.repo_path.join("tracked.txt"), "candidate change").expect("write tracked");
    git_sync(&h.repo_path, &["add", "tracked.txt"]);
    git_sync(&h.repo_path, &["commit", "-m", "candidate change"]);
    let candidate_head = head_oid(&h.repo_path).await.expect("candidate head");

    let item = ItemBuilder::new(h.project.id, revision_id)
        .id(item_id)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .seed_commit_oid(Some(seed_commit.clone()))
        .seed_target_commit_oid(Some(seed_commit.clone()))
        .build();
    h.db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");
    create_authoring_validation_workspace(&h, revision_id, &seed_commit, &candidate_head).await;

    write_harness_toml(
        &h.repo_path,
        r#"
[commands.quick]
run = "sleep 0.1"
timeout = "1s"
"#,
    );

    let validation_job = JobBuilder::new(
        h.project.id,
        item_id,
        revision_id,
        step::VALIDATE_CANDIDATE_INITIAL,
    )
    .phase_kind(PhaseKind::Validate)
    .workspace_kind(WorkspaceKind::Authoring)
    .execution_permission(ExecutionPermission::DaemonOnly)
    .context_policy(ContextPolicy::None)
    .phase_template_slug("")
    .job_input(JobInput::candidate_subject(
        seed_commit.clone(),
        candidate_head.clone(),
    ))
    .output_artifact_kind(OutputArtifactKind::ValidationReport)
    .build();
    h.db.create_job(&validation_job)
        .await
        .expect("create validation job");

    assert!(h.dispatcher.tick().await.expect("validation tick"));

    let job =
        h.db.get_job(validation_job.id)
            .await
            .expect("reload validation job");
    assert_eq!(job.state.status(), JobStatus::Completed);
    assert_eq!(job.state.outcome_class(), Some(OutcomeClass::Clean));
}

#[tokio::test]
async fn harness_validation_with_commands_produces_findings_on_failure() {
    let h = TestHarness::new(Arc::new(FakeRunner)).await;

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let seed_commit = head_oid(&h.repo_path).await.expect("seed head");
    std::fs::write(h.repo_path.join("tracked.txt"), "candidate change").expect("write tracked");
    git_sync(&h.repo_path, &["add", "tracked.txt"]);
    git_sync(&h.repo_path, &["commit", "-m", "candidate change"]);
    let candidate_head = head_oid(&h.repo_path).await.expect("candidate head");

    let item = ItemBuilder::new(h.project.id, revision_id)
        .id(item_id)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .seed_commit_oid(Some(seed_commit.clone()))
        .seed_target_commit_oid(Some(seed_commit.clone()))
        .build();
    h.db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    // Create authoring workspace
    let workspace_id = ingot_domain::ids::WorkspaceId::new();
    let workspace = make_runtime_workspace(
        h.project.id,
        WorkspaceKind::Authoring,
        workspace_id,
        &h.repo_path,
        revision_id,
        format!("refs/ingot/workspaces/{workspace_id}"),
        seed_commit.clone(),
        candidate_head.clone(),
    );
    h.db.create_workspace(&workspace)
        .await
        .expect("create authoring workspace");

    // Create a harness profile with a command that will fail
    let project_path = std::path::Path::new(&h.project.path);
    let ingot_dir = project_path.join(".ingot");
    std::fs::create_dir_all(&ingot_dir).expect("create .ingot dir");
    std::fs::write(
        ingot_dir.join("harness.toml"),
        r#"
[commands.check]
run = "exit 0"
timeout = "30s"

[commands.failing_test]
run = "echo 'test failed' && exit 1"
timeout = "30s"
"#,
    )
    .expect("write harness.toml");

    let validation_job = JobBuilder::new(
        h.project.id,
        item_id,
        revision_id,
        step::VALIDATE_CANDIDATE_INITIAL,
    )
    .phase_kind(PhaseKind::Validate)
    .workspace_kind(WorkspaceKind::Authoring)
    .execution_permission(ExecutionPermission::DaemonOnly)
    .context_policy(ContextPolicy::None)
    .phase_template_slug("")
    .job_input(JobInput::candidate_subject(
        seed_commit.clone(),
        candidate_head.clone(),
    ))
    .output_artifact_kind(OutputArtifactKind::ValidationReport)
    .build();
    h.db.create_job(&validation_job)
        .await
        .expect("create validation job");

    assert!(h.dispatcher.tick().await.expect("validation tick"));

    let job =
        h.db.get_job(validation_job.id)
            .await
            .expect("reload validation job");
    assert_eq!(job.state.status(), JobStatus::Completed);
    assert_eq!(job.state.outcome_class(), Some(OutcomeClass::Findings));
    assert_eq!(
        job.state.result_schema_version(),
        Some("validation_report:v1")
    );

    // Parse the result payload to verify check structure
    let payload = job.state.result_payload().expect("result payload");
    let checks = payload["checks"].as_array().expect("checks array");
    assert_eq!(checks.len(), 2);
    assert_eq!(checks[0]["name"].as_str(), Some("check"));
    assert_eq!(checks[0]["status"].as_str(), Some("pass"));
    assert_eq!(checks[1]["name"].as_str(), Some("failing_test"));
    assert_eq!(checks[1]["status"].as_str(), Some("fail"));

    let findings = payload["findings"].as_array().expect("findings array");
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0]["code"].as_str(), Some("failing_test"));
    assert_eq!(findings[0]["severity"].as_str(), Some("high"));

    // Verify findings were extracted into durable rows
    let db_findings =
        h.db.list_findings_by_item(item_id)
            .await
            .expect("list findings");
    assert_eq!(db_findings.len(), 1);
    assert_eq!(db_findings[0].code, "failing_test");
}

#[tokio::test]
async fn daemon_only_validation_fails_on_invalid_harness_profile() {
    let h = TestHarness::new(Arc::new(FakeRunner)).await;

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let seed_commit = head_oid(&h.repo_path).await.expect("seed head");
    std::fs::write(h.repo_path.join("tracked.txt"), "candidate change").expect("write tracked");
    git_sync(&h.repo_path, &["add", "tracked.txt"]);
    git_sync(&h.repo_path, &["commit", "-m", "candidate change"]);
    let candidate_head = head_oid(&h.repo_path).await.expect("candidate head");

    let item = ItemBuilder::new(h.project.id, revision_id)
        .id(item_id)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .seed_commit_oid(Some(seed_commit.clone()))
        .seed_target_commit_oid(Some(seed_commit.clone()))
        .build();
    h.db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");
    create_authoring_validation_workspace(&h, revision_id, &seed_commit, &candidate_head).await;

    write_harness_toml(
        &h.repo_path,
        r#"
[commands.check]
run = "exit 0"
timeout = "bogus"
"#,
    );

    let validation_job = JobBuilder::new(
        h.project.id,
        item_id,
        revision_id,
        step::VALIDATE_CANDIDATE_INITIAL,
    )
    .phase_kind(PhaseKind::Validate)
    .workspace_kind(WorkspaceKind::Authoring)
    .execution_permission(ExecutionPermission::DaemonOnly)
    .context_policy(ContextPolicy::None)
    .phase_template_slug("")
    .job_input(JobInput::candidate_subject(
        seed_commit.clone(),
        candidate_head.clone(),
    ))
    .output_artifact_kind(OutputArtifactKind::ValidationReport)
    .build();
    h.db.create_job(&validation_job)
        .await
        .expect("create validation job");

    assert!(h.dispatcher.tick().await.expect("validation tick"));

    let job =
        h.db.get_job(validation_job.id)
            .await
            .expect("reload validation job");
    assert_eq!(job.state.status(), JobStatus::Failed);
    assert_eq!(
        job.state.outcome_class(),
        Some(OutcomeClass::TerminalFailure)
    );
    assert_eq!(job.state.error_code(), Some("invalid_harness_profile"));
    assert!(
        job.state
            .error_message()
            .expect("error message")
            .contains("invalid duration")
    );
}

#[tokio::test]
async fn queued_authoring_job_fails_on_invalid_harness_profile() {
    let h = TestHarness::new(Arc::new(FakeRunner)).await;
    h.register_mutating_agent().await;

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let seed_commit = head_oid(&h.repo_path).await.expect("seed head");

    let item = ItemBuilder::new(h.project.id, revision_id)
        .id(item_id)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .explicit_seed(&seed_commit)
        .template_map_snapshot(serde_json::json!({ "author_initial": "author-initial" }))
        .build();
    h.db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    write_harness_toml(
        &h.repo_path,
        r#"
[commands.check]
run = "exit 0"
timeout = "bogus"
"#,
    );

    let job = test_authoring_job(h.project.id, item_id, revision_id, &seed_commit);
    h.db.create_job(&job).await.expect("create job");

    assert!(h.dispatcher.tick().await.expect("tick should run"));

    let updated_job = h.db.get_job(job.id).await.expect("updated job");
    assert_eq!(updated_job.state.status(), JobStatus::Failed);
    assert_eq!(
        updated_job.state.outcome_class(),
        Some(OutcomeClass::TerminalFailure)
    );
    assert_eq!(
        updated_job.state.error_code(),
        Some("invalid_harness_profile")
    );

    let prompt_path = h
        .state_root
        .join("logs")
        .join(job.id.to_string())
        .join("prompt.txt");
    assert!(
        !prompt_path.exists(),
        "prep-time harness failure should not write a prompt artifact"
    );
}

#[tokio::test]
async fn authoring_prompt_includes_resolved_repo_local_skill_files() {
    let h = TestHarness::new(Arc::new(FakeRunner)).await;
    h.register_mutating_agent().await;

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let seed_commit = head_oid(&h.repo_path).await.expect("seed head");

    let item = ItemBuilder::new(h.project.id, revision_id)
        .id(item_id)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .explicit_seed(&seed_commit)
        .template_map_snapshot(serde_json::json!({ "author_initial": "author-initial" }))
        .build();
    h.db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let skill_dir = h.repo_path.join(".ingot/skills");
    std::fs::create_dir_all(&skill_dir).expect("create skill dir");
    std::fs::write(
        skill_dir.join("local.md"),
        "Follow the repo-local rule before touching tests.",
    )
    .expect("write skill file");
    write_harness_toml(
        &h.repo_path,
        r#"
[commands.check]
run = "cargo check"
timeout = "5m"

[skills]
paths = [".ingot/skills/*.md"]
"#,
    );

    let job = test_authoring_job(h.project.id, item_id, revision_id, &seed_commit);
    h.db.create_job(&job).await.expect("create job");

    assert!(h.dispatcher.tick().await.expect("tick should run"));

    let prompt_path = h
        .state_root
        .join("logs")
        .join(job.id.to_string())
        .join("prompt.txt");
    let prompt = std::fs::read_to_string(&prompt_path).expect("read prompt artifact");
    assert!(prompt.contains("Available verification commands:"));
    assert!(prompt.contains("`check`: `cargo check`"));
    assert!(prompt.contains("Skill file: .ingot/skills/local.md"));
    assert!(prompt.contains("Follow the repo-local rule before touching tests."));
    assert!(
        !prompt.contains(".ingot/skills/*.md"),
        "prompt should inline resolved skills, not raw glob patterns"
    );
}

#[tokio::test]
async fn queued_authoring_job_fails_when_harness_skill_glob_escapes_repo() {
    let h = TestHarness::new(Arc::new(FakeRunner)).await;
    h.register_mutating_agent().await;

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let seed_commit = head_oid(&h.repo_path).await.expect("seed head");

    let item = ItemBuilder::new(h.project.id, revision_id)
        .id(item_id)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .explicit_seed(&seed_commit)
        .template_map_snapshot(serde_json::json!({ "author_initial": "author-initial" }))
        .build();
    h.db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let escaped_root = unique_temp_path("ingot-runtime-escaped-skills");
    std::fs::create_dir_all(&escaped_root).expect("create escaped skill dir");
    std::fs::write(
        escaped_root.join("outside.md"),
        "This file should never be loaded into the prompt.",
    )
    .expect("write escaped skill file");
    let escaped_dir_name = escaped_root
        .file_name()
        .expect("escaped dir name")
        .to_string_lossy();
    write_harness_toml(
        &h.repo_path,
        &format!(
            r#"
[commands.check]
run = "cargo check"
timeout = "5m"

[skills]
paths = ["../{escaped_dir_name}/*.md"]
"#
        ),
    );

    let job = test_authoring_job(h.project.id, item_id, revision_id, &seed_commit);
    h.db.create_job(&job).await.expect("create job");

    assert!(h.dispatcher.tick().await.expect("tick should run"));

    let updated_job = h.db.get_job(job.id).await.expect("updated job");
    assert_eq!(updated_job.state.status(), JobStatus::Failed);
    assert_eq!(
        updated_job.state.outcome_class(),
        Some(OutcomeClass::TerminalFailure)
    );
    assert_eq!(
        updated_job.state.error_code(),
        Some("invalid_harness_profile")
    );
    assert!(
        updated_job
            .state
            .error_message()
            .expect("error message")
            .contains("escapes project root")
    );

    let prompt_path = h
        .state_root
        .join("logs")
        .join(job.id.to_string())
        .join("prompt.txt");
    assert!(
        !prompt_path.exists(),
        "prep-time harness failure should not write a prompt artifact"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn queued_authoring_job_fails_when_repo_local_skill_symlink_points_outside_repo() {
    let h = TestHarness::new(Arc::new(FakeRunner)).await;
    h.register_mutating_agent().await;

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let seed_commit = head_oid(&h.repo_path).await.expect("seed head");

    let item = ItemBuilder::new(h.project.id, revision_id)
        .id(item_id)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .explicit_seed(&seed_commit)
        .template_map_snapshot(serde_json::json!({ "author_initial": "author-initial" }))
        .build();
    h.db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let escaped_root = unique_temp_path("ingot-runtime-escaped-skills");
    std::fs::create_dir_all(&escaped_root).expect("create escaped skill dir");
    let escaped_skill_path = escaped_root.join("outside.md");
    std::fs::write(
        &escaped_skill_path,
        "This symlink target should never be loaded into the prompt.",
    )
    .expect("write escaped skill file");

    let skill_dir = h.repo_path.join(".ingot/skills");
    std::fs::create_dir_all(&skill_dir).expect("create skill dir");
    std::os::unix::fs::symlink(&escaped_skill_path, skill_dir.join("escaped.md"))
        .expect("create escaping symlink");
    write_harness_toml(
        &h.repo_path,
        r#"
[commands.check]
run = "cargo check"
timeout = "5m"

[skills]
paths = [".ingot/skills/*.md"]
"#,
    );

    let job = test_authoring_job(h.project.id, item_id, revision_id, &seed_commit);
    h.db.create_job(&job).await.expect("create job");

    assert!(h.dispatcher.tick().await.expect("tick should run"));

    let updated_job = h.db.get_job(job.id).await.expect("updated job");
    assert_eq!(updated_job.state.status(), JobStatus::Failed);
    assert_eq!(
        updated_job.state.outcome_class(),
        Some(OutcomeClass::TerminalFailure)
    );
    assert_eq!(
        updated_job.state.error_code(),
        Some("invalid_harness_profile")
    );
    assert!(
        updated_job
            .state
            .error_message()
            .expect("error message")
            .contains("escapes project root")
    );

    let prompt_path = h
        .state_root
        .join("logs")
        .join(job.id.to_string())
        .join("prompt.txt");
    assert!(
        !prompt_path.exists(),
        "prep-time harness failure should not write a prompt artifact"
    );
}

#[tokio::test]
async fn harness_validation_timeout_kills_background_processes() {
    let h = TestHarness::new(Arc::new(FakeRunner)).await;

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let seed_commit = head_oid(&h.repo_path).await.expect("seed head");
    std::fs::write(h.repo_path.join("tracked.txt"), "candidate change").expect("write tracked");
    git_sync(&h.repo_path, &["add", "tracked.txt"]);
    git_sync(&h.repo_path, &["commit", "-m", "candidate change"]);
    let candidate_head = head_oid(&h.repo_path).await.expect("candidate head");

    let item = ItemBuilder::new(h.project.id, revision_id)
        .id(item_id)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .seed_commit_oid(Some(seed_commit.clone()))
        .seed_target_commit_oid(Some(seed_commit.clone()))
        .build();
    h.db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let workspace =
        create_authoring_validation_workspace(&h, revision_id, &seed_commit, &candidate_head).await;
    write_harness_toml(
        &h.repo_path,
        r#"
[commands.timeout]
run = "(sleep 2; echo orphan > timeout-orphan.txt) & sleep 30"
timeout = "1s"
"#,
    );

    let validation_job = JobBuilder::new(
        h.project.id,
        item_id,
        revision_id,
        step::VALIDATE_CANDIDATE_INITIAL,
    )
    .phase_kind(PhaseKind::Validate)
    .workspace_kind(WorkspaceKind::Authoring)
    .execution_permission(ExecutionPermission::DaemonOnly)
    .context_policy(ContextPolicy::None)
    .phase_template_slug("")
    .job_input(JobInput::candidate_subject(
        seed_commit.clone(),
        candidate_head.clone(),
    ))
    .output_artifact_kind(OutputArtifactKind::ValidationReport)
    .build();
    h.db.create_job(&validation_job)
        .await
        .expect("create validation job");

    assert!(h.dispatcher.tick().await.expect("validation tick"));

    let job =
        h.db.get_job(validation_job.id)
            .await
            .expect("reload validation job");
    assert_eq!(job.state.status(), JobStatus::Completed);
    assert_eq!(job.state.outcome_class(), Some(OutcomeClass::Findings));
    let payload = job.state.result_payload().expect("result payload");
    assert!(
        payload["checks"][0]["summary"]
            .as_str()
            .expect("check summary")
            .contains("timed out")
    );

    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    assert!(
        !Path::new(&workspace.path)
            .join("timeout-orphan.txt")
            .exists(),
        "timed out harness command should not leave a background writer alive"
    );
}

#[tokio::test]
async fn daemon_validation_resyncs_authoring_workspace_before_running_harness() {
    let h = TestHarness::new(Arc::new(FakeRunner)).await;

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let seed_commit = head_oid(&h.repo_path).await.expect("seed head");
    std::fs::write(h.repo_path.join("tracked.txt"), "candidate change").expect("write tracked");
    git_sync(&h.repo_path, &["add", "tracked.txt"]);
    git_sync(&h.repo_path, &["commit", "-m", "candidate change"]);
    let candidate_head = head_oid(&h.repo_path).await.expect("candidate head");

    let item = ItemBuilder::new(h.project.id, revision_id)
        .id(item_id)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .seed_commit_oid(Some(seed_commit.clone()))
        .seed_target_commit_oid(Some(seed_commit.clone()))
        .build();
    h.db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let workspace =
        create_authoring_validation_workspace(&h, revision_id, &seed_commit, &candidate_head).await;
    git_sync(Path::new(&workspace.path), &["checkout", &seed_commit]);
    write_harness_toml(
        &h.repo_path,
        &format!(
            r#"
[commands.head_matches]
run = "test \"$(git rev-parse HEAD)\" = \"{candidate_head}\""
timeout = "30s"
"#
        ),
    );

    let validation_job = JobBuilder::new(
        h.project.id,
        item_id,
        revision_id,
        step::VALIDATE_CANDIDATE_INITIAL,
    )
    .phase_kind(PhaseKind::Validate)
    .workspace_kind(WorkspaceKind::Authoring)
    .execution_permission(ExecutionPermission::DaemonOnly)
    .context_policy(ContextPolicy::None)
    .phase_template_slug("")
    .job_input(JobInput::candidate_subject(
        seed_commit.clone(),
        candidate_head.clone(),
    ))
    .output_artifact_kind(OutputArtifactKind::ValidationReport)
    .build();
    h.db.create_job(&validation_job)
        .await
        .expect("create validation job");

    assert!(h.dispatcher.tick().await.expect("validation tick"));

    let job =
        h.db.get_job(validation_job.id)
            .await
            .expect("reload validation job");
    assert_eq!(job.state.status(), JobStatus::Completed);
    assert_eq!(job.state.outcome_class(), Some(OutcomeClass::Clean));
    assert_eq!(
        head_oid(Path::new(&workspace.path))
            .await
            .expect("workspace head"),
        candidate_head
    );
}

#[tokio::test]
async fn daemon_validation_resyncs_integration_workspace_before_running_harness() {
    let h = TestHarness::new(Arc::new(FakeRunner)).await;

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let seed_commit = head_oid(&h.repo_path).await.expect("seed head");

    let item = ItemBuilder::new(h.project.id, revision_id)
        .id(item_id)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .seed_commit_oid(Some(seed_commit.clone()))
        .seed_target_commit_oid(Some(seed_commit.clone()))
        .build();
    h.db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let paths = ensure_test_mirror(&h.state_root, &h.project).await;
    let workspace_id = ingot_domain::ids::WorkspaceId::new();
    let workspace_path = paths.worktree_root.join(workspace_id.to_string());
    let workspace_ref = format!("refs/ingot/workspaces/{workspace_id}");
    let (_prepared_path, integrated_head) = create_mirror_only_commit(
        paths.mirror_git_dir.as_path(),
        &seed_commit,
        &workspace_ref,
        "integrated change",
    )
    .await;
    let provisioned = provision_integration_workspace(
        paths.mirror_git_dir.as_path(),
        &workspace_path,
        &workspace_ref,
        &integrated_head,
    )
    .await
    .expect("provision integration workspace");
    let workspace = make_runtime_workspace(
        h.project.id,
        WorkspaceKind::Integration,
        workspace_id,
        provisioned.workspace_path.as_path(),
        revision_id,
        provisioned.workspace_ref,
        seed_commit.clone(),
        provisioned.head_commit_oid,
    );
    h.db.create_workspace(&workspace)
        .await
        .expect("create integration workspace");
    let source_workspace =
        create_authoring_validation_workspace(&h, revision_id, &seed_commit, &integrated_head)
            .await;
    let convergence = ConvergenceBuilder::new(h.project.id, item_id, revision_id)
        .source_workspace_id(source_workspace.id)
        .integration_workspace_id(workspace.id)
        .source_head_commit_oid(integrated_head.clone())
        .input_target_commit_oid(seed_commit.clone())
        .prepared_commit_oid(integrated_head.clone())
        .target_head_valid(true)
        .build();
    h.db.create_convergence(&convergence)
        .await
        .expect("create convergence");
    git_sync(Path::new(&workspace.path), &["checkout", &seed_commit]);
    write_harness_toml(
        &h.repo_path,
        &format!(
            r#"
[commands.head_matches]
run = "test \"$(git rev-parse HEAD)\" = \"{integrated_head}\""
timeout = "30s"
"#
        ),
    );

    let validation_job = JobBuilder::new(
        h.project.id,
        item_id,
        revision_id,
        step::VALIDATE_INTEGRATED,
    )
    .phase_kind(PhaseKind::Validate)
    .workspace_kind(WorkspaceKind::Integration)
    .execution_permission(ExecutionPermission::DaemonOnly)
    .context_policy(ContextPolicy::None)
    .phase_template_slug("")
    .job_input(JobInput::integrated_subject(
        seed_commit.clone(),
        integrated_head.clone(),
    ))
    .output_artifact_kind(OutputArtifactKind::ValidationReport)
    .build();
    h.db.create_job(&validation_job)
        .await
        .expect("create validation job");

    assert!(h.dispatcher.tick().await.expect("validation tick"));

    let job =
        h.db.get_job(validation_job.id)
            .await
            .expect("reload validation job");
    assert_eq!(job.state.status(), JobStatus::Completed);
    assert_eq!(job.state.outcome_class(), Some(OutcomeClass::Clean));
    assert_eq!(
        head_oid(Path::new(&workspace.path))
            .await
            .expect("workspace head"),
        integrated_head
    );
}

#[tokio::test]
async fn idle_item_auto_dispatches_candidate_review_after_nonblocking_incremental_triage() {
    let h = TestHarness::new(Arc::new(FakeRunner)).await;

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let seed_commit = head_oid(&h.repo_path).await.expect("seed head");
    std::fs::write(h.repo_path.join("feature.txt"), "authored change").expect("write feature");
    git_sync(&h.repo_path, &["add", "feature.txt"]);
    git_sync(&h.repo_path, &["commit", "-m", "author change"]);
    let authored_commit = head_oid(&h.repo_path).await.expect("authored head");

    let item = ItemBuilder::new(h.project.id, revision_id)
        .id(item_id)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .seed_commit_oid(Some(seed_commit.clone()))
        .seed_target_commit_oid(Some(seed_commit.clone()))
        .build();
    h.db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let created_at = default_timestamp();
    let author_job = JobBuilder::new(h.project.id, item_id, revision_id, step::AUTHOR_INITIAL)
        .status(JobStatus::Completed)
        .outcome_class(OutcomeClass::Clean)
        .phase_template_slug("author-initial")
        .job_input(JobInput::authoring_head(seed_commit.clone()))
        .output_artifact_kind(OutputArtifactKind::Commit)
        .output_commit_oid(authored_commit.clone())
        .created_at(created_at)
        .started_at(created_at)
        .ended_at(created_at)
        .build();
    h.db.create_job(&author_job)
        .await
        .expect("create author job");

    let mut review_payload = findings_review_report(
        &seed_commit,
        &authored_commit,
        "non-blocking note",
        "low",
        vec![serde_json::json!({
            "finding_key": "note",
            "code": "NOTE001",
            "severity": "low",
            "summary": "acceptable note",
            "paths": ["feature.txt"],
            "evidence": ["acceptable"]
        })],
    );
    review_payload
        .as_object_mut()
        .expect("review payload object")
        .insert("extensions".into(), serde_json::Value::Null);

    let review_job = JobBuilder::new(
        h.project.id,
        item_id,
        revision_id,
        step::REVIEW_INCREMENTAL_INITIAL,
    )
    .status(JobStatus::Completed)
    .outcome_class(OutcomeClass::Findings)
    .phase_kind(PhaseKind::Review)
    .workspace_kind(WorkspaceKind::Review)
    .execution_permission(ExecutionPermission::MustNotMutate)
    .phase_template_slug("review-incremental")
    .job_input(JobInput::candidate_subject(
        seed_commit.clone(),
        authored_commit.clone(),
    ))
    .output_artifact_kind(OutputArtifactKind::ReviewReport)
    .result_schema_version("review_report:v1")
    .result_payload(review_payload)
    .created_at(created_at)
    .started_at(created_at)
    .ended_at(created_at)
    .build();
    h.db.create_job(&review_job)
        .await
        .expect("create review job");

    h.db.create_finding(
        &FindingBuilder::new(h.project.id, item_id, revision_id, review_job.id)
            .source_step_id(step::REVIEW_INCREMENTAL_INITIAL)
            .source_finding_key("note")
            .source_subject_base_commit_oid(
                review_job
                    .job_input
                    .base_commit_oid()
                    .map(ToOwned::to_owned),
            )
            .source_subject_head_commit_oid(
                review_job
                    .job_input
                    .head_commit_oid()
                    .map(ToOwned::to_owned)
                    .expect("review head"),
            )
            .code("NOTE001")
            .severity(FindingSeverity::Low)
            .summary("acceptable note")
            .paths(vec!["feature.txt".into()])
            .evidence(serde_json::json!(["acceptable"]))
            .triage_state(FindingTriageState::WontFix)
            .triage_note("accepted for now")
            .created_at(created_at)
            .triaged_at(created_at)
            .build(),
    )
    .await
    .expect("create finding");

    assert!(
        h.dispatcher
            .tick()
            .await
            .expect("tick should recover review dispatch")
    );

    let jobs = h.db.list_jobs_by_item(item.id).await.expect("jobs");
    let candidate_review = jobs
        .iter()
        .find(|job| job.step_id == step::REVIEW_CANDIDATE_INITIAL)
        .expect("auto-dispatched candidate review");
    assert_eq!(candidate_review.state.status(), JobStatus::Queued);
    assert_eq!(
        candidate_review.job_input.base_commit_oid(),
        revision.seed.seed_commit_oid()
    );
    assert_eq!(
        candidate_review.job_input.head_commit_oid(),
        author_job.state.output_commit_oid()
    );
}
