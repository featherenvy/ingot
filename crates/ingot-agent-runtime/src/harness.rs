// Harness validation execution and harness profile loading.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use chrono::Utc;
use glob::glob;
use ingot_domain::activity::{ActivityEventType, ActivitySubject};
use ingot_domain::harness::{HarnessCommand, HarnessProfile, HarnessProfileError};
use ingot_domain::ids::WorkspaceId;
use ingot_domain::item::ApprovalState;
use ingot_domain::job::{
    ExecutionPermission, Job, JobAssignment, JobStatus, OutcomeClass, PhaseKind,
};
use ingot_domain::ports::ProjectMutationLockPort;
use ingot_domain::step_id::StepId;
use ingot_domain::workspace::WorkspaceKind;
use ingot_git::diff::changed_paths_between;
use ingot_store_sqlite::{FinishJobNonSuccessParams, StartJobExecutionParams};
use ingot_usecases::{CompleteJobCommand, rebuild_revision_context};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::OwnedSemaphorePermit;
use tokio::time::interval;
use tracing::{debug, info, warn};

use crate::{JobDispatcher, RunningJobResult, RuntimeError, report};

#[cfg(test)]
use crate::PreSpawnPausePoint;

#[derive(Debug, Clone)]
pub(crate) struct PreparedHarnessValidation {
    pub(crate) harness: HarnessProfile,
    pub(crate) job_id: ingot_domain::ids::JobId,
    pub(crate) item_id: ingot_domain::ids::ItemId,
    pub(crate) project_id: ingot_domain::ids::ProjectId,
    pub(crate) revision_id: ingot_domain::ids::ItemRevisionId,
    pub(crate) workspace_id: WorkspaceId,
    pub(crate) workspace_path: PathBuf,
    pub(crate) step_id: StepId,
}

pub(crate) enum PrepareHarnessValidationOutcome {
    NotPrepared,
    FailedBeforeLaunch,
    Prepared(Box<PreparedHarnessValidation>),
}

#[derive(Debug, Clone, Default)]
pub(crate) struct HarnessPromptContext {
    pub(crate) commands: Vec<HarnessCommand>,
    pub(crate) skills: Vec<ResolvedHarnessSkill>,
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedHarnessSkill {
    pub(crate) relative_path: String,
    pub(crate) contents: String,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum HarnessLoadError {
    #[error("failed to read harness profile at {}: {source}", path.display())]
    ReadProfile { path: PathBuf, source: io::Error },
    #[error("invalid harness profile at {}: {source}", path.display())]
    InvalidProfile {
        path: PathBuf,
        source: HarnessProfileError,
    },
    #[error("failed to canonicalize project path {}: {source}", path.display())]
    CanonicalizeProjectPath { path: PathBuf, source: io::Error },
    #[error("invalid harness skill glob '{pattern}': {message}")]
    InvalidSkillGlob { pattern: String, message: String },
    #[error("failed to resolve harness skill path from pattern '{pattern}': {source}")]
    ResolveSkillPath { pattern: String, source: io::Error },
    #[error(
        "harness skill path from pattern '{pattern}' escapes project root {}: {}",
        project_path.display(),
        path.display()
    )]
    SkillPathEscapesProjectRoot {
        pattern: String,
        project_path: PathBuf,
        path: PathBuf,
    },
    #[error("failed to read harness skill {}: {source}", path.display())]
    ReadSkill { path: PathBuf, source: io::Error },
}

impl HarnessLoadError {
    pub(crate) fn error_code(&self) -> &'static str {
        match self {
            Self::InvalidProfile { .. }
            | Self::InvalidSkillGlob { .. }
            | Self::SkillPathEscapesProjectRoot { .. } => "invalid_harness_profile",
            Self::ReadProfile { .. }
            | Self::CanonicalizeProjectPath { .. }
            | Self::ResolveSkillPath { .. }
            | Self::ReadSkill { .. } => "harness_io_error",
        }
    }
}

pub(crate) struct HarnessCommandResult {
    pub(crate) exit_code: i32,
    pub(crate) stdout_tail: String,
    pub(crate) stderr_tail: String,
    pub(crate) timed_out: bool,
    pub(crate) cancelled: bool,
}

impl JobDispatcher {
    pub(crate) async fn execute_harness_validation(
        &self,
        queued_job: Job,
    ) -> Result<(), RuntimeError> {
        match self.prepare_harness_validation(queued_job).await? {
            PrepareHarnessValidationOutcome::NotPrepared
            | PrepareHarnessValidationOutcome::FailedBeforeLaunch => Ok(()),
            PrepareHarnessValidationOutcome::Prepared(prepared) => {
                self.run_prepared_harness_validation(*prepared).await
            }
        }
    }

