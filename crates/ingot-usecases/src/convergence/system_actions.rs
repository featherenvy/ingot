use chrono::Utc;
use ingot_domain::activity::{Activity, ActivityEventType, ActivitySubject};
use ingot_domain::convergence::{Convergence, ConvergenceStatus};
use ingot_domain::convergence_queue::ConvergenceQueueEntryStatus;
use ingot_domain::item::Item;
use ingot_domain::ports::{
    ActivityRepository, ConvergenceQueueRepository, InvalidatePreparedConvergenceMutation,
    InvalidatePreparedConvergenceRepository, WorkspaceRepository,
};
use ingot_domain::revision::ItemRevision;

use crate::UseCaseError;
use crate::item::approval_state_for_policy;

use super::command::ConvergenceService;
use super::finalization::{
    should_auto_finalize_prepared_convergence, should_invalidate_prepared_convergence,
    should_prepare_convergence,
};
use super::types::ConvergenceSystemActionPort;

impl<P> ConvergenceService<P>
where
    P: ConvergenceSystemActionPort,
{
    pub async fn tick_system_actions(&self) -> Result<bool, UseCaseError> {
        let projects = self.port.load_system_action_projects().await?;

        for project_state in projects {
            self.port
                .promote_queue_heads(project_state.project.id)
                .await?;

            for state in &project_state.items {
                if should_invalidate_prepared_convergence(
                    &state.item,
                    &state.revision,
                    &state.jobs,
                    &state.findings,
                    &state.convergences,
                ) {
                    self.port
                        .invalidate_prepared_convergence(project_state.project.id, state.item_id)
                        .await?;
                    return Ok(true);
                }

                let has_prepared_convergence = state.convergences.iter().any(|convergence| {
                    convergence.item_revision_id == state.revision.id
                        && convergence.state.status() == ConvergenceStatus::Prepared
                });

                if let Some(queue_entry) = state.queue_entry.as_ref() {
                    let should_prepare_queue_head = queue_entry.status
                        == ConvergenceQueueEntryStatus::Head
                        && should_prepare_convergence(
                            &state.item,
                            &state.revision,
                            &state.jobs,
                            &state.findings,
                            &state.convergences,
                        );

                    if should_prepare_queue_head {
                        self.port
                            .prepare_queue_head_convergence(
                                &project_state.project,
                                state,
                                queue_entry,
                            )
                            .await?;
                        return Ok(true);
                    }

                    let should_finalize = has_prepared_convergence
                        && should_auto_finalize_prepared_convergence(
                            &state.item,
                            &state.revision,
                            &state.jobs,
                            &state.findings,
                            &state.convergences,
                            Some(queue_entry),
                        );

                    if should_finalize
                        && self
                            .port
                            .auto_finalize_prepared_convergence(
                                project_state.project.id,
                                state.item_id,
                            )
                            .await?
                    {
                        return Ok(true);
                    }
                } else if project_state.project.execution_mode
                    == ingot_domain::project::ExecutionMode::Autopilot
                    && should_prepare_convergence(
                        &state.item,
                        &state.revision,
                        &state.jobs,
                        &state.findings,
                        &state.convergences,
                    )
                    && self
                        .port
                        .auto_queue_convergence(project_state.project.id, state.item_id)
                        .await?
                {
                    return Ok(true);
                }
            }
        }

        Ok(false)
    }
}

pub async fn promote_queue_heads<CQ, A>(
    queue_repo: &CQ,
    activity_repo: &A,
    project_id: ingot_domain::ids::ProjectId,
) -> Result<bool, UseCaseError>
where
    CQ: ConvergenceQueueRepository,
    A: ActivityRepository,
{
    let entries = queue_repo.list_active_by_project(project_id).await?;
    let mut lanes_with_heads = entries
        .iter()
        .filter(|entry| entry.status == ConvergenceQueueEntryStatus::Head)
        .map(|entry| entry.target_ref.clone())
        .collect::<std::collections::HashSet<_>>();

    let mut promoted = false;
    for entry in entries {
        if entry.status != ConvergenceQueueEntryStatus::Queued
            || lanes_with_heads.contains(&entry.target_ref)
        {
            continue;
        }

        let mut entry = entry;
        entry.status = ConvergenceQueueEntryStatus::Head;
        entry.head_acquired_at = Some(Utc::now());
        entry.updated_at = Utc::now();
        queue_repo.update(&entry).await?;
        activity_repo
            .append(&Activity {
                id: ingot_domain::ids::ActivityId::new(),
                project_id,
                event_type: ActivityEventType::ConvergenceLaneAcquired,
                subject: ActivitySubject::QueueEntry(entry.id),
                payload: serde_json::json!({
                    "item_id": entry.item_id,
                    "target_ref": entry.target_ref,
                }),
                created_at: Utc::now(),
            })
            .await?;
        lanes_with_heads.insert(entry.target_ref);
        promoted = true;
    }

    Ok(promoted)
}

pub async fn invalidate_prepared_convergence<W, T>(
    workspace_repo: &W,
    invalidate_repo: &T,
    item: &mut Item,
    revision: &ItemRevision,
    convergences: &[Convergence],
) -> Result<bool, UseCaseError>
where
    W: WorkspaceRepository,
    T: InvalidatePreparedConvergenceRepository,
{
    let mut convergence = match convergences
        .iter()
        .find(|convergence| {
            convergence.item_revision_id == revision.id
                && convergence.state.status() == ConvergenceStatus::Prepared
        })
        .cloned()
    {
        Some(c) => c,
        None => return Ok(false),
    };

    convergence.transition_to_failed(Some("target_ref_moved".into()), Utc::now());

    let workspace_update = if let Some(workspace_id) = convergence.state.integration_workspace_id()
    {
        let mut workspace = workspace_repo.get(workspace_id).await?;
        workspace.mark_stale(Utc::now());
        Some(workspace)
    } else {
        None
    };

    item.approval_state = approval_state_for_policy(revision.approval_policy);
    item.updated_at = Utc::now();

    let activity = Activity {
        id: ingot_domain::ids::ActivityId::new(),
        project_id: convergence.project_id,
        event_type: ActivityEventType::ConvergenceFailed,
        subject: ActivitySubject::Convergence(convergence.id),
        payload: serde_json::json!({ "item_id": item.id, "reason": "target_ref_moved" }),
        created_at: Utc::now(),
    };

    invalidate_repo
        .apply_invalidate_prepared_convergence(InvalidatePreparedConvergenceMutation {
            convergence,
            workspace_update,
            item: item.clone(),
            activity,
        })
        .await?;

    Ok(true)
}
