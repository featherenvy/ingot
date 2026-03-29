#![allow(dead_code, unused_imports)]

// Shared runtime-test helpers are compiled into multiple test binaries, and each binary
// intentionally uses only a subset of them.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use ingot_agent_protocol::adapter::AgentError;
use ingot_agent_protocol::request::AgentRequest;
use ingot_agent_protocol::response::AgentResponse;
use ingot_agent_runtime::AgentRunner;
use ingot_domain::agent::Agent;
use ingot_domain::ids;
use ingot_domain::job::{ExecutionPermission, Job, JobInput, OutputArtifactKind, PhaseKind};
use ingot_domain::project::Project;
pub use ingot_domain::test_support::{
    AgentBuilder, ConvergenceBuilder, ConvergenceQueueEntryBuilder, FindingBuilder,
    GitOperationBuilder, ItemBuilder, JobBuilder, ProjectBuilder, RevisionBuilder,
    WorkspaceBuilder, default_timestamp, parse_timestamp,
};
use ingot_domain::workspace::WorkspaceKind;
use ingot_git::commands::head_oid;
use ingot_git::project_repo::{ProjectRepoPaths, ensure_mirror, project_repo_paths};
use ingot_test_support::git::unique_temp_path;
use ingot_test_support::reports::{
    clean_review_report, clean_validation_report, findings_review_report,
};
pub use ingot_test_support::sqlite::migrated_test_db;
#[allow(dead_code)]
mod shared_harness {
    use ingot_agent_runtime as runtime_crate;

    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/common/shared_harness.rs"
    ));
}

pub use shared_harness::{BlockingRunner, TestAgentProfile, TestHarness, agent_fixture};

// ---------------------------------------------------------------------------
// Entity builders
// ---------------------------------------------------------------------------

pub fn test_authoring_job(
    project_id: ids::ProjectId,
    item_id: ids::ItemId,
    revision_id: ids::ItemRevisionId,
    seed_commit: &str,
) -> Job {
    JobBuilder::new(project_id, item_id, revision_id, "author_initial")
        .phase_kind(PhaseKind::Author)
        .workspace_kind(WorkspaceKind::Authoring)
        .execution_permission(ExecutionPermission::MayMutate)
        .phase_template_slug("author-initial")
        .job_input(JobInput::authoring_head(
            ingot_domain::commit_oid::CommitOid::new(seed_commit),
        ))
        .output_artifact_kind(OutputArtifactKind::Commit)
        .build()
}

pub fn test_review_job(
    project_id: ids::ProjectId,
    item_id: ids::ItemId,
    revision_id: ids::ItemRevisionId,
    base_commit: &str,
    head_commit: &str,
) -> Job {
    JobBuilder::new(project_id, item_id, revision_id, "review_candidate_initial")
        .phase_kind(PhaseKind::Review)
        .workspace_kind(WorkspaceKind::Review)
        .execution_permission(ExecutionPermission::MustNotMutate)
        .phase_template_slug("review-candidate")
        .job_input(JobInput::candidate_subject(
            ingot_domain::commit_oid::CommitOid::new(base_commit),
            ingot_domain::commit_oid::CommitOid::new(head_commit),
        ))
        .output_artifact_kind(OutputArtifactKind::ReviewReport)
        .build()
}

// ---------------------------------------------------------------------------
// Fake runners
// ---------------------------------------------------------------------------

pub struct FakeRunner;

impl AgentRunner for FakeRunner {
    fn launch<'a>(
        &'a self,
        _agent: &'a Agent,
        _request: &'a AgentRequest,
        working_dir: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<AgentResponse, AgentError>> + Send + 'a>> {
        Box::pin(async move {
            tokio::fs::write(working_dir.join("generated.txt"), "hello")
                .await
                .unwrap();
            Ok(AgentResponse {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
                result: Some(serde_json::json!({ "message": "implemented change" })),
            })
        })
    }
}

pub struct StaticReviewRunner {
    pub base_commit_oid: String,
    pub head_commit_oid: String,
}

impl AgentRunner for StaticReviewRunner {
    fn launch<'a>(
        &'a self,
        _agent: &'a Agent,
        _request: &'a AgentRequest,
        _working_dir: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<AgentResponse, AgentError>> + Send + 'a>> {
        Box::pin(async move {
            Ok(AgentResponse {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
                result: Some(clean_review_report(
                    &self.base_commit_oid,
                    &self.head_commit_oid,
                )),
            })
        })
    }
}

pub struct ScriptedLoopRunner;

