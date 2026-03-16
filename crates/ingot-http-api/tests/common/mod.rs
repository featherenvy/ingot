#![allow(dead_code, unused_imports)]

// Shared route-test helpers are compiled into multiple test binaries, and each binary
// intentionally uses only a subset of them.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use ingot_domain::ids::{JobId, WorkspaceId};
use ingot_domain::job::{
    ContextPolicy, ExecutionPermission, Job, JobAssignment, JobInput, JobLease, JobState,
    JobStatus, OutcomeClass, OutputArtifactKind, PhaseKind, TerminalStatus,
};
use ingot_domain::workspace::{
    RetentionPolicy, Workspace, WorkspaceKind, WorkspaceStatus, WorkspaceStrategy,
};
use ingot_store_sqlite::Database;
pub use ingot_test_support::fixtures::{DEFAULT_TEST_TIMESTAMP, parse_timestamp};
pub use ingot_test_support::git::{git_output, run_git as git, temp_git_repo, write_file};
pub use ingot_test_support::reports::clean_validation_report;
pub use ingot_test_support::sqlite::migrated_test_db;
use ingot_usecases::ProjectLocks;
use uuid::Uuid;

pub const TS: &str = DEFAULT_TEST_TIMESTAMP;

/// Build a router with an isolated temp state root (avoids production `$HOME/.ingot`).
pub fn test_router(db: Database) -> axum::Router {
    let state_root = std::env::temp_dir().join(format!("ingot-http-api-state-{}", Uuid::now_v7()));
    ingot_http_api::build_router_with_project_locks_and_state_root(
        db,
        ProjectLocks::default(),
        state_root,
    )
}

pub fn parse_id<T: FromStr>(value: &str) -> T {
    value
        .parse()
        .unwrap_or_else(|_| panic!("invalid test id: {value}"))
}

pub fn fake_codex_probe_script() -> PathBuf {
    let path = std::env::temp_dir().join(format!("ingot-fake-codex-{}.sh", Uuid::now_v7()));
    fs::write(
        &path,
        r#"#!/bin/sh
if [ "$1" = "exec" ] && [ "$2" = "--help" ]; then
  cat <<'EOF'
Usage: codex exec [OPTIONS] [PROMPT] [COMMAND]
  -s, --sandbox <SANDBOX_MODE>
  -C, --cd <DIR>
      --output-schema <FILE>
      --json
  -o, --output-last-message <FILE>
EOF
  exit 0
fi
echo "unexpected arguments: $@" >&2
exit 1
"#,
    )
    .expect("write fake codex");
    let mut permissions = fs::metadata(&path)
        .expect("fake codex metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).expect("chmod fake codex");
    path
}

// ---------------------------------------------------------------------------
// TestJobInsert helper (string-ID–based job row insertion for route tests)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
pub enum TestJobInput<'a> {
    None,
    AuthoringHead(&'a str),
    CandidateSubject(&'a str, &'a str),
    IntegratedSubject(&'a str, &'a str),
}

pub struct TestJobInsert<'a> {
    pub id: &'a str,
    pub project_id: &'a str,
    pub item_id: &'a str,
    pub item_revision_id: &'a str,
    pub step_id: &'a str,
    pub semantic_attempt_no: u32,
    pub retry_no: u32,
    pub supersedes_job_id: Option<&'a str>,
    pub status: JobStatus,
    pub outcome_class: Option<OutcomeClass>,
    pub phase_kind: PhaseKind,
    pub workspace_id: Option<&'a str>,
    pub workspace_kind: WorkspaceKind,
    pub execution_permission: ExecutionPermission,
    pub context_policy: ContextPolicy,
    pub phase_template_slug: &'a str,
    pub output_artifact_kind: OutputArtifactKind,
    pub job_input: TestJobInput<'a>,
    pub output_commit_oid: Option<&'a str>,
    pub result_schema_version: Option<&'a str>,
    pub result_payload: Option<serde_json::Value>,
    pub error_code: Option<&'a str>,
    pub error_message: Option<&'a str>,
    pub created_at: &'a str,
    pub started_at: Option<&'a str>,
    pub ended_at: Option<&'a str>,
}

