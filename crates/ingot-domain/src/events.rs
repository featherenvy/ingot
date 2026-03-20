use serde::{Deserialize, Serialize};

use crate::finding::FindingTriageState;
use crate::ids::*;
use crate::item::EscalationReason;
use crate::job::OutcomeClass;

/// Domain events emitted by command handlers.
/// These are used for activity logging and WebSocket broadcasting.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum DomainEvent {
    ItemCreated {
        item_id: ItemId,
        project_id: ProjectId,
    },
    ItemRevisionCreated {
        item_id: ItemId,
        revision_id: ItemRevisionId,
    },
    ItemUpdated {
        item_id: ItemId,
    },
    ItemDeferred {
        item_id: ItemId,
    },
    ItemResumed {
        item_id: ItemId,
    },
    ItemDismissed {
        item_id: ItemId,
    },
    ItemInvalidated {
        item_id: ItemId,
    },
    ItemReopened {
        item_id: ItemId,
        new_revision_id: ItemRevisionId,
    },
    ItemEscalated {
        item_id: ItemId,
        reason: EscalationReason,
    },
    ItemEscalationCleared {
        item_id: ItemId,
    },
    JobDispatched {
        job_id: JobId,
        item_id: ItemId,
        step_id: String,
    },
    JobCompleted {
        job_id: JobId,
        item_id: ItemId,
        outcome: OutcomeClass,
    },
    JobFailed {
        job_id: JobId,
        item_id: ItemId,
        error_code: Option<String>,
    },
    JobCancelled {
        job_id: JobId,
        item_id: ItemId,
    },
    FindingPromoted {
        finding_id: FindingId,
        item_id: ItemId,
        promoted_item_id: ItemId,
    },
    FindingDismissed {
        finding_id: FindingId,
        item_id: ItemId,
    },
    FindingTriaged {
        finding_id: FindingId,
        item_id: ItemId,
        triage_state: FindingTriageState,
    },
    ApprovalRequested {
        item_id: ItemId,
    },
    ApprovalApproved {
        item_id: ItemId,
        convergence_id: ConvergenceId,
    },
    ApprovalRejected {
        item_id: ItemId,
        new_revision_id: ItemRevisionId,
    },
    ConvergenceQueued {
        queue_entry_id: ConvergenceQueueEntryId,
        item_id: ItemId,
    },
    ConvergenceLaneAcquired {
        queue_entry_id: ConvergenceQueueEntryId,
        item_id: ItemId,
    },
    ConvergenceStarted {
        convergence_id: ConvergenceId,
        item_id: ItemId,
    },
    ConvergenceConflicted {
        convergence_id: ConvergenceId,
        item_id: ItemId,
    },
    ConvergencePrepared {
        convergence_id: ConvergenceId,
        item_id: ItemId,
    },
    ConvergenceFinalized {
        convergence_id: ConvergenceId,
        item_id: ItemId,
    },
    ConvergenceFailed {
        convergence_id: ConvergenceId,
        item_id: ItemId,
    },
    CheckoutSyncBlocked {
        item_id: ItemId,
    },
    CheckoutSyncCleared {
        item_id: ItemId,
    },
}
