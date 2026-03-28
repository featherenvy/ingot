use std::future::Future;

use chrono::Utc;
use ingot_domain::activity::{Activity, ActivityEventType, ActivitySubject};
use ingot_domain::commit_oid::CommitOid;
use ingot_domain::convergence::{Convergence, ConvergenceStatus};
use ingot_domain::convergence_queue::{ConvergenceQueueEntry, ConvergenceQueueEntryStatus};
use ingot_domain::finding::Finding;
use ingot_domain::git_operation::{
    GitOperation, GitOperationEntityRef, GitOperationStatus, OperationPayload,
};
use ingot_domain::ids::{ActivityId, ItemId, ProjectId};
use ingot_domain::item::ApprovalState;
use ingot_domain::job::Job;
use ingot_domain::ports::ConvergenceQueuePrepareContext;
use ingot_domain::ports::{
    ActivityRepository, ConvergenceQueueRepository, GitOperationRepository,
    InvalidatePreparedConvergenceMutation, InvalidatePreparedConvergenceRepository,
    RepositoryError, WorkspaceRepository,
};
use ingot_domain::project::Project;
use ingot_domain::revision::ItemRevision;

use ingot_workflow::{Evaluator, RecommendedAction};

use crate::UseCaseError;
use crate::item::approval_state_for_policy;

#[derive(Debug, Clone)]
pub struct SystemActionItemState {
    pub item_id: ItemId,
    pub item: ingot_domain::item::Item,
    pub revision: ItemRevision,
    pub jobs: Vec<Job>,
    pub findings: Vec<Finding>,
    pub convergences: Vec<Convergence>,
    pub queue_entry: Option<ConvergenceQueueEntry>,
}

#[derive(Debug, Clone)]
pub struct SystemActionProjectState {
    pub project: Project,
    pub items: Vec<SystemActionItemState>,
}

#[derive(Debug, Clone)]
pub struct ConvergenceApprovalContext {
    pub project: Project,
    pub item: ingot_domain::item::Item,
    pub revision: ItemRevision,
    pub has_active_job: bool,
    pub has_active_convergence: bool,
    pub finalize_readiness: ApprovalFinalizeReadiness,
}

