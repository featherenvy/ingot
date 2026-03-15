use std::path::PathBuf;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use ingot_domain::ids::ProjectId;
use ingot_domain::job::{ContextPolicy, ExecutionPermission, JobStatus, OutcomeClass, OutputArtifactKind, PhaseKind};
use ingot_domain::workspace::WorkspaceKind;
use ingot_git::commands::resolve_ref_oid;
use ingot_git::project_repo::{ensure_mirror, project_repo_paths};
use ingot_http_api::build_router_with_project_locks_and_state_root;
use ingot_store_sqlite::Database;
use ingot_usecases::ProjectLocks;
use tower::ServiceExt;
use uuid::Uuid;

mod common;
use common::*;

#[tokio::test]
async fn dispatch_item_job_route_creates_queued_author_initial_job_and_workspace() {
    let repo = temp_git_repo("ingot-http-api");
    let seed_head = git_output(&repo, &["rev-parse", "HEAD"]);
    let db_path = std::env::temp_dir().join(format!("ingot-http-api-db-{}.db", Uuid::now_v7()));
    let db = Database::connect(&db_path).await.expect("connect db");
    db.migrate().await.expect("migrate db");

    let project_id = "prj_00000000000000000000000000000031".to_string();
    let item_id = "itm_00000000000000000000000000000031".to_string();
    let revision_id = "rev_00000000000000000000000000000031".to_string();

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
        "INSERT INTO items (
            id, project_id, classification, workflow_version, lifecycle_state, parking_state,
            approval_state, escalation_state, current_revision_id, origin_kind, origin_finding_id,
            priority, labels, created_at, updated_at
         ) VALUES (?, ?, 'change', 'delivery:v1', 'open', 'active', 'not_requested', 'none', ?, 'manual', NULL, 'major', '[]', ?, ?)",
    )
    .bind(&item_id)
    .bind(&project_id)
    .bind(&revision_id)
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert item");

    sqlx::query(
        "INSERT INTO item_revisions (
            id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
            approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid,
            seed_target_commit_oid, supersedes_revision_id, created_at
         ) VALUES (?, ?, 1, 'Title', 'Desc', 'AC', 'refs/heads/main', 'required', '{}', '{\"author_initial\":\"author-initial\"}', ?, ?, NULL, ?)",
    )
    .bind(&revision_id)
    .bind(&item_id)
    .bind(&seed_head)
    .bind(&seed_head)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert revision");

    let app = test_router(db.clone());
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/projects/{project_id}/items/{item_id}/jobs"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .expect("build dispatch request"),
        )
        .await
        .expect("dispatch route response");

    assert_eq!(response.status(), StatusCode::CREATED);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("dispatch json");

    assert_eq!(json["step_id"].as_str(), Some("author_initial"));
    assert_eq!(json["status"].as_str(), Some("queued"));
    assert_eq!(json["phase_template_slug"].as_str(), Some("author-initial"));
    assert_eq!(json["job_input"]["kind"].as_str(), Some("authoring_head"));
    assert_eq!(
        json["job_input"]["head_commit_oid"].as_str(),
        Some(seed_head.as_str())
    );
    let workspace_id = json["workspace_id"]
        .as_str()
        .expect("workspace id assigned on dispatch");

    let detail_response = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/projects/{project_id}/items/{item_id}"))
                .body(Body::empty())
                .expect("build detail request"),
        )
        .await
        .expect("detail response");

    let detail_body = to_bytes(detail_response.into_body(), usize::MAX)
        .await
        .expect("read detail body");
    let detail_json: serde_json::Value =
        serde_json::from_slice(&detail_body).expect("detail json");

    assert_eq!(
        detail_json["evaluation"]["current_step_id"].as_str(),
        Some("author_initial")
    );
    assert_eq!(
        detail_json["evaluation"]["phase_status"].as_str(),
        Some("running")
    );
    assert_eq!(detail_json["workspaces"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        detail_json["workspaces"][0]["id"].as_str(),
        Some(workspace_id)
    );
    assert_eq!(
        detail_json["workspaces"][0]["kind"].as_str(),
        Some("authoring")
    );
    assert_eq!(
        detail_json["workspaces"][0]["status"].as_str(),
        Some("ready")
    );
    assert_eq!(
        detail_json["workspaces"][0]["head_commit_oid"].as_str(),
        Some(seed_head.as_str())
    );
    let workspace_path = detail_json["workspaces"][0]["path"]
        .as_str()
        .expect("workspace path");
    assert!(PathBuf::from(workspace_path).exists());
    assert_eq!(
        git_output(&PathBuf::from(workspace_path), &["rev-parse", "HEAD"]),
        seed_head
    );
}

