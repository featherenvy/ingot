use super::deps::*;
use super::support::errors::{repo_to_internal, repo_to_item, repo_to_project};
use super::types::*;
use crate::router::infra_ports::HttpInfraAdapter;

pub(super) struct ItemRuntimeSnapshot {
    pub current_revision: ItemRevision,
    pub jobs: Vec<Job>,
    pub findings: Vec<Finding>,
    pub convergences: Vec<Convergence>,
}

pub(super) async fn load_item_runtime_snapshot(
    state: &AppState,
    project_id: ProjectId,
    item: &Item,
) -> Result<ItemRuntimeSnapshot, ApiError> {
    let db = &state.db;

    let current_revision = db
        .get_revision(item.current_revision_id)
        .await
        .map_err(repo_to_internal)?;
    let jobs = db
        .list_jobs_by_item(item.id)
        .await
        .map_err(repo_to_internal)?;
    let findings = db
        .list_findings_by_item(item.id)
        .await
        .map_err(repo_to_internal)?;
    let convergences = db
        .list_convergences_by_item(item.id)
        .await
        .map_err(repo_to_internal)?;
    let convergences = hydrate_convergence_validity(state, project_id, convergences).await?;
    Ok(ItemRuntimeSnapshot {
        current_revision,
        jobs,
        findings,
        convergences,
    })
}

pub(super) async fn load_item_detail(
    state: &AppState,
    project_id: ProjectId,
    item_id: ItemId,
) -> Result<ItemDetailResponse, ApiError> {
    let db = &state.db;
    let item = db.get_item(item_id).await.map_err(repo_to_item)?;
    if item.project_id != project_id {
        return Err(UseCaseError::ItemNotFound.into());
    }
    let project = db
        .get_project(item.project_id)
        .await
        .map_err(repo_to_project)?;
    let snapshot = load_item_runtime_snapshot(state, project.id, &item).await?;
    let revision_history = db
        .list_revisions_by_item(item.id)
        .await
        .map_err(repo_to_internal)?;
    let workspaces = db
        .list_workspaces_by_item(item.id)
        .await
        .map_err(repo_to_internal)?;
    let revision_context = db
        .get_revision_context(item.current_revision_id)
        .await
        .map_err(repo_to_internal)?;
    let revision_context_summary = parse_revision_context_summary(revision_context.as_ref());
    let evaluator = Evaluator::new();
    let (evaluation, queue) =
        evaluate_item_snapshot(state, &project, &item, &snapshot, &evaluator).await?;
    let diagnostics = evaluation.diagnostics.clone();
    let ItemRuntimeSnapshot {
        current_revision,
        jobs,
        findings,
        convergences,
    } = snapshot;

    Ok(ItemDetailResponse {
        item,
        execution_mode: project.execution_mode,
        current_revision,
        evaluation,
        queue,
        revision_history,
        jobs,
        findings,
        workspaces,
        convergences: convergences.into_iter().map(convergence_response).collect(),
        revision_context_summary,
        diagnostics,
    })
}

pub(super) async fn evaluate_item_snapshot(
    state: &AppState,
    project: &Project,
    item: &Item,
    snapshot: &ItemRuntimeSnapshot,
    evaluator: &Evaluator,
) -> Result<(Evaluation, QueueStatusResponse), ApiError> {
    let ItemRuntimeSnapshot {
        current_revision,
        jobs,
        findings,
        convergences,
    } = snapshot;
    let evaluation = evaluator.evaluate(item, current_revision, jobs, findings, convergences);
    let queue = load_queue_status(state, current_revision, project, &evaluation).await?;
    let evaluation =
        overlay_evaluation_with_queue_state(current_revision, convergences, evaluation, &queue);

    Ok((evaluation, queue))
}

fn convergence_response(convergence: Convergence) -> ConvergenceResponse {
    ConvergenceResponse {
        id: convergence.id,
        status: convergence.state.status(),
        input_target_commit_oid: convergence.state.input_target_commit_oid().cloned(),
        prepared_commit_oid: convergence.state.prepared_commit_oid().cloned(),
        final_target_commit_oid: convergence.state.final_target_commit_oid().cloned(),
        target_head_valid: convergence.target_head_valid.unwrap_or(true),
    }
}

fn empty_queue_status() -> QueueStatusResponse {
    QueueStatusResponse {
        state: None,
        position: None,
        lane_owner_item_id: None,
        lane_target_ref: None,
        checkout_sync_blocked: false,
        checkout_sync_message: None,
    }
}

pub(super) fn overlay_evaluation_with_queue_state(
    revision: &ItemRevision,
    convergences: &[Convergence],
    mut evaluation: Evaluation,
    queue: &QueueStatusResponse,
) -> Evaluation {
    let has_prepared_convergence = convergences.iter().any(|convergence| {
        convergence.item_revision_id == revision.id
            && convergence.state.status() == ingot_domain::convergence::ConvergenceStatus::Prepared
    });

    let awaiting_lane = (queue.state.is_some()
        && evaluation.next_recommended_action
            == RecommendedAction::named(NamedRecommendedAction::PrepareConvergence))
        || queue.state == Some(ConvergenceQueueEntryStatus::Queued);
    if awaiting_lane {
        set_awaiting_convergence_lane(&mut evaluation);
    }

    if queue.checkout_sync_blocked
        && revision.approval_policy == ApprovalPolicy::NotRequired
        && has_prepared_convergence
        && evaluation.next_recommended_action
            == RecommendedAction::named(NamedRecommendedAction::FinalizePreparedConvergence)
    {
        evaluation.next_recommended_action =
            RecommendedAction::named(NamedRecommendedAction::ResolveCheckoutSync);
        evaluation.dispatchable_step_id = None;
        evaluation.allowed_actions.clear();
        evaluation.phase_status = Some(PhaseStatus::AwaitingConvergence);
    }

    evaluation
}