#[derive(Debug, Clone)]
pub enum ApprovalFinalizeReadiness {
    MissingPreparedConvergence,
    PreparedConvergenceStale,
    ConvergenceNotQueued,
    ConvergenceNotLaneHead,
    Ready {
        convergence: Box<Convergence>,
        queue_entry: ConvergenceQueueEntry,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalizePreparedTrigger {
    ApprovalCommand,
    SystemCommand,
}

pub struct FinalizationTarget<'a> {
    pub convergence: &'a Convergence,
    pub queue_entry: &'a ConvergenceQueueEntry,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckoutFinalizationReadiness {
    Blocked { message: String },
    NeedsSync,
    Synced,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalizeTargetRefResult {
    AlreadyFinalized,
    UpdatedNow,
    Stale,
}

#[derive(Debug, Clone, Default)]
pub struct RejectApprovalTeardown {
    pub has_cancelled_convergence: bool,
    pub has_cancelled_queue_entry: bool,
    pub first_cancelled_convergence_id: Option<String>,
    pub first_cancelled_queue_entry_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RejectApprovalContext {
    pub item: ingot_domain::item::Item,
    pub has_active_job: bool,
    pub has_active_convergence: bool,
}

pub trait ConvergenceCommandPort: Send + Sync {
    fn load_queue_prepare_context(
        &self,
        project_id: ProjectId,
        item_id: ItemId,
    ) -> impl Future<Output = Result<ConvergenceQueuePrepareContext, UseCaseError>> + Send;

    fn create_queue_entry(
        &self,
        queue_entry: &ConvergenceQueueEntry,
    ) -> impl Future<Output = Result<(), UseCaseError>> + Send;

    fn update_queue_entry(
        &self,
        queue_entry: &ConvergenceQueueEntry,
    ) -> impl Future<Output = Result<(), UseCaseError>> + Send;

    fn append_activity(
        &self,
        activity: &Activity,
    ) -> impl Future<Output = Result<(), UseCaseError>> + Send;

    fn load_approval_context(
        &self,
        project_id: ProjectId,
        item_id: ItemId,
    ) -> impl Future<Output = Result<ConvergenceApprovalContext, UseCaseError>> + Send;

    fn update_item(
        &self,
        item: &ingot_domain::item::Item,
    ) -> impl Future<Output = Result<(), UseCaseError>> + Send;

    fn load_reject_approval_context(
        &self,
        project_id: ProjectId,
        item_id: ItemId,
    ) -> impl Future<Output = Result<RejectApprovalContext, UseCaseError>> + Send;

    fn teardown_reject_approval(
        &self,
        project_id: ProjectId,
        item_id: ItemId,
    ) -> impl Future<Output = Result<RejectApprovalTeardown, UseCaseError>> + Send;

    fn apply_rejected_approval(
        &self,
        item: &ingot_domain::item::Item,
        next_revision: &ItemRevision,
    ) -> impl Future<Output = Result<(), UseCaseError>> + Send;
}

pub trait ConvergenceSystemActionPort: Send + Sync {
    fn load_system_action_projects(
        &self,
    ) -> impl Future<Output = Result<Vec<SystemActionProjectState>, UseCaseError>> + Send;

    fn promote_queue_heads(
        &self,
        project_id: ProjectId,
    ) -> impl Future<Output = Result<(), UseCaseError>> + Send;

    fn prepare_queue_head_convergence(
        &self,
        project: &Project,
        state: &SystemActionItemState,
        queue_entry: &ConvergenceQueueEntry,
    ) -> impl Future<Output = Result<(), UseCaseError>> + Send;

    fn invalidate_prepared_convergence(
        &self,
        project_id: ProjectId,
        item_id: ItemId,
    ) -> impl Future<Output = Result<(), UseCaseError>> + Send;

    fn auto_finalize_prepared_convergence(
        &self,
        project_id: ProjectId,
        item_id: ItemId,
    ) -> impl Future<Output = Result<bool, UseCaseError>> + Send;

    fn auto_queue_convergence(
        &self,
        project_id: ProjectId,
        item_id: ItemId,
    ) -> impl Future<Output = Result<bool, UseCaseError>> + Send;
}

pub trait PreparedConvergenceFinalizePort: Send + Sync {
    fn find_or_create_finalize_operation(
        &self,
        operation: &GitOperation,
    ) -> impl Future<Output = Result<GitOperation, UseCaseError>> + Send;

    fn finalize_target_ref(
        &self,
        project: &Project,
        convergence: &Convergence,
    ) -> impl Future<Output = Result<FinalizeTargetRefResult, UseCaseError>> + Send;

    fn checkout_finalization_readiness(
        &self,
        project: &Project,
        item: &ingot_domain::item::Item,
        revision: &ItemRevision,
        prepared_commit_oid: &CommitOid,
    ) -> impl Future<Output = Result<CheckoutFinalizationReadiness, UseCaseError>> + Send;

    fn sync_checkout_to_prepared_commit(
        &self,
        project: &Project,
        revision: &ItemRevision,
        prepared_commit_oid: &CommitOid,
    ) -> impl Future<Output = Result<(), UseCaseError>> + Send;

    fn update_git_operation(
        &self,
        operation: &GitOperation,
    ) -> impl Future<Output = Result<(), UseCaseError>> + Send;

    fn apply_successful_finalization(
        &self,
        trigger: FinalizePreparedTrigger,
        project: &Project,
        item: &ingot_domain::item::Item,
        revision: &ItemRevision,
        target: FinalizationTarget<'_>,
        operation: &GitOperation,
    ) -> impl Future<Output = Result<(), UseCaseError>> + Send;
}

#[must_use]
pub fn should_prepare_convergence(
    item: &ingot_domain::item::Item,
    revision: &ItemRevision,
    jobs: &[Job],
    findings: &[Finding],
    convergences: &[Convergence],
) -> bool {
    Evaluator::new()
        .evaluate(item, revision, jobs, findings, convergences)
        .next_recommended_action
        == RecommendedAction::PrepareConvergence
}

#[must_use]
pub fn should_invalidate_prepared_convergence(
    item: &ingot_domain::item::Item,
    revision: &ItemRevision,
    jobs: &[Job],
    findings: &[Finding],
    convergences: &[Convergence],
) -> bool {
    Evaluator::new()
        .evaluate(item, revision, jobs, findings, convergences)
        .next_recommended_action
        == RecommendedAction::InvalidatePreparedConvergence
}

#[must_use]
pub fn should_auto_finalize_prepared_convergence(
    item: &ingot_domain::item::Item,
    revision: &ItemRevision,
    jobs: &[Job],
    findings: &[Finding],
    convergences: &[Convergence],
    queue_entry: Option<&ConvergenceQueueEntry>,
) -> bool {
    revision.approval_policy == ingot_domain::revision::ApprovalPolicy::NotRequired
        && matches!(
            queue_entry,
            Some(queue_entry) if queue_entry.status == ConvergenceQueueEntryStatus::Head
        )
        && Evaluator::new()
            .evaluate(item, revision, jobs, findings, convergences)
            .next_recommended_action
            == RecommendedAction::FinalizePreparedConvergence
}

/// Shared implementation of the find-or-create-finalize-operation logic
/// used by both the runtime and HTTP adapter `PreparedConvergenceFinalizePort`
/// implementations.
pub async fn find_or_create_finalize_operation<DB>(
    db: &DB,
    operation: &GitOperation,
) -> Result<GitOperation, UseCaseError>
where
    DB: GitOperationRepository + ActivityRepository,
{
    let convergence_id = match &operation.entity {
        GitOperationEntityRef::Convergence(id) => *id,
        other => {
            return Err(UseCaseError::Internal(format!(
                "expected convergence entity, got {:?}",
                other.entity_type()
            )));
        }
    };

    if let Some(existing) = db
        .find_unresolved_finalize_for_convergence(convergence_id)
        .await
        .map_err(UseCaseError::Repository)?
    {
        return Ok(existing);
    }

    match db.create(operation).await {
        Ok(()) => {
            db.append(&Activity {
                id: ActivityId::new(),
                project_id: operation.project_id,
                event_type: ActivityEventType::GitOperationPlanned,
                subject: ActivitySubject::GitOperation(operation.id),
                payload: serde_json::json!({
                    "operation_kind": operation.operation_kind(),
                    "entity_id": operation.entity.entity_id_string(),
                }),
                created_at: Utc::now(),
            })
            .await
            .map_err(UseCaseError::Repository)?;
            Ok(operation.clone())
        }
        Err(RepositoryError::Conflict(_)) => db
            .find_unresolved_finalize_for_convergence(convergence_id)
            .await
            .map_err(UseCaseError::Repository)?
            .ok_or_else(|| {
                UseCaseError::Internal(
                    "finalize git operation conflict without existing row".into(),
                )
            }),
        Err(other) => Err(UseCaseError::Repository(other)),
    }
}

#[derive(Clone)]
pub struct ConvergenceService<P> {
    port: P,
}

impl<P> ConvergenceService<P> {
    pub fn new(port: P) -> Self {
        Self { port }
    }
}

pub async fn finalize_prepared_convergence<P>(
    port: &P,
    trigger: FinalizePreparedTrigger,
    project: &Project,
    item: &ingot_domain::item::Item,
    revision: &ItemRevision,
    convergence: &Convergence,
    queue_entry: &ConvergenceQueueEntry,
) -> Result<(), UseCaseError>
where
    P: PreparedConvergenceFinalizePort,
{
    let prepared_commit_oid = convergence
        .state
        .prepared_commit_oid()
        .map(ToOwned::to_owned)
        .ok_or(UseCaseError::PreparedConvergenceMissing)?;
    let input_target_commit_oid = convergence
        .state
        .input_target_commit_oid()
        .map(ToOwned::to_owned)
        .ok_or(UseCaseError::PreparedConvergenceMissing)?;

    let planned_operation = GitOperation {
        id: ingot_domain::ids::GitOperationId::new(),
        project_id: project.id,
        entity: GitOperationEntityRef::Convergence(convergence.id),
        payload: OperationPayload::FinalizeTargetRef {
            workspace_id: convergence.state.integration_workspace_id(),
            ref_name: convergence.target_ref.clone(),
            expected_old_oid: input_target_commit_oid,
            new_oid: prepared_commit_oid.clone(),
            commit_oid: Some(prepared_commit_oid.clone()),
        },
        status: GitOperationStatus::Planned,
        created_at: Utc::now(),
        completed_at: None,
    };
    let mut operation = port
        .find_or_create_finalize_operation(&planned_operation)
        .await?;

    if port.finalize_target_ref(project, convergence).await? == FinalizeTargetRefResult::Stale {
        operation.status = GitOperationStatus::Failed;
        operation.completed_at = Some(Utc::now());
        port.update_git_operation(&operation).await?;
        return Err(UseCaseError::PreparedConvergenceStale);
    }

    if operation.status == GitOperationStatus::Planned {
        operation.status = GitOperationStatus::Applied;
        operation.completed_at = Some(Utc::now());
        port.update_git_operation(&operation).await?;
    }

    match port
        .checkout_finalization_readiness(project, item, revision, &prepared_commit_oid)
        .await?
    {
        CheckoutFinalizationReadiness::Blocked { message } => {
            return Err(UseCaseError::ProtocolViolation(message));
        }
        CheckoutFinalizationReadiness::NeedsSync => {
            port.sync_checkout_to_prepared_commit(project, revision, &prepared_commit_oid)
                .await?;
        }
        CheckoutFinalizationReadiness::Synced => {}
    }

    port.apply_successful_finalization(
        trigger,
        project,
        item,
        revision,
        FinalizationTarget {
            convergence,
            queue_entry,
        },
        &operation,
    )
    .await?;

    operation.status = GitOperationStatus::Reconciled;
    operation.completed_at = Some(Utc::now());
    port.update_git_operation(&operation).await?;

    Ok(())
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

/// Promote queued convergence queue entries to head when no head exists for their lane.
/// Pure DB operation.
pub async fn promote_queue_heads<CQ, A>(
    queue_repo: &CQ,
    activity_repo: &A,
    project_id: ProjectId,
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
                id: ActivityId::new(),
                project_id,
                event_type: ActivityEventType::ConvergenceLaneAcquired,
                subject: ActivitySubject::QueueEntry(entry.id),
                payload: serde_json::json!({ "item_id": entry.item_id, "target_ref": entry.target_ref }),
                created_at: Utc::now(),
            })
            .await?;
        lanes_with_heads.insert(entry.target_ref);
        promoted = true;
    }

    Ok(promoted)
}

/// Invalidate a prepared convergence whose target ref has moved.
/// Pure DB: marks convergence as failed, sets integration workspace to Stale,
/// resets approval state, appends activity.
///
/// All writes are applied atomically via `InvalidatePreparedConvergenceRepository`.
/// Returns true if a convergence was invalidated.
pub async fn invalidate_prepared_convergence<W, T>(
    workspace_repo: &W,
    invalidate_repo: &T,
    item: &mut ingot_domain::item::Item,
    revision: &ItemRevision,
    convergences: &[Convergence],
) -> Result<bool, UseCaseError>
where
    W: WorkspaceRepository,
    T: InvalidatePreparedConvergenceRepository,
{
    // --- Read/Compute phase ---

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
        id: ActivityId::new(),
        project_id: convergence.project_id,
        event_type: ActivityEventType::ConvergenceFailed,
        subject: ActivitySubject::Convergence(convergence.id),
        payload: serde_json::json!({ "item_id": item.id, "reason": "target_ref_moved" }),
        created_at: Utc::now(),
    };