#[tokio::test]
async fn dispatch_item_job_route_binds_implicit_author_initial_from_target_head() {
    let repo = temp_git_repo("ingot-http-api");
    let target_head = git_output(&repo, &["rev-parse", "HEAD"]);
    let db_path = std::env::temp_dir().join(format!("ingot-http-api-db-{}.db", Uuid::now_v7()));
    let db = Database::connect(&db_path).await.expect("connect db");
    db.migrate().await.expect("migrate db");

    let project_id = "prj_00000000000000000000000000000091".to_string();
    let item_id = "itm_00000000000000000000000000000091".to_string();
    let revision_id = "rev_00000000000000000000000000000091".to_string();
    let project_uuid = project_id.parse::<ProjectId>().expect("parse project id");
    let state_root =
        std::env::temp_dir().join(format!("ingot-http-api-state-{}", Uuid::now_v7()));

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
        "INSERT INTO items (
            id, project_id, classification, workflow_version, lifecycle_state, parking_state,
            approval_state, escalation_state, current_revision_id, origin_kind, origin_finding_id,
            priority, labels, created_at, updated_at
         ) VALUES (?, ?, 'change', 'delivery:v1', 'open', 'active', 'not_requested', 'none', ?, 'manual', NULL, 'major', '[]', ?, ?)",
    )
    .bind(&item_id)
    .bind(&project_id)
    .bind(&revision_id)
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert item");

    sqlx::query(
        "INSERT INTO item_revisions (
            id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
            approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid,
            seed_target_commit_oid, supersedes_revision_id, created_at
         ) VALUES (?, ?, 1, 'Title', 'Desc', 'AC', 'refs/heads/main', 'required', '{}', '{\"author_initial\":\"author-initial\"}', ?, ?, NULL, ?)",
    )
    .bind(&revision_id)
    .bind(&item_id)
    .bind(Option::<String>::None)
    .bind(&target_head)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert revision");

    let app = build_router_with_project_locks_and_state_root(
        db.clone(),
        ProjectLocks::default(),
        state_root.clone(),
    );
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/projects/{project_id}/items/{item_id}/jobs"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .expect("build dispatch request"),
        )
        .await
        .expect("dispatch route response");

    assert_eq!(response.status(), StatusCode::CREATED);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("dispatch json");
    let workspace_id = json["workspace_id"].as_str().expect("workspace id");

    assert_eq!(json["step_id"].as_str(), Some("author_initial"));
    assert_eq!(json["job_input"]["kind"].as_str(), Some("authoring_head"));
    assert_eq!(
        json["job_input"]["head_commit_oid"].as_str(),
        Some(target_head.as_str())
    );

    let paths = project_repo_paths(state_root.as_path(), project_uuid, &repo);
    ensure_mirror(&paths).await.expect("ensure mirror");
    let detail_response = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/projects/{project_id}/items/{item_id}"))
                .body(Body::empty())
                .expect("build detail request"),
        )
        .await
        .expect("detail response");
    let detail_body = to_bytes(detail_response.into_body(), usize::MAX)
        .await
        .expect("detail body");
    let detail_json: serde_json::Value =
        serde_json::from_slice(&detail_body).expect("detail json");
    assert_eq!(
        detail_json["workspaces"][0]["id"].as_str(),
        Some(workspace_id)
    );
    assert_eq!(
        detail_json["workspaces"][0]["base_commit_oid"].as_str(),
        Some(target_head.as_str())
    );
    assert_eq!(
        detail_json["workspaces"][0]["head_commit_oid"].as_str(),
        Some(target_head.as_str())
    );
}

