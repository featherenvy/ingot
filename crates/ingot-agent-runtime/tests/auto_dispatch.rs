use std::sync::Arc;

use ingot_agent_runtime::RuntimeError;
use ingot_domain::job::{
    ContextPolicy, ExecutionPermission, JobInput, JobStatus, OutcomeClass, OutputArtifactKind,
    PhaseKind,
};
use ingot_domain::workspace::WorkspaceKind;
use ingot_git::commands::head_oid;
use ingot_test_support::git::unique_temp_path;
use ingot_test_support::reports::{clean_review_report, findings_review_report};

mod common;
use common::*;
use ingot_domain::finding::{FindingSeverity, FindingTriageState};
use ingot_git::commands::git;
use ingot_usecases::job::{DispatchJobCommand, dispatch_job};
use ingot_workflow::step;

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
    assert_eq!(completed_author.status, JobStatus::Completed);
    assert_eq!(completed_author.outcome_class, Some(OutcomeClass::Clean));

    let review_job = jobs
        .iter()
        .find(|job| job.step_id == step::REVIEW_INCREMENTAL_INITIAL)
        .expect("auto-dispatched incremental review job");
    assert_eq!(review_job.status, JobStatus::Queued);
    assert_eq!(
        review_job.job_input.base_commit_oid(),
        Some(seed_commit.as_str())
    );
    assert_eq!(
        review_job.job_input.head_commit_oid(),
        completed_author.output_commit_oid.as_deref()
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
    assert_eq!(review_job.status, JobStatus::Queued);
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
    assert_eq!(busy_completed_author.status, JobStatus::Completed);
    assert!(
        busy_jobs
            .iter()
            .any(|job| job.step_id == step::REVIEW_INCREMENTAL_INITIAL
                && job.status == JobStatus::Queued)
    );

    let idle_jobs =
        h.db.list_jobs_by_item(idle_item_id)
            .await
            .expect("idle jobs");
    let idle_candidate_review = idle_jobs
        .iter()
        .find(|job| job.step_id == step::REVIEW_CANDIDATE_INITIAL)
        .expect("recovered idle candidate review");
    assert_eq!(idle_candidate_review.status, JobStatus::Queued);
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
    assert_eq!(completed_review.status, JobStatus::Completed);
    assert_eq!(completed_review.outcome_class, Some(OutcomeClass::Clean));

    let candidate_review = jobs
        .iter()
        .find(|job| job.step_id == step::REVIEW_CANDIDATE_INITIAL)
        .expect("auto-dispatched candidate review");
    assert_eq!(candidate_review.status, JobStatus::Queued);
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
    assert_eq!(candidate_review.status, JobStatus::Queued);
    assert_eq!(
        candidate_review.job_input.base_commit_oid(),
        revision.seed_commit_oid.as_deref()
    );
    assert_eq!(
        candidate_review.job_input.head_commit_oid(),
        author_job.output_commit_oid.as_deref()
    );
}
