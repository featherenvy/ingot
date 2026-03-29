// Job execution pipeline: claim, heartbeat, run, finish, and workspace lifecycle.

use std::path::{Path, PathBuf};

use chrono::Utc;
use ingot_agent_protocol::adapter::AgentError;
use ingot_agent_protocol::request::AgentRequest;
use ingot_agent_protocol::response::AgentResponse;
use ingot_domain::activity::{ActivityEventType, ActivitySubject};
use ingot_domain::commit_oid::CommitOid;
use ingot_domain::git_operation::{
    GitOperation, GitOperationEntityRef, GitOperationStatus, OperationPayload,
};
use ingot_domain::ids::{GitOperationId, JobId};
use ingot_domain::item::ApprovalState;
use ingot_domain::job::{ExecutionPermission, Job, JobStatus, OutcomeClass, OutputArtifactKind};
use ingot_domain::ports::FinishJobNonSuccessParams;
use ingot_domain::ports::{
    ConflictKind, JobCompletionMutation, ProjectMutationLockPort, RepositoryError,
};
use ingot_domain::project::Project;
use ingot_domain::revision::ItemRevision;
use ingot_domain::step_id::StepId;
use ingot_domain::workspace::{Workspace, WorkspaceStatus};
use ingot_git::commands::{git, head_oid, resolve_ref_oid};
use ingot_git::commit::{JobCommitTrailers, create_daemon_job_commit, working_tree_has_changes};
use ingot_git::diff::changed_paths_between;
use ingot_store_sqlite::ClaimQueuedAgentJobExecutionParams;
use ingot_usecases::{CompleteJobCommand, rebuild_revision_context};
use ingot_workspace::remove_workspace;
use tokio::sync::OwnedSemaphorePermit;
use tokio::time::interval;
use tracing::{Instrument, debug, info, info_span, warn};

use crate::{
    JobDispatcher, PreparedRun, RunningJobResult, RuntimeError, WorkspaceLifecycle, commit_subject,
    failure_escalation_reason, non_empty_message, outcome_class_name, report,
    should_clear_item_escalation_on_success,
};

pub(crate) enum AgentRunOutcome {
    Completed(AgentResponse),
    TimedOut,
    Cancelled,
    OwnershipLostBeforeSpawn,
    OwnershipLostDuringRun,
    LaunchFailed(AgentError),
}

enum RunningJobState {
    Running,
    Cancelled,
    OwnershipLost(JobStatus),
}