#[tokio::test]
async fn resume_route_implicit_revision_queues_incremental_review_from_bound_workspace_base() {
    let repo = temp_git_repo("ingot-http-api");
    let bound_base = git_output(&repo, &["rev-parse", "HEAD"]);
    write_file(&repo.join("tracked.txt"), "authored change");
    git(&repo, &["add", "tracked.txt"]);
    git(&repo, &["commit", "-m", "authored change"]);
    let authored_head = git_output(&repo, &["rev-parse", "HEAD"]);

    let db_path = std::env::temp_dir().join(format!("ingot-http-api-db-{}.db", Uuid::now_v7()));
    let db = Database::connect(&db_path).await.expect("connect db");
    db.migrate().await.expect("migrate db");
    let project_id = "prj_00000000000000000000000000000094".to_string();
    let item_id = "itm_00000000000000000000000000000094".to_string();
    let revision_id = "rev_00000000000000000000000000000094".to_string();
    let author_job_id = "job_00000000000000000000000000000094".to_string();

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
        "INSERT INTO items (
            id, project_id, classification, workflow_version, lifecycle_state, parking_state,
            approval_state, escalation_state, current_revision_id, origin_kind, origin_finding_id,
            priority, labels, created_at, updated_at
         ) VALUES (?, ?, 'change', 'delivery:v1', 'open', 'deferred', 'not_requested', 'none', ?, 'manual', NULL, 'major', '[]', ?, ?)",
    )
    .bind(&item_id)
    .bind(&project_id)
    .bind(&revision_id)
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert item");
    sqlx::query(
        "INSERT INTO item_revisions (
            id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
            approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid,
            seed_target_commit_oid, supersedes_revision_id, created_at
         ) VALUES (?, ?, 1, 'Title', 'Desc', 'AC', 'refs/heads/main', 'required', '{}', '{}', ?, ?, NULL, ?)",
    )
    .bind(&revision_id)
    .bind(&item_id)
    .bind(Option::<String>::None)
    .bind(&bound_base)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert revision");
    sqlx::query(
        "INSERT INTO workspaces (
            id, project_id, kind, strategy, path, created_for_revision_id, parent_workspace_id,
            target_ref, workspace_ref, base_commit_oid, head_commit_oid, retention_policy,
            status, current_job_id, created_at, updated_at
         ) VALUES ('wrk_00000000000000000000000000000094', ?, 'authoring', 'worktree', ?, ?, NULL, 'refs/heads/main', 'refs/ingot/workspaces/wrk_00000000000000000000000000000094', ?, ?, 'persistent', 'ready', NULL, ?, ?)",
    )
    .bind(&project_id)
    .bind(repo.join("auth-ws").display().to_string())
    .bind(&revision_id)
    .bind(&bound_base)
    .bind(&authored_head)
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert authoring workspace");
    insert_test_job_row(
        &db,
        TestJobInsert {
            id: &author_job_id,
            project_id: &project_id,
            item_id: &item_id,
            item_revision_id: &revision_id,
            step_id: "author_initial",
            status: JobStatus::Completed,
            outcome_class: Some(OutcomeClass::Clean),
            phase_kind: PhaseKind::Author,
            workspace_kind: WorkspaceKind::Authoring,
            execution_permission: ExecutionPermission::MayMutate,
            context_policy: ContextPolicy::Fresh,
            phase_template_slug: "author-initial",
            output_artifact_kind: OutputArtifactKind::Commit,
            job_input: TestJobInput::AuthoringHead(&bound_base),
            output_commit_oid: Some(&authored_head),
            created_at: "2026-03-12T00:00:00Z",
            started_at: Some("2026-03-12T00:00:00Z"),
            ended_at: Some("2026-03-12T00:01:00Z"),
            ..TestJobInsert::new(
                &author_job_id,
                &project_id,
                &item_id,
                &revision_id,
                "author_initial",
            )
        },
    )
    .await;

    let response = test_router(db.clone())
        .oneshot(
            Request::builder()
                .uri(format!("/api/projects/{project_id}/items/{item_id}/resume"))
                .method("POST")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("resume route response");

    assert_eq!(response.status(), StatusCode::OK);
    let queued_review: (String, String, String) = sqlx::query_as(
        "SELECT step_id, input_base_commit_oid, input_head_commit_oid
         FROM jobs
         WHERE item_id = ? AND step_id = 'review_incremental_initial' AND status = 'queued'",
    )
    .bind(&item_id)
    .fetch_one(&db.pool)
    .await
    .expect("queued review job");
    assert_eq!(queued_review.0, "review_incremental_initial");
    assert_eq!(queued_review.1, bound_base);
    assert_eq!(queued_review.2, authored_head);
}

