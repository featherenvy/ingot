use super::*;

impl JobDispatcher {
    pub(super) async fn recover_projected_review_jobs(&self) -> Result<bool, RuntimeError> {
        let dispatcher = self.clone();
        let dispatched_any = ingot_usecases::dispatch::recover_projected_jobs(
            &self.db,
            &self.db,
            &self.project_locks,
            move |project, item_id| {
                let dispatcher = dispatcher.clone();
                let project = project.clone();
                async move {
                    dispatcher
                        .auto_dispatch_projected_review_locked(&project, item_id)
                        .await
                        .map_err(usecase_from_runtime_error)
                }
            },
        )
        .await
        .map_err(usecase_to_runtime_error)?;

        if dispatched_any {
            info!("projected review recovery queued review work");
        }

        Ok(dispatched_any)
    }

    pub(super) async fn auto_dispatch_projected_review(
        &self,
        project_id: ingot_domain::ids::ProjectId,
        item_id: ingot_domain::ids::ItemId,
    ) -> Result<bool, RuntimeError> {
        let _guard = self
            .project_locks
            .acquire_project_mutation(project_id)
            .await;

        let project = self.db.get_project(project_id).await?;
        self.auto_dispatch_projected_review_locked(&project, item_id)
            .await
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

        let result = ingot_usecases::dispatch::auto_dispatch_projected(
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
            RuntimeError::InvalidState(format!("failed to auto-dispatch follow-up: {error}"))
        })?;

        if let Some(job) = result {
            info!(job_id = %job.id, step_id = %job.step_id, item_id = %item.id, "auto-dispatched projected follow-up");
            Ok(true)
        } else {
            Ok(false)
        }
    }
}
