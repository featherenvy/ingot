use super::items::{
    append_activity, build_superseding_revision, hydrate_convergence_validity, load_item_detail,
};
use super::support::*;
use super::types::*;
use super::*;

#[derive(Clone)]
pub(super) struct HttpConvergencePort {
    pub(super) state: AppState,
}

impl ConvergenceCommandPort for HttpConvergencePort {
    fn load_queue_prepare_context(
        &self,
        project_id: ProjectId,
        item_id: ItemId,
    ) -> impl std::future::Future<
        Output = Result<ingot_domain::ports::ConvergenceQueuePrepareContext, UseCaseError>,
    > + Send {
        let state = self.state.clone();
        async move {
            let project = state
                .db
                .get_project(project_id)
                .await
                .map_err(repo_to_project)
                .map_err(api_to_usecase_error)?;
            let paths = refresh_project_mirror(&state, &project)
                .await
                .map_err(api_to_usecase_error)?;
            let item = state
                .db
                .get_item(item_id)
                .await
                .map_err(repo_to_item)
                .map_err(api_to_usecase_error)?;
            let revision = state
                .db
                .get_revision(item.current_revision_id)
                .await
                .map_err(UseCaseError::Repository)?;
            let jobs = state
                .db
                .list_jobs_by_item(item.id)
                .await
                .map_err(UseCaseError::Repository)?;
            let findings = state
                .db
                .list_findings_by_item(item.id)
                .await
                .map_err(UseCaseError::Repository)?;
            let convergences = hydrate_convergence_validity(
                paths.mirror_git_dir.as_path(),
                state
                    .db
                    .list_convergences_by_item(item.id)
                    .await
                    .map_err(UseCaseError::Repository)?,
            )
            .await
            .map_err(api_to_usecase_error)?;
            let active_queue_entry = state
                .db
                .find_active_queue_entry_for_revision(revision.id)
                .await
                .map_err(UseCaseError::Repository)?;
            let lane_head = state
                .db
                .find_queue_head(project.id, &revision.target_ref)
                .await
                .map_err(UseCaseError::Repository)?;

            Ok(ingot_domain::ports::ConvergenceQueuePrepareContext {
                project,
                item,
                revision,
                jobs,
                findings,
                convergences,
                active_queue_entry,
                lane_head,
            })
        }
    }

    fn create_queue_entry(
        &self,
        queue_entry: &ConvergenceQueueEntry,
    ) -> impl std::future::Future<Output = Result<(), UseCaseError>> + Send {
        let state = self.state.clone();
        let queue_entry = queue_entry.clone();
        async move {
            state
                .db
                .create_queue_entry(&queue_entry)
                .await
                .map_err(UseCaseError::Repository)?;
            Ok(())
        }
    }

    fn update_queue_entry(
        &self,
        queue_entry: &ConvergenceQueueEntry,
    ) -> impl std::future::Future<Output = Result<(), UseCaseError>> + Send {
        let state = self.state.clone();
        let queue_entry = queue_entry.clone();
        async move {
            state
                .db
                .update_queue_entry(&queue_entry)
                .await
                .map_err(UseCaseError::Repository)?;
            Ok(())
        }
    }

    fn append_activity(
        &self,
        activity: &Activity,
    ) -> impl std::future::Future<Output = Result<(), UseCaseError>> + Send {
        let state = self.state.clone();
        let activity = activity.clone();
        async move {
            state
                .db
                .append_activity(&activity)
                .await
                .map_err(UseCaseError::Repository)?;
            Ok(())
        }
    }