impl JobDispatcher {
    pub(crate) async fn run_with_heartbeats(
        &self,
        prepared: &PreparedRun,
        request: AgentRequest,
    ) -> AgentRunOutcome {
        let timeout_duration = self.config.job_timeout;
        let lease_expires_at = self.next_lease_expiration();
        let mut dispatch_listener = self.dispatch_notify.subscribe();
        if let Err(error) = self
            .db
            .claim_queued_agent_job_execution(ClaimQueuedAgentJobExecutionParams {
                job_id: prepared.job.id,
                item_id: prepared.item.id,
                expected_item_revision_id: prepared.job.item_revision_id,
                assignment: prepared.assignment.clone(),
                lease_owner_id: self.lease_owner_id.clone(),
                lease_expires_at,
            })
            .await
        {
            return match error {
                RepositoryError::Conflict(_) => match self.db.get_job(prepared.job.id).await {
                    Ok(job) if job.state.status() == JobStatus::Cancelled => {
                        AgentRunOutcome::Cancelled
                    }
                    Ok(_) => AgentRunOutcome::OwnershipLostBeforeSpawn,
                    Err(load_error) => AgentRunOutcome::LaunchFailed(AgentError::ProcessError(
                        load_error.to_string(),
                    )),
                },
                other => AgentRunOutcome::LaunchFailed(AgentError::ProcessError(other.to_string())),
            };
        }
        info!(
            job_id = %prepared.job.id,
            agent_id = %prepared.agent.id,
            workspace_id = %prepared.assignment.workspace_id,
            lease_owner_id = %self.lease_owner_id,
            timeout_seconds = timeout_duration.as_secs(),
            "job entered running state"
        );

        #[cfg(test)]
        self.pause_before_agent_spawn().await;

        if self.job_is_cancelled(prepared.job.id).await {
            info!(
                job_id = %prepared.job.id,
                "skipping agent launch because job was cancelled before spawn"
            );
            return AgentRunOutcome::Cancelled;
        }

        let runner = self.runner.clone();
        let agent = prepared.agent.clone();
        let working_dir = PathBuf::from(&prepared.workspace.path);
        let span = info_span!(
            "job_execution",
            job_id = %prepared.job.id,
            item_id = %prepared.item.id,
            step_id = %prepared.job.step_id,
            agent_id = %prepared.agent.id,
            workspace_id = %prepared.workspace.id
        );
        let mut handle = tokio::spawn(async move {
            runner
                .launch(&agent, &request, &working_dir)
                .instrument(span)
                .await
        });
        let mut ticker = interval(self.config.heartbeat_interval);
        let timeout = tokio::time::sleep(timeout_duration);
        tokio::pin!(timeout);

        loop {
            tokio::select! {
                result = &mut handle => {
                    let result = match result {
                        Ok(result) => result,
                        Err(error) => {
                            return AgentRunOutcome::LaunchFailed(AgentError::ProcessError(error.to_string()));
                        }
                    };
                    debug!(job_id = %prepared.job.id, "job execution future resolved");
                    return match result {
                        Ok(response) => AgentRunOutcome::Completed(response),
                        Err(error) => AgentRunOutcome::LaunchFailed(error),
                    };
                }
                _ = &mut timeout => {
                    handle.abort();
                    warn!(job_id = %prepared.job.id, timeout_seconds = timeout_duration.as_secs(), "job execution timed out");
                    return AgentRunOutcome::TimedOut;
                }
                notification = dispatch_listener.notified() => {
                    match self.load_running_job_state(prepared.job.id).await {
                        Ok(RunningJobState::Cancelled) => {
                            handle.abort();
                            info!(job_id = %prepared.job.id, "cancelling running job after operator request");
                            return AgentRunOutcome::Cancelled;
                        }
                        Ok(RunningJobState::OwnershipLost(status)) => {
                            handle.abort();
                            info!(job_id = %prepared.job.id, status = ?status, "stopping runner after job lost ownership");
                            return AgentRunOutcome::OwnershipLostDuringRun;
                        }
                        Ok(RunningJobState::Running) => {
                            debug!(
                                job_id = %prepared.job.id,
                                generation = notification.generation(),
                                reason = %notification.reason(),
                                "running job woke on unrelated dispatcher notification"
                            );
                        }
                        Err(error) => {
                            warn!(
                                ?error,
                                job_id = %prepared.job.id,
                                generation = notification.generation(),
                                reason = %notification.reason(),
                                "failed to load job after dispatcher notification"
                            );
                        }
                    }
                }
                _ = ticker.tick() => {
                    match self.load_running_job_state(prepared.job.id).await {
                        Ok(RunningJobState::Cancelled) => {
                            handle.abort();
                            info!(job_id = %prepared.job.id, "cancelling running job after operator request");
                            return AgentRunOutcome::Cancelled;
                        }
                        Ok(RunningJobState::OwnershipLost(status)) => {
                            handle.abort();
                            info!(job_id = %prepared.job.id, status = ?status, "stopping runner after job lost ownership");
                            return AgentRunOutcome::OwnershipLostDuringRun;
                        }
                        Ok(RunningJobState::Running) => {}
                        Err(error) => {
                            warn!(?error, job_id = %prepared.job.id, "failed to load job during heartbeat tick");
                        }
                    }
                    let lease_expires_at = self.next_lease_expiration();
                    if let Err(error) = self.db.heartbeat_job_execution(
                        prepared.job.id,
                        prepared.item.id,
                        prepared.job.item_revision_id,
                        &self.lease_owner_id,
                        lease_expires_at,
                    ).await {
                        if matches!(&error, RepositoryError::Conflict(ConflictKind::JobNotActive)) {
                            handle.abort();
                            info!(job_id = %prepared.job.id, "stopping runner after heartbeat lost job ownership");
                            return AgentRunOutcome::OwnershipLostDuringRun;
                        }
                        warn!(?error, job_id = %prepared.job.id, "job heartbeat update failed");
                    } else {
                        debug!(job_id = %prepared.job.id, "job heartbeat updated");
                    }
                }
            }
        }
    }