#[tokio::test]
async fn investigate_item_dispatch_creates_and_triage_removes_anchor_ref() {
    let repo = temp_git_repo("ingot-http-api");
    let target_head = git_output(&repo, &["rev-parse", "HEAD"]);
    let db_path = std::env::temp_dir().join(format!("ingot-http-api-db-{}.db", Uuid::now_v7()));
    let db = Database::connect(&db_path).await.expect("connect db");
    db.migrate().await.expect("migrate db");

    let project_id = "prj_00000000000000000000000000000092".to_string();
    let item_id = "itm_00000000000000000000000000000092".to_string();
    let revision_id = "rev_00000000000000000000000000000092".to_string();
    let finding_id = "fnd_00000000000000000000000000000092".to_string();
    let project_uuid = project_id.parse::<ProjectId>().expect("parse project id");
    let state_root =
        std::env::temp_dir().join(format!("ingot-http-api-state-{}", Uuid::now_v7()));

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
        "INSERT INTO items (
            id, project_id, classification, workflow_version, lifecycle_state, parking_state,
            approval_state, escalation_state, current_revision_id, origin_kind, origin_finding_id,
            priority, labels, created_at, updated_at
         ) VALUES (?, ?, 'change', 'delivery:v1', 'open', 'active', 'not_requested', 'none', ?, 'manual', NULL, 'major', '[]', ?, ?)",
    )
    .bind(&item_id)
    .bind(&project_id)
    .bind(&revision_id)
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert item");
    sqlx::query(
        "INSERT INTO item_revisions (
            id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
            approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid,
            seed_target_commit_oid, supersedes_revision_id, created_at
         ) VALUES (?, ?, 1, 'Title', 'Desc', 'AC', 'refs/heads/main', 'required', '{}', '{}', ?, ?, NULL, ?)",
    )
    .bind(&revision_id)
    .bind(&item_id)
    .bind(Option::<String>::None)
    .bind(&target_head)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert revision");

    let app = build_router_with_project_locks_and_state_root(
        db.clone(),
        ProjectLocks::default(),
        state_root.clone(),
    );
    let dispatch_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/projects/{project_id}/items/{item_id}/jobs"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{\"step_id\":\"investigate_item\"}"))
                .expect("build dispatch request"),
        )
        .await
        .expect("dispatch response");
    assert_eq!(dispatch_response.status(), StatusCode::CREATED);
    let dispatch_body = to_bytes(dispatch_response.into_body(), usize::MAX)
        .await
        .expect("dispatch body");
    let dispatch_json: serde_json::Value =
        serde_json::from_slice(&dispatch_body).expect("dispatch json");
    let job_id = dispatch_json["id"].as_str().expect("job id");
    assert_eq!(
        dispatch_json["job_input"]["kind"].as_str(),
        Some("candidate_subject")
    );
    assert_eq!(
        dispatch_json["job_input"]["base_commit_oid"].as_str(),
        Some(target_head.as_str())
    );
    assert_eq!(
        dispatch_json["job_input"]["head_commit_oid"].as_str(),
        Some(target_head.as_str())
    );

    let paths = project_repo_paths(state_root.as_path(), project_uuid, &repo);
    ensure_mirror(&paths).await.expect("ensure mirror");
    let investigation_ref = format!("refs/ingot/investigations/{job_id}");
    assert_eq!(
        resolve_ref_oid(paths.mirror_git_dir.as_path(), &investigation_ref)
            .await
            .expect("resolve investigation ref")
            .as_deref(),
        Some(target_head.as_str())
    );

    sqlx::query(
        "INSERT INTO findings (
            id, project_id, source_item_id, source_item_revision_id, source_job_id, source_step_id,
            source_report_schema_version, source_finding_key, source_subject_kind,
            source_subject_base_commit_oid, source_subject_head_commit_oid, code, severity,
            summary, paths, evidence, triage_state, linked_item_id, triage_note, created_at, triaged_at
         ) VALUES (?, ?, ?, ?, ?, 'investigate_item', 'finding_report:v1', 'finding-1', 'candidate', ?, ?, 'BUG001', 'medium', 'summary', '[]', '[]', 'untriaged', NULL, NULL, ?, NULL)",
    )
    .bind(&finding_id)
    .bind(&project_id)
    .bind(&item_id)
    .bind(&revision_id)
    .bind(job_id)
    .bind(&target_head)
    .bind(&target_head)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert finding");

    let triage_response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/findings/{finding_id}/triage"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    "{\"triage_state\":\"dismissed_invalid\",\"triage_note\":\"not actionable\"}",
                ))
                .expect("build triage request"),
        )
        .await
        .expect("triage response");
    assert_eq!(triage_response.status(), StatusCode::OK);
    assert_eq!(
        resolve_ref_oid(paths.mirror_git_dir.as_path(), &investigation_ref)
            .await
            .expect("resolve deleted investigation ref"),
        None
    );
}

