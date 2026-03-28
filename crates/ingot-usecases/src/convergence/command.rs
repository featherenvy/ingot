use chrono::Utc;
use ingot_domain::activity::{Activity, ActivityEventType, ActivitySubject};
use ingot_domain::convergence_queue::{ConvergenceQueueEntry, ConvergenceQueueEntryStatus};
use ingot_domain::ids::{ActivityId, ItemId, ProjectId};
use ingot_domain::item::ApprovalState;
use ingot_domain::revision::ItemRevision;

use crate::UseCaseError;
use crate::item::approval_state_for_policy;

use super::finalization::{finalize_prepared_convergence, should_prepare_convergence};
use super::types::{
    ApprovalFinalizeReadiness, ConvergenceApprovalContext, ConvergenceCommandPort,
    FinalizePreparedTrigger, PreparedConvergenceFinalizePort, RejectApprovalTeardown,
};

#[derive(Clone)]
pub struct ConvergenceService<P> {
    pub(super) port: P,
}

impl<P> ConvergenceService<P> {
    pub fn new(port: P) -> Self {
        Self { port }
    }
}

impl<P> ConvergenceService<P>
where
    P: ConvergenceCommandPort + PreparedConvergenceFinalizePort,
{
    pub async fn queue_prepare(
        &self,
        project_id: ProjectId,
        item_id: ItemId,
    ) -> Result<(), UseCaseError> {
        let context = self
            .port
            .load_queue_prepare_context(project_id, item_id)
            .await?;
        if context.item.project_id != project_id {
            return Err(UseCaseError::ItemNotFound);
        }

        if context.active_queue_entry.is_none()
            && !should_prepare_convergence(
                &context.item,
                &context.revision,
                &context.jobs,
                &context.findings,
                &context.convergences,
            )
        {
            return Err(UseCaseError::ConvergenceNotPreparable);
        }

        let mut queue_entry = if let Some(queue_entry) = context.active_queue_entry {
            queue_entry
        } else {
            let now = Utc::now();
            let queue_entry = ConvergenceQueueEntry {
                id: ingot_domain::ids::ConvergenceQueueEntryId::new(),
                project_id: context.project.id,
                item_id: context.item.id,
                item_revision_id: context.revision.id,
                target_ref: context.revision.target_ref.clone(),
                status: if context.lane_head.is_some() {
                    ConvergenceQueueEntryStatus::Queued
                } else {
                    ConvergenceQueueEntryStatus::Head
                },
                head_acquired_at: context.lane_head.is_none().then_some(now),
                created_at: now,
                updated_at: now,
                released_at: None,
            };
            self.port.create_queue_entry(&queue_entry).await?;
            self.port
                .append_activity(&Activity {
                    id: ActivityId::new(),
                    project_id: context.project.id,
                    event_type: ActivityEventType::ConvergenceQueued,
                    subject: ActivitySubject::QueueEntry(queue_entry.id),
                    payload: serde_json::json!({
                        "item_id": context.item.id,
                        "target_ref": context.revision.target_ref,
                    }),
                    created_at: now,
                })
                .await?;
            queue_entry
        };

        if queue_entry.status == ConvergenceQueueEntryStatus::Queued && context.lane_head.is_none()
        {
            queue_entry.status = ConvergenceQueueEntryStatus::Head;
            queue_entry.head_acquired_at = Some(Utc::now());
            queue_entry.updated_at = Utc::now();
            self.port.update_queue_entry(&queue_entry).await?;
            self.port
                .append_activity(&Activity {
                    id: ActivityId::new(),
                    project_id: context.project.id,
                    event_type: ActivityEventType::ConvergenceLaneAcquired,
                    subject: ActivitySubject::QueueEntry(queue_entry.id),
                    payload: serde_json::json!({
                        "item_id": context.item.id,
                        "target_ref": context.revision.target_ref,
                    }),
                    created_at: Utc::now(),
                })
                .await?;
        }

        Ok(())
    }

    pub async fn approve_item(
        &self,
        project_id: ProjectId,
        item_id: ItemId,
    ) -> Result<(), UseCaseError> {
        let ConvergenceApprovalContext {
            project,
            item,
            revision,
            has_active_job,
            has_active_convergence,
            finalize_readiness,
        } = self.port.load_approval_context(project_id, item_id).await?;

        if item.approval_state != ApprovalState::Pending {
            return Err(UseCaseError::ApprovalNotPending);
        }
        if has_active_job {
            return Err(UseCaseError::ActiveJobExists);
        }
        if has_active_convergence {
            return Err(UseCaseError::ActiveConvergenceExists);
        }
        let (convergence, queue_entry) = match finalize_readiness {
            ApprovalFinalizeReadiness::MissingPreparedConvergence => {
                return Err(UseCaseError::PreparedConvergenceMissing);
            }
            ApprovalFinalizeReadiness::PreparedConvergenceStale => {
                return Err(UseCaseError::PreparedConvergenceStale);
            }
            ApprovalFinalizeReadiness::ConvergenceNotQueued => {
                return Err(UseCaseError::ConvergenceNotQueued);
            }
            ApprovalFinalizeReadiness::ConvergenceNotLaneHead => {
                return Err(UseCaseError::ConvergenceNotLaneHead);
            }
            ApprovalFinalizeReadiness::Ready {
                convergence,
                queue_entry,
            } => (convergence, queue_entry),
        };

        finalize_prepared_convergence(
            &self.port,
            FinalizePreparedTrigger::ApprovalCommand,
            &project,
            &item,
            &revision,
            &convergence,
            &queue_entry,
        )
        .await?;

        self.port
            .append_activity(&Activity {
                id: ActivityId::new(),
                project_id,
                event_type: ActivityEventType::ApprovalApproved,
                subject: ActivitySubject::Item(item.id),
                payload: serde_json::json!({
                    "convergence_id": convergence.id,
                    "queue_entry_id": queue_entry.id,
                }),
                created_at: Utc::now(),
            })
            .await?;
        Ok(())
    }

    pub async fn reject_item_approval(
        &self,
        project_id: ProjectId,
        item_id: ItemId,
        next_revision: &ItemRevision,
    ) -> Result<RejectApprovalTeardown, UseCaseError> {
        let mut context = self
            .port
            .load_reject_approval_context(project_id, item_id)
            .await?;
        if context.item.approval_state != ApprovalState::Pending {
            return Err(UseCaseError::ApprovalNotPending);
        }
        if context.has_active_job {
            return Err(UseCaseError::ActiveJobExists);
        }
        if context.has_active_convergence {
            return Err(UseCaseError::ActiveConvergenceExists);
        }
        let teardown = self
            .port
            .teardown_reject_approval(project_id, item_id)
            .await?;
        if !teardown.has_cancelled_convergence {
            return Err(UseCaseError::PreparedConvergenceMissing);
        }

        context.item.current_revision_id = next_revision.id;
        context.item.approval_state = approval_state_for_policy(next_revision.approval_policy);
        context.item.escalation = ingot_domain::item::Escalation::None;
        context.item.updated_at = Utc::now();
        self.port
            .apply_rejected_approval(&context.item, next_revision)
            .await?;
        Ok(teardown)
    }
}
