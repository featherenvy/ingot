use std::future::Future;

use chrono::Utc;
use ingot_domain::activity::{Activity, ActivityEventType};
use ingot_domain::convergence::{Convergence, ConvergenceStatus};
use ingot_domain::convergence_queue::{ConvergenceQueueEntry, ConvergenceQueueEntryStatus};
use ingot_domain::finding::Finding;
use ingot_domain::ids::{ActivityId, ConvergenceId, ItemId, ProjectId};
use ingot_domain::item::ApprovalState;
use ingot_domain::job::Job;
use ingot_domain::ports::ConvergenceQueuePrepareContext;
use ingot_domain::project::Project;
use ingot_domain::revision::ItemRevision;
use ingot_workflow::Evaluator;

use crate::UseCaseError;

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
    pub item: ingot_domain::item::Item,
    pub has_active_job: bool,
    pub has_active_convergence: bool,
    pub prepared_convergence_id: Option<ConvergenceId>,
    pub prepared_target_valid: bool,
    pub queue_entry: Option<ConvergenceQueueEntry>,
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

    fn reconcile_checkout_sync_ready(
        &self,
        project: &Project,
        item_id: ItemId,
        revision: &ItemRevision,
    ) -> impl Future<Output = Result<bool, UseCaseError>> + Send;

    fn auto_finalize_prepared_convergence(
        &self,
        project_id: ProjectId,
        item_id: ItemId,
    ) -> impl Future<Output = Result<(), UseCaseError>> + Send;
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