    // --- Apply phase (single atomic write) ---

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

#[cfg(test)]
mod tests {
    use std::future::ready;
    use std::sync::{Arc, Mutex};

    use chrono::Utc;
    use ingot_domain::activity::Activity;

    use super::FinalizationTarget;
    use ingot_domain::commit_oid::CommitOid;
    use ingot_domain::convergence::{Convergence, ConvergenceStatus};
    use ingot_domain::convergence_queue::{ConvergenceQueueEntry, ConvergenceQueueEntryStatus};
    use ingot_domain::git_operation::GitOperation;
    use ingot_domain::ids::{ConvergenceId, ItemId, ItemRevisionId, ProjectId};
    use ingot_domain::item::ApprovalState;
    use ingot_domain::job::Job;
    use ingot_domain::ports::ConvergenceQueuePrepareContext;
    use ingot_domain::project::Project;
    use ingot_domain::revision::ItemRevision;
    use ingot_test_support::fixtures::{
        ConvergenceBuilder, ItemBuilder, JobBuilder, ProjectBuilder, RevisionBuilder,
    };
    use ingot_test_support::git::unique_temp_path;
    use uuid::Uuid;

    use super::{
        ApprovalFinalizeReadiness, CheckoutFinalizationReadiness, ConvergenceApprovalContext,
        ConvergenceCommandPort, ConvergenceService, ConvergenceSystemActionPort,
        FinalizePreparedTrigger, FinalizeTargetRefResult, PreparedConvergenceFinalizePort,
        RejectApprovalContext, RejectApprovalTeardown, SystemActionItemState,
        SystemActionProjectState,
    };
    use crate::UseCaseError;