#[tokio::test]
async fn investigate_item_dispatch_uses_existing_authoring_workspace_subject() {
    let repo = temp_git_repo("ingot-http-api");
    let bound_base = git_output(&repo, &["rev-parse", "HEAD"]);
    write_file(&repo.join("tracked.txt"), "bound workspace change");
    git(&repo, &["add", "tracked.txt"]);
    git(&repo, &["commit", "-m", "bound workspace change"]);
    let bound_head = git_output(&repo, &["rev-parse", "HEAD"]);

    let db_path = std::env::temp_dir().join(format!("ingot-http-api-db-{}.db", Uuid::now_v7()));
    let db = Database::connect(&db_path).await.expect("connect db");
    db.migrate().await.expect("migrate db");

    let project_id = "prj_00000000000000000000000000000093".to_string();
    let item_id = "itm_00000000000000000000000000000093".to_string();
    let revision_id = "rev_00000000000000000000000000000093".to_string();
    let workspace_id = "wrk_00000000000000000000000000000093".to_string();
    let project_uuid = project_id.parse::<ProjectId>().expect("parse project id");
    let state_root =
        std::env::temp_dir().join(format!("ingot-http-api-state-{}", Uuid::now_v7()));
    let paths = project_repo_paths(state_root.as_path(), project_uuid, &repo);
    ensure_mirror(&paths).await.expect("ensure mirror");
    git(
        &paths.mirror_git_dir,
        &[
            "update-ref",
            &format!("refs/ingot/workspaces/{workspace_id}"),
            &bound_head,
        ],
    );
    let workspace_path = state_root.join("bound-workspace");
    git(
        &paths.mirror_git_dir,
        &[
            "worktree",
            "add",
            "--detach",
            workspace_path.to_str().expect("workspace path"),
            &format!("refs/ingot/workspaces/{workspace_id}"),
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
        "INSERT INTO items (
            id, project_id, classification, workflow_version, lifecycle_state, parking_state,
            approval_state, escalation_state, current_revision_id, origin_kind, origin_finding_id,
            priority, labels, created_at, updated_at
         ) VALUES (?, ?, 'change', 'delivery:v1', 'open', 'active', 'not_requested', 'none', ?, 'manual', NULL, 'major', '[]', ?, ?)",
    )
    .bind(&item_id)
    .bind(&project_id)
    .bind(&revision_id)
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert item");
    sqlx::query(
        "INSERT INTO item_revisions (
            id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
            approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid,
            seed_target_commit_oid, supersedes_revision_id, created_at
         ) VALUES (?, ?, 1, 'Title', 'Desc', 'AC', 'refs/heads/main', 'required', '{}', '{}', ?, ?, NULL, ?)",
    )
    .bind(&revision_id)
    .bind(&item_id)
    .bind(Option::<String>::None)
    .bind(&bound_base)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert revision");
    sqlx::query(
        "INSERT INTO workspaces (
            id, project_id, kind, strategy, path, created_for_revision_id, parent_workspace_id,
            target_ref, workspace_ref, base_commit_oid, head_commit_oid, retention_policy,
            status, current_job_id, created_at, updated_at
         ) VALUES (?, ?, 'authoring', 'worktree', ?, ?, NULL, 'refs/heads/main', ?, ?, ?, 'persistent', 'ready', NULL, ?, ?)",
    )
    .bind(&workspace_id)
    .bind(&project_id)
    .bind(workspace_path.display().to_string())
    .bind(&revision_id)
    .bind(format!("refs/ingot/workspaces/{workspace_id}"))
    .bind(&bound_base)
    .bind(&bound_head)
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert workspace");

    let app = build_router_with_project_locks_and_state_root(
        db.clone(),
        ProjectLocks::default(),
        state_root.clone(),
    );
    let dispatch_response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/projects/{project_id}/items/{item_id}/jobs"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{\"step_id\":\"investigate_item\"}"))
                .expect("build dispatch request"),
        )
        .await
        .expect("dispatch response");
    assert_eq!(dispatch_response.status(), StatusCode::CREATED);
    let dispatch_body = to_bytes(dispatch_response.into_body(), usize::MAX)
        .await
        .expect("dispatch body");
    let dispatch_json: serde_json::Value =
        serde_json::from_slice(&dispatch_body).expect("dispatch json");
    let job_id = dispatch_json["id"].as_str().expect("job id");

    assert_eq!(
        dispatch_json["job_input"]["kind"].as_str(),
        Some("candidate_subject")
    );
    assert_eq!(
        dispatch_json["job_input"]["base_commit_oid"].as_str(),
        Some(bound_base.as_str())
    );
    assert_eq!(
        dispatch_json["job_input"]["head_commit_oid"].as_str(),
        Some(bound_head.as_str())
    );
    assert_eq!(
        resolve_ref_oid(
            paths.mirror_git_dir.as_path(),
            &format!("refs/ingot/investigations/{job_id}")
        )
        .await
        .expect("resolve no investigation ref"),
        None
    );
}

