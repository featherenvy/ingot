use super::*;
use crate::dispatcher::completion::should_clear_item_escalation_on_success;
use crate::dispatcher::prompt::{
    commit_subject, non_empty_message, outcome_class_name, output_schema_for_job,
    report_outcome_class, result_schema_version,
};

impl JobDispatcher {
    pub(super) async fn run_with_heartbeats(
        &self,
        prepared: &PreparedRun,
        request: AgentRequest,
    ) -> Result<AgentResponse, AgentError> {
        let timeout_duration = self.config.job_timeout;
        let lease_expires_at = Utc::now() + ChronoDuration::minutes(30);
        self.db
            .start_job_execution(StartJobExecutionParams {
                job_id: prepared.job.id,
                item_id: prepared.item.id,
                expected_item_revision_id: prepared.job.item_revision_id,
                workspace_id: Some(prepared.workspace.id),
                agent_id: Some(prepared.agent.id),
                lease_owner_id: self.lease_owner_id.clone(),
                process_pid: None,
                lease_expires_at,
            })
            .await
            .map_err(|error| AgentError::ProcessError(error.to_string()))?;
        info!(
            job_id = %prepared.job.id,
            agent_id = %prepared.agent.id,
            workspace_id = %prepared.workspace.id,
            lease_owner_id = %self.lease_owner_id,
            timeout_seconds = timeout_duration.as_secs(),
            "job entered running state"
        );

        #[cfg(test)]
        self.pause_before_pre_spawn_guard(PreSpawnPausePoint::AgentBeforeSpawn)
            .await;

        if self.job_is_cancelled(prepared.job.id).await {
            info!(
                job_id = %prepared.job.id,
                "skipping agent launch because job was cancelled before spawn"
            );
            return Err(AgentError::ProcessError("job cancelled".into()));
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
        let mut dispatch_listener = self.dispatch_notify.subscribe();
        let timeout = tokio::time::sleep(timeout_duration);
        tokio::pin!(timeout);

        loop {
            tokio::select! {
                result = &mut handle => {
                    let result = result.map_err(|error| AgentError::ProcessError(error.to_string()))?;
                    debug!(job_id = %prepared.job.id, "job execution future resolved");
                    return result;
                }
                _ = &mut timeout => {
                    handle.abort();
                    warn!(job_id = %prepared.job.id, timeout_seconds = timeout_duration.as_secs(), "job execution timed out");
                    return Err(AgentError::Timeout);
                }
                _ = dispatch_listener.notified() => {
                    match self.db.get_job(prepared.job.id).await {
                        Ok(job) if job.state.status() == JobStatus::Cancelled => {
                            handle.abort();
                            info!(job_id = %prepared.job.id, "cancelling running job after operator request");
                            return Err(AgentError::ProcessError("job cancelled".into()));
                        }
                        Ok(_) => {
                            debug!(job_id = %prepared.job.id, "running job woke on unrelated dispatcher notification");
                        }
                        Err(error) => {
                            warn!(?error, job_id = %prepared.job.id, "failed to load job after dispatcher notification");
                        }
                    }
                }
                _ = ticker.tick() => {
                    match self.db.get_job(prepared.job.id).await {
                        Ok(job) if job.state.status() == JobStatus::Cancelled => {
                            handle.abort();
                            info!(job_id = %prepared.job.id, "cancelling running job after operator request");
                            return Err(AgentError::ProcessError("job cancelled".into()));
                        }
                        Ok(_) => {}
                        Err(error) => {
                            warn!(?error, job_id = %prepared.job.id, "failed to load job during heartbeat tick");
                        }
                    }
                    let lease_expires_at = Utc::now() + ChronoDuration::minutes(30);
                    if let Err(error) = self.db.heartbeat_job_execution(
                        prepared.job.id,
                        prepared.item.id,
                        prepared.job.item_revision_id,
                        &self.lease_owner_id,
                        lease_expires_at,
                    ).await {
                        warn!(?error, job_id = %prepared.job.id, "job heartbeat update failed");
                    } else {
                        debug!(job_id = %prepared.job.id, "job heartbeat updated");
                    }
                }
            }
        }
    }

    pub(super) async fn execute_prepared_agent_job(
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
            output_schema: output_schema_for_job(&prepared.job),
        };
        let response = self.run_with_heartbeats(&prepared, request).await;

        match response {
            Ok(response) => {
                self.write_response_artifacts(&prepared.job, &response)
                    .await?;
                self.finish_run(prepared, response).await?;
            }
            Err(AgentError::Timeout) => {
                self.fail_run(
                    &prepared,
                    OutcomeClass::TransientFailure,
                    "job_timeout",
                    Some("job execution timed out".into()),
                )
                .await?;
            }
            Err(error) => {
                let current_job = self.db.get_job(prepared.job.id).await?;
                if current_job.state.status() == JobStatus::Cancelled {
                    self.finalize_workspace_after_failure(&prepared).await?;
                    info!(
                        job_id = %prepared.job.id,
                        "job cancelled during runtime execution"
                    );
                    let _ = self.recover_projected_review_jobs().await?;
                    return Ok(());
                }
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

    pub(super) async fn finish_run(
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
            report_outcome_class(&result_payload).map_err(RuntimeError::InvalidState);
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

        let result_schema_version = result_schema_version(prepared.job.output_artifact_kind)
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
            "job",
            prepared.job.id.to_string(),
            serde_json::json!({ "item_id": prepared.item.id, "outcome": outcome_class_name(outcome_class) }),
        )
        .await?;
        if prepared.job.step_id == "validate_integrated" && outcome_class == OutcomeClass::Clean {
            let updated_item = self.db.get_item(prepared.item.id).await?;
            if updated_item.approval_state == ApprovalState::Pending {
                self.append_activity(
                    prepared.project.id,
                    ActivityEventType::ApprovalRequested,
                    "item",
                    prepared.item.id.to_string(),
                    serde_json::json!({ "job_id": prepared.job.id }),
                )
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
            prepared.workspace.workspace_ref.as_deref().ok_or_else(|| {
                RuntimeError::InvalidState("authoring workspace missing ref".into())
            })?;
        let actual_ref = resolve_ref_oid(repo_path, workspace_ref).await?;
        let actual_head = head_oid(Path::new(&prepared.workspace.path)).await?;

        if actual_ref.as_deref() != Some(prepared.original_head_commit_oid.as_str())
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
    ) -> Result<String, RuntimeError> {
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
            entity_id: prepared.job.id.to_string(),
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
            "git_operation",
            operation.id.to_string(),
            serde_json::json!({ "operation_kind": operation.operation_kind(), "entity_id": operation.entity_id }),
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
            &commit_subject(&prepared.revision.title, &prepared.job.step_id),
            summary,
            &JobCommitTrailers {
                operation_id: operation.id,
                item_id: prepared.item.id,
                revision_no: prepared.revision.revision_no,
                job_id: prepared.job.id,
            },
        )
        .await?;
        git(repo_path, &["update-ref", &workspace_ref, &commit_oid]).await?;

        operation.payload.set_job_commit_result(commit_oid.clone());
        operation.status = GitOperationStatus::Applied;
        operation.completed_at = Some(Utc::now());
        self.db.update_git_operation(&operation).await?;

        Ok(commit_oid)
    }

    async fn complete_commit_run(
        &self,
        prepared: &PreparedRun,
        commit_oid: &str,
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
                output_commit_oid: Some(commit_oid.to_string()),
                findings: vec![],
                prepared_convergence_guard: None,
            })
            .await?;
        self.append_activity(
            prepared.project.id,
            ActivityEventType::JobCompleted,
            "job",
            prepared.job.id.to_string(),
            serde_json::json!({ "item_id": prepared.item.id, "outcome": "clean" }),
        )
        .await?;

        self.refresh_revision_context(prepared).await?;
        self.append_escalation_cleared_activity_if_needed(prepared)
            .await?;
        self.auto_dispatch_projected_review(prepared.project.id, prepared.item.id)
            .await?;

        info!(job_id = %prepared.job.id, commit_oid, "completed authoring job");

        Ok(())
    }
}

pub(super) async fn run_prepared_agent_job(
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