impl<'a> TestJobInsert<'a> {
    pub fn new(
        id: &'a str,
        project_id: &'a str,
        item_id: &'a str,
        item_revision_id: &'a str,
        step_id: &'a str,
    ) -> Self {
        Self {
            id,
            project_id,
            item_id,
            item_revision_id,
            step_id,
            semantic_attempt_no: 1,
            retry_no: 0,
            supersedes_job_id: None,
            status: JobStatus::Queued,
            outcome_class: None,
            phase_kind: PhaseKind::Author,
            workspace_id: None,
            workspace_kind: WorkspaceKind::Authoring,
            execution_permission: ExecutionPermission::MayMutate,
            context_policy: ContextPolicy::Fresh,
            phase_template_slug: "template",
            output_artifact_kind: OutputArtifactKind::None,
            job_input: TestJobInput::None,
            output_commit_oid: None,
            result_schema_version: None,
            result_payload: None,
            error_code: None,
            error_message: None,
            created_at: "2026-03-12T00:00:00Z",
            started_at: None,
            ended_at: None,
        }
    }
}

fn parse_test_ts(value: &str) -> DateTime<Utc> {
    parse_timestamp(value)
}

fn into_test_job_input(input: TestJobInput<'_>) -> JobInput {
    match input {
        TestJobInput::None => JobInput::None,
        TestJobInput::AuthoringHead(head_commit_oid) => JobInput::authoring_head(head_commit_oid),
        TestJobInput::CandidateSubject(base_commit_oid, head_commit_oid) => {
            JobInput::candidate_subject(base_commit_oid, head_commit_oid)
        }
        TestJobInput::IntegratedSubject(base_commit_oid, head_commit_oid) => {
            JobInput::integrated_subject(base_commit_oid, head_commit_oid)
        }
    }
}

pub async fn insert_test_job_row(db: &Database, row: TestJobInsert<'_>) {
    let workspace_id = row.workspace_id.map(parse_id::<WorkspaceId>).or_else(|| {
        matches!(row.status, JobStatus::Assigned | JobStatus::Running).then(WorkspaceId::new)
    });
    let assignment = workspace_id.map(|workspace_id| JobAssignment {
        workspace_id,
        agent_id: None,
        prompt_snapshot: None,
        phase_template_digest: None,
    });

    let state = match row.status {
        JobStatus::Queued => JobState::Queued,
        JobStatus::Assigned => JobState::Assigned(assignment.expect("assigned workspace")),
        JobStatus::Running => {
            let now = Utc::now();
            JobState::Running {
                assignment: assignment.expect("running workspace"),
                lease: JobLease {
                    process_pid: None,
                    lease_owner_id: "test".into(),
                    heartbeat_at: now,
                    lease_expires_at: now + chrono::Duration::seconds(1800),
                    started_at: row.started_at.map(parse_test_ts).unwrap_or(now),
                },
            }
        }
        JobStatus::Completed => JobState::Completed {
            assignment,
            started_at: row.started_at.map(parse_test_ts),
            outcome_class: row.outcome_class.unwrap_or(OutcomeClass::Clean),
            ended_at: row.ended_at.map(parse_test_ts).unwrap_or_else(Utc::now),
            output_commit_oid: row.output_commit_oid.map(ToOwned::to_owned),
            result_schema_version: row.result_schema_version.map(ToOwned::to_owned),
            result_payload: row.result_payload.clone(),
        },
        JobStatus::Failed => JobState::Terminated {
            terminal_status: TerminalStatus::Failed,
            assignment,
            started_at: row.started_at.map(parse_test_ts),
            outcome_class: row.outcome_class,
            ended_at: row.ended_at.map(parse_test_ts).unwrap_or_else(Utc::now),
            error_code: row.error_code.map(ToOwned::to_owned),
            error_message: row.error_message.map(ToOwned::to_owned),
        },
        JobStatus::Cancelled => JobState::Terminated {
            terminal_status: TerminalStatus::Cancelled,
            assignment,
            started_at: row.started_at.map(parse_test_ts),
            outcome_class: row.outcome_class,
            ended_at: row.ended_at.map(parse_test_ts).unwrap_or_else(Utc::now),
            error_code: row.error_code.map(ToOwned::to_owned),
            error_message: row.error_message.map(ToOwned::to_owned),
        },
        JobStatus::Expired => JobState::Terminated {
            terminal_status: TerminalStatus::Expired,
            assignment,
            started_at: row.started_at.map(parse_test_ts),
            outcome_class: row.outcome_class,
            ended_at: row.ended_at.map(parse_test_ts).unwrap_or_else(Utc::now),
            error_code: row.error_code.map(ToOwned::to_owned),
            error_message: row.error_message.map(ToOwned::to_owned),
        },
        JobStatus::Superseded => JobState::Terminated {
            terminal_status: TerminalStatus::Superseded,
            assignment,
            started_at: row.started_at.map(parse_test_ts),
            outcome_class: row.outcome_class,
            ended_at: row.ended_at.map(parse_test_ts).unwrap_or_else(Utc::now),
            error_code: row.error_code.map(ToOwned::to_owned),
            error_message: row.error_message.map(ToOwned::to_owned),
        },
    };
    let job = Job {
        id: parse_id(row.id),
        project_id: parse_id(row.project_id),
        item_id: parse_id(row.item_id),
        item_revision_id: parse_id(row.item_revision_id),
        step_id: row.step_id.into(),
        semantic_attempt_no: row.semantic_attempt_no,
        retry_no: row.retry_no,
        supersedes_job_id: row.supersedes_job_id.map(parse_id::<JobId>),
        phase_kind: row.phase_kind,
        workspace_kind: row.workspace_kind,
        execution_permission: row.execution_permission,
        context_policy: row.context_policy,
        phase_template_slug: row.phase_template_slug.into(),
        output_artifact_kind: row.output_artifact_kind,
        job_input: into_test_job_input(row.job_input),
        created_at: parse_test_ts(row.created_at),
        state,
    };
    if let Some(workspace_id) = job.state.workspace_id() {
        let workspace = Workspace {
            id: workspace_id,
            project_id: job.project_id,
            kind: job.workspace_kind,
            status: if job.state.is_active() {
                WorkspaceStatus::Busy
            } else {
                WorkspaceStatus::Ready
            },
            retention_policy: RetentionPolicy::Persistent,
            strategy: WorkspaceStrategy::Worktree,
            created_for_revision_id: Some(job.item_revision_id),
            parent_workspace_id: None,
            path: std::env::temp_dir()
                .join(format!("ingot-http-api-workspace-{workspace_id}"))
                .display()
                .to_string(),
            base_commit_oid: None,
            head_commit_oid: None,
            workspace_ref: None,
            target_ref: None,
            current_job_id: job.state.is_active().then_some(job.id),
            updated_at: job.created_at,
            created_at: job.created_at,
        };
        let _ = db.create_workspace(&workspace).await;
    }
    db.create_job(&job).await.expect("insert test job");
}