    #[derive(Clone)]
    struct FakePort {
        queue_prepare_context: Arc<Mutex<Option<ConvergenceQueuePrepareContext>>>,
        approval_context: Arc<Mutex<Option<ConvergenceApprovalContext>>>,
        projects: Arc<Mutex<Vec<SystemActionProjectState>>>,
        calls: Arc<Mutex<Vec<String>>>,
        auto_finalize_progress: bool,
        checkout_finalization_readiness: CheckoutFinalizationReadiness,
        finalize_target_ref_result: FinalizeTargetRefResult,
        apply_successful_finalization_should_fail: bool,
    }

    impl FakePort {
        fn default_approval_context() -> ConvergenceApprovalContext {
            let nil = Uuid::nil();
            ConvergenceApprovalContext {
                project: ProjectBuilder::new(unique_temp_path("ingot-convergence-approve"))
                    .id(ProjectId::from_uuid(nil))
                    .build(),
                item: ItemBuilder::new(ProjectId::from_uuid(nil), ItemRevisionId::from_uuid(nil))
                    .id(ItemId::from_uuid(nil))
                    .approval_state(ApprovalState::Pending)
                    .build(),
                revision: RevisionBuilder::new(ItemId::from_uuid(nil))
                    .id(ItemRevisionId::from_uuid(nil))
                    .explicit_seed("abc123")
                    .build(),
                has_active_job: false,
                has_active_convergence: false,
                finalize_readiness: ApprovalFinalizeReadiness::Ready {
                    convergence: Box::new(
                        ingot_test_support::fixtures::ConvergenceBuilder::new(
                            ProjectId::from_uuid(nil),
                            ItemId::from_uuid(nil),
                            ItemRevisionId::from_uuid(nil),
                        )
                        .id(ConvergenceId::from_uuid(Uuid::nil()))
                        .status(ingot_domain::convergence::ConvergenceStatus::Prepared)
                        .target_head_valid(true)
                        .created_at(Utc::now())
                        .build(),
                    ),
                    queue_entry: ConvergenceQueueEntry {
                        id: ingot_domain::ids::ConvergenceQueueEntryId::from_uuid(Uuid::nil()),
                        project_id: ProjectId::from_uuid(Uuid::nil()),
                        item_id: ItemId::from_uuid(Uuid::nil()),
                        item_revision_id: ItemRevisionId::from_uuid(Uuid::nil()),
                        target_ref: "refs/heads/main".into(),
                        status: ConvergenceQueueEntryStatus::Head,
                        head_acquired_at: Some(Utc::now()),
                        created_at: Utc::now(),
                        updated_at: Utc::now(),
                        released_at: None,
                    },
                },
            }
        }