    fn load_approval_context(
        &self,
        project_id: ProjectId,
        item_id: ItemId,
    ) -> impl std::future::Future<
        Output = Result<ingot_usecases::convergence::ConvergenceApprovalContext, UseCaseError>,
    > + Send {
        let state = self.state.clone();
        async move {
            let project = state
                .db
                .get_project(project_id)
                .await
                .map_err(repo_to_project)
                .map_err(api_to_usecase_error)?;
            let paths = refresh_project_mirror(&state, &project)
                .await
                .map_err(api_to_usecase_error)?;
            let item = state
                .db
                .get_item(item_id)
                .await
                .map_err(repo_to_item)
                .map_err(api_to_usecase_error)?;
            if item.project_id != project_id {
                return Err(UseCaseError::ItemNotFound);
            }
            let revision = state
                .db
                .get_revision(item.current_revision_id)
                .await
                .map_err(UseCaseError::Repository)?;
            let jobs = state
                .db
                .list_jobs_by_item(item.id)
                .await
                .map_err(UseCaseError::Repository)?;
            let convergences = hydrate_convergence_validity(
                paths.mirror_git_dir.as_path(),
                state
                    .db
                    .list_convergences_by_item(item.id)
                    .await
                    .map_err(UseCaseError::Repository)?,
            )
            .await
            .map_err(api_to_usecase_error)?;
            let queue_entry = state
                .db
                .find_active_queue_entry_for_revision(revision.id)
                .await
                .map_err(UseCaseError::Repository)?;

            Ok(ingot_usecases::convergence::ConvergenceApprovalContext {
                item,
                has_active_job: jobs
                    .iter()
                    .any(|job| job.item_revision_id == revision.id && job.state.is_active()),
                has_active_convergence: convergences.iter().any(|convergence| {
                    convergence.item_revision_id == revision.id
                        && matches!(
                            convergence.status,
                            ingot_domain::convergence::ConvergenceStatus::Queued
                                | ingot_domain::convergence::ConvergenceStatus::Running
                        )
                }),
                prepared_convergence_id: convergences
                    .iter()
                    .filter(|convergence| convergence.item_revision_id == revision.id)
                    .find(|convergence| {
                        convergence.status == ingot_domain::convergence::ConvergenceStatus::Prepared
                    })
                    .map(|convergence| convergence.id),
                prepared_target_valid: convergences
                    .iter()
                    .filter(|convergence| convergence.item_revision_id == revision.id)
                    .find(|convergence| {
                        convergence.status == ingot_domain::convergence::ConvergenceStatus::Prepared
                    })
                    .and_then(|convergence| convergence.target_head_valid)
                    .unwrap_or(false),
                queue_entry,
            })
        }
    }

    fn update_item(
        &self,
        item: &Item,
    ) -> impl std::future::Future<Output = Result<(), UseCaseError>> + Send {
        let state = self.state.clone();
        let item = item.clone();
        async move {
            state
                .db
                .update_item(&item)
                .await
                .map_err(UseCaseError::Repository)?;
            Ok(())
        }
    }

    fn load_reject_approval_context(
        &self,
        project_id: ProjectId,
        item_id: ItemId,
    ) -> impl std::future::Future<
        Output = Result<ingot_usecases::convergence::RejectApprovalContext, UseCaseError>,
    > + Send {
        let state = self.state.clone();
        async move {
            let item = state
                .db
                .get_item(item_id)
                .await
                .map_err(repo_to_item)
                .map_err(api_to_usecase_error)?;
            if item.project_id != project_id {
                return Err(UseCaseError::ItemNotFound);
            }
            let revision = state
                .db
                .get_revision(item.current_revision_id)
                .await
                .map_err(UseCaseError::Repository)?;
            let jobs = state
                .db
                .list_jobs_by_item(item.id)
                .await
                .map_err(UseCaseError::Repository)?;
            let has_active_job = jobs
                .iter()
                .any(|job| job.item_revision_id == revision.id && job.state.is_active());
            let convergences = state
                .db
                .list_convergences_by_item(item.id)
                .await
                .map_err(UseCaseError::Repository)?;
            let has_active_convergence = convergences.iter().any(|convergence| {
                convergence.item_revision_id == revision.id
                    && matches!(
                        convergence.status,
                        ingot_domain::convergence::ConvergenceStatus::Queued
                            | ingot_domain::convergence::ConvergenceStatus::Running
                    )
            });

            Ok(ingot_usecases::convergence::RejectApprovalContext {
                item,
                has_active_job,
                has_active_convergence,
            })
        }
    }

    fn teardown_reject_approval(
        &self,
        project_id: ProjectId,
        item_id: ItemId,
    ) -> impl std::future::Future<
        Output = Result<ingot_usecases::convergence::RejectApprovalTeardown, UseCaseError>,
    > + Send {
        let state = self.state.clone();
        async move {
            let project = state
                .db
                .get_project(project_id)
                .await
                .map_err(UseCaseError::Repository)?;
            let item = state
                .db
                .get_item(item_id)
                .await
                .map_err(UseCaseError::Repository)?;
            let revision = state
                .db
                .get_revision(item.current_revision_id)
                .await
                .map_err(UseCaseError::Repository)?;
            let teardown = teardown_revision_lane_state(&state, &project, item.id, &revision)
                .await
                .map_err(api_to_usecase_error)?;
            Ok(ingot_usecases::convergence::RejectApprovalTeardown {
                has_cancelled_convergence: teardown.has_cancelled_convergence(),
                has_cancelled_queue_entry: teardown.has_cancelled_queue_entry(),
                first_cancelled_convergence_id: teardown
                    .first_cancelled_convergence_id()
                    .map(ToOwned::to_owned),
                first_cancelled_queue_entry_id: teardown
                    .first_cancelled_queue_entry_id()
                    .map(ToOwned::to_owned),
            })
        }
    }

