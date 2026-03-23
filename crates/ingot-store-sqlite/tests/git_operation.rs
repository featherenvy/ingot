mod common;

use ingot_domain::git_operation::{GitOperationEntityRef, GitOperationStatus, OperationKind};
use ingot_domain::ids::ConvergenceId;
use ingot_domain::ports::RepositoryError;
use ingot_test_support::fixtures::{GitOperationBuilder, ProjectBuilder};
use ingot_test_support::git::unique_temp_path;

#[tokio::test]
async fn find_unresolved_finalize_for_convergence_returns_matching_operation() {
    let db = common::migrated_test_db("ingot-store-git-op").await;

    let project = ProjectBuilder::new(unique_temp_path("ingot-store-project")).build();
    db.create_project(&project).await.expect("create project");

    let convergence_id = ConvergenceId::new();
    let operation = GitOperationBuilder::new(
        project.id,
        OperationKind::FinalizeTargetRef,
        GitOperationEntityRef::Convergence(convergence_id),
    )
    .ref_name("refs/heads/main")
    .expected_old_oid("base")
    .new_oid("prepared")
    .commit_oid("prepared")
    .status(GitOperationStatus::Planned)
    .build();
    db.create_git_operation(&operation)
        .await
        .expect("create git operation");

    let found = db
        .find_unresolved_finalize_for_convergence(convergence_id)
        .await
        .expect("find unresolved finalize")
        .expect("matching operation");
    assert_eq!(found.id, operation.id);
}

#[tokio::test]
async fn unique_index_rejects_second_unresolved_finalize_for_same_convergence() {
    let db = common::migrated_test_db("ingot-store-git-op-unique").await;

    let project = ProjectBuilder::new(unique_temp_path("ingot-store-project")).build();
    db.create_project(&project).await.expect("create project");

    let convergence_id = ConvergenceId::new();
    let first = GitOperationBuilder::new(
        project.id,
        OperationKind::FinalizeTargetRef,
        GitOperationEntityRef::Convergence(convergence_id),
    )
    .ref_name("refs/heads/main")
    .expected_old_oid("base")
    .new_oid("prepared")
    .commit_oid("prepared")
    .status(GitOperationStatus::Planned)
    .build();
    db.create_git_operation(&first)
        .await
        .expect("create first operation");

    let second = GitOperationBuilder::new(
        project.id,
        OperationKind::FinalizeTargetRef,
        GitOperationEntityRef::Convergence(convergence_id),
    )
    .ref_name("refs/heads/main")
    .expected_old_oid("base")
    .new_oid("prepared")
    .commit_oid("prepared")
    .status(GitOperationStatus::Applied)
    .build();
    let error = db
        .create_git_operation(&second)
        .await
        .expect_err("second unresolved finalize must conflict");
    assert!(matches!(error, RepositoryError::Conflict(_)));
}