fn set_awaiting_convergence_lane(evaluation: &mut Evaluation) {
    evaluation.next_recommended_action =
        RecommendedAction::named(NamedRecommendedAction::AwaitConvergenceLane);
    evaluation.dispatchable_step_id = None;
    evaluation
        .allowed_actions
        .retain(|action| *action != AllowedAction::PrepareConvergence);
    evaluation.phase_status = Some(PhaseStatus::AwaitingConvergence);
}

pub(super) async fn load_queue_status(
    state: &AppState,
    revision: &ItemRevision,
    project: &Project,
    evaluation: &Evaluation,
) -> Result<QueueStatusResponse, ApiError> {
    let db = &state.db;

    let Some(active_entry) = db
        .find_active_queue_entry_for_revision(revision.id)
        .await
        .map_err(repo_to_internal)?
    else {
        return Ok(empty_queue_status());
    };

    let lane_entries = db
        .list_active_queue_entries_for_lane(project.id, &revision.target_ref)
        .await
        .map_err(repo_to_internal)?;
    let lane_owner_item_id = lane_entries
        .iter()
        .find(|entry| entry.status == ConvergenceQueueEntryStatus::Head)
        .map(|entry| entry.item_id);
    let position = lane_entries
        .iter()
        .position(|entry| entry.id == active_entry.id)
        .map(|index| index as u32 + 1);

    let mut queue = QueueStatusResponse {
        state: Some(active_entry.status),
        position,
        lane_owner_item_id,
        lane_target_ref: Some(active_entry.target_ref.clone()),
        checkout_sync_blocked: false,
        checkout_sync_message: None,
    };

    let should_check_checkout = active_entry.status == ConvergenceQueueEntryStatus::Head
        && evaluation.next_recommended_action
            == RecommendedAction::named(NamedRecommendedAction::FinalizePreparedConvergence);
    if should_check_checkout {
        if let CheckoutSyncStatus::Blocked { message, .. } = HttpInfraAdapter::new(state)
            .checkout_sync_status(project, &revision.target_ref)
            .await?
        {
            queue.checkout_sync_blocked = true;
            queue.checkout_sync_message = Some(message);
        }
    }

    Ok(queue)
}

pub(super) async fn hydrate_convergence_validity(
    state: &AppState,
    project_id: ProjectId,
    mut convergences: Vec<Convergence>,
) -> Result<Vec<Convergence>, ApiError> {
    let infra = HttpInfraAdapter::new(state);
    for convergence in &mut convergences {
        convergence.target_head_valid = infra
            .compute_target_head_valid(project_id, convergence)
            .await?;
    }

    Ok(convergences)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::hydrate_convergence_validity;
    use crate::router::test_helpers::test_app_state;
    use chrono::Utc;
    use ingot_domain::ids::{ItemId, ItemRevisionId, ProjectId};
    use ingot_domain::test_support::{ConvergenceBuilder, ProjectBuilder};
    use ingot_test_support::git::{
        git_output as support_git_output, run_git as support_git,
        temp_git_repo as support_temp_git_repo, write_file as support_write_file,
    };
    use uuid::Uuid;

    fn temp_git_repo() -> PathBuf {
        support_temp_git_repo("ingot-http-api")
    }

    fn git(path: &std::path::Path, args: &[&str]) {
        support_git(path, args);
    }

    fn git_output(path: &std::path::Path, args: &[&str]) -> String {
        support_git_output(path, args)
    }

    fn write_file(path: &std::path::Path, contents: &str) {
        support_write_file(path, contents);
    }

    #[tokio::test]
    async fn target_head_valid_tracks_ref_movement() {
        let state = test_app_state().await;
        let repo = temp_git_repo();
        let project = ProjectBuilder::new(&repo)
            .id(ProjectId::from_uuid(Uuid::nil()))
            .created_at(Utc::now())
            .build();
        state
            .db
            .create_project(&project)
            .await
            .expect("create project");
        let first = git_output(&repo, &["rev-parse", "HEAD"]);
        let mut convergence = ConvergenceBuilder::new(
            project.id,
            ItemId::from_uuid(Uuid::nil()),
            ItemRevisionId::from_uuid(Uuid::nil()),
        )
        .target_head_valid(true)
        .created_at(Utc::now())
        .input_target_commit_oid(first.clone())
        .build();
        convergence.target_ref = "refs/heads/main".into();

        let valid = hydrate_convergence_validity(&state, project.id, vec![convergence.clone()])
            .await
            .expect("compute validity");
        assert_eq!(valid[0].target_head_valid, Some(true));

        write_file(&repo.join("tracked.txt"), "next");
        git(&repo, &["add", "tracked.txt"]);
        git(&repo, &["commit", "-m", "next"]);

        let stale = hydrate_convergence_validity(&state, project.id, vec![convergence])
            .await
            .expect("compute stale validity");
        assert_eq!(stale[0].target_head_valid, Some(false));
    }
}
