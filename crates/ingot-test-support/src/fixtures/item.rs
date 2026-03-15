use chrono::{DateTime, Utc};
use ingot_domain::ids;
use ingot_domain::item::{
    ApprovalState, Classification, DoneReason, EscalationReason, EscalationState, Item,
    LifecycleState, OriginKind, ParkingState, Priority, ResolutionSource,
};
use uuid::Uuid;

use super::timestamps::default_timestamp;

pub struct ItemBuilder {
    id: ids::ItemId,
    project_id: ids::ProjectId,
    current_revision_id: ids::ItemRevisionId,
    classification: Classification,
    workflow_version: String,
    lifecycle_state: LifecycleState,
    parking_state: ParkingState,
    done_reason: Option<DoneReason>,
    resolution_source: Option<ResolutionSource>,
    approval_state: ApprovalState,
    escalation_state: EscalationState,
    escalation_reason: Option<EscalationReason>,
    origin_kind: OriginKind,
    priority: Priority,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    closed_at: Option<DateTime<Utc>>,
}

impl ItemBuilder {
    pub fn nil() -> Self {
        let nil = Uuid::nil();
        Self::new(
            ids::ProjectId::from_uuid(nil),
            ids::ItemRevisionId::from_uuid(nil),
        )
        .id(ids::ItemId::from_uuid(nil))
    }

    pub fn new(project_id: ids::ProjectId, current_revision_id: ids::ItemRevisionId) -> Self {
        let now = default_timestamp();
        Self {
            id: ids::ItemId::new(),
            project_id,
            current_revision_id,
            classification: Classification::Change,
            workflow_version: "delivery:v1".into(),
            lifecycle_state: LifecycleState::Open,
            parking_state: ParkingState::Active,
            done_reason: None,
            resolution_source: None,
            approval_state: ApprovalState::NotRequested,
            escalation_state: EscalationState::None,
            escalation_reason: None,
            origin_kind: OriginKind::Manual,
            priority: Priority::Major,
            created_at: now,
            updated_at: now,
            closed_at: None,
        }
    }

    pub fn id(mut self, id: ids::ItemId) -> Self {
        self.id = id;
        self
    }

    pub fn approval_state(mut self, approval_state: ApprovalState) -> Self {
        self.approval_state = approval_state;
        self
    }

    pub fn escalation_state(mut self, escalation_state: EscalationState) -> Self {
        self.escalation_state = escalation_state;
        self
    }

    pub fn escalation_reason(mut self, escalation_reason: EscalationReason) -> Self {
        self.escalation_reason = Some(escalation_reason);
        self
    }

    pub fn lifecycle_state(mut self, lifecycle_state: LifecycleState) -> Self {
        self.lifecycle_state = lifecycle_state;
        self
    }

    pub fn done_reason(mut self, done_reason: DoneReason) -> Self {
        self.done_reason = Some(done_reason);
        self
    }

    pub fn resolution_source(mut self, resolution_source: ResolutionSource) -> Self {
        self.resolution_source = Some(resolution_source);
        self
    }

    pub fn closed_at(mut self, closed_at: DateTime<Utc>) -> Self {
        self.closed_at = Some(closed_at);
        self
    }

    pub fn created_at(mut self, created_at: DateTime<Utc>) -> Self {
        self.created_at = created_at;
        self.updated_at = created_at;
        self
    }

    pub fn build(self) -> Item {
        Item {
            id: self.id,
            project_id: self.project_id,
            classification: self.classification,
            workflow_version: self.workflow_version,
            lifecycle_state: self.lifecycle_state,
            parking_state: self.parking_state,
            done_reason: self.done_reason,
            resolution_source: self.resolution_source,
            approval_state: self.approval_state,
            escalation_state: self.escalation_state,
            escalation_reason: self.escalation_reason,
            current_revision_id: self.current_revision_id,
            origin_kind: self.origin_kind,
            origin_finding_id: None,
            priority: self.priority,
            labels: vec![],
            operator_notes: None,
            created_at: self.created_at,
            updated_at: self.updated_at,
            closed_at: self.closed_at,
        }
    }
}
