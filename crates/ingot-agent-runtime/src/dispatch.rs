// Job projection and auto-dispatch for review/validation jobs.

use ingot_domain::activity::{ActivityEventType, ActivitySubject};
use ingot_domain::convergence::Convergence;
use ingot_domain::job::Job;
use ingot_domain::ports::ProjectMutationLockPort;
use ingot_domain::project::Project;
use ingot_domain::revision::ItemRevision;
use ingot_usecases::job::{DispatchJobCommand, dispatch_job};
use ingot_workflow::{Evaluator, step};
use tracing::{info, warn};

use crate::{JobDispatcher, RuntimeError};

impl JobDispatcher {
    pub(crate) async fn recover_projected_jobs(&self) -> Result<bool, RuntimeError> {
        let mut dispatched_any = false;

        for project in self.db.list_projects().await? {
            #[cfg(test)]
            self.pause_before_projected_recovery_guard().await;
            let _guard = self
                .project_locks
                .acquire_project_mutation(project.id)
                .await;
            let project = match self.db.get_project(project.id).await {
                Ok(project) => project,
                Err(error) => {
                    warn!(
                        ?error,
                        project_id = %project.id,
                        "projected job recovery skipped project"
                    );
                    continue;
                }
            };
            if project.execution_mode == ingot_domain::project::ExecutionMode::Autopilot
                && self.project_has_active_autopilot_work(project.id).await?
            {
                continue;
            }
            let items = match self.db.list_items_by_project(project.id).await {
                Ok(items) => items,
                Err(error) => {
                    warn!(
                        ?error,
                        project_id = %project.id,
                        "projected job recovery skipped project"
                    );
                    continue;
                }
            };
            for item in items {
                if !item.lifecycle.is_open() {
                    continue;
                }
                let result = match project.execution_mode {
                    ingot_domain::project::ExecutionMode::Autopilot => {
                        self.auto_dispatch_autopilot_locked(&project, item.id).await
                    }
                    ingot_domain::project::ExecutionMode::Manual => {
                        self.auto_dispatch_projected_review_locked(&project, item.id)
                            .await
                    }
                };
                match result {
                    Ok(dispatched) => {
                        dispatched_any |= dispatched;
                        if project.execution_mode == ingot_domain::project::ExecutionMode::Autopilot
                        {
                            break;
                        }
                    }
                    Err(error) => {
                        warn!(
                            ?error,
                            project_id = %project.id,
                            item_id = %item.id,
                            "projected job recovery skipped item"
                        );
                        if project.execution_mode == ingot_domain::project::ExecutionMode::Autopilot
                        {
                            break;
                        }
                    }
                }
            }
        }

        if dispatched_any {
            info!("projected job recovery queued work");
        }

        Ok(dispatched_any)
    }

    pub(crate) async fn auto_dispatch_projected_review(
        &self,
        project_id: ingot_domain::ids::ProjectId,
        item_id: ingot_domain::ids::ItemId,
    ) -> Result<bool, RuntimeError> {
        let _guard = self
            .project_locks
            .acquire_project_mutation(project_id)
            .await;

        let project = self.db.get_project(project_id).await?;
        match project.execution_mode {
            ingot_domain::project::ExecutionMode::Autopilot => {
                if self.project_has_active_autopilot_work(project.id).await? {
                    return Ok(false);
                }
                self.auto_dispatch_autopilot_locked(&project, item_id).await
            }
            ingot_domain::project::ExecutionMode::Manual => {
                self.auto_dispatch_projected_review_locked(&project, item_id)
                    .await
            }
        }
    }

    pub async fn auto_dispatch_projected_review_locked(
        &self,
        project: &Project,
        item_id: ingot_domain::ids::ItemId,
    ) -> Result<bool, RuntimeError> {
        let item = self.db.get_item(item_id).await?;
        let revision = self.db.get_revision(item.current_revision_id).await?;
        let jobs = self.db.list_jobs_by_item(item.id).await?;
        let findings = self.db.list_findings_by_item(item.id).await?;
        let convergences = self
            .hydrate_convergences(project, self.db.list_convergences_by_item(item.id).await?)
            .await?;

        let result = ingot_usecases::dispatch::auto_dispatch_review(
            &self.db,
            &self.db,
            &self.db,
            project,
            &item,
            &revision,
            &jobs,
            &findings,
            &convergences,
        )
        .await
        .map_err(|error| {
            RuntimeError::InvalidState(format!("failed to auto-dispatch review: {error}"))
        })?;

        if let Some(job) = result {
            info!(job_id = %job.id, step_id = %job.step_id, item_id = %item.id, "auto-dispatched review");
            Ok(true)
        } else if let Some(job) = self
            .auto_dispatch_projected_validation_job(
                project,
                &item,
                &revision,
                &jobs,
                &findings,
                &convergences,
            )
            .await?
        {
            info!(job_id = %job.id, step_id = %job.step_id, item_id = %item.id, "auto-dispatched validation");
            Ok(true)
        } else {
            Ok(false)
        }
    }

    async fn auto_dispatch_projected_validation_job(
        &self,
        project: &Project,
        item: &ingot_domain::item::Item,
        revision: &ItemRevision,
        jobs: &[Job],
        findings: &[ingot_domain::finding::Finding],
        convergences: &[Convergence],
    ) -> Result<Option<Job>, RuntimeError> {
        let evaluation = Evaluator::new().evaluate(item, revision, jobs, findings, convergences);
        let Some(step_id) = evaluation.dispatchable_step_id else {
            return Ok(None);
        };
        if !step::is_closure_relevant_validate_step(step_id) {
            return Ok(None);
        }

        let mut job = dispatch_job(
            item,
            revision,
            jobs,
            findings,
            convergences,
            DispatchJobCommand {
                step_id: Some(step_id),
            },
        )
        .map_err(|error| {
            RuntimeError::InvalidState(format!("failed to auto-dispatch validation: {error}"))
        })?;

        if ingot_usecases::dispatch::should_fill_candidate_subject_from_workspace(job.step_id) {
            let authoring_workspace = self
                .db
                .find_authoring_workspace_for_revision(revision.id)
                .await?;
            let base = job
                .job_input
                .base_commit_oid()
                .map(ToOwned::to_owned)
                .or_else(|| {
                    ingot_usecases::dispatch::effective_authoring_base_commit_oid(
                        revision,
                        authoring_workspace.as_ref(),
                    )
                });
            let head = job
                .job_input
                .head_commit_oid()
                .map(ToOwned::to_owned)
                .or_else(|| {
                    ingot_usecases::dispatch::current_authoring_head_for_revision_with_workspace(
                        revision,
                        jobs,
                        authoring_workspace.as_ref(),
                    )
                });
            match (base, head) {
                (Some(base), Some(head)) => {
                    job.job_input = ingot_domain::job::JobInput::candidate_subject(base, head);
                }
                _ => {
                    return Err(RuntimeError::InvalidState(format!(
                        "failed to auto-dispatch validation: incomplete candidate subject for {}",
                        job.step_id
                    )));
                }
            }
        }

        self.db.create_job(&job).await?;
        self.append_activity(
            project.id,
            ActivityEventType::JobDispatched,
            ActivitySubject::Job(job.id),
            serde_json::json!({ "item_id": item.id, "step_id": job.step_id }),
        )
        .await?;

        Ok(Some(job))
    }
}
