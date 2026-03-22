#![allow(dead_code, unused_imports)]

// Shared route-test helpers are compiled into multiple test binaries, and each binary
// intentionally uses only a subset of them.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use ingot_domain::convergence::Convergence;
use ingot_domain::finding::Finding;
use ingot_domain::ids::{ItemId, ItemRevisionId, JobId, ProjectId, WorkspaceId};
use ingot_domain::item::Item;
use ingot_domain::job::{
    ContextPolicy, ExecutionPermission, Job, JobAssignment, JobInput, JobLease, JobState,
    JobStatus, OutcomeClass, OutputArtifactKind, PhaseKind, TerminalStatus,
};
use ingot_domain::ports::RepositoryError;
use ingot_domain::project::Project;
use ingot_domain::revision::ItemRevision;
use ingot_domain::workspace::{
    RetentionPolicy, Workspace, WorkspaceKind, WorkspaceState, WorkspaceStatus, WorkspaceStrategy,
};
use ingot_store_sqlite::Database;
use ingot_test_support::fixtures::{
    default_timestamp, ConvergenceBuilder, FindingBuilder, ItemBuilder, JobBuilder, ProjectBuilder,
    RevisionBuilder, WorkspaceBuilder,
};
pub use ingot_test_support::fixtures::{parse_timestamp, DEFAULT_TEST_TIMESTAMP};
pub use ingot_test_support::git::{git_output, run_git as git, temp_git_repo, write_file};
pub use ingot_test_support::reports::clean_validation_report;
pub use ingot_test_support::sqlite::migrated_test_db;
use ingot_usecases::{DispatchNotify, ProjectLocks};
use uuid::Uuid;

pub const TS: &str = DEFAULT_TEST_TIMESTAMP;

/// Build a router with an isolated temp state root (avoids production `$HOME/.ingot`).
pub fn test_router(db: Database) -> axum::Router {
    let state_root = std::env::temp_dir().join(format!("ingot-http-api-state-{}", Uuid::now_v7()));
    ingot_http_api::build_router_with_project_locks_and_state_root(
        db,
        ProjectLocks::default(),
        state_root,
        DispatchNotify::default(),
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
      --config <key=value>
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
        TestJobInput::AuthoringHead(head_commit_oid) => {
            JobInput::authoring_head(head_commit_oid.into())
        }
        TestJobInput::CandidateSubject(base_commit_oid, head_commit_oid) => {
            JobInput::candidate_subject(base_commit_oid.into(), head_commit_oid.into())
        }
        TestJobInput::IntegratedSubject(base_commit_oid, head_commit_oid) => {
            JobInput::integrated_subject(base_commit_oid.into(), head_commit_oid.into())
        }
    }
}

pub trait PersistFixture: Sized {
    async fn persist(self, db: &Database) -> Result<Self, RepositoryError>;
}

impl PersistFixture for Project {
    async fn persist(self, db: &Database) -> Result<Self, RepositoryError> {
        db.create_project(&self).await?;
        Ok(self)
    }
}

impl PersistFixture for Item {
    async fn persist(self, db: &Database) -> Result<Self, RepositoryError> {
        db.create_item(&self).await?;
        Ok(self)
    }
}

impl PersistFixture for ItemRevision {
    async fn persist(self, db: &Database) -> Result<Self, RepositoryError> {
        db.create_revision(&self).await?;
        Ok(self)
    }
}

impl PersistFixture for (Item, ItemRevision) {
    async fn persist(self, db: &Database) -> Result<Self, RepositoryError> {
        db.create_item_with_revision(&self.0, &self.1).await?;
        Ok(self)
    }
}

impl PersistFixture for Workspace {
    async fn persist(self, db: &Database) -> Result<Self, RepositoryError> {
        db.create_workspace(&self).await?;
        Ok(self)
    }
}

impl PersistFixture for Convergence {
    async fn persist(self, db: &Database) -> Result<Self, RepositoryError> {
        db.create_convergence(&self).await?;
        Ok(self)
    }
}

impl PersistFixture for Finding {
    async fn persist(self, db: &Database) -> Result<Self, RepositoryError> {
        db.create_finding(&self).await?;
        Ok(self)
    }
}

