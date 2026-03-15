use std::process::Command;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use ingot_domain::ids::ProjectId;
use ingot_git::project_repo::{ensure_mirror, project_repo_paths};
use ingot_http_api::build_router_with_project_locks_and_state_root;
use ingot_store_sqlite::Database;
use ingot_usecases::ProjectLocks;
use tower::ServiceExt;
use uuid::Uuid;

mod common;
use common::*;

#[tokio::test]
async fn reset_workspace_route_restores_authoring_workspace_head() {
    let repo = temp_git_repo("ingot-http-api");
    let base_commit_oid = git_output(&repo, &["rev-parse", "HEAD"]);
    let workspace_path =
        std::env::temp_dir().join(format!("ingot-http-api-workspace-{}", Uuid::now_v7()));
    git(
        &repo,
        &[
            "update-ref",
            "refs/ingot/workspaces/wrk_reset_test",
            &base_commit_oid,
        ],
    );
    git(
        &repo,
        &[
            "worktree",
            "add",
            "--detach",
            workspace_path.to_str().expect("workspace path"),
            "refs/ingot/workspaces/wrk_reset_test",
        ],
    );
    write_file(&workspace_path.join("tracked.txt"), "changed");

    let db_path = std::env::temp_dir().join(format!("ingot-http-api-db-{}.db", Uuid::now_v7()));
    let db = Database::connect(&db_path).await.expect("connect db");
    db.migrate().await.expect("migrate db");
    let project_id = "prj_00000000000000000000000000000044".to_string();
    let workspace_id = "wrk_00000000000000000000000000000044".to_string();

    sqlx::query(
        "INSERT INTO projects (id, name, path, default_branch, color, created_at, updated_at)
         VALUES (?, 'Test', ?, 'main', '#000', ?, ?)",
    )
    .bind(&project_id)
    .bind(repo.display().to_string())
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert project");

    sqlx::query(
        "INSERT INTO workspaces (
            id, project_id, kind, strategy, path, created_for_revision_id, parent_workspace_id,
            target_ref, workspace_ref, base_commit_oid, head_commit_oid, retention_policy,
            status, current_job_id, created_at, updated_at
         ) VALUES (?, ?, 'authoring', 'worktree', ?, NULL, NULL, 'refs/heads/main', 'refs/ingot/workspaces/wrk_reset_test', ?, ?, 'persistent', 'ready', NULL, ?, ?)",
    )
    .bind(&workspace_id)
    .bind(&project_id)
    .bind(workspace_path.display().to_string())
    .bind(&base_commit_oid)
    .bind(&base_commit_oid)
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert workspace");

    let app = test_router(db.clone());
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/projects/{project_id}/workspaces/{workspace_id}/reset"
                ))
                .method("POST")
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("route response");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        git_output(&workspace_path, &["rev-parse", "HEAD"]),
        base_commit_oid
    );
    assert_eq!(
        std::fs::read_to_string(workspace_path.join("tracked.txt")).expect("tracked file"),
        "initial"
    );
}

#[tokio::test]
async fn remove_workspace_route_deletes_abandoned_workspace_ref_and_path() {
    let repo = temp_git_repo("ingot-http-api");
    let head_commit_oid = git_output(&repo, &["rev-parse", "HEAD"]);
    let workspace_path = std::env::temp_dir().join(format!(
        "ingot-http-api-remove-workspace-{}",
        Uuid::now_v7()
    ));

    let db_path = std::env::temp_dir().join(format!("ingot-http-api-db-{}.db", Uuid::now_v7()));
    let db = Database::connect(&db_path).await.expect("connect db");
    db.migrate().await.expect("migrate db");
    let project_id = "prj_00000000000000000000000000000043".to_string();
    let workspace_id = "wrk_00000000000000000000000000000043".to_string();
    let project_uuid = project_id.parse::<ProjectId>().expect("parse project id");
    let state_root =
        std::env::temp_dir().join(format!("ingot-http-api-remove-state-{}", Uuid::now_v7()));
    let paths = project_repo_paths(state_root.as_path(), project_uuid, &repo);
    ensure_mirror(&paths).await.expect("ensure mirror");
    git(
        &paths.mirror_git_dir,
        &[
            "update-ref",
            "refs/ingot/workspaces/wrk_remove_test",
            &head_commit_oid,
        ],
    );
    git(
        &paths.mirror_git_dir,
        &[
            "worktree",
            "add",
            "--detach",
            workspace_path.to_str().expect("workspace path"),
            "refs/ingot/workspaces/wrk_remove_test",
        ],
    );

    sqlx::query(
        "INSERT INTO projects (id, name, path, default_branch, color, created_at, updated_at)
         VALUES (?, 'Test', ?, 'main', '#000', ?, ?)",
    )
    .bind(&project_id)
    .bind(repo.display().to_string())
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert project");

    sqlx::query(
        "INSERT INTO workspaces (
            id, project_id, kind, strategy, path, created_for_revision_id, parent_workspace_id,
            target_ref, workspace_ref, base_commit_oid, head_commit_oid, retention_policy,
            status, current_job_id, created_at, updated_at
         ) VALUES (?, ?, 'review', 'worktree', ?, NULL, NULL, NULL, 'refs/ingot/workspaces/wrk_remove_test', ?, ?, 'ephemeral', 'abandoned', NULL, ?, ?)",
    )
    .bind(&workspace_id)
    .bind(&project_id)
    .bind(workspace_path.display().to_string())
    .bind(&head_commit_oid)
    .bind(&head_commit_oid)
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert workspace");

    let app = build_router_with_project_locks_and_state_root(
        db.clone(),
        ProjectLocks::default(),
        state_root,
    );
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/projects/{project_id}/workspaces/{workspace_id}/remove"
                ))
                .method("POST")
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("route response");

    assert_eq!(response.status(), StatusCode::OK);
    assert!(!workspace_path.exists());
    let ref_exists = Command::new("git")
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            "refs/ingot/workspaces/wrk_remove_test",
        ])
        .current_dir(paths.mirror_git_dir)
        .status()
        .expect("check ref");
    assert!(!ref_exists.success());
}