    pub(crate) async fn prepare_harness_validation(
        &self,
        queued_job: Job,
    ) -> Result<PrepareHarnessValidationOutcome, RuntimeError> {
        let _guard = self
            .project_locks
            .acquire_project_mutation(queued_job.project_id)
            .await;

        let mut job = self.db.get_job(queued_job.id).await?;
        if job.state.status() != JobStatus::Queued || !is_daemon_only_validation(&job) {
            return Ok(PrepareHarnessValidationOutcome::NotPrepared);
        }

        let item = self.db.get_item(job.item_id).await?;
        if item.current_revision_id != job.item_revision_id {
            return Ok(PrepareHarnessValidationOutcome::NotPrepared);
        }

        let revision = self.db.get_revision(job.item_revision_id).await?;
        let project = self.db.get_project(job.project_id).await?;
        let harness = match load_harness_profile(&project.path) {
            Ok(harness) => harness,
            Err(error) => {
                self.fail_job_preparation(
                    &job,
                    &item,
                    &project,
                    error.error_code(),
                    error.to_string(),
                )
                .await?;
                return Ok(PrepareHarnessValidationOutcome::FailedBeforeLaunch);
            }
        };

        let paths = self.refresh_project_mirror(&project).await?;
        let now = Utc::now();
        let (workspace, workspace_exists) = match job.workspace_kind {
            WorkspaceKind::Authoring | WorkspaceKind::Integration => {
                let (workspace, _lifecycle, workspace_exists) = self
                    .prepare_workspace(
                        &project,
                        paths.mirror_git_dir.as_path(),
                        &paths.worktree_root,
                        &revision,
                        &job,
                        now,
                    )
                    .await?;
                (workspace, workspace_exists)
            }
            _ => {
                return Err(RuntimeError::InvalidState(format!(
                    "unsupported workspace kind {:?} for harness validation",
                    job.workspace_kind
                )));
            }
        };

        if workspace_exists {
            self.db.update_workspace(&workspace).await?;
        } else {
            self.db.create_workspace(&workspace).await?;
        }

        job.assign(JobAssignment::new(workspace.id));
        self.db.update_job(&job).await?;
        self.db
            .start_job_execution(StartJobExecutionParams {
                job_id: job.id,
                item_id: job.item_id,
                expected_item_revision_id: job.item_revision_id,
                workspace_id: Some(workspace.id),
                agent_id: None,
                lease_owner_id: self.lease_owner_id.clone(),
                process_pid: None,
                lease_expires_at: now + self.lease_ttl(),
            })
            .await?;

        Ok(PrepareHarnessValidationOutcome::Prepared(Box::new(
            PreparedHarnessValidation {
                harness,
                job_id: job.id,
                item_id: job.item_id,
                project_id: project.id,
                revision_id: job.item_revision_id,
                workspace_id: workspace.id,
                workspace_path: workspace.path.clone(),
                step_id: job.step_id,
            },
        )))
    }

