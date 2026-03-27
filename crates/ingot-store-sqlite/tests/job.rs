mod common;

use chrono::Utc;
use ingot_domain::agent::AgentCapability;
use ingot_domain::ids::{ItemId, ItemRevisionId};
use ingot_domain::item::EscalationReason;
use ingot_domain::job::{
    ContextPolicy, ExecutionPermission, JobAssignment, JobStatus, OutcomeClass, OutputArtifactKind,
    PhaseKind,
};
use ingot_domain::lease_owner_id::LeaseOwnerId;
use ingot_domain::ports::{ConflictKind, RepositoryError};
use ingot_domain::workspace::WorkspaceKind;
use ingot_store_sqlite::{
    ClaimQueuedAgentJobExecutionParams, FinishJobNonSuccessParams, PersistFixture,
    StartJobExecutionParams,
};
use ingot_test_support::fixtures::{
    AgentBuilder, ItemBuilder, JobBuilder, ProjectBuilder, RevisionBuilder, WorkspaceBuilder,
    parse_timestamp,
};

#[tokio::test]
async fn finish_job_non_success_rolls_back_when_item_revision_changes_before_commit() {
    let db = common::migrated_test_db("ingot-store").await;

    let project = ProjectBuilder::new("/tmp/test")
        .name("Test")
        .build()
        .persist(&db)
        .await
        .expect("create project");

    let item_id = ItemId::new();
    let revision = RevisionBuilder::new(item_id)
        .seed_commit_oid(Some("abc"))
        .seed_target_commit_oid(Some("def"))
        .build();
    let mut next_revision = RevisionBuilder::new(item_id)
        .id(ItemRevisionId::new())
        .revision_no(2)
        .seed_commit_oid(Some("ghi"))
        .seed_target_commit_oid(Some("jkl"))
        .created_at(parse_timestamp("2026-03-13T00:00:00Z"))
        .build();
    next_revision.supersedes_revision_id = Some(revision.id);

    let item = ItemBuilder::new(project.id, next_revision.id)
        .id(item_id)
        .build();
    let (item, revision) = (item, revision)
        .persist(&db)
        .await
        .expect("create item with source revision");
    let next_revision = next_revision
        .persist(&db)
        .await
        .expect("create next revision");

    let job = JobBuilder::new(project.id, item.id, revision.id, "repair_candidate")
        .status(JobStatus::Running)
        .phase_kind(PhaseKind::Author)
        .workspace_kind(WorkspaceKind::Authoring)
        .execution_permission(ExecutionPermission::MayMutate)
        .context_policy(ContextPolicy::ResumeContext)
        .phase_template_slug("repair-candidate")
        .output_artifact_kind(OutputArtifactKind::Commit)
        .build()
        .persist(&db)
        .await
        .expect("create job");

    let error = db
        .finish_job_non_success(FinishJobNonSuccessParams {
            job_id: job.id,
            item_id: item.id,
            expected_item_revision_id: revision.id,
            status: JobStatus::Failed,
            outcome_class: Some(OutcomeClass::TerminalFailure),
            error_code: Some("worker_failed".into()),
            error_message: Some("boom".into()),
            escalation_reason: Some(EscalationReason::StepFailed),
        })
        .await
        .expect_err("revision drift should fail");

    assert!(matches!(
        error,
        RepositoryError::Conflict(ConflictKind::JobRevisionStale)
    ));

    let persisted_job = db.get_job(job.id).await.expect("load job after rollback");
    let persisted_item = db
        .get_item(item.id)
        .await
        .expect("load item after rollback");

    assert_eq!(next_revision.id, persisted_item.current_revision_id);
    assert_eq!(persisted_job.state.status(), JobStatus::Running);
    assert!(!persisted_item.escalation.is_escalated());
}

#[tokio::test]
async fn start_job_execution_rejects_jobs_without_workspace_binding() {
    let db = common::migrated_test_db("ingot-store").await;

    let project = ProjectBuilder::new("/tmp/test")
        .name("Test")
        .build()
        .persist(&db)
        .await
        .expect("create project");

    let item_id = ItemId::new();
    let revision = RevisionBuilder::new(item_id)
        .seed_commit_oid(Some("abc"))
        .seed_target_commit_oid(Some("def"))
        .build();
    let item = ItemBuilder::new(project.id, revision.id)
        .id(item_id)
        .build();
    let (item, revision) = (item, revision)
        .persist(&db)
        .await
        .expect("create item with revision");

    let job = JobBuilder::new(project.id, item.id, revision.id, "author_initial")
        .status(JobStatus::Queued)
        .phase_kind(PhaseKind::Author)
        .workspace_kind(WorkspaceKind::Authoring)
        .execution_permission(ExecutionPermission::MayMutate)
        .context_policy(ContextPolicy::Fresh)
        .phase_template_slug("author-initial")
        .output_artifact_kind(OutputArtifactKind::Commit)
        .build()
        .persist(&db)
        .await
        .expect("create queued job");

    let error = db
        .start_job_execution(StartJobExecutionParams {
            job_id: job.id,
            item_id: item.id,
            expected_item_revision_id: revision.id,
            workspace_id: None,
            agent_id: None,
            lease_owner_id: "ingotd:test".into(),
            process_pid: Some(1234),
            lease_expires_at: Utc::now() + chrono::Duration::seconds(60),
        })
        .await
        .expect_err("missing workspace binding should fail");

    assert!(matches!(
        error,
        RepositoryError::Conflict(ConflictKind::JobMissingWorkspace)
    ));

    let persisted_job = db.get_job(job.id).await.expect("job remains readable");
    assert_eq!(persisted_job.state.status(), JobStatus::Queued);
    assert_eq!(persisted_job.state.workspace_id(), None);
}