    pub(crate) async fn execute_prepared_agent_job(
        &self,
        prepared: PreparedRun,
    ) -> Result<(), RuntimeError> {
        self.write_prompt_artifact(&prepared.job, &prepared.prompt)
            .await?;
        let request = AgentRequest {
            prompt: prepared.prompt.clone(),
            working_dir: prepared.workspace.path.clone(),
            may_mutate: prepared.job.execution_permission == ExecutionPermission::MayMutate,
            timeout_seconds: Some(self.config.job_timeout.as_secs()),
            output_schema: report::output_schema(prepared.job.output_artifact_kind),
        };
        match self.run_with_heartbeats(&prepared, request).await {
            AgentRunOutcome::Completed(response) => {
                self.write_response_artifacts(&prepared.job, &response)
                    .await?;
                self.finish_run(prepared, response).await?;
            }
            AgentRunOutcome::TimedOut => {
                self.fail_run(
                    &prepared,
                    OutcomeClass::TransientFailure,
                    "job_timeout",
                    Some("job execution timed out".into()),
                )
                .await?;
            }
            AgentRunOutcome::Cancelled => {
                info!(
                    job_id = %prepared.job.id,
                    "job cancelled during runtime execution"
                );
                let _ = self.recover_projected_jobs().await?;
            }
            AgentRunOutcome::OwnershipLostBeforeSpawn => {
                self.cleanup_unclaimed_prepared_agent_run(&prepared).await?;
                info!(
                    job_id = %prepared.job.id,
                    "prepared job lost ownership before agent spawn"
                );
            }
            AgentRunOutcome::OwnershipLostDuringRun => {
                info!(
                    job_id = %prepared.job.id,
                    "running job lost ownership before completion handling"
                );
            }
            AgentRunOutcome::LaunchFailed(error) => {
                warn!(?error, job_id = %prepared.job.id, "agent launch failed");
                self.fail_run(
                    &prepared,
                    OutcomeClass::TerminalFailure,
                    "agent_launch_failed",
                    Some(error.to_string()),
                )
                .await?;
            }
        }

        Ok(())
    }

    async fn load_running_job_state(
        &self,
        job_id: JobId,
    ) -> Result<RunningJobState, RepositoryError> {
        let status = self.db.get_job(job_id).await?.state.status();
        Ok(match status {
            JobStatus::Running => RunningJobState::Running,
            JobStatus::Cancelled => RunningJobState::Cancelled,
            status => RunningJobState::OwnershipLost(status),
        })
    }

    async fn finish_run(
        &self,
        prepared: PreparedRun,
        response: AgentResponse,
    ) -> Result<(), RuntimeError> {
        let current_job = self.db.get_job(prepared.job.id).await?;
        if current_job.state.status() == JobStatus::Cancelled {
            self.finalize_workspace_after_failure(&prepared).await?;
            info!(job_id = %prepared.job.id, "job was cancelled while subprocess was running");
            return Ok(());
        }

        if response.exit_code != 0 {
            warn!(
                job_id = %prepared.job.id,
                exit_code = response.exit_code,
                stderr = non_empty_message(&response.stderr).as_deref().unwrap_or(""),
                stdout = non_empty_message(&response.stdout).as_deref().unwrap_or(""),
                "agent process exited non-zero"
            );
            return self
                .fail_run(
                    &prepared,
                    OutcomeClass::TerminalFailure,
                    "agent_exit_nonzero",
                    non_empty_message(&response.stderr)
                        .or_else(|| non_empty_message(&response.stdout)),
                )
                .await;
        }

        match prepared.job.output_artifact_kind {
            OutputArtifactKind::Commit => self.finish_commit_run(prepared, response).await,
            OutputArtifactKind::ReviewReport
            | OutputArtifactKind::ValidationReport
            | OutputArtifactKind::FindingReport => self.finish_report_run(prepared, response).await,
            OutputArtifactKind::None => {
                self.fail_run(
                    &prepared,
                    OutcomeClass::TerminalFailure,
                    "unsupported_output_artifact",
                    Some("runtime does not support artifact-free jobs yet".into()),
                )
                .await
            }
        }
    }

    async fn finish_commit_run(
        &self,
        prepared: PreparedRun,
        response: AgentResponse,
    ) -> Result<(), RuntimeError> {
        if let Err(error) = self.verify_mutating_workspace_protocol(&prepared).await {
            return self
                .fail_run(
                    &prepared,
                    OutcomeClass::ProtocolViolation,
                    "workspace_protocol_violation",
                    Some(format!("{error:?}")),
                )
                .await;
        }

        let workspace_path = Path::new(&prepared.workspace.path);
        if !working_tree_has_changes(workspace_path).await? {
            return self
                .fail_run(
                    &prepared,
                    OutcomeClass::TerminalFailure,
                    "no_valid_change_set",
                    Some("authoring job completed without producing a change set".into()),
                )
                .await;
        }

        let commit_oid = self.create_commit(&prepared, &response).await?;
        self.complete_commit_run(&prepared, &commit_oid).await
    }

