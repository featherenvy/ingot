// Main dispatcher loop, supervisor task management, and job ticking.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::Utc;
use ingot_domain::activity::{ActivityEventType, ActivitySubject};
use ingot_domain::job::{Job, JobStatus, OutcomeClass};
use ingot_domain::ports::ProjectMutationLockPort;
use ingot_domain::workspace::WorkspaceKind;
use ingot_store_sqlite::FinishJobNonSuccessParams;
use ingot_usecases::{ConvergenceService, ReconciliationService};
use ingot_workspace::WorkspaceError;
use tokio::sync::Semaphore;
use tokio::sync::TryAcquireError;
use tokio::task::{Id as TaskId, JoinError, JoinSet};
use tokio::time::sleep;
use tracing::{debug, error, warn};

use crate::bootstrap;
use crate::harness::{
    PrepareHarnessValidationOutcome, PreparedHarnessValidation, is_daemon_only_validation,
    run_prepared_harness_validation_job,
};
use crate::{
    JobDispatcher, PrepareRunOutcome, PreparedRun, RuntimeConvergencePort, RuntimeError,
    RuntimeReconciliationPort, drain_until_idle, is_supported_runtime_job, run_prepared_agent_job,
    usecase_to_runtime_error,
};

#[derive(Debug, Clone, Copy, Default)]
struct NonJobWorkProgress {
    made_progress: bool,
    system_actions_progressed: bool,
}

pub(crate) struct RunningJobResult {
    pub(crate) job_id: ingot_domain::ids::JobId,
    pub(crate) result: Result<(), RuntimeError>,
}

#[derive(Debug, Clone)]
pub(crate) enum RunningJobMeta {
    Agent(Box<PreparedRun>),
    HarnessValidation(PreparedHarnessValidation),
}

impl RunningJobMeta {
    fn job_id(&self) -> ingot_domain::ids::JobId {
        match self {
            Self::Agent(prepared) => prepared.job.id,
            Self::HarnessValidation(prepared) => prepared.job_id,
        }
    }
}