#[tokio::test]
async fn claim_queued_agent_job_execution_persists_assignment_and_running_lease() {
    let db = common::migrated_test_db("ingot-store").await;

    let project = ProjectBuilder::new("/tmp/test")
        .name("Test")
        .build()
        .persist(&db)
        .await
        .expect("create project");

    let item_id = ItemId::new();
    let revision = RevisionBuilder::new(item_id)
        .seed_commit_oid(Some("abc"))
        .seed_target_commit_oid(Some("def"))
        .build();
    let item = ItemBuilder::new(project.id, revision.id)
        .id(item_id)
        .build();
    let (item, revision) = (item, revision)
        .persist(&db)
        .await
        .expect("create item with revision");

    let job = JobBuilder::new(project.id, item.id, revision.id, "author_initial")
        .status(JobStatus::Queued)
        .phase_kind(PhaseKind::Author)
        .workspace_kind(WorkspaceKind::Authoring)
        .execution_permission(ExecutionPermission::MayMutate)
        .context_policy(ContextPolicy::Fresh)
        .phase_template_slug("author-initial")
        .output_artifact_kind(OutputArtifactKind::Commit)
        .build()
        .persist(&db)
        .await
        .expect("create queued job");

    let workspace = WorkspaceBuilder::new(project.id, WorkspaceKind::Authoring)
        .created_for_revision_id(revision.id)
        .status(ingot_domain::workspace::WorkspaceStatus::Ready)
        .base_commit_oid("abc")
        .head_commit_oid("abc")
        .build();
    db.create_workspace(&workspace)
        .await
        .expect("create workspace");
    let agent = AgentBuilder::new("codex", vec![AgentCapability::MutatingJobs]).build();
    db.create_agent(&agent).await.expect("create agent");
    let lease_expires_at = Utc::now() + chrono::Duration::seconds(60);
    db.claim_queued_agent_job_execution(ClaimQueuedAgentJobExecutionParams {
        job_id: job.id,
        item_id: item.id,
        expected_item_revision_id: revision.id,
        assignment: JobAssignment::new(workspace.id)
            .with_agent(agent.id)
            .with_prompt_snapshot("prompt body")
            .with_phase_template_digest("template-digest"),
        lease_owner_id: "ingotd:test".into(),
        lease_expires_at,
    })
    .await
    .expect("claim queued job");

    let persisted_job = db.get_job(job.id).await.expect("load claimed job");
    assert_eq!(persisted_job.state.status(), JobStatus::Running);
    assert_eq!(persisted_job.state.workspace_id(), Some(workspace.id));
    assert_eq!(persisted_job.state.agent_id(), Some(agent.id));
    assert_eq!(persisted_job.state.prompt_snapshot(), Some("prompt body"));
    assert_eq!(
        persisted_job.state.phase_template_digest(),
        Some("template-digest")
    );
    assert_eq!(
        persisted_job.state.lease_owner_id(),
        Some(&LeaseOwnerId::new("ingotd:test"))
    );
    assert!(persisted_job.state.heartbeat_at().is_some());
    assert_eq!(
        persisted_job.state.lease_expires_at(),
        Some(lease_expires_at)
    );
    assert!(persisted_job.state.started_at().is_some());
}

#[tokio::test]
async fn claim_queued_agent_job_execution_rejects_rows_that_left_queued() {
    let db = common::migrated_test_db("ingot-store").await;

    let project = ProjectBuilder::new("/tmp/test")
        .name("Test")
        .build()
        .persist(&db)
        .await
        .expect("create project");

    let item_id = ItemId::new();
    let revision = RevisionBuilder::new(item_id)
        .seed_commit_oid(Some("abc"))
        .seed_target_commit_oid(Some("def"))
        .build();
    let item = ItemBuilder::new(project.id, revision.id)
        .id(item_id)
        .build();
    let (item, revision) = (item, revision)
        .persist(&db)
        .await
        .expect("create item with revision");

    let workspace_id = ingot_domain::ids::WorkspaceId::new();
    let job = JobBuilder::new(project.id, item.id, revision.id, "author_initial")
        .status(JobStatus::Assigned)
        .phase_kind(PhaseKind::Author)
        .workspace_kind(WorkspaceKind::Authoring)
        .execution_permission(ExecutionPermission::MayMutate)
        .context_policy(ContextPolicy::Fresh)
        .phase_template_slug("author-initial")
        .output_artifact_kind(OutputArtifactKind::Commit)
        .workspace_id(workspace_id)
        .build()
        .persist(&db)
        .await
        .expect("create assigned job");

    let error = db
        .claim_queued_agent_job_execution(ClaimQueuedAgentJobExecutionParams {
            job_id: job.id,
            item_id: item.id,
            expected_item_revision_id: revision.id,
            assignment: JobAssignment::new(workspace_id)
                .with_prompt_snapshot("prompt body")
                .with_phase_template_digest("template-digest"),
            lease_owner_id: "ingotd:test".into(),
            lease_expires_at: Utc::now() + chrono::Duration::seconds(60),
        })
        .await
        .expect_err("non-queued job should fail");

    assert!(matches!(
        error,
        RepositoryError::Conflict(ConflictKind::JobNotActive)
    ));

    let persisted_job = db.get_job(job.id).await.expect("load unchanged job");
    assert_eq!(persisted_job.state.status(), JobStatus::Assigned);
    assert_eq!(persisted_job.state.workspace_id(), Some(workspace_id));
}
