// Autopilot-mode orchestration methods on JobDispatcher.
// These methods are called from lib.rs when project.execution_mode == Autopilot.

use chrono::Utc;
use ingot_domain::ports::RepositoryError;
use ingot_domain::project::Project;
use ingot_git::commands::resolve_ref_oid;
use tracing::info;

use crate::{JobDispatcher, RuntimeError, usecase_from_runtime_error};

impl JobDispatcher {
    pub async fn auto_dispatch_autopilot_locked(
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
        let author_initial_head_commit_oid =
            if ingot_usecases::dispatch::autopilot_dispatch_requires_live_target_head(
                &item,
                &revision,
                &jobs,
                &findings,
                &convergences,
            ) {
                let paths = self.refresh_project_mirror(project).await?;
                Some(
                    resolve_ref_oid(paths.mirror_git_dir.as_path(), &revision.target_ref)
                        .await?
                        .ok_or_else(|| {
                            RuntimeError::InvalidState("target ref unresolved".into())
                        })?,
                )
            } else {
                None
            };

        let result = ingot_usecases::dispatch::auto_dispatch_autopilot(
            &self.db,
            &self.db,
            &self.db,
            project,
            &item,
            &revision,
            &jobs,
            &findings,
            &convergences,
            author_initial_head_commit_oid,
        )
        .await
        .map_err(|error| {
            RuntimeError::InvalidState(format!("autopilot dispatch failed: {error}"))
        })?;

        if let Some(job) = result {
            info!(job_id = %job.id, step_id = %job.step_id, item_id = %item.id, "autopilot dispatched step");
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub(crate) async fn project_has_active_autopilot_work(
        &self,
        project_id: ingot_domain::ids::ProjectId,
    ) -> Result<bool, RuntimeError> {
        if self
            .db
            .list_jobs_by_project(project_id)
            .await?
            .into_iter()
            .any(|job| job.state.is_active())
        {
            return Ok(true);
        }

        if self
            .db
            .list_active_convergences()
            .await?
            .into_iter()
            .any(|convergence| convergence.project_id == project_id)
        {
            return Ok(true);
        }

        Ok(!self
            .db
            .list_active_queue_entries_by_project(project_id)
            .await?
            .is_empty())
    }

    pub(crate) async fn auto_triage_job_findings(
        &self,
        project: &Project,
        job_id: ingot_domain::ids::JobId,
        item: &ingot_domain::item::Item,
    ) -> Result<(), RuntimeError> {
        let Some(policy) = project.auto_triage_policy.as_ref() else {
            return Ok(());
        };
        let job = self.db.get_job(job_id).await?;
        ingot_usecases::finding::execute_auto_triage(
            &self.db,
            &self.db,
            &self.db,
            &self.db,
            project,
            item,
            job_id,
            job.step_id,
            policy,
        )
        .await
        .map_err(|e| RuntimeError::InvalidState(format!("auto-triage failed: {e}")))
    }

    pub(crate) async fn auto_queue_convergence_inner(
        &self,
        project_id: ingot_domain::ids::ProjectId,
        item_id: ingot_domain::ids::ItemId,
        project: &Project,
    ) -> Result<bool, ingot_usecases::UseCaseError> {
        let mut retry_after_conflict = true;
        loop {
            let item = self
                .db
                .get_item(item_id)
                .await
                .map_err(ingot_usecases::UseCaseError::Repository)?;
            let revision = self
                .db
                .get_revision(item.current_revision_id)
                .await
                .map_err(ingot_usecases::UseCaseError::Repository)?;
            let existing = self
                .db
                .find_active_queue_entry_for_revision(revision.id)
                .await
                .map_err(ingot_usecases::UseCaseError::Repository)?;
            if existing.is_some() {
                return Ok(false);
            }
            let jobs = self
                .db
                .list_jobs_by_item(item.id)
                .await
                .map_err(ingot_usecases::UseCaseError::Repository)?;
            let findings = self
                .db
                .list_findings_by_item(item.id)
                .await
                .map_err(ingot_usecases::UseCaseError::Repository)?;
            let convergences = self
                .hydrate_convergences(
                    project,
                    self.db
                        .list_convergences_by_item(item.id)
                        .await
                        .map_err(ingot_usecases::UseCaseError::Repository)?,
                )
                .await
                .map_err(usecase_from_runtime_error)?;
            if !ingot_usecases::convergence::should_prepare_convergence(
                &item,
                &revision,
                &jobs,
                &findings,
                &convergences,
            ) {
                return Ok(false);
            }
            let lane_head = self
                .db
                .find_queue_head(project_id, &revision.target_ref)
                .await
                .map_err(ingot_usecases::UseCaseError::Repository)?;
            let now = Utc::now();
            let queue_entry = ingot_domain::convergence_queue::ConvergenceQueueEntry {
                id: ingot_domain::ids::ConvergenceQueueEntryId::new(),
                project_id,
                item_id,
                item_revision_id: revision.id,
                target_ref: revision.target_ref.clone(),
                status: if lane_head.is_some() {
                    ingot_domain::convergence_queue::ConvergenceQueueEntryStatus::Queued
                } else {
                    ingot_domain::convergence_queue::ConvergenceQueueEntryStatus::Head
                },
                head_acquired_at: lane_head.is_none().then_some(now),
                created_at: now,
                updated_at: now,
                released_at: None,
            };
            #[cfg(test)]
            self.pause_before_auto_queue_insert().await;
            match self.db.create_queue_entry(&queue_entry).await {
                Ok(()) => {
                    self.db
                        .append_activity(&ingot_domain::activity::Activity {
                            id: ingot_domain::ids::ActivityId::new(),
                            project_id,
                            event_type:
                                ingot_domain::activity::ActivityEventType::ConvergenceQueued,
                            subject: ingot_domain::activity::ActivitySubject::QueueEntry(
                                queue_entry.id,
                            ),
                            payload: serde_json::json!({
                                "item_id": item_id,
                                "target_ref": revision.target_ref,
                                "dispatch_origin": "autopilot",
                            }),
                            created_at: now,
                        })
                        .await
                        .map_err(ingot_usecases::UseCaseError::Repository)?;
                    info!(
                        project_id = %project_id,
                        item_id = %item_id,
                        "autopilot queued convergence"
                    );
                    return Ok(true);
                }
                Err(RepositoryError::Conflict(_)) if retry_after_conflict => {
                    retry_after_conflict = false;
                }
                Err(RepositoryError::Conflict(message)) => {
                    let existing = self
                        .db
                        .find_active_queue_entry_for_revision(revision.id)
                        .await
                        .map_err(ingot_usecases::UseCaseError::Repository)?;
                    if existing.is_some() {
                        return Ok(false);
                    }
                    return Err(ingot_usecases::UseCaseError::Repository(
                        RepositoryError::Conflict(message),
                    ));
                }
                Err(error) => {
                    return Err(ingot_usecases::UseCaseError::Repository(error));
                }
            }
        }
    }
}
