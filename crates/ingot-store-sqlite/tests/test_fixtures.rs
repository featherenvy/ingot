mod common;

use ingot_domain::job::JobStatus;
use ingot_domain::workspace::WorkspaceStatus;
use ingot_store_sqlite::PersistFixture;
use ingot_test_support::fixtures::{ItemBuilder, JobBuilder, ProjectBuilder, RevisionBuilder};

#[tokio::test]
async fn persisting_terminal_job_does_not_create_busy_current_workspace() {
    let db = common::migrated_test_db("ingot-store-fixture").await;

    let project = ProjectBuilder::new("/tmp/test")
        .name("Test")
        .build()
        .persist(&db)
        .await
        .expect("create project");
    let revision = RevisionBuilder::new(ingot_domain::ids::ItemId::new()).build();
    let item = ItemBuilder::new(project.id, revision.id)
        .id(revision.item_id)
        .build();
    let (item, revision) = (item, revision)
        .persist(&db)
        .await
        .expect("create item with revision");

    let workspace_id = ingot_domain::ids::WorkspaceId::new();
    let job = JobBuilder::new(project.id, item.id, revision.id, "author_initial")
        .workspace_id(workspace_id)
        .status(JobStatus::Completed)
        .build()
        .persist(&db)
        .await
        .expect("persist terminal job");

    let workspace = db
        .get_workspace(workspace_id)
        .await
        .expect("auto-created workspace");
    assert_eq!(job.state.workspace_id(), Some(workspace.id));
    assert_eq!(workspace.state.status(), WorkspaceStatus::Ready);
    assert_eq!(workspace.state.current_job_id(), None);
}
