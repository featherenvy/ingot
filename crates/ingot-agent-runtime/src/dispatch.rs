// Job projection and auto-dispatch for review/validation jobs.

use ingot_domain::convergence::Convergence;
use ingot_domain::job::Job;
use ingot_domain::ports::ProjectMutationLockPort;
use ingot_domain::project::Project;
use ingot_domain::revision::ItemRevision;
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
        ingot_usecases::dispatch::auto_dispatch_validation(
            &self.db,
            &self.db,
            &self.db,
            project,
            item,
            revision,
            jobs,
            findings,
            convergences,
        )
        .await
        .map_err(|error| {
            RuntimeError::InvalidState(format!("failed to auto-dispatch validation: {error}"))
        })
    }
}