        fn with_projects(projects: Vec<SystemActionProjectState>) -> Self {
            Self {
                queue_prepare_context: Arc::new(Mutex::new(None)),
                approval_context: Arc::new(Mutex::new(Some(Self::default_approval_context()))),
                projects: Arc::new(Mutex::new(projects)),
                calls: Arc::new(Mutex::new(Vec::new())),
                auto_finalize_progress: true,
                checkout_finalization_readiness: CheckoutFinalizationReadiness::Synced,
                finalize_target_ref_result: FinalizeTargetRefResult::UpdatedNow,
                apply_successful_finalization_should_fail: false,
            }
        }

        fn with_queue_prepare_context(context: ConvergenceQueuePrepareContext) -> Self {
            Self {
                queue_prepare_context: Arc::new(Mutex::new(Some(context))),
                approval_context: Arc::new(Mutex::new(Some(Self::default_approval_context()))),
                projects: Arc::new(Mutex::new(Vec::new())),
                calls: Arc::new(Mutex::new(Vec::new())),
                auto_finalize_progress: true,
                checkout_finalization_readiness: CheckoutFinalizationReadiness::Synced,
                finalize_target_ref_result: FinalizeTargetRefResult::UpdatedNow,
                apply_successful_finalization_should_fail: false,
            }
        }

        fn with_approval_context(context: ConvergenceApprovalContext) -> Self {
            Self {
                queue_prepare_context: Arc::new(Mutex::new(None)),
                approval_context: Arc::new(Mutex::new(Some(context))),
                projects: Arc::new(Mutex::new(Vec::new())),
                calls: Arc::new(Mutex::new(Vec::new())),
                auto_finalize_progress: true,
                checkout_finalization_readiness: CheckoutFinalizationReadiness::Synced,
                finalize_target_ref_result: FinalizeTargetRefResult::UpdatedNow,
                apply_successful_finalization_should_fail: false,
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().expect("calls lock").clone()
        }
    }

    impl ConvergenceCommandPort for FakePort {
        fn load_queue_prepare_context(
            &self,
            _project_id: ProjectId,
            _item_id: ItemId,
        ) -> impl Future<Output = Result<ConvergenceQueuePrepareContext, UseCaseError>> + Send
        {
            ready(
                self.queue_prepare_context
                    .lock()
                    .expect("queue prepare lock")
                    .clone()
                    .ok_or(UseCaseError::Internal(
                        "missing queue prepare context".into(),
                    )),
            )
        }

        fn create_queue_entry(
            &self,
            queue_entry: &ConvergenceQueueEntry,
        ) -> impl Future<Output = Result<(), UseCaseError>> + Send {
            self.calls
                .lock()
                .expect("calls lock")
                .push(format!("create_queue:{}", queue_entry.id));
            ready(Ok(()))
        }

        fn update_queue_entry(
            &self,
            queue_entry: &ConvergenceQueueEntry,
        ) -> impl Future<Output = Result<(), UseCaseError>> + Send {
            self.calls
                .lock()
                .expect("calls lock")
                .push(format!("update_queue:{}", queue_entry.id));
            ready(Ok(()))
        }

        fn append_activity(
            &self,
            activity: &Activity,
        ) -> impl Future<Output = Result<(), UseCaseError>> + Send {
            self.calls
                .lock()
                .expect("calls lock")
                .push(format!("activity:{:?}", activity.event_type));
            ready(Ok(()))
        }

        fn load_approval_context(
            &self,
            _project_id: ProjectId,
            _item_id: ItemId,
        ) -> impl Future<Output = Result<ConvergenceApprovalContext, UseCaseError>> + Send {
            ready(
                self.approval_context
                    .lock()
                    .expect("approval context lock")
                    .clone()
                    .ok_or(UseCaseError::Internal("missing approval context".into())),
            )
        }

        fn update_item(
            &self,
            item: &ingot_domain::item::Item,
        ) -> impl Future<Output = Result<(), UseCaseError>> + Send {
            self.calls
                .lock()
                .expect("calls lock")
                .push(format!("update_item:{}", item.id));
            ready(Ok(()))
        }

        fn load_reject_approval_context(
            &self,
            _project_id: ProjectId,
            _item_id: ItemId,
        ) -> impl Future<Output = Result<RejectApprovalContext, UseCaseError>> + Send {
            let nil = Uuid::nil();
            ready(Ok(RejectApprovalContext {
                item: ItemBuilder::new(ProjectId::from_uuid(nil), ItemRevisionId::from_uuid(nil))
                    .id(ItemId::from_uuid(nil))
                    .approval_state(ApprovalState::Pending)
                    .build(),
                has_active_job: false,
                has_active_convergence: false,
            }))
        }