    async fn run_prepared_harness_validation(
        &self,
        prepared: PreparedHarnessValidation,
    ) -> Result<(), RuntimeError> {
        let mut checks = Vec::new();
        let mut findings = Vec::new();

        for command in &prepared.harness.commands {
            if self.harness_validation_cancelled(&prepared).await? {
                info!(
                    job_id = %prepared.job_id,
                    "daemon-only validation cancelled before next harness command"
                );
                return Ok(());
            }

            let result = self
                .run_harness_command_with_heartbeats(&prepared, command)
                .await;
            if result.cancelled {
                info!(
                    job_id = %prepared.job_id,
                    command = %command.name,
                    "daemon-only validation cancelled while harness command was running"
                );
                return Ok(());
            }
            let status = if result.timed_out || result.exit_code != 0 {
                "fail"
            } else {
                "pass"
            };
            let summary = if result.timed_out {
                format!(
                    "command '{}' timed out after {:?}",
                    command.name, command.timeout
                )
            } else if result.exit_code != 0 {
                format!(
                    "command '{}' exited with code {}",
                    command.name, result.exit_code
                )
            } else {
                format!("command '{}' passed", command.name)
            };
            checks.push(serde_json::json!({
                "name": command.name,
                "status": status,
                "summary": summary,
            }));
            if status == "fail" {
                let mut evidence = Vec::new();
                if !result.stdout_tail.is_empty() {
                    evidence.push(format!("stdout:\n{}", result.stdout_tail));
                }
                if !result.stderr_tail.is_empty() {
                    evidence.push(format!("stderr:\n{}", result.stderr_tail));
                }
                if evidence.is_empty() {
                    evidence.push(format!("exit code: {}", result.exit_code));
                }
                findings.push(serde_json::json!({
                    "finding_key": command.name,
                    "code": command.name,
                    "severity": "high",
                    "summary": summary,
                    "paths": [],
                    "evidence": evidence,
                }));
            }
        }

        let outcome = if findings.is_empty() {
            "clean"
        } else {
            "findings"
        };
        let result_summary = if findings.is_empty() {
            "all harness checks passed".to_string()
        } else {
            format!(
                "{} of {} harness checks failed",
                findings.len(),
                checks.len()
            )
        };
        let result_payload = serde_json::json!({
            "outcome": outcome,
            "summary": result_summary,
            "checks": checks,
            "findings": findings,
        });
        let outcome_class = if findings.is_empty() {
            OutcomeClass::Clean
        } else {
            OutcomeClass::Findings
        };

        if self.harness_validation_cancelled(&prepared).await? {
            info!(
                job_id = %prepared.job_id,
                "daemon-only validation cancelled before completion"
            );
            return Ok(());
        }

        if let Err(error) = self
            .complete_job_service()
            .execute(CompleteJobCommand {
                job_id: prepared.job_id,
                outcome_class,
                result_schema_version: Some(report::VALIDATION_REPORT_V1.to_string()),
                result_payload: Some(result_payload),
                output_commit_oid: None,
            })
            .await
        {
            let current_job = self.db.get_job(prepared.job_id).await?;
            if current_job.state.status() == JobStatus::Cancelled {
                info!(
                    job_id = %prepared.job_id,
                    "daemon-only validation was cancelled before completion was persisted"
                );
                return Ok(());
            }
            warn!(?error, job_id = %prepared.job_id, "harness validation completion failed");
            self.db
                .finish_job_non_success(FinishJobNonSuccessParams {
                    job_id: prepared.job_id,
                    item_id: prepared.item_id,
                    expected_item_revision_id: prepared.revision_id,
                    status: JobStatus::Failed,
                    outcome_class: Some(OutcomeClass::TerminalFailure),
                    error_code: Some("harness_command_failed".into()),
                    error_message: Some(format!("{error:?}")),
                    escalation_reason: None,
                })
                .await?;
            return Ok(());
        }

        self.append_activity(
            prepared.project_id,
            ActivityEventType::JobCompleted,
            ActivitySubject::Job(prepared.job_id),
            serde_json::json!({ "item_id": prepared.item_id, "outcome": outcome }),
        )
        .await?;

        if prepared.step_id == StepId::ValidateIntegrated && outcome_class == OutcomeClass::Clean {
            let updated_item = self.db.get_item(prepared.item_id).await?;
            if updated_item.approval_state == ApprovalState::Pending {
                self.append_activity(
                    prepared.project_id,
                    ActivityEventType::ApprovalRequested,
                    ActivitySubject::Item(prepared.item_id),
                    serde_json::json!({ "job_id": prepared.job_id }),
                )
                .await?;
            }
        }

        if outcome_class == OutcomeClass::Findings {
            let project = self.db.get_project(prepared.project_id).await?;
            if project.execution_mode == ingot_domain::project::ExecutionMode::Autopilot {
                let item = self.db.get_item(prepared.item_id).await?;
                self.auto_triage_job_findings(&project, prepared.job_id, &item)
                    .await?;
            }
        }

        let revision = self.db.get_revision(prepared.revision_id).await?;
        let item = self.db.get_item(prepared.item_id).await?;
        let jobs = self.db.list_jobs_by_item(prepared.item_id).await?;
        let authoring_workspace = self
            .db
            .find_authoring_workspace_for_revision(prepared.revision_id)
            .await?;
        let authoring_head =
            ingot_usecases::dispatch::current_authoring_head_for_revision_with_workspace(
                &revision,
                &jobs,
                authoring_workspace.as_ref(),
            );
        let changed_paths = if let Some(ref head) = authoring_head {
            let base = revision
                .seed
                .seed_commit_oid()
                .or_else(|| {
                    authoring_workspace
                        .as_ref()
                        .and_then(|ws| ws.state.base_commit_oid())
                })
                .unwrap_or(head);
            let project = self.db.get_project(prepared.project_id).await?;
            let paths = self.refresh_project_mirror(&project).await?;
            changed_paths_between(&paths.mirror_git_dir, base, head)
                .await
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let context = rebuild_revision_context(
            &item,
            &revision,
            &jobs,
            authoring_head,
            changed_paths,
            Some(prepared.job_id),
            Utc::now(),
        );
        self.db.upsert_revision_context(&context).await?;

        self.auto_dispatch_projected_review(prepared.project_id, prepared.item_id)
            .await?;

        info!(
            job_id = %prepared.job_id,
            step_id = %prepared.step_id,
            outcome = outcome,
            "completed harness validation"
        );
        Ok(())
    }

    async fn harness_validation_cancelled(
        &self,
        prepared: &PreparedHarnessValidation,
    ) -> Result<bool, RuntimeError> {
        Ok(self.db.get_job(prepared.job_id).await?.state.status() == JobStatus::Cancelled)
    }

    async fn refresh_daemon_validation_heartbeat(&self, prepared: &PreparedHarnessValidation) {
        let lease_expires_at = self.next_lease_expiration();
        if let Err(error) = self
            .db
            .heartbeat_job_execution(
                prepared.job_id,
                prepared.item_id,
                prepared.revision_id,
                &self.lease_owner_id,
                lease_expires_at,
            )
            .await
        {
            warn!(
                ?error,
                job_id = %prepared.job_id,
                "daemon-only validation heartbeat update failed"
            );
        } else {
            debug!(
                job_id = %prepared.job_id,
                "daemon-only validation heartbeat updated"
            );
        }
    }

    pub(crate) async fn run_harness_command_with_heartbeats(
        &self,
        prepared: &PreparedHarnessValidation,
        command_spec: &HarnessCommand,
    ) -> HarnessCommandResult {
        let mut dispatch_listener = self.dispatch_notify.subscribe();
        let mut command = tokio::process::Command::new("sh");
        command
            .arg("-c")
            .arg(&command_spec.run)
            .current_dir(&prepared.workspace_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        command.process_group(0);

        #[cfg(test)]
        self.pause_before_pre_spawn_guard(PreSpawnPausePoint::HarnessBeforeSpawn)
            .await;

        if self.job_is_cancelled(prepared.job_id).await {
            info!(
                job_id = %prepared.job_id,
                command = %command_spec.name,
                "skipping harness command because job was cancelled before spawn"
            );
            return build_harness_command_result(
                -1,
                Ok(String::new()),
                Ok(String::new()),
                false,
                true,
                vec!["command cancelled".to_string()],
            );
        }

        let child = command.spawn();

        let mut child = match child {
            Ok(child) => child,
            Err(error) => {
                return HarnessCommandResult {
                    exit_code: -1,
                    stdout_tail: String::new(),
                    stderr_tail: format!("failed to spawn command: {error}"),
                    timed_out: false,
                    cancelled: false,
                };
            }
        };

        let stdout_task = spawn_pipe_reader(child.stdout.take());
        let stderr_task = spawn_pipe_reader(child.stderr.take());
        let mut ticker = interval(self.config.heartbeat_interval);
        let timeout = tokio::time::sleep(command_spec.timeout);
        tokio::pin!(timeout);

        loop {
            tokio::select! {
                result = child.wait() => {
                    match result {
                        Ok(status) => {
                            return build_harness_command_result(
                                status.code().unwrap_or(-1),
                                collect_pipe_output(stdout_task, "stdout").await,
                                collect_pipe_output(stderr_task, "stderr").await,
                                false,
                                false,
                                Vec::new(),
                            );
                        }
                        Err(error) => {
                            return build_harness_command_result(
                                -1,
                                collect_pipe_output(stdout_task, "stdout").await,
                                collect_pipe_output(stderr_task, "stderr").await,
                                false,
                                false,
                                vec![format!("command I/O error: {error}")],
                            );
                        }
                    }
                }
                _ = &mut timeout => {
                    if let Ok(Some(status)) = child.try_wait() {
                        return build_harness_command_result(
                            status.code().unwrap_or(-1),
                            collect_pipe_output(stdout_task, "stdout").await,
                            collect_pipe_output(stderr_task, "stderr").await,
                            false,
                            false,
                            Vec::new(),
                        );
                    }
                    let mut notes = vec!["command timed out".to_string()];
                    if let Err(error) = terminate_harness_command(&mut child).await {
                        notes.push(format!("failed to terminate timed out command: {error}"));
                    }
                    if let Err(error) = child.wait().await {
                        notes.push(format!("failed to reap timed out command: {error}"));
                    }
                    return build_harness_command_result(
                        -1,
                        collect_pipe_output(stdout_task, "stdout").await,
                        collect_pipe_output(stderr_task, "stderr").await,
                        true,
                        false,
                        notes,
                    );
                }
                notification = dispatch_listener.notified() => {
                    match self.db.get_job(prepared.job_id).await {
                        Ok(job) if job.state.status() == JobStatus::Cancelled => {
                            let mut notes = vec!["command cancelled".to_string()];
                            if let Err(error) = terminate_harness_command(&mut child).await {
                                notes.push(format!("failed to terminate cancelled command: {error}"));
                            }
                            if let Err(error) = child.wait().await {
                                notes.push(format!("failed to reap cancelled command: {error}"));
                            }
                            return build_harness_command_result(
                                -1,
                                collect_pipe_output(stdout_task, "stdout").await,
                                collect_pipe_output(stderr_task, "stderr").await,
                                false,
                                true,
                                notes,
                            );
                        }
                        Ok(_) => {
                            debug!(
                                job_id = %prepared.job_id,
                                command = %command_spec.name,
                                generation = notification.generation(),
                                reason = %notification.reason(),
                                "harness command woke on unrelated dispatcher notification"
                            );
                        }
                        Err(error) => {
                            warn!(
                                ?error,
                                job_id = %prepared.job_id,
                                command = %command_spec.name,
                                generation = notification.generation(),
                                reason = %notification.reason(),
                                "failed to load harness job after dispatcher notification"
                            );
                        }
                    }
                }
                _ = ticker.tick() => {
                    match self.db.get_job(prepared.job_id).await {
                        Ok(job) if job.state.status() == JobStatus::Cancelled => {
                            let mut notes = vec!["command cancelled".to_string()];
                            if let Err(error) = terminate_harness_command(&mut child).await {
                                notes.push(format!("failed to terminate cancelled command: {error}"));
                            }
                            if let Err(error) = child.wait().await {
                                notes.push(format!("failed to reap cancelled command: {error}"));
                            }
                            return build_harness_command_result(
                                -1,
                                collect_pipe_output(stdout_task, "stdout").await,
                                collect_pipe_output(stderr_task, "stderr").await,
                                false,
                                true,
                                notes,
                            );
                        }
                        Ok(_) => {}
                        Err(error) => {
                            warn!(
                                ?error,
                                job_id = %prepared.job_id,
                                "failed to load daemon-only validation job during heartbeat tick"
                            );
                        }
                    }
                    self.refresh_daemon_validation_heartbeat(prepared).await;
                }
            }
        }
    }
}

pub(crate) async fn run_prepared_harness_validation_job(
    dispatcher: JobDispatcher,
    prepared: PreparedHarnessValidation,
    _permit: OwnedSemaphorePermit,
) -> RunningJobResult {
    let job_id = prepared.job_id;
    RunningJobResult {
        job_id,
        result: dispatcher.run_prepared_harness_validation(prepared).await,
    }
}

pub(crate) fn is_daemon_only_validation(job: &Job) -> bool {
    job.execution_permission == ExecutionPermission::DaemonOnly
        && job.phase_kind == PhaseKind::Validate
}

pub(crate) fn read_harness_profile_if_present(
    project_path: &Path,
) -> Result<Option<HarnessProfile>, HarnessLoadError> {
    let path = project_path.join(".ingot/harness.toml");
    match std::fs::read_to_string(&path) {
        Ok(content) => HarnessProfile::from_toml(&content)
            .map(Some)
            .map_err(|source| HarnessLoadError::InvalidProfile { path, source }),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(HarnessLoadError::ReadProfile { path, source }),
    }
}

fn load_harness_profile(project_path: &Path) -> Result<HarnessProfile, HarnessLoadError> {
    Ok(read_harness_profile_if_present(project_path)?.unwrap_or_default())
}

pub(crate) fn resolve_harness_prompt_context(
    project_path: &Path,
) -> Result<HarnessPromptContext, HarnessLoadError> {
    let harness = load_harness_profile(project_path)?;
    Ok(HarnessPromptContext {
        commands: harness.commands,
        skills: resolve_harness_skills(project_path, &harness.skills.paths)?,
    })
}

fn resolve_harness_skills(
    project_path: &Path,
    patterns: &[String],
) -> Result<Vec<ResolvedHarnessSkill>, HarnessLoadError> {
    let canonical_project_path = std::fs::canonicalize(project_path).map_err(|source| {
        HarnessLoadError::CanonicalizeProjectPath {
            path: project_path.to_path_buf(),
            source,
        }
    })?;
    let mut seen = BTreeSet::new();
    let mut resolved = Vec::new();
    for pattern in patterns {
        let pattern_path = project_path.join(pattern);
        let pattern_glob = pattern_path.to_string_lossy().into_owned();
        let mut matches = Vec::new();
        for entry in glob(&pattern_glob).map_err(|error| HarnessLoadError::InvalidSkillGlob {
            pattern: pattern.clone(),
            message: error.msg.to_string(),
        })? {
            match entry {
                Ok(path) => matches.push(path),
                Err(error) => {
                    return Err(HarnessLoadError::ResolveSkillPath {
                        pattern: pattern.clone(),
                        source: io::Error::new(error.error().kind(), error.error().to_string()),
                    });
                }
            }
        }
        matches.sort();
        for path in matches {
            if !path.is_file() {
                continue;
            }
            let canonical_path = std::fs::canonicalize(&path).map_err(|source| {
                HarnessLoadError::ResolveSkillPath {
                    pattern: pattern.clone(),
                    source,
                }
            })?;
            let relative_path = canonical_path
                .strip_prefix(&canonical_project_path)
                .map_err(|_| HarnessLoadError::SkillPathEscapesProjectRoot {
                    pattern: pattern.clone(),
                    project_path: canonical_project_path.clone(),
                    path: canonical_path.clone(),
                })?
                .display()
                .to_string();
            if !seen.insert(relative_path.clone()) {
                continue;
            }
            let contents = std::fs::read_to_string(&canonical_path).map_err(|source| {
                HarnessLoadError::ReadSkill {
                    path: canonical_path.clone(),
                    source,
                }
            })?;
            resolved.push(ResolvedHarnessSkill {
                relative_path,
                contents,
            });
        }
    }
    Ok(resolved)
}

fn build_harness_command_result(
    exit_code: i32,
    stdout: Result<String, String>,
    stderr: Result<String, String>,
    timed_out: bool,
    cancelled: bool,
    mut notes: Vec<String>,
) -> HarnessCommandResult {
    let stdout_tail = match stdout {
        Ok(output) => tail_lines(&output, 50),
        Err(error) => {
            notes.push(error);
            String::new()
        }
    };
    let stderr_tail = match stderr {
        Ok(output) => {
            if output.is_empty() {
                notes.join("\n\n")
            } else {
                let mut parts = notes;
                parts.push(tail_lines(&output, 50));
                parts.join("\n\n")
            }
        }
        Err(error) => {
            notes.push(error);
            notes.join("\n\n")
        }
    };
    HarnessCommandResult {
        exit_code,
        stdout_tail,
        stderr_tail,
        timed_out,
        cancelled,
    }
}

fn spawn_pipe_reader<R>(pipe: Option<R>) -> tokio::task::JoinHandle<io::Result<String>>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move { read_pipe_to_string(pipe).await })
}