    fn apply_rejected_approval(
        &self,
        item: &Item,
        next_revision: &ItemRevision,
    ) -> impl std::future::Future<Output = Result<(), UseCaseError>> + Send {
        let state = self.state.clone();
        let item = item.clone();
        let next_revision = next_revision.clone();
        async move {
            state
                .db
                .create_revision(&next_revision)
                .await
                .map_err(UseCaseError::Repository)?;
            state
                .db
                .update_item(&item)
                .await
                .map_err(UseCaseError::Repository)?;
            Ok(())
        }
    }
}

impl ConvergenceSystemActionPort for HttpConvergencePort {
    async fn load_system_action_projects(
        &self,
    ) -> Result<Vec<ingot_usecases::convergence::SystemActionProjectState>, UseCaseError> {
        Err(UseCaseError::Internal(
            "http convergence port does not load system actions".into(),
        ))
    }

    async fn promote_queue_heads(&self, _project_id: ProjectId) -> Result<(), UseCaseError> {
        Err(UseCaseError::Internal(
            "http convergence port does not promote queue heads".into(),
        ))
    }

    async fn prepare_queue_head_convergence(
        &self,
        _project: &Project,
        _state: &ingot_usecases::convergence::SystemActionItemState,
        _queue_entry: &ConvergenceQueueEntry,
    ) -> Result<(), UseCaseError> {
        Err(UseCaseError::Internal(
            "http convergence port does not prepare queue heads".into(),
        ))
    }

    async fn invalidate_prepared_convergence(
        &self,
        _project_id: ProjectId,
        _item_id: ItemId,
    ) -> Result<(), UseCaseError> {
        Err(UseCaseError::Internal(
            "http convergence port does not invalidate prepared convergence".into(),
        ))
    }

    async fn reconcile_checkout_sync_ready(
        &self,
        _project: &Project,
        _item_id: ItemId,
        _revision: &ItemRevision,
    ) -> Result<bool, UseCaseError> {
        Err(UseCaseError::Internal(
            "http convergence port does not reconcile checkout sync".into(),
        ))
    }

    async fn auto_finalize_prepared_convergence(
        &self,
        _project_id: ProjectId,
        _item_id: ItemId,
    ) -> Result<(), UseCaseError> {
        Err(UseCaseError::Internal(
            "http convergence port does not auto-finalize convergence".into(),
        ))
    }
}

pub(super) async fn prepare_item_convergence(
    State(state): State<AppState>,
    Path((project_id, item_id)): Path<(String, String)>,
) -> Result<Json<ItemDetailResponse>, ApiError> {
    let project_id = parse_id::<ProjectId>(&project_id, "project")?;
    let item_id = parse_id::<ItemId>(&item_id, "item")?;
    let _guard = state
        .project_locks
        .acquire_project_mutation(project_id)
        .await;
    ConvergenceService::new(HttpConvergencePort {
        state: state.clone(),
    })
    .queue_prepare(project_id, item_id)
    .await
    .map_err(ApiError::from)?;
    let detail = load_item_detail(&state, project_id, item_id).await?;
    Ok(Json(detail))
}

pub(super) async fn approve_item(
    State(state): State<AppState>,
    Path((project_id, item_id)): Path<(String, String)>,
) -> Result<Json<ItemDetailResponse>, ApiError> {
    let project_id = parse_id::<ProjectId>(&project_id, "project")?;
    let item_id = parse_id::<ItemId>(&item_id, "item")?;
    let _guard = state
        .project_locks
        .acquire_project_mutation(project_id)
        .await;
    ConvergenceService::new(HttpConvergencePort {
        state: state.clone(),
    })
    .approve_item(project_id, item_id)
    .await
    .map_err(ApiError::from)?;
    let detail = load_item_detail(&state, project_id, item_id).await?;
    Ok(Json(detail))
}