        fn teardown_reject_approval(
            &self,
            _project_id: ProjectId,
            _item_id: ItemId,
        ) -> impl Future<Output = Result<RejectApprovalTeardown, UseCaseError>> + Send {
            ready(Ok(RejectApprovalTeardown {
                has_cancelled_convergence: true,
                has_cancelled_queue_entry: true,
                first_cancelled_convergence_id: None,
                first_cancelled_queue_entry_id: None,
            }))
        }

        fn apply_rejected_approval(
            &self,
            item: &ingot_domain::item::Item,
            next_revision: &ItemRevision,
        ) -> impl Future<Output = Result<(), UseCaseError>> + Send {
            self.calls
                .lock()
                .expect("calls lock")
                .push(format!("reject:{}:{}", item.id, next_revision.id));
            ready(Ok(()))
        }
    }

    impl ConvergenceSystemActionPort for FakePort {
        fn load_system_action_projects(
            &self,
        ) -> impl Future<Output = Result<Vec<SystemActionProjectState>, UseCaseError>> + Send
        {
            ready(Ok(self.projects.lock().expect("projects lock").clone()))
        }

        fn promote_queue_heads(
            &self,
            project_id: ProjectId,
        ) -> impl Future<Output = Result<(), UseCaseError>> + Send {
            self.calls
                .lock()
                .expect("calls lock")
                .push(format!("promote:{project_id}"));
            ready(Ok(()))
        }

        fn prepare_queue_head_convergence(
            &self,
            project: &Project,
            state: &SystemActionItemState,
            _queue_entry: &ConvergenceQueueEntry,
        ) -> impl Future<Output = Result<(), UseCaseError>> + Send {
            self.calls
                .lock()
                .expect("calls lock")
                .push(format!("prepare:{}:{}", project.id, state.item_id));
            ready(Ok(()))
        }

        fn invalidate_prepared_convergence(
            &self,
            project_id: ProjectId,
            item_id: ItemId,
        ) -> impl Future<Output = Result<(), UseCaseError>> + Send {
            self.calls
                .lock()
                .expect("calls lock")
                .push(format!("invalidate:{project_id}:{item_id}"));
            ready(Ok(()))
        }

        fn auto_finalize_prepared_convergence(
            &self,
            project_id: ProjectId,
            item_id: ItemId,
        ) -> impl Future<Output = Result<bool, UseCaseError>> + Send {
            self.calls
                .lock()
                .expect("calls lock")
                .push(format!("finalize:{project_id}:{item_id}"));
            ready(Ok(self.auto_finalize_progress))
        }

        fn auto_queue_convergence(
            &self,
            project_id: ProjectId,
            item_id: ItemId,
        ) -> impl Future<Output = Result<bool, UseCaseError>> + Send {
            self.calls
                .lock()
                .expect("calls lock")
                .push(format!("auto_queue:{project_id}:{item_id}"));
            ready(Ok(true))
        }
    }

    impl PreparedConvergenceFinalizePort for FakePort {
        fn find_or_create_finalize_operation(
            &self,
            operation: &GitOperation,
        ) -> impl Future<Output = Result<GitOperation, UseCaseError>> + Send {
            self.calls.lock().expect("calls lock").push(format!(
                "find_or_create_op:{}",
                operation.entity.entity_id_string()
            ));
            ready(Ok(operation.clone()))
        }

        fn finalize_target_ref(
            &self,
            _project: &Project,
            convergence: &Convergence,
        ) -> impl Future<Output = Result<FinalizeTargetRefResult, UseCaseError>> + Send {
            self.calls
                .lock()
                .expect("calls lock")
                .push(format!("finalize_target_ref:{}", convergence.id));
            ready(Ok(self.finalize_target_ref_result))
        }

        fn checkout_finalization_readiness(
            &self,
            _project: &Project,
            item: &ingot_domain::item::Item,
            _revision: &ItemRevision,
            _prepared_commit_oid: &CommitOid,
        ) -> impl Future<Output = Result<CheckoutFinalizationReadiness, UseCaseError>> + Send
        {
            self.calls
                .lock()
                .expect("calls lock")
                .push(format!("checkout_readiness:{}", item.id));
            ready(Ok(self.checkout_finalization_readiness.clone()))
        }

        fn sync_checkout_to_prepared_commit(
            &self,
            _project: &Project,
            revision: &ItemRevision,
            _prepared_commit_oid: &CommitOid,
        ) -> impl Future<Output = Result<(), UseCaseError>> + Send {
            self.calls
                .lock()
                .expect("calls lock")
                .push(format!("sync_checkout:{}", revision.id));
            ready(Ok(()))
        }

        fn update_git_operation(
            &self,
            operation: &GitOperation,
        ) -> impl Future<Output = Result<(), UseCaseError>> + Send {
            self.calls
                .lock()
                .expect("calls lock")
                .push(format!("update_op:{:?}", operation.status));
            ready(Ok(()))
        }