    async fn finish_report_run(
        &self,
        prepared: PreparedRun,
        response: AgentResponse,
    ) -> Result<(), RuntimeError> {
        if let Err(error) = self.verify_read_only_workspace_protocol(&prepared).await {
            return self
                .fail_run(
                    &prepared,
                    OutcomeClass::ProtocolViolation,
                    "workspace_protocol_violation",
                    Some(error.to_string()),
                )
                .await;
        }

        let result_payload = response.result.clone().ok_or_else(|| {
            RuntimeError::InvalidState("report job did not return structured output".into())
        });
        let result_payload = match result_payload {
            Ok(payload) => payload,
            Err(error) => {
                return self
                    .fail_run(
                        &prepared,
                        OutcomeClass::ProtocolViolation,
                        "missing_structured_result",
                        Some(error.to_string()),
                    )
                    .await;
            }
        };

        let outcome_class =
            report::parse_outcome_class(&result_payload).map_err(RuntimeError::InvalidState);
        let outcome_class = match outcome_class {
            Ok(outcome_class) => outcome_class,
            Err(error) => {
                return self
                    .fail_run(
                        &prepared,
                        OutcomeClass::ProtocolViolation,
                        "invalid_report_outcome",
                        Some(error.to_string()),
                    )
                    .await;
            }
        };

        let result_schema_version = report::schema_version(prepared.job.output_artifact_kind)
            .ok_or_else(|| {
                RuntimeError::InvalidState("report job missing schema version mapping".into())
            })?;

        if let Err(error) = self
            .complete_job_service()
            .execute(CompleteJobCommand {
                job_id: prepared.job.id,
                outcome_class,
                result_schema_version: Some(result_schema_version.to_string()),
                result_payload: Some(result_payload),
                output_commit_oid: None,
            })
            .await
        {
            return self
                .fail_run(
                    &prepared,
                    OutcomeClass::ProtocolViolation,
                    "report_completion_rejected",
                    Some(format!("{error:?}")),
                )
                .await;
        }
        self.append_activity(
            prepared.project.id,
            ActivityEventType::JobCompleted,
            ActivitySubject::Job(prepared.job.id),
            serde_json::json!({ "item_id": prepared.item.id, "outcome": outcome_class_name(outcome_class) }),
        )
        .await?;
        if prepared.job.step_id == StepId::ValidateIntegrated
            && outcome_class == OutcomeClass::Clean
        {
            let updated_item = self.db.get_item(prepared.item.id).await?;
            if updated_item.approval_state == ApprovalState::Pending {
                self.append_activity(
                    prepared.project.id,
                    ActivityEventType::ApprovalRequested,
                    ActivitySubject::Item(prepared.item.id),
                    serde_json::json!({ "job_id": prepared.job.id }),
                )
                .await?;
            }
        }

        if outcome_class == OutcomeClass::Findings {
            let project = self.db.get_project(prepared.project.id).await?;
            if project.execution_mode == ingot_domain::project::ExecutionMode::Autopilot {
                let item = self.db.get_item(prepared.item.id).await?;
                self.auto_triage_job_findings(&project, prepared.job.id, &item)
                    .await?;
            }
        }

        self.finalize_workspace_after_success(&prepared, None)
            .await?;
        self.refresh_revision_context(&prepared).await?;
        self.append_escalation_cleared_activity_if_needed(&prepared)
            .await?;
        self.auto_dispatch_projected_review(prepared.project.id, prepared.item.id)
            .await?;
        info!(
            job_id = %prepared.job.id,
            step_id = %prepared.job.step_id,
            "completed report job"
        );
        Ok(())
    }

    async fn verify_mutating_workspace_protocol(
        &self,
        prepared: &PreparedRun,
    ) -> Result<(), RuntimeError> {
        let repo_path = prepared.canonical_repo_path.as_path();
        let workspace_ref =
            prepared.workspace.workspace_ref.as_ref().ok_or_else(|| {
                RuntimeError::InvalidState("authoring workspace missing ref".into())
            })?;
        let actual_ref = resolve_ref_oid(repo_path, workspace_ref).await?;
        let actual_head = head_oid(Path::new(&prepared.workspace.path)).await?;

        if actual_ref.as_ref() != Some(&prepared.original_head_commit_oid)
            || actual_head != prepared.original_head_commit_oid
        {
            self.reset_workspace(prepared).await?;
            return Err(RuntimeError::InvalidState(
                "agent created commits or moved refs in the authoring workspace".into(),
            ));
        }

        Ok(())
    }

    async fn verify_read_only_workspace_protocol(
        &self,
        prepared: &PreparedRun,
    ) -> Result<(), RuntimeError> {
        let actual_head = head_oid(Path::new(&prepared.workspace.path)).await?;
        if actual_head != prepared.original_head_commit_oid {
            return Err(RuntimeError::InvalidState(
                "read-only job moved HEAD away from the expected commit".into(),
            ));
        }

        if working_tree_has_changes(Path::new(&prepared.workspace.path)).await? {
            return Err(RuntimeError::InvalidState(
                "read-only job dirtied the workspace".into(),
            ));
        }

        Ok(())
    }