impl JobDispatcher {
    pub async fn run_forever(&self) {
        let semaphore = Arc::new(Semaphore::new(self.config.max_concurrent_jobs));
        let mut running = JoinSet::<RunningJobResult>::new();
        let mut running_meta = HashMap::<TaskId, RunningJobMeta>::new();
        let mut running_job_ids = HashSet::new();
        let mut dispatch_listener = self.dispatch_notify.subscribe();

        loop {
            let made_progress = match self
                .run_supervisor_iteration(
                    &mut running,
                    &mut running_meta,
                    &mut running_job_ids,
                    &semaphore,
                )
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
                    notification = dispatch_listener.notified() => {
                        debug!(
                            generation = notification.generation(),
                            reason = %notification.reason(),
                            "dispatcher woken by notification"
                        );
                        Ok(())
                    }
                    () = sleep(self.config.poll_interval) => Ok(()),
                }
            } else {
                tokio::select! {
                    join_result = running.join_next_with_id() => {
                        if let Some(join_result) = join_result {
                            self.handle_supervised_join_result(
                                join_result,
                                &mut running_meta,
                                &mut running_job_ids,
                            )
                            .await
                        } else {
                            Ok(())
                        }
                    }
                    notification = dispatch_listener.notified() => {
                        debug!(
                            generation = notification.generation(),
                            reason = %notification.reason(),
                            "dispatcher woken by notification"
                        );
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

    pub async fn reconcile_startup(&self) -> Result<(), RuntimeError> {
        bootstrap::ensure_default_agents(&self.db).await?;
        let _ = self.reconcile_startup_assigned_jobs().await?;
        ReconciliationService::new(RuntimeReconciliationPort {
            dispatcher: self.clone(),
        })
        .reconcile_startup()
        .await
        .map_err(usecase_to_runtime_error)?;
        drain_until_idle(|| self.tick_system_action()).await?;
        let _ = self.recover_projected_jobs().await?;
        Ok(())
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
                        self.execute_prepared_agent_job(*prepared).await?;
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

    #[allow(dead_code)]
    pub(crate) async fn tick_system_action(&self) -> Result<bool, RuntimeError> {
        ConvergenceService::new(RuntimeConvergencePort {
            dispatcher: self.clone(),
        })
        .tick_system_actions()
        .await
        .map_err(usecase_to_runtime_error)
    }

    pub(crate) async fn promote_queue_heads(
        &self,
        project_id: ingot_domain::ids::ProjectId,
    ) -> Result<(), RuntimeError> {
        ingot_usecases::convergence::promote_queue_heads(&self.db, &self.db, project_id)
            .await
            .map_err(|e| RuntimeError::InvalidState(e.to_string()))?;
        Ok(())
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
        made_progress |= self.recover_projected_jobs().await?;
        Ok(NonJobWorkProgress {
            made_progress,
            system_actions_progressed,
        })
    }

    pub(crate) async fn run_supervisor_iteration(
        &self,
        running: &mut JoinSet<RunningJobResult>,
        running_meta: &mut HashMap<TaskId, RunningJobMeta>,
        running_job_ids: &mut HashSet<ingot_domain::ids::JobId>,
        semaphore: &Arc<Semaphore>,
    ) -> Result<bool, RuntimeError> {
        let mut made_progress = self
            .reap_completed_tasks(running, running_meta, running_job_ids)
            .await?;
        made_progress |= self.drive_non_job_work().await?.made_progress;
        made_progress |= self
            .launch_supervised_jobs(running, running_meta, running_job_ids, semaphore)
            .await?;
        Ok(made_progress)
    }

    pub(crate) async fn reap_completed_tasks(
        &self,
        running: &mut JoinSet<RunningJobResult>,
        running_meta: &mut HashMap<TaskId, RunningJobMeta>,
        running_job_ids: &mut HashSet<ingot_domain::ids::JobId>,
    ) -> Result<bool, RuntimeError> {
        let mut made_progress = false;
        while let Some(join_result) = running.try_join_next_with_id() {
            self.handle_supervised_join_result(join_result, running_meta, running_job_ids)
                .await?;
            made_progress = true;
        }
        Ok(made_progress)
    }

    async fn handle_supervised_join_result(
        &self,
        join_result: Result<(TaskId, RunningJobResult), JoinError>,
        running_meta: &mut HashMap<TaskId, RunningJobMeta>,
        running_job_ids: &mut HashSet<ingot_domain::ids::JobId>,
    ) -> Result<(), RuntimeError> {
        match join_result {
            Ok((task_id, task_result)) => {
                let meta = running_meta.remove(&task_id);
                running_job_ids.remove(&task_result.job_id);
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
                    running_job_ids.remove(&meta.job_id());
                    self.cleanup_supervised_task(meta, error.to_string())
                        .await?;
                }
            }
        }
        Ok(())
    }

    pub(crate) async fn cleanup_supervised_task(
        &self,
        meta: RunningJobMeta,
        error_message: String,
    ) -> Result<(), RuntimeError> {
        match meta {
            RunningJobMeta::Agent(prepared) => {
                let current_job = self.db.get_job(prepared.job.id).await?;
                match current_job.state.status() {
                    JobStatus::Queued => {
                        self.cleanup_unclaimed_prepared_agent_run(&prepared).await?
                    }
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
                            ActivitySubject::Job(prepared.job_id),
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
        running_job_ids: &mut HashSet<ingot_domain::ids::JobId>,
        semaphore: &Arc<Semaphore>,
    ) -> Result<bool, RuntimeError> {
        let mut made_progress = false;
        for job in self.db.list_queued_jobs(32).await? {
            if running_job_ids.contains(&job.id) {
                continue;
            }
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
                        running_job_ids.insert(prepared.job_id);
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
                    running_job_ids.insert(prepared.job.id);
                    running_meta.insert(handle.id(), RunningJobMeta::Agent(Box::new(prepared)));
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