        fn apply_successful_finalization(
            &self,
            trigger: FinalizePreparedTrigger,
            _project: &Project,
            item: &ingot_domain::item::Item,
            _revision: &ItemRevision,
            target: FinalizationTarget<'_>,
            _operation: &GitOperation,
        ) -> impl Future<Output = Result<(), UseCaseError>> + Send {
            self.calls.lock().expect("calls lock").push(format!(
                "apply_successful_finalization:{trigger:?}:{}:{}",
                item.id, target.convergence.id
            ));
            ready(if self.apply_successful_finalization_should_fail {
                Err(UseCaseError::Internal("boom".into()))
            } else {
                Ok(())
            })
        }
    }

    #[tokio::test]
    async fn queue_prepare_creates_lane_head_when_lane_is_empty() {
        let now = Utc::now();
        let project_id = ProjectId::from_uuid(Uuid::nil());
        let item_id = ItemId::from_uuid(Uuid::nil());
        let revision_id = ItemRevisionId::from_uuid(Uuid::nil());
        let project = ProjectBuilder::new(unique_temp_path("ingot-convergence"))
            .id(project_id)
            .created_at(now)
            .build();
        let item = ItemBuilder::new(project_id, revision_id)
            .id(item_id)
            .created_at(now)
            .build();
        let revision = RevisionBuilder::new(item_id)
            .id(revision_id)
            .explicit_seed("seed")
            .created_at(now)
            .build();
        let port = FakePort::with_queue_prepare_context(ConvergenceQueuePrepareContext {
            project,
            item,
            revision,
            jobs: vec![fake_completed_validate_job("prepare_convergence")],
            findings: vec![],
            convergences: vec![],
            active_queue_entry: None,
            lane_head: None,
        });
        let service = ConvergenceService::new(port.clone());

        service
            .queue_prepare(project_id, item_id)
            .await
            .expect("queue prepare");

        let calls = port.calls();
        assert!(calls.iter().any(|call| call.starts_with("create_queue:")));
        assert!(
            calls
                .iter()
                .any(|call| call == "activity:ConvergenceQueued")
        );
    }

    #[tokio::test]
    async fn invalidation_wins_first() {
        let port = FakePort::with_projects(vec![project_state("invalidate_prepared_convergence")]);
        let service = ConvergenceService::new(port.clone());

        let made_progress = service
            .tick_system_actions()
            .await
            .expect("tick system actions");

        assert!(made_progress);
        assert!(
            port.calls()
                .iter()
                .any(|call| call.starts_with("invalidate:"))
        );
    }

    #[tokio::test]
    async fn prepare_runs_for_queue_head() {
        let port = FakePort::with_projects(vec![project_state("prepare_convergence")]);
        let service = ConvergenceService::new(port.clone());

        let made_progress = service
            .tick_system_actions()
            .await
            .expect("tick system actions");

        assert!(made_progress);
        assert!(port.calls().iter().any(|call| call.starts_with("prepare:")));
    }

    #[tokio::test]
    async fn blocked_auto_finalize_does_not_count_as_progress() {
        let port = FakePort {
            auto_finalize_progress: false,
            ..FakePort::with_projects(vec![project_state("finalize_prepared_convergence")])
        };
        let service = ConvergenceService::new(port.clone());

        let made_progress = service
            .tick_system_actions()
            .await
            .expect("tick system actions");

        assert!(!made_progress);
        let calls = port.calls();
        assert!(calls.iter().any(|call| call.starts_with("finalize:")));
        assert!(!calls.iter().any(|call| call.starts_with("prepare:")));
    }

    #[tokio::test]
    async fn blocked_auto_finalize_allows_later_system_action_to_run() {
        let port = FakePort {
            auto_finalize_progress: false,
            ..FakePort::with_projects(vec![
                project_state("finalize_prepared_convergence"),
                project_state("prepare_convergence"),
            ])
        };
        let service = ConvergenceService::new(port.clone());

        let made_progress = service
            .tick_system_actions()
            .await
            .expect("tick system actions");

        assert!(made_progress);
        let calls = port.calls();
        let finalize_index = calls
            .iter()
            .position(|call| call.starts_with("finalize:"))
            .expect("finalize call");
        let prepare_index = calls
            .iter()
            .position(|call| call.starts_with("prepare:"))
            .expect("prepare call");
        assert!(finalize_index < prepare_index);
    }

    #[tokio::test]
    async fn approve_item_returns_stale_when_readiness_is_stale() {
        let mut context = FakePort::default_approval_context();
        context.finalize_readiness = ApprovalFinalizeReadiness::PreparedConvergenceStale;
        let port = FakePort::with_approval_context(context);
        let service = ConvergenceService::new(port);

        let error = service
            .approve_item(
                ProjectId::from_uuid(Uuid::nil()),
                ItemId::from_uuid(Uuid::nil()),
            )
            .await
            .expect_err("approval should reject stale convergence");

        assert!(matches!(error, UseCaseError::PreparedConvergenceStale));
    }