impl AgentRunner for ScriptedLoopRunner {
    fn launch<'a>(
        &'a self,
        _agent: &'a Agent,
        request: &'a AgentRequest,
        working_dir: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<AgentResponse, AgentError>> + Send + 'a>> {
        Box::pin(async move {
            let step = prompt_value(&request.prompt, "Step");
            match step.as_deref() {
                Some("author_initial") => {
                    tokio::fs::write(working_dir.join("feature.txt"), "initial change")
                        .await
                        .expect("write feature");
                    Ok(AgentResponse {
                        exit_code: 0,
                        stdout: String::new(),
                        stderr: String::new(),
                        result: Some(serde_json::json!({ "summary": "initial authored" })),
                    })
                }
                Some("repair_candidate") => {
                    tokio::fs::write(working_dir.join("feature.txt"), "repaired change")
                        .await
                        .expect("repair feature");
                    Ok(AgentResponse {
                        exit_code: 0,
                        stdout: String::new(),
                        stderr: String::new(),
                        result: Some(serde_json::json!({ "summary": "candidate repaired" })),
                    })
                }
                Some("review_incremental_initial") => Ok(AgentResponse {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                    result: Some(findings_review_report(
                        &prompt_value(&request.prompt, "Input base commit").unwrap_or_default(),
                        &prompt_value(&request.prompt, "Input head commit").unwrap_or_default(),
                        "initial review found an issue",
                        "medium",
                        vec![serde_json::json!({
                            "finding_key": "fix-me",
                            "code": "BUG",
                            "severity": "medium",
                            "summary": "needs repair",
                            "paths": ["feature.txt"],
                            "evidence": ["fix me"]
                        })],
                    )),
                }),
                Some("review_incremental_repair") | Some("review_candidate_repair") => {
                    Ok(AgentResponse {
                        exit_code: 0,
                        stdout: String::new(),
                        stderr: String::new(),
                        result: Some(clean_review_report(
                            &prompt_value(&request.prompt, "Input base commit").unwrap_or_default(),
                            &prompt_value(&request.prompt, "Input head commit").unwrap_or_default(),
                        )),
                    })
                }
                Some("validate_candidate_repair") => Ok(AgentResponse {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                    result: Some(clean_validation_report("validation clean")),
                }),
                other => Err(AgentError::ProtocolViolation(format!(
                    "unexpected step in scripted loop runner: {other:?}"
                ))),
            }
        })
    }
}

pub struct CleanInitialReviewRunner;

impl AgentRunner for CleanInitialReviewRunner {
    fn launch<'a>(
        &'a self,
        _agent: &'a Agent,
        request: &'a AgentRequest,
        _working_dir: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<AgentResponse, AgentError>> + Send + 'a>> {
        Box::pin(async move {
            match prompt_value(&request.prompt, "Step").as_deref() {
                Some("review_incremental_initial") => Ok(AgentResponse {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                    result: Some(clean_review_report(
                        &prompt_value(&request.prompt, "Input base commit").unwrap_or_default(),
                        &prompt_value(&request.prompt, "Input head commit").unwrap_or_default(),
                    )),
                }),
                other => Err(AgentError::ProtocolViolation(format!(
                    "unexpected step in clean initial review runner: {other:?}"
                ))),
            }
        })
    }
}

pub struct CleanCandidateReviewRunner;

impl AgentRunner for CleanCandidateReviewRunner {
    fn launch<'a>(
        &'a self,
        _agent: &'a Agent,
        request: &'a AgentRequest,
        _working_dir: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<AgentResponse, AgentError>> + Send + 'a>> {
        Box::pin(async move {
            match prompt_value(&request.prompt, "Step").as_deref() {
                Some("review_candidate_initial") | Some("review_candidate_repair") => {
                    Ok(AgentResponse {
                        exit_code: 0,
                        stdout: String::new(),
                        stderr: String::new(),
                        result: Some(clean_review_report(
                            &prompt_value(&request.prompt, "Input base commit").unwrap_or_default(),
                            &prompt_value(&request.prompt, "Input head commit").unwrap_or_default(),
                        )),
                    })
                }
                other => Err(AgentError::ProtocolViolation(format!(
                    "unexpected step in clean candidate review runner: {other:?}"
                ))),
            }
        })
    }
}

pub struct CleanValidationRunner;

impl AgentRunner for CleanValidationRunner {
    fn launch<'a>(
        &'a self,
        _agent: &'a Agent,
        request: &'a AgentRequest,
        _working_dir: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<AgentResponse, AgentError>> + Send + 'a>> {
        Box::pin(async move {
            match prompt_value(&request.prompt, "Step").as_deref() {
                Some("validate_candidate_initial")
                | Some("validate_candidate_repair")
                | Some("validate_after_integration_repair")
                | Some("validate_integrated") => Ok(AgentResponse {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                    result: Some(clean_validation_report("validation clean")),
                }),
                other => Err(AgentError::ProtocolViolation(format!(
                    "unexpected step in clean validation runner: {other:?}"
                ))),
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Mirror / git helpers
// ---------------------------------------------------------------------------

pub async fn ensure_test_mirror(state_root: &Path, project: &Project) -> ProjectRepoPaths {
    let paths = project_repo_paths(state_root, project.id, &project.path);
    ensure_mirror(&paths).await.expect("ensure mirror");
    paths
}

pub async fn create_mirror_only_commit(
    mirror_git_dir: &Path,
    base_commit: &str,
    workspace_ref: &str,
    message: &str,
) -> (PathBuf, String) {
    let worktree_path = unique_temp_path("ingot-runtime-mirror-only");
    git_sync(
        mirror_git_dir,
        &[
            "worktree",
            "add",
            "--detach",
            worktree_path.to_str().expect("worktree path"),
            base_commit,
        ],
    );
    git_sync(&worktree_path, &["config", "user.name", "Ingot Test"]);
    git_sync(
        &worktree_path,
        &["config", "user.email", "ingot@example.com"],
    );
    std::fs::write(worktree_path.join("tracked.txt"), message).expect("write tracked file");
    git_sync(&worktree_path, &["add", "tracked.txt"]);
    git_sync(&worktree_path, &["commit", "-m", message]);
    let commit_oid = head_oid(&worktree_path).await.expect("mirror-only head");
    git_sync(
        mirror_git_dir,
        &["update-ref", workspace_ref, commit_oid.as_str()],
    );
    (worktree_path, commit_oid.into_inner())
}

pub use ingot_test_support::git::{git_output, run_git as git_sync, temp_git_repo};

pub fn prompt_value(prompt: &str, label: &str) -> Option<String> {
    prompt.lines().find_map(|line| {
        let prefix = format!("- {label}: ");
        line.strip_prefix(&prefix).map(ToOwned::to_owned)
    })
}