    async fn create_commit(
        &self,
        prepared: &PreparedRun,
        response: &AgentResponse,
    ) -> Result<CommitOid, RuntimeError> {
        let _guard = self
            .project_locks
            .acquire_project_mutation(prepared.job.project_id)
            .await;

        let repo_path = prepared.canonical_repo_path.as_path();
        let workspace_ref =
            prepared.workspace.workspace_ref.clone().ok_or_else(|| {
                RuntimeError::InvalidState("authoring workspace missing ref".into())
            })?;
        let now = Utc::now();
        let mut operation = GitOperation {
            id: GitOperationId::new(),
            project_id: prepared.project.id,
            entity: GitOperationEntityRef::Job(prepared.job.id),
            payload: OperationPayload::CreateJobCommit {
                workspace_id: prepared.workspace.id,
                ref_name: workspace_ref.clone(),
                expected_old_oid: prepared.original_head_commit_oid.clone(),
                new_oid: None,
                commit_oid: None,
            },
            status: GitOperationStatus::Planned,
            created_at: now,
            completed_at: None,
        };
        self.db.create_git_operation(&operation).await?;
        self.append_activity(
            prepared.project.id,
            ActivityEventType::GitOperationPlanned,
            ActivitySubject::GitOperation(operation.id),
            serde_json::json!({ "operation_kind": operation.operation_kind(), "entity_id": operation.entity.entity_id_string() }),
        )
        .await?;

        let summary = response
            .result
            .as_ref()
            .and_then(|value| value.get("summary"))
            .and_then(|value| value.as_str())
            .unwrap_or("Authoring changes generated by Ingot");
        let commit_oid = create_daemon_job_commit(
            Path::new(&prepared.workspace.path),
            &commit_subject(&prepared.revision.title, prepared.job.step_id),
            summary,
            &JobCommitTrailers {
                operation_id: operation.id,
                item_id: prepared.item.id,
                revision_no: prepared.revision.revision_no,
                job_id: prepared.job.id,
            },
        )
        .await?;
        git(
            repo_path,
            &["update-ref", workspace_ref.as_str(), commit_oid.as_str()],
        )
        .await?;

        operation
            .payload
            .set_job_commit_result(commit_oid.clone())
            .map_err(|error| RuntimeError::InvalidState(error.to_string()))?;
        operation.status = GitOperationStatus::Applied;
        operation.completed_at = Some(Utc::now());
        self.db.update_git_operation(&operation).await?;

        Ok(commit_oid)
    }

    async fn complete_commit_run(
        &self,
        prepared: &PreparedRun,
        commit_oid: &CommitOid,
    ) -> Result<(), RuntimeError> {
        self.finalize_workspace_after_success(prepared, Some(commit_oid))
            .await?;

        self.db
            .apply_job_completion(JobCompletionMutation {
                job_id: prepared.job.id,
                item_id: prepared.item.id,
                expected_item_revision_id: prepared.job.item_revision_id,
                outcome_class: OutcomeClass::Clean,
                clear_item_escalation: should_clear_item_escalation_on_success(
                    &prepared.item,
                    &prepared.job,
                ),
                result_schema_version: None,
                result_payload: None,
                output_commit_oid: Some(commit_oid.clone()),
                findings: vec![],
                prepared_convergence_guard: None,
            })
            .await?;
        self.append_activity(
            prepared.project.id,
            ActivityEventType::JobCompleted,
            ActivitySubject::Job(prepared.job.id),
            serde_json::json!({ "item_id": prepared.item.id, "outcome": "clean" }),
        )
        .await?;

        self.refresh_revision_context(prepared).await?;
        self.append_escalation_cleared_activity_if_needed(prepared)
            .await?;
        self.auto_dispatch_projected_review(prepared.project.id, prepared.item.id)
            .await?;

        info!(job_id = %prepared.job.id, commit_oid = %commit_oid, "completed authoring job");

        Ok(())
    }

