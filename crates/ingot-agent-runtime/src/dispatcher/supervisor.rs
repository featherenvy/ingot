use super::*;
use crate::dispatcher::agent_execution::run_prepared_agent_job;
use crate::dispatcher::harness_execution::run_prepared_harness_validation_job;
use crate::dispatcher::prompt::output_schema_for_job;
use ingot_usecases::job_preparation::{is_daemon_only_validation, is_supported_runtime_job};

impl JobDispatcher {
    pub async fn run_forever(&self) {
        let semaphore = Arc::new(Semaphore::new(self.config.max_concurrent_jobs));
        let mut running = JoinSet::<RunningJobResult>::new();
        let mut running_meta = HashMap::<TaskId, RunningJobMeta>::new();
        let mut dispatch_listener = self.dispatch_notify.subscribe();

        loop {
            let made_progress = match self
                .run_supervisor_iteration(&mut running, &mut running_meta, &semaphore)
                .await
            {
                Ok(made_progress) => made_progress,
                Err(error) => {
                    error!(?error, "authoring job dispatcher iteration failed");
                    false
                }
            };

            if made_progress {
                continue;
            }

            let wait_result: Result<(), RuntimeError> = if running.is_empty() {
                tokio::select! {
                    () = dispatch_listener.notified() => {
                        debug!("dispatcher woken by notification");
                        Ok(())
                    }
                    () = sleep(self.config.poll_interval) => Ok(()),
                }
            } else {
                tokio::select! {
                    join_result = running.join_next_with_id() => {
                        if let Some(join_result) = join_result {
                            self.handle_supervised_join_result(join_result, &mut running_meta).await
                        } else {
                            Ok(())
                        }
                    }
                    () = dispatch_listener.notified() => {
                        debug!("dispatcher woken by notification");
                        Ok(())
                    }
                    () = sleep(self.config.poll_interval) => Ok(()),
                }
            };

            if let Err(error) = wait_result {
                error!(?error, "authoring job dispatcher wait failed");
            }
        }
    }