impl<P> ConvergenceService<P>
where
    P: ConvergenceCommandPort,
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

        let evaluation = Evaluator::new().evaluate(
            &context.item,
            &context.revision,
            &context.jobs,
            &context.findings,
            &context.convergences,
        );
        if context.active_queue_entry.is_none()
            && evaluation.next_recommended_action != "prepare_convergence"
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
                    entity_type: "queue_entry".into(),
                    entity_id: queue_entry.id.to_string(),
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
                    entity_type: "queue_entry".into(),
                    entity_id: queue_entry.id.to_string(),
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
        let mut context = self.port.load_approval_context(project_id, item_id).await?;
        if context.item.approval_state != ApprovalState::Pending {
            return Err(UseCaseError::ApprovalNotPending);
        }
        if context.has_active_job {
            return Err(UseCaseError::ActiveJobExists);
        }
        if context.has_active_convergence {
            return Err(UseCaseError::ActiveConvergenceExists);
        }
        let convergence_id = context
            .prepared_convergence_id
            .ok_or(UseCaseError::PreparedConvergenceMissing)?;
        if !context.prepared_target_valid {
            return Err(UseCaseError::PreparedConvergenceStale);
        }
        let queue_entry = context
            .queue_entry
            .ok_or(UseCaseError::ConvergenceNotQueued)?;
        if queue_entry.status != ConvergenceQueueEntryStatus::Head {
            return Err(UseCaseError::ConvergenceNotLaneHead);
        }

        context.item.approval_state = ApprovalState::Granted;
        context.item.updated_at = Utc::now();
        self.port.update_item(&context.item).await?;
        self.port
            .append_activity(&Activity {
                id: ActivityId::new(),
                project_id,
                event_type: ActivityEventType::ApprovalApproved,
                entity_type: "item".into(),
                entity_id: context.item.id.to_string(),
                payload: serde_json::json!({
                    "convergence_id": convergence_id,
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
        if !matches!(
            context.item.approval_state,
            ApprovalState::Pending | ApprovalState::Granted
        ) {
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
        if context.item.approval_state == ApprovalState::Pending
            && !teardown.has_cancelled_convergence
        {
            return Err(UseCaseError::PreparedConvergenceMissing);
        }
        if context.item.approval_state == ApprovalState::Granted
            && !teardown.has_cancelled_convergence
            && !teardown.has_cancelled_queue_entry
        {
            return Err(UseCaseError::PreparedConvergenceMissing);
        }

        context.item.current_revision_id = next_revision.id;
        context.item.approval_state =
            crate::item::approval_state_for_policy(next_revision.approval_policy);
        context.item.escalation_state = ingot_domain::item::EscalationState::None;
        context.item.escalation_reason = None;
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
                let evaluation = Evaluator::new().evaluate(
                    &state.item,
                    &state.revision,
                    &state.jobs,
                    &state.findings,
                    &state.convergences,
                );

                if evaluation.next_recommended_action == "invalidate_prepared_convergence" {
                    self.port
                        .invalidate_prepared_convergence(project_state.project.id, state.item_id)
                        .await?;
                    return Ok(true);
                }

                let prepared_convergence = state.convergences.iter().find(|convergence| {
                    convergence.item_revision_id == state.revision.id
                        && convergence.status == ConvergenceStatus::Prepared
                });

                if let Some(queue_entry) = state.queue_entry.as_ref() {
                    let should_prepare_queue_head = queue_entry.status
                        == ConvergenceQueueEntryStatus::Head
                        && (evaluation.next_recommended_action == "prepare_convergence"
                            || (state.item.approval_state == ApprovalState::Granted
                                && prepared_convergence.is_none()));

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

                    let should_finalize = queue_entry.status == ConvergenceQueueEntryStatus::Head
                        && prepared_convergence.is_some()
                        && (state.item.approval_state == ApprovalState::Granted
                            || (state.revision.approval_policy
                                == ingot_domain::revision::ApprovalPolicy::NotRequired
                                && evaluation.next_recommended_action
                                    == "finalize_prepared_convergence"));

                    if should_finalize
                        && self
                            .port
                            .reconcile_checkout_sync_ready(
                                &project_state.project,
                                state.item_id,
                                &state.revision,
                            )
                            .await?
                    {
                        self.port
                            .auto_finalize_prepared_convergence(
                                project_state.project.id,
                                state.item_id,
                            )
                            .await?;
                        return Ok(true);
                    }
                }
            }
        }

        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use std::future::ready;
    use std::sync::{Arc, Mutex};

    use chrono::Utc;
    use ingot_domain::activity::Activity;
    use ingot_domain::convergence::{Convergence, ConvergenceStatus, ConvergenceStrategy};
    use ingot_domain::convergence_queue::{ConvergenceQueueEntry, ConvergenceQueueEntryStatus};
    use ingot_domain::ids::{ConvergenceId, ItemId, ItemRevisionId, ProjectId, WorkspaceId};
    use ingot_domain::item::{
        ApprovalState, Classification, EscalationState, Item, LifecycleState, OriginKind,
        ParkingState, Priority,
    };
    use ingot_domain::job::Job;
    use ingot_domain::ports::ConvergenceQueuePrepareContext;
    use ingot_domain::project::Project;
    use ingot_domain::revision::{ApprovalPolicy, ItemRevision};
    use uuid::Uuid;

    use super::{
        ConvergenceApprovalContext, ConvergenceCommandPort, ConvergenceService,
        ConvergenceSystemActionPort, RejectApprovalContext, RejectApprovalTeardown,
        SystemActionItemState, SystemActionProjectState,
    };
    use crate::UseCaseError;

    #[derive(Default, Clone)]
    struct FakePort {
        queue_prepare_context: Arc<Mutex<Option<ConvergenceQueuePrepareContext>>>,
        projects: Arc<Mutex<Vec<SystemActionProjectState>>>,
        calls: Arc<Mutex<Vec<String>>>,
        checkout_sync_ready: bool,
    }

    impl FakePort {
        fn with_projects(projects: Vec<SystemActionProjectState>) -> Self {
            Self {
                queue_prepare_context: Arc::new(Mutex::new(None)),
                projects: Arc::new(Mutex::new(projects)),
                calls: Arc::new(Mutex::new(Vec::new())),
                checkout_sync_ready: true,
            }
        }

        fn with_queue_prepare_context(context: ConvergenceQueuePrepareContext) -> Self {
            Self {
                queue_prepare_context: Arc::new(Mutex::new(Some(context))),
                projects: Arc::new(Mutex::new(Vec::new())),
                calls: Arc::new(Mutex::new(Vec::new())),
                checkout_sync_ready: true,
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
            ready(Ok(ConvergenceApprovalContext {
                item: Item {
                    id: ItemId::from_uuid(Uuid::nil()),
                    project_id: ProjectId::from_uuid(Uuid::nil()),
                    classification: Classification::Change,
                    workflow_version: "delivery:v1".into(),
                    lifecycle_state: LifecycleState::Open,
                    parking_state: ParkingState::Active,
                    done_reason: None,
                    resolution_source: None,
                    approval_state: ApprovalState::Pending,
                    escalation_state: EscalationState::None,
                    escalation_reason: None,
                    current_revision_id: ItemRevisionId::from_uuid(Uuid::nil()),
                    origin_kind: OriginKind::Manual,
                    origin_finding_id: None,
                    priority: Priority::Major,
                    labels: vec![],
                    operator_notes: None,
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                    closed_at: None,
                },
                has_active_job: false,
                has_active_convergence: false,
                prepared_convergence_id: Some(ConvergenceId::from_uuid(Uuid::nil())),
                prepared_target_valid: true,
                queue_entry: Some(ConvergenceQueueEntry {
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
                }),
            }))
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
            ready(Ok(RejectApprovalContext {
                item: Item {
                    id: ItemId::from_uuid(Uuid::nil()),
                    project_id: ProjectId::from_uuid(Uuid::nil()),
                    classification: Classification::Change,
                    workflow_version: "delivery:v1".into(),
                    lifecycle_state: LifecycleState::Open,
                    parking_state: ParkingState::Active,
                    done_reason: None,
                    resolution_source: None,
                    approval_state: ApprovalState::Granted,
                    escalation_state: EscalationState::None,
                    escalation_reason: None,
                    current_revision_id: ItemRevisionId::from_uuid(Uuid::nil()),
                    origin_kind: OriginKind::Manual,
                    origin_finding_id: None,
                    priority: Priority::Major,
                    labels: vec![],
                    operator_notes: None,
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                    closed_at: None,
                },
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
                has_cancelled_convergence: false,
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

        fn reconcile_checkout_sync_ready(
            &self,
            _project: &Project,
            _item_id: ItemId,
            _revision: &ItemRevision,
        ) -> impl Future<Output = Result<bool, UseCaseError>> + Send {
            ready(Ok(self.checkout_sync_ready))
        }

        fn auto_finalize_prepared_convergence(
            &self,
            project_id: ProjectId,
            item_id: ItemId,
        ) -> impl Future<Output = Result<(), UseCaseError>> + Send {
            self.calls
                .lock()
                .expect("calls lock")
                .push(format!("finalize:{project_id}:{item_id}"));
            ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn queue_prepare_creates_lane_head_when_lane_is_empty() {
        let now = Utc::now();
        let project_id = ProjectId::from_uuid(Uuid::nil());
        let item_id = ItemId::from_uuid(Uuid::nil());
        let revision_id = ItemRevisionId::from_uuid(Uuid::nil());
        let project = Project {
            id: project_id,
            name: "repo".into(),
            path: "/tmp/repo".into(),
            default_branch: "main".into(),
            color: "#000".into(),
            created_at: now,
            updated_at: now,
        };
        let item = Item {
            id: item_id,
            project_id,
            classification: Classification::Change,
            workflow_version: "delivery:v1".into(),
            lifecycle_state: LifecycleState::Open,
            parking_state: ParkingState::Active,
            done_reason: None,
            resolution_source: None,
            approval_state: ApprovalState::NotRequested,
            escalation_state: EscalationState::None,
            escalation_reason: None,
            current_revision_id: revision_id,
            origin_kind: OriginKind::Manual,
            origin_finding_id: None,
            priority: Priority::Major,
            labels: vec![],
            operator_notes: None,
            created_at: now,
            updated_at: now,
            closed_at: None,
        };
        let revision = ItemRevision {
            id: revision_id,
            item_id,
            revision_no: 1,
            title: "Title".into(),
            description: "Desc".into(),
            acceptance_criteria: "AC".into(),
            target_ref: "refs/heads/main".into(),
            approval_policy: ApprovalPolicy::Required,
            policy_snapshot: serde_json::json!({}),
            template_map_snapshot: serde_json::json!({}),
            seed_commit_oid: Some("seed".into()),
            seed_target_commit_oid: Some("seed".into()),
            supersedes_revision_id: None,
            created_at: now,
        };
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
    async fn finalize_runs_for_granted_prepared_head() {
        let mut state = project_state("idle");
        state.items[0].item.approval_state = ApprovalState::Granted;
        state.items[0].convergences[0].status = ConvergenceStatus::Prepared;

        let port = FakePort::with_projects(vec![state]);
        let service = ConvergenceService::new(port.clone());

        let made_progress = service
            .tick_system_actions()
            .await
            .expect("tick system actions");

        assert!(made_progress);
        assert!(
            port.calls()
                .iter()
                .any(|call| call.starts_with("finalize:"))
        );
    }

    fn project_state(next_action: &str) -> SystemActionProjectState {
        let created_at = Utc::now();
        let project_id = ProjectId::from_uuid(Uuid::nil());
        let item_id = ItemId::from_uuid(Uuid::nil());
        let revision_id = ItemRevisionId::from_uuid(Uuid::nil());
        let project = Project {
            id: project_id,
            name: "repo".into(),
            path: "/tmp/repo".into(),
            default_branch: "main".into(),
            color: "#000".into(),
            created_at,
            updated_at: created_at,
        };
        let revision = ItemRevision {
            id: revision_id,
            item_id,
            revision_no: 1,
            title: "Title".into(),
            description: "Desc".into(),
            acceptance_criteria: "AC".into(),
            target_ref: "refs/heads/main".into(),
            approval_policy: ApprovalPolicy::Required,
            policy_snapshot: serde_json::json!({}),
            template_map_snapshot: serde_json::json!({}),
            seed_commit_oid: Some("seed".into()),
            seed_target_commit_oid: Some("seed".into()),
            supersedes_revision_id: None,
            created_at,
        };
        let approval_state = if next_action == "finalize_prepared_convergence" {
            ApprovalState::NotRequired
        } else {
            ApprovalState::NotRequested
        };
        let item = Item {
            id: item_id,
            project_id,
            classification: Classification::Change,
            workflow_version: "delivery:v1".into(),
            lifecycle_state: LifecycleState::Open,
            parking_state: ParkingState::Active,
            done_reason: None,
            resolution_source: None,
            approval_state,
            escalation_state: EscalationState::None,
            escalation_reason: None,
            current_revision_id: revision_id,
            origin_kind: OriginKind::Manual,
            origin_finding_id: None,
            priority: Priority::Major,
            labels: vec![],
            operator_notes: None,
            created_at,
            updated_at: created_at,
            closed_at: None,
        };
        let convergence = Convergence {
            id: ConvergenceId::from_uuid(Uuid::nil()),
            project_id,
            item_id,
            item_revision_id: revision_id,
            source_workspace_id: WorkspaceId::from_uuid(Uuid::nil()),
            integration_workspace_id: Some(WorkspaceId::from_uuid(Uuid::nil())),
            source_head_commit_oid: "head".into(),
            target_ref: "refs/heads/main".into(),
            strategy: ConvergenceStrategy::RebaseThenFastForward,
            status: if next_action == "prepare_convergence" {
                ConvergenceStatus::Failed
            } else {
                ConvergenceStatus::Prepared
            },
            input_target_commit_oid: Some("base".into()),
            prepared_commit_oid: Some("prepared".into()),
            final_target_commit_oid: None,
            target_head_valid: Some(next_action != "invalidate_prepared_convergence"),
            conflict_summary: None,
            created_at,
            completed_at: None,
        };
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
        Job {
            id: ingot_domain::ids::JobId::new(),
            project_id: ProjectId::from_uuid(Uuid::nil()),
            item_id: ItemId::from_uuid(Uuid::nil()),
            item_revision_id: ItemRevisionId::from_uuid(Uuid::nil()),
            step_id: if next_action == "prepare_convergence" {
                "validate_candidate_initial".into()
            } else {
                "validate_integrated".into()
            },
            semantic_attempt_no: 1,
            retry_no: 0,
            supersedes_job_id: None,
            status: ingot_domain::job::JobStatus::Completed,
            outcome_class: Some(ingot_domain::job::OutcomeClass::Clean),
            phase_kind: ingot_domain::job::PhaseKind::Validate,
            workspace_id: None,
            workspace_kind: ingot_domain::workspace::WorkspaceKind::Integration,
            execution_permission: ingot_domain::job::ExecutionPermission::MustNotMutate,
            context_policy: ingot_domain::job::ContextPolicy::ResumeContext,
            phase_template_slug: "template".into(),
            phase_template_digest: None,
            prompt_snapshot: None,
            job_input: ingot_domain::job::JobInput::None,
            output_artifact_kind: ingot_domain::job::OutputArtifactKind::ValidationReport,
            output_commit_oid: None,
            result_schema_version: None,
            result_payload: None,
            agent_id: None,
            process_pid: None,
            lease_owner_id: None,
            heartbeat_at: None,
            lease_expires_at: None,
            error_code: None,
            error_message: None,
            created_at,
            started_at: Some(created_at),
            ended_at: Some(created_at),
        }
    }
}