    pub(crate) async fn fail_run(
        &self,
        prepared: &PreparedRun,
        outcome_class: OutcomeClass,
        error_code: &'static str,
        error_message: Option<String>,
    ) -> Result<(), RuntimeError> {
        self.finalize_workspace_after_failure(prepared).await?;

        let status = match outcome_class {
            OutcomeClass::Cancelled => JobStatus::Cancelled,
            OutcomeClass::TransientFailure
            | OutcomeClass::TerminalFailure
            | OutcomeClass::ProtocolViolation => JobStatus::Failed,
            OutcomeClass::Clean | OutcomeClass::Findings => JobStatus::Failed,
        };
        let escalation_reason = failure_escalation_reason(&prepared.job, outcome_class);

        let error_message_log = error_message.as_deref().unwrap_or("").to_string();
        self.db
            .finish_job_non_success(FinishJobNonSuccessParams {
                job_id: prepared.job.id,
                item_id: prepared.item.id,
                expected_item_revision_id: prepared.job.item_revision_id,
                status,
                outcome_class: Some(outcome_class),
                error_code: Some(error_code.into()),
                error_message,
                escalation_reason,
            })
            .await?;
        let event_type = if outcome_class == OutcomeClass::Cancelled {
            ActivityEventType::JobCancelled
        } else {
            ActivityEventType::JobFailed
        };
        self.append_activity(
            prepared.project.id,
            event_type,
            ActivitySubject::Job(prepared.job.id),
            serde_json::json!({ "item_id": prepared.item.id, "error_code": error_code }),
        )
        .await?;
        if let Some(escalation_reason) = escalation_reason {
            self.append_activity(
                prepared.project.id,
                ActivityEventType::ItemEscalated,
                ActivitySubject::Item(prepared.item.id),
                serde_json::json!({ "reason": escalation_reason }),
            )
            .await?;
        }

        self.refresh_revision_context(prepared).await?;
        warn!(
            job_id = %prepared.job.id,
            outcome_class = ?outcome_class,
            error_code,
            error_message = %error_message_log,
            "job failed"
        );

        Ok(())
    }

    pub(crate) async fn cleanup_unclaimed_prepared_agent_run(
        &self,
        prepared: &PreparedRun,
    ) -> Result<(), RuntimeError> {
        self.cleanup_unclaimed_prepared_workspace(
            prepared.job.project_id,
            prepared.job.id,
            &prepared.workspace,
            prepared.workspace_lifecycle,
            &prepared.original_head_commit_oid,
            prepared.canonical_repo_path.as_path(),
        )
        .await
    }

    pub(crate) async fn cleanup_unclaimed_prepared_workspace(
        &self,
        project_id: ingot_domain::ids::ProjectId,
        job_id: ingot_domain::ids::JobId,
        workspace: &Workspace,
        workspace_lifecycle: WorkspaceLifecycle,
        original_head_commit_oid: &CommitOid,
        canonical_repo_path: &Path,
    ) -> Result<(), RuntimeError> {
        let _guard = self
            .project_locks
            .acquire_project_mutation(project_id)
            .await;

        let current_job = self.db.get_job(job_id).await?;
        if current_job.state.status() != JobStatus::Queued {
            return Ok(());
        }

        let mut persisted_workspace = self.db.get_workspace(workspace.id).await?;
        if persisted_workspace.state.current_job_id() != Some(job_id) {
            return Ok(());
        }

        let now = Utc::now();
        match workspace_lifecycle {
            WorkspaceLifecycle::PersistentAuthoring | WorkspaceLifecycle::PersistentIntegration => {
                persisted_workspace.release_with_head(original_head_commit_oid.clone(), now);
                self.db.update_workspace(&persisted_workspace).await?;
            }
            WorkspaceLifecycle::EphemeralReview => {
                remove_workspace(canonical_repo_path, Path::new(&persisted_workspace.path)).await?;
                persisted_workspace.mark_abandoned(now);
                self.db.update_workspace(&persisted_workspace).await?;
            }
        }

        Ok(())
    }

    pub(crate) async fn fail_job_preparation(
        &self,
        job: &Job,
        item: &ingot_domain::item::Item,
        project: &Project,
        error_code: &'static str,
        error_message: String,
    ) -> Result<(), RuntimeError> {
        let outcome_class = OutcomeClass::TerminalFailure;
        let escalation_reason = failure_escalation_reason(job, outcome_class);

        self.db
            .finish_job_non_success(FinishJobNonSuccessParams {
                job_id: job.id,
                item_id: item.id,
                expected_item_revision_id: job.item_revision_id,
                status: JobStatus::Failed,
                outcome_class: Some(outcome_class),
                error_code: Some(error_code.into()),
                error_message: Some(error_message.clone()),
                escalation_reason,
            })
            .await?;
        self.append_activity(
            project.id,
            ActivityEventType::JobFailed,
            ActivitySubject::Job(job.id),
            serde_json::json!({ "item_id": item.id, "error_code": error_code }),
        )
        .await?;
        if let Some(escalation_reason) = escalation_reason {
            self.append_activity(
                project.id,
                ActivityEventType::ItemEscalated,
                ActivitySubject::Item(item.id),
                serde_json::json!({ "reason": escalation_reason }),
            )
            .await?;
        }
        self.refresh_revision_context_for_ids(
            project.id,
            item.id,
            job.item_revision_id,
            Some(job.id),
        )
        .await?;
        warn!(
            job_id = %job.id,
            error_code,
            error_message = %error_message,
            "job failed during preparation"
        );
        Ok(())
    }