async fn collect_pipe_output(
    handle: tokio::task::JoinHandle<io::Result<String>>,
    stream_name: &str,
) -> Result<String, String> {
    match handle.await {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(error)) => Err(format!("failed to read {stream_name}: {error}")),
        Err(error) => Err(format!("{stream_name} reader task failed: {error}")),
    }
}

async fn read_pipe_to_string<R>(pipe: Option<R>) -> io::Result<String>
where
    R: AsyncRead + Unpin,
{
    let Some(mut pipe) = pipe else {
        return Ok(String::new());
    };
    let mut output = Vec::new();
    pipe.read_to_end(&mut output).await?;
    Ok(String::from_utf8_lossy(&output).into_owned())
}

#[cfg(unix)]
async fn terminate_harness_command(child: &mut tokio::process::Child) -> io::Result<()> {
    let Some(pid) = child.id() else {
        return Ok(());
    };
    let result = unsafe { libc::killpg(pid as i32, libc::SIGKILL) };
    if result == 0 {
        Ok(())
    } else {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        }
    }
}

#[cfg(not(unix))]
async fn terminate_harness_command(child: &mut tokio::process::Child) -> io::Result<()> {
    child.kill().await
}

fn tail_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() <= n {
        s.to_string()
    } else {
        lines[lines.len() - n..].join("\n")
    }
}