pub async fn seeded_route_test_app() -> (PathBuf, Database, String, String, String) {
    let repo = temp_git_repo("ingot-http-api");
    let db = migrated_test_db("ingot-http-api-db").await;

    let project_id = "prj_00000000000000000000000000000000".to_string();
    let item_id = "itm_00000000000000000000000000000000".to_string();
    let revision_id = "rev_00000000000000000000000000000000".to_string();
    let job_id = "job_00000000000000000000000000000000".to_string();
    let workspace_id = "wrk_00000000000000000000000000000000".to_string();

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
         ) VALUES (?, ?, 1, 'Title', 'Desc', 'AC', 'refs/heads/main', 'required', '{}', '{}', 'base', 'target', NULL, ?)",
    )
    .bind(&revision_id)
    .bind(&item_id)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert revision");

    sqlx::query(
        "INSERT INTO workspaces (
            id, project_id, kind, status, retention_policy,
            created_for_revision_id, path, created_at, updated_at
         ) VALUES (?, ?, 'authoring', 'busy', 'persistent', ?, ?, ?, ?)",
    )
    .bind(&workspace_id)
    .bind(&project_id)
    .bind(&revision_id)
    .bind(repo.display().to_string())
    .bind(TS)
    .bind(TS)
    .execute(&db.pool)
    .await
    .expect("insert workspace");

    insert_test_job_row(
        &db,
        TestJobInsert {
            id: &job_id,
            project_id: &project_id,
            item_id: &item_id,
            item_revision_id: &revision_id,
            step_id: "validate_candidate_initial",
            status: JobStatus::Running,
            workspace_id: Some(&workspace_id),
            phase_kind: PhaseKind::Validate,
            workspace_kind: WorkspaceKind::Authoring,
            execution_permission: ExecutionPermission::MustNotMutate,
            context_policy: ContextPolicy::ResumeContext,
            phase_template_slug: "validate-candidate",
            output_artifact_kind: OutputArtifactKind::ValidationReport,
            job_input: TestJobInput::CandidateSubject("base", "head"),
            created_at: "2026-03-12T00:00:00Z",
            ..TestJobInsert::new(
                &job_id,
                &project_id,
                &item_id,
                &revision_id,
                "validate_candidate_initial",
            )
        },
    )
    .await;

    (repo, db, project_id, item_id, job_id)
}