pub(super) async fn reject_item_approval(
    State(state): State<AppState>,
    Path((project_id, item_id)): Path<(String, String)>,
    maybe_request: Option<Json<RejectApprovalRequest>>,
) -> Result<Json<ItemDetailResponse>, ApiError> {
    let project_id = parse_id::<ProjectId>(&project_id, "project")?;
    let item_id = parse_id::<ItemId>(&item_id, "item")?;
    let project = state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;
    let _guard = state
        .project_locks
        .acquire_project_mutation(project_id)
        .await;

    let item = state.db.get_item(item_id).await.map_err(repo_to_item)?;
    if item.project_id != project_id {
        return Err(UseCaseError::ItemNotFound.into());
    }
    let current_revision = state
        .db
        .get_revision(item.current_revision_id)
        .await
        .map_err(repo_to_internal)?;
    let jobs = state
        .db
        .list_jobs_by_item(item.id)
        .await
        .map_err(repo_to_internal)?;
    let request: ReviseItemRequest = maybe_request
        .map(|Json(request)| request)
        .unwrap_or_default()
        .into();
    let next_revision =
        build_superseding_revision(&state, &project, &item, &current_revision, &jobs, request)
            .await?;
    let cleared_escalation = item.escalation.is_escalated();
    let teardown = ConvergenceService::new(HttpConvergencePort {
        state: state.clone(),
    })
    .reject_item_approval(project_id, item.id, &next_revision)
    .await
    .map_err(ApiError::from)?;
    append_activity(
        &state,
        project_id,
        ActivityEventType::ApprovalRejected,
        "item",
        item.id,
        serde_json::json!({
            "new_revision_id": next_revision.id,
            "cancelled_convergence_id": teardown.first_cancelled_convergence_id,
            "cancelled_queue_entry_id": teardown.first_cancelled_queue_entry_id
        }),
    )
    .await?;
    if cleared_escalation {
        append_activity(
            &state,
            project_id,
            ActivityEventType::ItemEscalationCleared,
            "item",
            item.id,
            serde_json::json!({ "reason": "approval_reject" }),
        )
        .await?;
    }

    let detail = load_item_detail(&state, project_id, item.id).await?;
    Ok(Json(detail))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use chrono::Utc;
    use ingot_domain::ids::{ItemId, ItemRevisionId, ProjectId};
    use ingot_domain::item::ApprovalState;
    use ingot_test_support::fixtures::{ItemBuilder, RevisionBuilder};
    use ingot_test_support::git::{
        git_output as support_git_output, temp_git_repo as support_temp_git_repo,
    };
    use ingot_usecases::UseCaseError;
    use ingot_usecases::convergence::ConvergenceCommandPort;

    use super::super::test_helpers::test_app_state;

    fn temp_git_repo() -> PathBuf {
        support_temp_git_repo("ingot-http-api")
    }

    fn git_output(path: &std::path::Path, args: &[&str]) -> String {
        support_git_output(path, args)
    }

    #[tokio::test]
    async fn convergence_port_maps_missing_project_to_project_not_found() {
        let state = test_app_state().await;
        let error = HttpConvergencePort {
            state: state.clone(),
        }
        .load_queue_prepare_context(ProjectId::new(), ItemId::new())
        .await
        .expect_err("missing project should fail");

        assert!(matches!(error, UseCaseError::ProjectNotFound));
    }

    #[tokio::test]
    async fn convergence_port_rejects_cross_project_approval_context() {
        let state = test_app_state().await;
        let repo_a = temp_git_repo();
        let repo_b = temp_git_repo();
        let project_a = Project {
            id: ProjectId::new(),
            name: "A".into(),
            path: repo_a.display().to_string(),
            default_branch: "main".into(),
            color: "#000".into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let project_b = Project {
            id: ProjectId::new(),
            name: "B".into(),
            path: repo_b.display().to_string(),
            default_branch: "main".into(),
            color: "#111".into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        state
            .db
            .create_project(&project_a)
            .await
            .expect("project a");
        state
            .db
            .create_project(&project_b)
            .await
            .expect("project b");

        let head = git_output(&repo_b, &["rev-parse", "HEAD"]);
        let item = ItemBuilder::new(project_b.id, ItemRevisionId::new())
            .approval_state(ApprovalState::Pending)
            .build();
        let revision = RevisionBuilder::new(item.id)
            .id(item.current_revision_id)
            .explicit_seed(&head)
            .build();
        state
            .db
            .create_item_with_revision(&item, &revision)
            .await
            .expect("item b");

        let error = HttpConvergencePort {
            state: state.clone(),
        }
        .load_approval_context(project_a.id, item.id)
        .await
        .expect_err("cross-project item should fail");

        assert!(matches!(error, UseCaseError::ItemNotFound));
    }
}