impl PersistFixture for Job {
    async fn persist(self, db: &Database) -> Result<Self, RepositoryError> {
        if let Some(workspace_id) = self.state.workspace_id() {
            if db.get_workspace(workspace_id).await.is_err() {
                let mut workspace = WorkspaceBuilder::new(self.project_id, self.workspace_kind)
                    .id(workspace_id)
                    .created_for_revision_id(self.item_revision_id)
                    .path(
                        std::env::temp_dir()
                            .join(format!("ingot-http-api-workspace-{workspace_id}"))
                            .display()
                            .to_string(),
                    )
                    .created_at(self.created_at);
                workspace = if self.state.is_active() {
                    workspace
                        .status(WorkspaceStatus::Busy)
                        .current_job_id(self.id)
                } else {
                    workspace.status(WorkspaceStatus::Ready)
                };
                let workspace = workspace.build();
                db.create_workspace(&workspace).await?;
            }
        }
        db.create_job(&self).await?;
        Ok(self)
    }
}

pub fn test_project_builder(path: impl AsRef<Path>, id: &str) -> ProjectBuilder {
    ProjectBuilder::new(path)
        .id(parse_id::<ProjectId>(id))
        .created_at(default_timestamp())
}

pub fn test_item_builder(project_id: &str, revision_id: &str, item_id: &str) -> ItemBuilder {
    ItemBuilder::new(
        parse_id::<ProjectId>(project_id),
        parse_id::<ItemRevisionId>(revision_id),
    )
    .id(parse_id::<ItemId>(item_id))
    .created_at(default_timestamp())
}

pub fn test_revision_builder(item_id: &str, revision_id: &str) -> RevisionBuilder {
    RevisionBuilder::new(parse_id::<ItemId>(item_id))
        .id(parse_id::<ItemRevisionId>(revision_id))
        .created_at(default_timestamp())
}

pub fn test_workspace_builder(
    project_id: &str,
    kind: WorkspaceKind,
    workspace_id: &str,
) -> WorkspaceBuilder {
    WorkspaceBuilder::new(parse_id::<ProjectId>(project_id), kind)
        .id(parse_id::<WorkspaceId>(workspace_id))
        .created_at(default_timestamp())
}

pub fn test_convergence_builder(
    project_id: &str,
    item_id: &str,
    revision_id: &str,
    convergence_id: &str,
) -> ConvergenceBuilder {
    ConvergenceBuilder::new(
        parse_id::<ProjectId>(project_id),
        parse_id::<ItemId>(item_id),
        parse_id::<ItemRevisionId>(revision_id),
    )
    .id(parse_id(convergence_id))
    .created_at(default_timestamp())
}

pub fn test_finding_builder(
    project_id: &str,
    item_id: &str,
    revision_id: &str,
    job_id: &str,
    finding_id: &str,
) -> FindingBuilder {
    FindingBuilder::new(
        parse_id::<ProjectId>(project_id),
        parse_id::<ItemId>(item_id),
        parse_id::<ItemRevisionId>(revision_id),
        parse_id::<JobId>(job_id),
    )
    .id(parse_id(finding_id))
    .created_at(default_timestamp())
}

pub async fn persist_test_project(
    db: &Database,
    path: impl AsRef<Path>,
    project_id: &str,
) -> Project {
    test_project_builder(path, project_id)
        .name("Test")
        .build()
        .persist(db)
        .await
        .expect("persist test project")
}

pub async fn persist_test_change(
    db: &Database,
    path: impl AsRef<Path>,
    project_id: &str,
    item_id: &str,
    revision_id: &str,
    configure_item: impl FnOnce(ItemBuilder) -> ItemBuilder,
    configure_revision: impl FnOnce(RevisionBuilder) -> RevisionBuilder,
) -> (Project, Item, ItemRevision) {
    let project = persist_test_project(db, path, project_id).await;
    let revision = configure_revision(test_revision_builder(item_id, revision_id)).build();
    let item = configure_item(test_item_builder(project_id, revision_id, item_id)).build();
    let (item, revision) = (item, revision)
        .persist(db)
        .await
        .expect("persist test item with revision");
    (project, item, revision)
}

pub async fn persist_test_workspace(
    db: &Database,
    project_id: &str,
    kind: WorkspaceKind,
    workspace_id: &str,
    configure_workspace: impl FnOnce(WorkspaceBuilder) -> WorkspaceBuilder,
) -> Workspace {
    configure_workspace(test_workspace_builder(project_id, kind, workspace_id))
        .build()
        .persist(db)
        .await
        .expect("persist test workspace")
}

pub async fn persist_test_convergence(
    db: &Database,
    project_id: &str,
    item_id: &str,
    revision_id: &str,
    convergence_id: &str,
    configure_convergence: impl FnOnce(ConvergenceBuilder) -> ConvergenceBuilder,
) -> Convergence {
    configure_convergence(test_convergence_builder(
        project_id,
        item_id,
        revision_id,
        convergence_id,
    ))
    .build()
    .persist(db)
    .await
    .expect("persist test convergence")
}

