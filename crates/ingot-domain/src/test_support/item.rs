use crate::ids;
use crate::item::{
    ApprovalState, Classification, DoneReason, Escalation, EscalationReason, Item, Lifecycle,
    Origin, ParkingState, Priority, ResolutionSource, WorkflowVersion,
};
use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::timestamps::default_timestamp;

pub struct ItemBuilder {
    id: ids::ItemId,
    project_id: ids::ProjectId,
    current_revision_id: ids::ItemRevisionId,
    classification: Classification,
    workflow_version: WorkflowVersion,
    lifecycle: Lifecycle,
    parking_state: ParkingState,
    approval_state: ApprovalState,
    escalation: Escalation,
    origin: Origin,
    priority: Priority,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
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
            workflow_version: WorkflowVersion::DeliveryV1,
            lifecycle: Lifecycle::Open,
            parking_state: ParkingState::Active,
            approval_state: ApprovalState::NotRequested,
            escalation: Escalation::None,
            origin: Origin::Manual,
            priority: Priority::Major,
            created_at: now,
            updated_at: now,
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

    pub fn escalation(mut self, escalation: Escalation) -> Self {
        self.escalation = escalation;
        self
    }

    pub fn escalated(mut self, reason: EscalationReason) -> Self {
        self.escalation = Escalation::OperatorRequired { reason };
        self
    }

    pub fn lifecycle(mut self, lifecycle: Lifecycle) -> Self {
        self.lifecycle = lifecycle;
        self
    }

    pub fn done(mut self, reason: DoneReason, source: ResolutionSource) -> Self {
        self.lifecycle = Lifecycle::Done {
            reason,
            source,
            closed_at: self.created_at,
        };
        self
    }

    pub fn created_at(mut self, created_at: DateTime<Utc>) -> Self {
        self.created_at = created_at;
        self.updated_at = created_at;
        self
    }

    pub fn origin(mut self, origin: Origin) -> Self {
        self.origin = origin;
        self
    }

    pub fn build(self) -> Item {
        Item {
            id: self.id,
            project_id: self.project_id,
            classification: self.classification,
            workflow_version: self.workflow_version,
            lifecycle: self.lifecycle,
            parking_state: self.parking_state,
            approval_state: self.approval_state,
            escalation: self.escalation,
            current_revision_id: self.current_revision_id,
            origin: self.origin,
            priority: self.priority,
            labels: vec![],
            operator_notes: None,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}