#[tokio::test]
async fn dispatch_item_job_route_reuses_existing_authoring_workspace_for_revision() {
    let repo = temp_git_repo("ingot-http-api");
    let seed_head = git_output(&repo, &["rev-parse", "HEAD"]);
    let db_path = std::env::temp_dir().join(format!("ingot-http-api-db-{}.db", Uuid::now_v7()));
    let db = Database::connect(&db_path).await.expect("connect db");
    db.migrate().await.expect("migrate db");

    let project_id = "prj_00000000000000000000000000000032".to_string();
    let item_id = "itm_00000000000000000000000000000032".to_string();
    let revision_id = "rev_00000000000000000000000000000032".to_string();
    let workspace_id = "wrk_00000000000000000000000000000032".to_string();
    let workspace_path =
        std::env::temp_dir().join(format!("ingot-authoring-existing-{}", Uuid::now_v7()));

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
        "INSERT INTO items (
            id, project_id, classification, workflow_version, lifecycle_state, parking_state,
            approval_state, escalation_state, current_revision_id, origin_kind, origin_finding_id,
            priority, labels, created_at, updated_at
         ) VALUES (?, ?, 'change', 'delivery:v1', 'open', 'active', 'not_requested', 'none', ?, 'manual', NULL, 'major', '[]', ?, ?)",
    )
    .bind(&item_id)
    .bind(&project_id)
    .bind(&revision_id)
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert item");

    sqlx::query(
        "INSERT INTO item_revisions (
            id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
            approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid,
            seed_target_commit_oid, supersedes_revision_id, created_at
         ) VALUES (?, ?, 1, 'Title', 'Desc', 'AC', 'refs/heads/main', 'required', '{}', '{\"author_initial\":\"author-initial\"}', ?, ?, NULL, ?)",
    )
    .bind(&revision_id)
    .bind(&item_id)
    .bind(&seed_head)
    .bind(&seed_head)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert revision");

    sqlx::query(
        "INSERT INTO workspaces (
            id, project_id, kind, strategy, path, created_for_revision_id, parent_workspace_id,
            target_ref, workspace_ref, base_commit_oid, head_commit_oid, retention_policy,
            status, current_job_id, created_at, updated_at
         ) VALUES (?, ?, 'authoring', 'worktree', ?, ?, NULL, 'refs/heads/main', ?, ?, ?, 'ephemeral', 'ready', NULL, ?, ?)",
    )
    .bind(&workspace_id)
    .bind(&project_id)
    .bind(workspace_path.display().to_string())
    .bind(&revision_id)
    .bind(format!("refs/ingot/workspaces/{workspace_id}"))
    .bind(&seed_head)
    .bind(&seed_head)
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert workspace");

    let app = test_router(db.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/projects/{project_id}/items/{item_id}/jobs"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .expect("build dispatch request"),
        )
        .await
        .expect("dispatch route response");

    assert_eq!(response.status(), StatusCode::CREATED);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("dispatch json");
    assert_eq!(json["workspace_id"].as_str(), Some(workspace_id.as_str()));

    let workspace_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM workspaces WHERE created_for_revision_id = ?")
            .bind(&revision_id)
            .fetch_one(&db.pool)
            .await
            .expect("workspace count");
    assert_eq!(workspace_count, 1);
}