pub async fn persist_test_finding(
    db: &Database,
    project_id: &str,
    item_id: &str,
    revision_id: &str,
    job_id: &str,
    finding_id: &str,
    configure_finding: impl FnOnce(FindingBuilder) -> FindingBuilder,
) -> Finding {
    configure_finding(test_finding_builder(
        project_id,
        item_id,
        revision_id,
        job_id,
        finding_id,
    ))
    .build()
    .persist(db)
    .await
    .expect("persist test finding")
}

pub async fn insert_test_job_row(db: &Database, row: TestJobInsert<'_>) {
    let workspace_id = row.workspace_id.map(parse_id::<WorkspaceId>).or_else(|| {
        matches!(row.status, JobStatus::Assigned | JobStatus::Running).then(WorkspaceId::new)
    });
    let created_at = parse_test_ts(row.created_at);
    let mut job = JobBuilder::new(
        parse_id::<ProjectId>(row.project_id),
        parse_id::<ItemId>(row.item_id),
        parse_id::<ItemRevisionId>(row.item_revision_id),
        row.step_id,
    )
    .id(parse_id::<JobId>(row.id))
    .retry_no(row.retry_no)
    .status(row.status)
    .phase_kind(row.phase_kind)
    .workspace_kind(row.workspace_kind)
    .execution_permission(row.execution_permission)
    .context_policy(row.context_policy)
    .phase_template_slug(row.phase_template_slug)
    .job_input(into_test_job_input(row.job_input))
    .output_artifact_kind(row.output_artifact_kind)
    .created_at(created_at);
    if let Some(supersedes_job_id) = row.supersedes_job_id {
        job = job.supersedes_job_id(parse_id::<JobId>(supersedes_job_id));
    }
    if let Some(workspace_id) = workspace_id {
        job = job.workspace_id(workspace_id);
    }
    if let Some(outcome_class) = row.outcome_class {
        job = job.outcome_class(outcome_class);
    }
    if let Some(output_commit_oid) = row.output_commit_oid {
        job = job.output_commit_oid(output_commit_oid);
    }
    if let Some(result_schema_version) = row.result_schema_version {
        job = job.result_schema_version(result_schema_version);
    }
    if let Some(result_payload) = row.result_payload.clone() {
        job = job.result_payload(result_payload);
    }
    if let Some(error_code) = row.error_code {
        job = job.error_code(error_code);
    }
    if let Some(error_message) = row.error_message {
        job = job.error_message(error_message);
    }
    if let Some(started_at) = row.started_at {
        job = job.started_at(parse_test_ts(started_at));
    }
    if let Some(ended_at) = row.ended_at {
        job = job.ended_at(parse_test_ts(ended_at));
    }
    let mut job = job.build();
    job.semantic_attempt_no = row.semantic_attempt_no;
    job.persist(db).await.expect("insert test job");
}

pub async fn seeded_route_test_app() -> (PathBuf, Database, String, String, String) {
    let repo = temp_git_repo("ingot-http-api");
    let db = migrated_test_db("ingot-http-api-db").await;

    let project_id = "prj_00000000000000000000000000000000".to_string();
    let item_id = "itm_00000000000000000000000000000000".to_string();
    let revision_id = "rev_00000000000000000000000000000000".to_string();
    let job_id = "job_00000000000000000000000000000000".to_string();
    let workspace_id = "wrk_00000000000000000000000000000000".to_string();

    test_project_builder(&repo, &project_id)
        .name("Test")
        .build()
        .persist(&db)
        .await
        .expect("insert project");

    let revision = test_revision_builder(&item_id, &revision_id)
        .seed(ingot_domain::revision::AuthoringBaseSeed::Explicit {
            seed_commit_oid: "base".into(),
            seed_target_commit_oid: "target".into(),
        })
        .build();
    let item = test_item_builder(&project_id, &revision_id, &item_id).build();
    (item, revision)
        .persist(&db)
        .await
        .expect("insert item with revision");

    test_workspace_builder(&project_id, WorkspaceKind::Authoring, &workspace_id)
        .created_for_revision_id(parse_id::<ItemRevisionId>(&revision_id))
        .status(WorkspaceStatus::Busy)
        .current_job_id(parse_id::<JobId>(&job_id))
        .base_commit_oid("base")
        .head_commit_oid("head")
        .path(repo.display().to_string())
        .build()
        .persist(&db)
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