    #[tokio::test]
    async fn approve_item_uses_shared_finalizer_for_already_finalized_target() {
        let port = FakePort {
            finalize_target_ref_result: FinalizeTargetRefResult::AlreadyFinalized,
            ..FakePort::with_approval_context(FakePort::default_approval_context())
        };
        let service = ConvergenceService::new(port.clone());

        service
            .approve_item(
                ProjectId::from_uuid(Uuid::nil()),
                ItemId::from_uuid(Uuid::nil()),
            )
            .await
            .expect("approval should finalize");

        let calls = port.calls();
        assert!(
            calls
                .iter()
                .any(|call| call == "update_op:Applied" || call == "update_op:Reconciled")
        );
        assert!(
            calls
                .iter()
                .any(|call| call.starts_with("finalize_target_ref:"))
        );
        assert!(
            calls
                .iter()
                .any(|call| { call.starts_with("apply_successful_finalization:ApprovalCommand:") })
        );
    }

    #[tokio::test]
    async fn approve_item_keeps_finalize_operation_unresolved_when_success_persistence_fails() {
        let port = FakePort {
            apply_successful_finalization_should_fail: true,
            ..FakePort::with_approval_context(FakePort::default_approval_context())
        };
        let service = ConvergenceService::new(port.clone());

        let error = service
            .approve_item(
                ProjectId::from_uuid(Uuid::nil()),
                ItemId::from_uuid(Uuid::nil()),
            )
            .await
            .expect_err("approval should surface persistence failure");

        assert!(matches!(error, UseCaseError::Internal(message) if message == "boom"));
        let calls = port.calls();
        assert!(calls.iter().any(|call| call == "update_op:Applied"));
        assert!(!calls.iter().any(|call| call == "update_op:Reconciled"));
    }

    fn project_state(next_action: &str) -> SystemActionProjectState {
        let created_at = Utc::now();
        let project_id = ProjectId::from_uuid(Uuid::nil());
        let item_id = ItemId::from_uuid(Uuid::nil());
        let revision_id = ItemRevisionId::from_uuid(Uuid::nil());
        let project = ProjectBuilder::new(unique_temp_path("ingot-convergence"))
            .id(project_id)
            .build();
        let revision = RevisionBuilder::new(item_id)
            .id(revision_id)
            .approval_policy(if next_action == "finalize_prepared_convergence" {
                ingot_domain::revision::ApprovalPolicy::NotRequired
            } else {
                ingot_domain::revision::ApprovalPolicy::Required
            })
            .explicit_seed("seed")
            .created_at(created_at)
            .build();
        let approval_state = if next_action == "finalize_prepared_convergence" {
            ApprovalState::NotRequired
        } else {
            ApprovalState::NotRequested
        };
        let item = ItemBuilder::new(project_id, revision_id)
            .id(item_id)
            .approval_state(approval_state)
            .created_at(created_at)
            .build();
        let convergence = ConvergenceBuilder::new(project_id, item_id, revision_id)
            .id(ConvergenceId::from_uuid(Uuid::nil()))
            .status(if next_action == "prepare_convergence" {
                ConvergenceStatus::Failed
            } else {
                ConvergenceStatus::Prepared
            })
            .target_head_valid(next_action != "invalidate_prepared_convergence")
            .created_at(created_at)
            .build();
        let queue_entry = ConvergenceQueueEntry {
            id: ingot_domain::ids::ConvergenceQueueEntryId::from_uuid(Uuid::nil()),
            project_id,
            item_id,
            item_revision_id: revision_id,
            target_ref: "refs/heads/main".into(),
            status: ConvergenceQueueEntryStatus::Head,
            head_acquired_at: Some(created_at),
            created_at,
            updated_at: created_at,
            released_at: None,
        };

        SystemActionProjectState {
            project,
            items: vec![SystemActionItemState {
                item_id,
                item,
                revision,
                jobs: vec![fake_completed_validate_job(next_action)],
                findings: vec![],
                convergences: vec![convergence],
                queue_entry: Some(queue_entry),
            }],
        }
    }

    fn fake_completed_validate_job(next_action: &str) -> Job {
        let created_at = Utc::now();
        let nil = Uuid::nil();
        let step_id = if next_action == "prepare_convergence" {
            "validate_candidate_initial"
        } else {
            "validate_integrated"
        };
        JobBuilder::new(
            ProjectId::from_uuid(nil),
            ItemId::from_uuid(nil),
            ItemRevisionId::from_uuid(nil),
            step_id,
        )
        .status(ingot_domain::job::JobStatus::Completed)
        .outcome_class(ingot_domain::job::OutcomeClass::Clean)
        .phase_kind(ingot_domain::job::PhaseKind::Validate)
        .workspace_kind(ingot_domain::workspace::WorkspaceKind::Integration)
        .execution_permission(ingot_domain::job::ExecutionPermission::MustNotMutate)
        .context_policy(ingot_domain::job::ContextPolicy::ResumeContext)
        .job_input(ingot_domain::job::JobInput::None)
        .output_artifact_kind(ingot_domain::job::OutputArtifactKind::ValidationReport)
        .created_at(created_at)
        .started_at(created_at)
        .ended_at(created_at)
        .build()
    }
}
