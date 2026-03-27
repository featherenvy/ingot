mod common;

use ingot_domain::ids::{ActivityId, ItemId, ItemRevisionId};
use ingot_domain::job::{JobStatus, OutcomeClass};
use ingot_domain::ports::{
    ConflictKind, FinishJobNonSuccessParams, RepositoryError, RevisionLaneTeardownMutation,
    TeardownJobCancellation,
};
use ingot_domain::workspace::{WorkspaceKind, WorkspaceStatus};
use ingot_store_sqlite::PersistFixture;
use ingot_test_support::fixtures::{
    ItemBuilder, JobBuilder, ProjectBuilder, RevisionBuilder, WorkspaceBuilder,
};

#[tokio::test]
async fn apply_teardown_cancels_job_and_updates_workspace_atomically() {
    let db = common::migrated_test_db("teardown-atomic").await;

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
        .build();
    let (item, revision) = (item, revision)
        .persist(&db)
        .await
        .expect("create item with revision");

    let job_id = ingot_domain::ids::JobId::new();
    let workspace = WorkspaceBuilder::new(project.id, WorkspaceKind::Authoring)
        .created_for_revision_id(revision.id)
        .status(WorkspaceStatus::Busy)
        .current_job_id(job_id)
        .build()
        .persist(&db)
        .await
        .expect("create workspace");

    let job = JobBuilder::new(project.id, item.id, revision.id, "author_initial")
        .id(job_id)
        .status(JobStatus::Running)
        .workspace_id(workspace.id)
        .build()
        .persist(&db)
        .await
        .expect("create job");

    let mut released_workspace = db.get_workspace(workspace.id).await.expect("get workspace");
    released_workspace.release_to(WorkspaceStatus::Ready, chrono::Utc::now());

    let activity = ingot_domain::activity::Activity {
        id: ActivityId::new(),
        project_id: project.id,
        event_type: ingot_domain::activity::ActivityEventType::JobCancelled,
        subject: ingot_domain::activity::ActivitySubject::Job(job.id),
        payload: serde_json::json!({ "item_id": item.id }),
        created_at: chrono::Utc::now(),
    };

    let mutation = RevisionLaneTeardownMutation {
        job_cancellations: vec![TeardownJobCancellation {
            params: FinishJobNonSuccessParams {
                job_id: job.id,
                item_id: item.id,
                expected_item_revision_id: revision.id,
                status: JobStatus::Cancelled,
                outcome_class: Some(OutcomeClass::Cancelled),
                error_code: Some("item_mutation_cancelled".into()),
                error_message: None,
                escalation_reason: None,
            },
            workspace_update: Some(released_workspace),
            activity,
        }],
        ..Default::default()
    };

    db.apply_revision_lane_teardown(mutation)
        .await
        .expect("apply teardown");

    let persisted_job = db.get_job(job.id).await.expect("load job");
    assert_eq!(persisted_job.state.status(), JobStatus::Cancelled);

    let persisted_workspace = db
        .get_workspace(workspace.id)
        .await
        .expect("load workspace");
    assert_eq!(persisted_workspace.state.status(), WorkspaceStatus::Ready);
    assert_eq!(persisted_workspace.state.current_job_id(), None);
}

#[tokio::test]
async fn apply_teardown_rolls_back_on_stale_revision() {
    let db = common::migrated_test_db("teardown-rollback").await;

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
        .build();
    next_revision.supersedes_revision_id = Some(revision.id);

    // Item points to next_revision but job points to original revision
    let item = ItemBuilder::new(project.id, next_revision.id)
        .id(item_id)
        .build();
    let (item, revision) = (item, revision)
        .persist(&db)
        .await
        .expect("create item with source revision");
    let _next_revision = next_revision
        .persist(&db)
        .await
        .expect("create next revision");

    let job_id = ingot_domain::ids::JobId::new();
    let workspace = WorkspaceBuilder::new(project.id, WorkspaceKind::Authoring)
        .created_for_revision_id(revision.id)
        .status(WorkspaceStatus::Busy)
        .current_job_id(job_id)
        .build()
        .persist(&db)
        .await
        .expect("create workspace");

    let job = JobBuilder::new(project.id, item.id, revision.id, "author_initial")
        .id(job_id)
        .status(JobStatus::Running)
        .workspace_id(workspace.id)
        .build()
        .persist(&db)
        .await
        .expect("create job");

    let mut released_workspace = db.get_workspace(workspace.id).await.expect("get workspace");
    released_workspace.release_to(WorkspaceStatus::Ready, chrono::Utc::now());

    let mutation = RevisionLaneTeardownMutation {
        job_cancellations: vec![TeardownJobCancellation {
            params: FinishJobNonSuccessParams {
                job_id: job.id,
                item_id: item.id,
                expected_item_revision_id: revision.id, // stale!
                status: JobStatus::Cancelled,
                outcome_class: Some(OutcomeClass::Cancelled),
                error_code: Some("item_mutation_cancelled".into()),
                error_message: None,
                escalation_reason: None,
            },
            workspace_update: Some(released_workspace),
            activity: ingot_domain::activity::Activity {
                id: ActivityId::new(),
                project_id: project.id,
                event_type: ingot_domain::activity::ActivityEventType::JobCancelled,
                subject: ingot_domain::activity::ActivitySubject::Job(job.id),
                payload: serde_json::json!({}),
                created_at: chrono::Utc::now(),
            },
        }],
        ..Default::default()
    };

    let error = db
        .apply_revision_lane_teardown(mutation)
        .await
        .expect_err("stale revision should fail");

    assert!(matches!(
        error,
        RepositoryError::Conflict(ConflictKind::JobRevisionStale)
    ));

    // Verify rollback: workspace should still be Busy
    let persisted_workspace = db
        .get_workspace(workspace.id)
        .await
        .expect("load workspace");
    assert_eq!(persisted_workspace.state.status(), WorkspaceStatus::Busy);

    // Job should still be Running
    let persisted_job = db.get_job(job.id).await.expect("load job");
    assert_eq!(persisted_job.state.status(), JobStatus::Running);
}