    async fn append_escalation_cleared_activity_if_needed(
        &self,
        prepared: &PreparedRun,
    ) -> Result<(), RuntimeError> {
        if !prepared.item.escalation.is_escalated() {
            return Ok(());
        }

        let item = self.db.get_item(prepared.item.id).await?;
        if item.current_revision_id != prepared.job.item_revision_id
            || item.escalation.is_escalated()
        {
            return Ok(());
        }

        self.append_activity(
            prepared.project.id,
            ActivityEventType::ItemEscalationCleared,
            ActivitySubject::Item(prepared.item.id),
            serde_json::json!({ "reason": "successful_retry", "job_id": prepared.job.id }),
        )
        .await?;

        Ok(())
    }

    async fn finalize_workspace_after_success(
        &self,
        prepared: &PreparedRun,
        head_commit_oid: Option<&CommitOid>,
    ) -> Result<(), RuntimeError> {
        let _guard = self
            .project_locks
            .acquire_project_mutation(prepared.job.project_id)
            .await;

        match prepared.workspace_lifecycle {
            WorkspaceLifecycle::PersistentAuthoring => {
                let mut workspace = self.db.get_workspace(prepared.workspace.id).await?;
                let now = Utc::now();
                workspace.release_to(WorkspaceStatus::Ready, now);
                if let Some(head_commit_oid) = head_commit_oid {
                    workspace.set_head_commit_oid(head_commit_oid.clone(), now);
                }
                self.db.update_workspace(&workspace).await?;
            }
            WorkspaceLifecycle::PersistentIntegration => {
                let mut workspace = self.db.get_workspace(prepared.workspace.id).await?;
                workspace.release_to(WorkspaceStatus::Ready, Utc::now());
                self.db.update_workspace(&workspace).await?;
            }
            WorkspaceLifecycle::EphemeralReview => {
                remove_workspace(
                    prepared.canonical_repo_path.as_path(),
                    Path::new(&prepared.workspace.path),
                )
                .await?;
                let mut workspace = self.db.get_workspace(prepared.workspace.id).await?;
                workspace.mark_abandoned(Utc::now());
                self.db.update_workspace(&workspace).await?;
            }
        }

        Ok(())
    }

    pub(crate) async fn finalize_integration_workspace_after_close(
        &self,
        project: &Project,
        workspace: &Workspace,
    ) -> Result<(), RuntimeError> {
        let repo_path = self.project_paths(project).mirror_git_dir;
        remove_workspace(repo_path.as_path(), &workspace.path).await?;
        let mut workspace = workspace.clone();
        workspace.mark_abandoned(Utc::now());
        self.db.update_workspace(&workspace).await?;
        Ok(())
    }

    async fn finalize_workspace_after_failure(
        &self,
        prepared: &PreparedRun,
    ) -> Result<(), RuntimeError> {
        let _guard = self
            .project_locks
            .acquire_project_mutation(prepared.job.project_id)
            .await;

        self.reset_workspace(prepared).await?;

        match prepared.workspace_lifecycle {
            WorkspaceLifecycle::PersistentAuthoring => {
                let mut workspace = self.db.get_workspace(prepared.workspace.id).await?;
                let now = Utc::now();
                workspace.release_with_head(prepared.original_head_commit_oid.clone(), now);
                self.db.update_workspace(&workspace).await?;
            }
            WorkspaceLifecycle::PersistentIntegration => {
                let mut workspace = self.db.get_workspace(prepared.workspace.id).await?;
                let now = Utc::now();
                workspace.release_with_head(prepared.original_head_commit_oid.clone(), now);
                self.db.update_workspace(&workspace).await?;
            }
            WorkspaceLifecycle::EphemeralReview => {
                let mut workspace = self.db.get_workspace(prepared.workspace.id).await?;
                workspace.mark_abandoned(Utc::now());
                self.db.update_workspace(&workspace).await?;
            }
        }

        Ok(())
    }

