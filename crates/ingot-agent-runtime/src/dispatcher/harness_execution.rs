use super::*;

pub(super) struct HarnessCommandResult {
    pub(super) exit_code: i32,
    pub(super) stdout_tail: String,
    pub(super) stderr_tail: String,
    pub(super) timed_out: bool,
    pub(super) cancelled: bool,
}

impl JobDispatcher {
    pub(super) async fn run_prepared_harness_validation(
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
                result_schema_version: Some("validation_report:v1".to_string()),
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
            "job",
            prepared.job_id.to_string(),
            serde_json::json!({ "item_id": prepared.item_id, "outcome": outcome }),
        )
        .await?;

        if prepared.step_id == "validate_integrated" && outcome_class == OutcomeClass::Clean {
            let updated_item = self.db.get_item(prepared.item_id).await?;
            if updated_item.approval_state == ApprovalState::Pending {
                self.append_activity(
                    prepared.project_id,
                    ActivityEventType::ApprovalRequested,
                    "item",
                    prepared.item_id.to_string(),
                    serde_json::json!({ "job_id": prepared.job_id }),
                )
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
        let lease_expires_at = Utc::now() + ChronoDuration::minutes(30);
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

    pub(super) async fn run_harness_command_with_heartbeats(
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
                _ = dispatch_listener.notified() => {
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
                                "harness command woke on unrelated dispatcher notification"
                            );
                        }
                        Err(error) => {
                            warn!(
                                ?error,
                                job_id = %prepared.job_id,
                                command = %command_spec.name,
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

    pub(super) async fn execute_harness_validation(
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
}

pub(super) async fn run_prepared_harness_validation_job(
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