    pub async fn tick(&self) -> Result<bool, RuntimeError> {
        let non_job_progress = self.drive_non_job_work().await?;
        let mut made_progress = non_job_progress.made_progress;
        if non_job_progress.system_actions_progressed {
            return Ok(true);
        }

        if let Some(job) = self.next_runnable_job().await? {
            if is_daemon_only_validation(&job) {
                self.execute_harness_validation(job).await?;
                made_progress = true;
            } else {
                match self.prepare_run(job).await? {
                    PrepareRunOutcome::Prepared(prepared) => {
                        let prepared = *prepared;
                        self.write_prompt_artifact(&prepared.job, &prepared.prompt)
                            .await?;
                        let request = AgentRequest {
                            prompt: prepared.prompt.clone(),
                            working_dir: prepared.workspace.path.clone(),
                            may_mutate: prepared.job.execution_permission
                                == ExecutionPermission::MayMutate,
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
                                    return Ok(true);
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

                        made_progress = true;
                    }
                    PrepareRunOutcome::FailedBeforeLaunch => {
                        made_progress = true;
                    }
                    PrepareRunOutcome::NotPrepared => {}
                }
            }
        }

        Ok(made_progress)
    }

    async fn next_runnable_job(&self) -> Result<Option<Job>, RuntimeError> {
        let jobs = self.db.list_queued_jobs(32).await?;
        let runnable_job = jobs.into_iter().find(is_supported_runtime_job);
        if let Some(job) = runnable_job.as_ref() {
            debug!(
                job_id = %job.id,
                step_id = %job.step_id,
                workspace_kind = ?job.workspace_kind,
                execution_permission = ?job.execution_permission,
                "selected queued job for runtime"
            );
        }
        Ok(runnable_job)
    }

    async fn drive_non_job_work(&self) -> Result<NonJobWorkProgress, RuntimeError> {
        let mut made_progress = ReconciliationService::new(RuntimeReconciliationPort {
            dispatcher: self.clone(),
        })
        .tick_maintenance()
        .await
        .map_err(usecase_to_runtime_error)?;
        let system_actions_progressed = ConvergenceService::new(RuntimeConvergencePort {
            dispatcher: self.clone(),
        })
        .tick_system_actions()
        .await
        .map_err(usecase_to_runtime_error)?;
        made_progress |= system_actions_progressed;
        made_progress |= self.recover_projected_review_jobs().await?;
        Ok(NonJobWorkProgress {
            made_progress,
            system_actions_progressed,
        })
    }

    async fn run_supervisor_iteration(
        &self,
        running: &mut JoinSet<RunningJobResult>,
        running_meta: &mut HashMap<TaskId, RunningJobMeta>,
        semaphore: &Arc<Semaphore>,
    ) -> Result<bool, RuntimeError> {
        let mut made_progress = self.reap_completed_tasks(running, running_meta).await?;
        made_progress |= self.drive_non_job_work().await?.made_progress;
        made_progress |= self
            .launch_supervised_jobs(running, running_meta, semaphore)
            .await?;
        Ok(made_progress)
    }

    async fn reap_completed_tasks(
        &self,
        running: &mut JoinSet<RunningJobResult>,
        running_meta: &mut HashMap<TaskId, RunningJobMeta>,
    ) -> Result<bool, RuntimeError> {
        let mut made_progress = false;
        while let Some(join_result) = running.try_join_next_with_id() {
            self.handle_supervised_join_result(join_result, running_meta)
                .await?;
            made_progress = true;
        }
        Ok(made_progress)
    }

    async fn handle_supervised_join_result(
        &self,
        join_result: Result<(TaskId, RunningJobResult), JoinError>,
        running_meta: &mut HashMap<TaskId, RunningJobMeta>,
    ) -> Result<(), RuntimeError> {
        match join_result {
            Ok((task_id, task_result)) => {
                let meta = running_meta.remove(&task_id);
                match task_result.result {
                    Ok(()) => {
                        debug!(job_id = %task_result.job_id, task_id = %task_id, "supervised job task completed");
                    }
                    Err(error) => {
                        warn!(?error, job_id = %task_result.job_id, task_id = %task_id, "supervised job task returned error");
                        if let Some(meta) = meta {
                            self.cleanup_supervised_task(meta, error.to_string())
                                .await?;
                        }
                    }
                }
            }
            Err(error) => {
                let task_id = error.id();
                warn!(?error, task_id = %task_id, "supervised job task failed");
                if let Some(meta) = running_meta.remove(&task_id) {
                    self.cleanup_supervised_task(meta, error.to_string())
                        .await?;
                }
            }
        }
        Ok(())
    }

    async fn cleanup_supervised_task(
        &self,
        meta: RunningJobMeta,
        error_message: String,
    ) -> Result<(), RuntimeError> {
        match meta {
            RunningJobMeta::Agent(prepared) => {
                let current_job = self.db.get_job(prepared.job.id).await?;
                match current_job.state.status() {
                    JobStatus::Assigned => self.reconcile_assigned_job(current_job).await?,
                    JobStatus::Running => {
                        self.fail_run(
                            &prepared,
                            OutcomeClass::TerminalFailure,
                            "supervised_task_failed",
                            Some(error_message),
                        )
                        .await?;
                    }
                    _ => {}
                }
            }
            RunningJobMeta::HarnessValidation(prepared) => {
                let current_job = self.db.get_job(prepared.job_id).await?;
                match current_job.state.status() {
                    JobStatus::Assigned => self.reconcile_assigned_job(current_job).await?,
                    JobStatus::Running => {
                        let _guard = self
                            .project_locks
                            .acquire_project_mutation(prepared.project_id)
                            .await;
                        let current_job = self.db.get_job(prepared.job_id).await?;
                        if current_job.state.status() != JobStatus::Running {
                            return Ok(());
                        }
                        self.db
                            .finish_job_non_success(FinishJobNonSuccessParams {
                                job_id: prepared.job_id,
                                item_id: prepared.item_id,
                                expected_item_revision_id: prepared.revision_id,
                                status: JobStatus::Failed,
                                outcome_class: Some(OutcomeClass::TerminalFailure),
                                error_code: Some("supervised_task_failed".into()),
                                error_message: Some(error_message),
                                escalation_reason: None,
                            })
                            .await?;
                        let mut workspace = self.db.get_workspace(prepared.workspace_id).await?;
                        workspace.mark_stale(Utc::now());
                        self.db.update_workspace(&workspace).await?;
                        self.append_activity(
                            prepared.project_id,
                            ActivityEventType::JobFailed,
                            "job",
                            prepared.job_id.to_string(),
                            serde_json::json!({ "item_id": prepared.item_id, "error_code": "supervised_task_failed" }),
                        )
                        .await?;
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    async fn launch_supervised_jobs(
        &self,
        running: &mut JoinSet<RunningJobResult>,
        running_meta: &mut HashMap<TaskId, RunningJobMeta>,
        semaphore: &Arc<Semaphore>,
    ) -> Result<bool, RuntimeError> {
        let mut made_progress = false;
        for job in self.db.list_queued_jobs(32).await? {
            let permit = match semaphore.clone().try_acquire_owned() {
                Ok(permit) => permit,
                Err(TryAcquireError::NoPermits) => break,
                Err(TryAcquireError::Closed) => {
                    return Err(RuntimeError::InvalidState(
                        "dispatcher semaphore unexpectedly closed".into(),
                    ));
                }
            };

            if !is_supported_runtime_job(&job) {
                drop(permit);
                continue;
            }

            if is_daemon_only_validation(&job) {
                match self.prepare_harness_validation(job.clone()).await {
                    Ok(PrepareHarnessValidationOutcome::NotPrepared) => {
                        drop(permit);
                    }
                    Ok(PrepareHarnessValidationOutcome::FailedBeforeLaunch) => {
                        drop(permit);
                        made_progress = true;
                    }
                    Ok(PrepareHarnessValidationOutcome::Prepared(prepared)) => {
                        let prepared = *prepared;
                        let handle = running.spawn(run_prepared_harness_validation_job(
                            self.clone(),
                            prepared.clone(),
                            permit,
                        ));
                        running_meta
                            .insert(handle.id(), RunningJobMeta::HarnessValidation(prepared));
                        made_progress = true;
                    }
                    Err(RuntimeError::Workspace(WorkspaceError::Busy))
                        if job.workspace_kind == WorkspaceKind::Authoring =>
                    {
                        drop(permit);
                    }
                    Err(error) => {
                        drop(permit);
                        return Err(error);
                    }
                }
                continue;
            }

            match self.prepare_run(job.clone()).await {
                Ok(PrepareRunOutcome::NotPrepared) => {
                    drop(permit);
                }
                Ok(PrepareRunOutcome::FailedBeforeLaunch) => {
                    drop(permit);
                    made_progress = true;
                }
                Ok(PrepareRunOutcome::Prepared(prepared)) => {
                    let prepared = *prepared;
                    let handle = running.spawn(run_prepared_agent_job(
                        self.clone(),
                        prepared.clone(),
                        permit,
                    ));
                    running_meta.insert(handle.id(), RunningJobMeta::Agent(prepared));
                    made_progress = true;
                }
                Err(RuntimeError::Workspace(WorkspaceError::Busy))
                    if job.workspace_kind == WorkspaceKind::Authoring =>
                {
                    drop(permit);
                }
                Err(error) => {
                    drop(permit);
                    return Err(error);
                }
            }
        }

        Ok(made_progress)
    }
}