    async fn reset_workspace(&self, prepared: &PreparedRun) -> Result<(), RuntimeError> {
        match prepared.workspace_lifecycle {
            WorkspaceLifecycle::PersistentAuthoring => {
                let workspace_path = Path::new(&prepared.workspace.path);
                git(
                    workspace_path,
                    &[
                        "reset",
                        "--hard",
                        prepared.original_head_commit_oid.as_str(),
                    ],
                )
                .await?;
                git(workspace_path, &["clean", "-fd"]).await?;
                if let Some(workspace_ref) = prepared.workspace.workspace_ref.as_ref() {
                    git(
                        prepared.canonical_repo_path.as_path(),
                        &[
                            "update-ref",
                            workspace_ref.as_str(),
                            prepared.original_head_commit_oid.as_str(),
                        ],
                    )
                    .await?;
                }
            }
            WorkspaceLifecycle::PersistentIntegration => {
                let workspace_path = Path::new(&prepared.workspace.path);
                git(
                    workspace_path,
                    &[
                        "reset",
                        "--hard",
                        prepared.original_head_commit_oid.as_str(),
                    ],
                )
                .await?;
                git(workspace_path, &["clean", "-fd"]).await?;
                if let Some(workspace_ref) = prepared.workspace.workspace_ref.as_ref() {
                    git(
                        prepared.canonical_repo_path.as_path(),
                        &[
                            "update-ref",
                            workspace_ref.as_str(),
                            prepared.original_head_commit_oid.as_str(),
                        ],
                    )
                    .await?;
                }
            }
            WorkspaceLifecycle::EphemeralReview => {
                remove_workspace(
                    prepared.canonical_repo_path.as_path(),
                    Path::new(&prepared.workspace.path),
                )
                .await?;
            }
        }
        Ok(())
    }

    async fn refresh_revision_context(&self, prepared: &PreparedRun) -> Result<(), RuntimeError> {
        self.refresh_revision_context_for_ids(
            prepared.project.id,
            prepared.item.id,
            prepared.revision.id,
            Some(prepared.job.id),
        )
        .await
    }

    pub(crate) async fn refresh_revision_context_for_ids(
        &self,
        project_id: ingot_domain::ids::ProjectId,
        item_id: ingot_domain::ids::ItemId,
        revision_id: ingot_domain::ids::ItemRevisionId,
        updated_from_job_id: Option<ingot_domain::ids::JobId>,
    ) -> Result<(), RuntimeError> {
        let project = self.db.get_project(project_id).await?;
        let item = self.db.get_item(item_id).await?;
        let revision = self.db.get_revision(revision_id).await?;
        let jobs = self.db.list_jobs_by_item(item.id).await?;
        let authoring_head_commit_oid = self
            .current_authoring_head_for_revision_with_workspace(&revision, &jobs)
            .await?;
        let authoring_base_commit_oid = self.effective_authoring_base_commit_oid(&revision).await?;
        let changed_paths = if let (Some(base_commit_oid), Some(head_commit_oid)) = (
            authoring_base_commit_oid.as_ref(),
            authoring_head_commit_oid.as_ref(),
        ) {
            changed_paths_between(
                self.project_paths(&project).mirror_git_dir.as_path(),
                base_commit_oid,
                head_commit_oid,
            )
            .await?
        } else {
            Vec::new()
        };
        let context = rebuild_revision_context(
            &item,
            &revision,
            &jobs,
            authoring_head_commit_oid,
            changed_paths,
            updated_from_job_id,
            Utc::now(),
        );
        self.db.upsert_revision_context(&context).await?;
        Ok(())
    }

    pub(crate) async fn current_authoring_head_for_revision_with_workspace(
        &self,
        revision: &ItemRevision,
        jobs: &[Job],
    ) -> Result<Option<CommitOid>, RuntimeError> {
        let workspace = self
            .db
            .find_authoring_workspace_for_revision(revision.id)
            .await?;
        Ok(
            ingot_usecases::dispatch::current_authoring_head_for_revision_with_workspace(
                revision,
                jobs,
                workspace.as_ref(),
            ),
        )
    }

    pub(crate) async fn effective_authoring_base_commit_oid(
        &self,
        revision: &ItemRevision,
    ) -> Result<Option<CommitOid>, RuntimeError> {
        let workspace = self
            .db
            .find_authoring_workspace_for_revision(revision.id)
            .await?;
        Ok(
            ingot_usecases::dispatch::effective_authoring_base_commit_oid(
                revision,
                workspace.as_ref(),
            ),
        )
    }
}

pub(crate) async fn run_prepared_agent_job(
    dispatcher: JobDispatcher,
    prepared: PreparedRun,
    _permit: OwnedSemaphorePermit,
) -> RunningJobResult {
    let job_id = prepared.job.id;
    RunningJobResult {
        job_id,
        result: dispatcher.execute_prepared_agent_job(prepared).await,
    }
}
