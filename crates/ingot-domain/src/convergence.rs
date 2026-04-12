use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::commit_oid::CommitOid;
use crate::git_ref::GitRef;
use crate::ids::{ConvergenceId, ItemId, ItemRevisionId, ProjectId, WorkspaceId};

#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "sqlx", sqlx(rename_all = "snake_case"))]
pub enum ConvergenceStrategy {
    RebaseThenFastForward,
}

#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "sqlx", sqlx(rename_all = "snake_case"))]
pub enum ConvergenceStatus {
    Queued,
    Running,
    Conflicted,
    Prepared,
    Finalized,
    Failed,
    Cancelled,
}

#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "sqlx", sqlx(rename_all = "snake_case"))]
pub enum CheckoutAdoptionState {
    Pending,
    Blocked,
    Synced,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinalizedCheckoutAdoption {
    pub state: CheckoutAdoptionState,
    pub blocker_message: Option<String>,
    pub updated_at: DateTime<Utc>,
    pub synced_at: Option<DateTime<Utc>>,
}

impl FinalizedCheckoutAdoption {
    #[must_use]
    pub fn pending(updated_at: DateTime<Utc>) -> Self {
        Self {
            state: CheckoutAdoptionState::Pending,
            blocker_message: None,
            updated_at,
            synced_at: None,
        }
    }

    #[must_use]
    pub fn blocked(message: impl Into<String>, updated_at: DateTime<Utc>) -> Self {
        Self {
            state: CheckoutAdoptionState::Blocked,
            blocker_message: Some(message.into()),
            updated_at,
            synced_at: None,
        }
    }

    #[must_use]
    pub fn synced(synced_at: DateTime<Utc>) -> Self {
        Self {
            state: CheckoutAdoptionState::Synced,
            blocker_message: None,
            updated_at: synced_at,
            synced_at: Some(synced_at),
        }
    }

    fn from_parts(
        state: CheckoutAdoptionState,
        blocker_message: Option<String>,
        updated_at: Option<DateTime<Utc>>,
        synced_at: Option<DateTime<Utc>>,
    ) -> Result<Self, String> {
        let updated_at = required_convergence_field("checkout_adoption_updated_at", updated_at)?;

        match state {
            CheckoutAdoptionState::Pending => {
                if blocker_message.is_some() {
                    return Err(
                        "convergence checkout_adoption_message must be empty for pending adoption"
                            .into(),
                    );
                }
                if synced_at.is_some() {
                    return Err(
                        "convergence checkout_adoption_synced_at must be empty for pending adoption"
                            .into(),
                    );
                }
            }
            CheckoutAdoptionState::Blocked => {
                if blocker_message.is_none() {
                    return Err(
                        "convergence checkout_adoption_message is required for blocked adoption"
                            .into(),
                    );
                }
                if synced_at.is_some() {
                    return Err(
                        "convergence checkout_adoption_synced_at must be empty for blocked adoption"
                            .into(),
                    );
                }
            }
            CheckoutAdoptionState::Synced => {
                if blocker_message.is_some() {
                    return Err(
                        "convergence checkout_adoption_message must be empty for synced adoption"
                            .into(),
                    );
                }
                if synced_at.is_none() {
                    return Err(
                        "convergence checkout_adoption_synced_at is required for synced adoption"
                            .into(),
                    );
                }
            }
        }

        Ok(Self {
            state,
            blocker_message,
            updated_at,
            synced_at,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ConvergenceTransitionError {
    #[error("queued convergence is missing transition context")]
    QueuedMissingContext,
    #[error("finalized convergence is missing integration workspace for transition")]
    FinalizedMissingWorkspace,
    #[error("failed convergence is missing integration workspace for transition")]
    FailedMissingWorkspace,
    #[error("failed convergence is missing input target for transition")]
    FailedMissingInputTarget,
    #[error("cancelled convergence is missing integration workspace for transition")]
    CancelledMissingWorkspace,
    #[error("cancelled convergence is missing input target for transition")]
    CancelledMissingInputTarget,
    #[error("convergence is not finalized")]
    NotFinalized,
}

impl ConvergenceStatus {
    #[must_use]
    pub fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::Prepared)
    }

    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Finalized | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone)]
pub enum ConvergenceState {
    Queued,
    Running {
        integration_workspace_id: WorkspaceId,
        input_target_commit_oid: CommitOid,
    },
    Conflicted {
        integration_workspace_id: WorkspaceId,
        input_target_commit_oid: CommitOid,
        conflict_summary: String,
        completed_at: DateTime<Utc>,
    },
    Prepared {
        integration_workspace_id: WorkspaceId,
        input_target_commit_oid: CommitOid,
        prepared_commit_oid: CommitOid,
        completed_at: Option<DateTime<Utc>>,
    },
    Finalized {
        integration_workspace_id: Option<WorkspaceId>,
        input_target_commit_oid: CommitOid,
        prepared_commit_oid: CommitOid,
        final_target_commit_oid: CommitOid,
        checkout_adoption: FinalizedCheckoutAdoption,
        completed_at: DateTime<Utc>,
    },
    Failed {
        integration_workspace_id: Option<WorkspaceId>,
        input_target_commit_oid: Option<CommitOid>,
        conflict_summary: Option<String>,
        completed_at: DateTime<Utc>,
    },
    Cancelled {
        integration_workspace_id: Option<WorkspaceId>,
        input_target_commit_oid: Option<CommitOid>,
        completed_at: DateTime<Utc>,
    },
}

#[derive(Debug, Clone, Default)]
pub struct ConvergenceStateParts {
    pub integration_workspace_id: Option<WorkspaceId>,
    pub input_target_commit_oid: Option<CommitOid>,
    pub prepared_commit_oid: Option<CommitOid>,
    pub final_target_commit_oid: Option<CommitOid>,
    pub checkout_adoption_state: Option<CheckoutAdoptionState>,
    pub checkout_adoption_message: Option<String>,
    pub checkout_adoption_updated_at: Option<DateTime<Utc>>,
    pub checkout_adoption_synced_at: Option<DateTime<Utc>>,
    pub conflict_summary: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
}

fn required_convergence_field<T>(field: &'static str, value: Option<T>) -> Result<T, String> {
    value.ok_or_else(|| format!("convergence {field} is required for this status"))
}

impl ConvergenceState {
    fn require_workspace_id(
        workspace_id: Option<WorkspaceId>,
        error: ConvergenceTransitionError,
    ) -> Result<WorkspaceId, ConvergenceTransitionError> {
        workspace_id.ok_or(error)
    }

    fn require_input_target_commit_oid(
        input_target_commit_oid: Option<CommitOid>,
        error: ConvergenceTransitionError,
    ) -> Result<CommitOid, ConvergenceTransitionError> {
        input_target_commit_oid.ok_or(error)
    }

    #[must_use]
    pub fn status(&self) -> ConvergenceStatus {
        match self {
            Self::Queued => ConvergenceStatus::Queued,
            Self::Running { .. } => ConvergenceStatus::Running,
            Self::Conflicted { .. } => ConvergenceStatus::Conflicted,
            Self::Prepared { .. } => ConvergenceStatus::Prepared,
            Self::Finalized { .. } => ConvergenceStatus::Finalized,
            Self::Failed { .. } => ConvergenceStatus::Failed,
            Self::Cancelled { .. } => ConvergenceStatus::Cancelled,
        }
    }

    #[must_use]
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            Self::Queued | Self::Running { .. } | Self::Prepared { .. }
        )
    }

    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Finalized { .. } | Self::Failed { .. } | Self::Cancelled { .. }
        )
    }

    #[must_use]
    pub fn integration_workspace_id(&self) -> Option<WorkspaceId> {
        match self {
            Self::Queued => None,
            Self::Running {
                integration_workspace_id,
                ..
            }
            | Self::Conflicted {
                integration_workspace_id,
                ..
            }
            | Self::Prepared {
                integration_workspace_id,
                ..
            } => Some(*integration_workspace_id),
            Self::Finalized {
                integration_workspace_id,
                ..
            }
            | Self::Failed {
                integration_workspace_id,
                ..
            }
            | Self::Cancelled {
                integration_workspace_id,
                ..
            } => *integration_workspace_id,
        }
    }

    #[must_use]
    pub fn input_target_commit_oid(&self) -> Option<&CommitOid> {
        match self {
            Self::Queued => None,
            Self::Running {
                input_target_commit_oid,
                ..
            }
            | Self::Conflicted {
                input_target_commit_oid,
                ..
            }
            | Self::Prepared {
                input_target_commit_oid,
                ..
            }
            | Self::Finalized {
                input_target_commit_oid,
                ..
            } => Some(input_target_commit_oid),
            Self::Failed {
                input_target_commit_oid,
                ..
            }
            | Self::Cancelled {
                input_target_commit_oid,
                ..
            } => input_target_commit_oid.as_ref(),
        }
    }

    #[must_use]
    pub fn prepared_commit_oid(&self) -> Option<&CommitOid> {
        match self {
            Self::Prepared {
                prepared_commit_oid,
                ..
            }
            | Self::Finalized {
                prepared_commit_oid,
                ..
            } => Some(prepared_commit_oid),
            _ => None,
        }
    }

    #[must_use]
    pub fn final_target_commit_oid(&self) -> Option<&CommitOid> {
        match self {
            Self::Finalized {
                final_target_commit_oid,
                ..
            } => Some(final_target_commit_oid),
            _ => None,
        }
    }

    #[must_use]
    pub fn finalized_checkout_adoption(&self) -> Option<&FinalizedCheckoutAdoption> {
        match self {
            Self::Finalized {
                checkout_adoption, ..
            } => Some(checkout_adoption),
            _ => None,
        }
    }

    #[must_use]
    pub fn checkout_adoption_state(&self) -> Option<CheckoutAdoptionState> {
        self.finalized_checkout_adoption()
            .map(|adoption| adoption.state)
    }

    #[must_use]
    pub fn checkout_adoption_message(&self) -> Option<&str> {
        self.finalized_checkout_adoption()
            .and_then(|adoption| adoption.blocker_message.as_deref())
    }

    #[must_use]
    pub fn checkout_adoption_updated_at(&self) -> Option<DateTime<Utc>> {
        self.finalized_checkout_adoption()
            .map(|adoption| adoption.updated_at)
    }

    #[must_use]
    pub fn checkout_adoption_synced_at(&self) -> Option<DateTime<Utc>> {
        self.finalized_checkout_adoption()
            .and_then(|adoption| adoption.synced_at)
    }

    #[must_use]
    pub fn conflict_summary(&self) -> Option<&str> {
        match self {
            Self::Conflicted {
                conflict_summary, ..
            } => Some(conflict_summary.as_str()),
            Self::Failed {
                conflict_summary, ..
            } => conflict_summary.as_deref(),
            _ => None,
        }
    }

    #[must_use]
    pub fn completed_at(&self) -> Option<DateTime<Utc>> {
        match self {
            Self::Queued | Self::Running { .. } => None,
            Self::Conflicted { completed_at, .. }
            | Self::Finalized { completed_at, .. }
            | Self::Failed { completed_at, .. }
            | Self::Cancelled { completed_at, .. } => Some(*completed_at),
            Self::Prepared { completed_at, .. } => *completed_at,
        }
    }

    pub fn from_parts(
        status: ConvergenceStatus,
        parts: ConvergenceStateParts,
    ) -> Result<Self, String> {
        match status {
            ConvergenceStatus::Queued => Ok(Self::Queued),
            ConvergenceStatus::Running => Ok(Self::Running {
                integration_workspace_id: required_convergence_field(
                    "integration_workspace_id",
                    parts.integration_workspace_id,
                )?,
                input_target_commit_oid: required_convergence_field(
                    "input_target_commit_oid",
                    parts.input_target_commit_oid,
                )?,
            }),
            ConvergenceStatus::Conflicted => Ok(Self::Conflicted {
                integration_workspace_id: required_convergence_field(
                    "integration_workspace_id",
                    parts.integration_workspace_id,
                )?,
                input_target_commit_oid: required_convergence_field(
                    "input_target_commit_oid",
                    parts.input_target_commit_oid,
                )?,
                conflict_summary: required_convergence_field(
                    "conflict_summary",
                    parts.conflict_summary,
                )?,
                completed_at: required_convergence_field("completed_at", parts.completed_at)?,
            }),
            ConvergenceStatus::Prepared => Ok(Self::Prepared {
                integration_workspace_id: required_convergence_field(
                    "integration_workspace_id",
                    parts.integration_workspace_id,
                )?,
                input_target_commit_oid: required_convergence_field(
                    "input_target_commit_oid",
                    parts.input_target_commit_oid,
                )?,
                prepared_commit_oid: required_convergence_field(
                    "prepared_commit_oid",
                    parts.prepared_commit_oid,
                )?,
                completed_at: parts.completed_at,
            }),
            ConvergenceStatus::Finalized => Ok(Self::Finalized {
                integration_workspace_id: parts.integration_workspace_id,
                input_target_commit_oid: required_convergence_field(
                    "input_target_commit_oid",
                    parts.input_target_commit_oid,
                )?,
                prepared_commit_oid: required_convergence_field(
                    "prepared_commit_oid",
                    parts.prepared_commit_oid,
                )?,
                final_target_commit_oid: required_convergence_field(
                    "final_target_commit_oid",
                    parts.final_target_commit_oid,
                )?,
                checkout_adoption: FinalizedCheckoutAdoption::from_parts(
                    required_convergence_field(
                        "checkout_adoption_state",
                        parts.checkout_adoption_state,
                    )?,
                    parts.checkout_adoption_message,
                    parts.checkout_adoption_updated_at,
                    parts.checkout_adoption_synced_at,
                )?,
                completed_at: required_convergence_field("completed_at", parts.completed_at)?,
            }),
            ConvergenceStatus::Failed => Ok(Self::Failed {
                integration_workspace_id: parts.integration_workspace_id,
                input_target_commit_oid: parts.input_target_commit_oid,
                conflict_summary: parts.conflict_summary,
                completed_at: required_convergence_field("completed_at", parts.completed_at)?,
            }),
            ConvergenceStatus::Cancelled => Ok(Self::Cancelled {
                integration_workspace_id: parts.integration_workspace_id,
                input_target_commit_oid: parts.input_target_commit_oid,
                completed_at: required_convergence_field("completed_at", parts.completed_at)?,
            }),
        }
    }

    fn into_required_transition_context(
        self,
    ) -> Result<(WorkspaceId, CommitOid), ConvergenceTransitionError> {
        match self {
            Self::Running {
                integration_workspace_id,
                input_target_commit_oid,
            } => Ok((integration_workspace_id, input_target_commit_oid)),
            Self::Prepared {
                integration_workspace_id,
                input_target_commit_oid,
                ..
            } => Ok((integration_workspace_id, input_target_commit_oid)),
            Self::Finalized {
                integration_workspace_id,
                input_target_commit_oid,
                ..
            } => Ok((
                Self::require_workspace_id(
                    integration_workspace_id,
                    ConvergenceTransitionError::FinalizedMissingWorkspace,
                )?,
                input_target_commit_oid,
            )),
            Self::Conflicted {
                integration_workspace_id,
                input_target_commit_oid,
                ..
            } => Ok((integration_workspace_id, input_target_commit_oid)),
            Self::Failed {
                integration_workspace_id,
                input_target_commit_oid,
                ..
            } => Ok((
                Self::require_workspace_id(
                    integration_workspace_id,
                    ConvergenceTransitionError::FailedMissingWorkspace,
                )?,
                Self::require_input_target_commit_oid(
                    input_target_commit_oid,
                    ConvergenceTransitionError::FailedMissingInputTarget,
                )?,
            )),
            Self::Cancelled {
                integration_workspace_id,
                input_target_commit_oid,
                ..
            } => Ok((
                Self::require_workspace_id(
                    integration_workspace_id,
                    ConvergenceTransitionError::CancelledMissingWorkspace,
                )?,
                Self::require_input_target_commit_oid(
                    input_target_commit_oid,
                    ConvergenceTransitionError::CancelledMissingInputTarget,
                )?,
            )),
            Self::Queued => Err(ConvergenceTransitionError::QueuedMissingContext),
        }
    }

    fn into_optional_transition_context(self) -> (Option<WorkspaceId>, Option<CommitOid>) {
        match self {
            Self::Queued => (None, None),
            Self::Running {
                integration_workspace_id,
                input_target_commit_oid,
            } => (
                Some(integration_workspace_id),
                Some(input_target_commit_oid),
            ),
            Self::Prepared {
                integration_workspace_id,
                input_target_commit_oid,
                ..
            } => (
                Some(integration_workspace_id),
                Some(input_target_commit_oid),
            ),
            Self::Finalized {
                integration_workspace_id,
                input_target_commit_oid,
                ..
            } => (integration_workspace_id, Some(input_target_commit_oid)),
            Self::Conflicted {
                integration_workspace_id,
                input_target_commit_oid,
                ..
            } => (
                Some(integration_workspace_id),
                Some(input_target_commit_oid),
            ),
            Self::Failed {
                integration_workspace_id,
                input_target_commit_oid,
                ..
            }
            | Self::Cancelled {
                integration_workspace_id,
                input_target_commit_oid,
                ..
            } => (integration_workspace_id, input_target_commit_oid),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(try_from = "ConvergenceWire", into = "ConvergenceWire")]
pub struct Convergence {
    pub id: ConvergenceId,
    pub project_id: ProjectId,
    pub item_id: ItemId,
    pub item_revision_id: ItemRevisionId,
    pub source_workspace_id: WorkspaceId,
    pub source_head_commit_oid: CommitOid,
    pub target_ref: GitRef,
    pub strategy: ConvergenceStrategy,
    pub created_at: DateTime<Utc>,
    pub target_head_valid: Option<bool>,
    pub state: ConvergenceState,
}

impl Convergence {
    #[must_use]
    pub fn target_head_valid_for_resolved_oid(
        &self,
        resolved_target_oid: Option<&CommitOid>,
    ) -> Option<bool> {
        let input_target_oid = self.state.input_target_commit_oid()?;
        if resolved_target_oid == Some(input_target_oid) {
            return Some(true);
        }

        let integrated_target_oid = self
            .state
            .final_target_commit_oid()
            .or(self.state.prepared_commit_oid());
        if let Some(integrated_target_oid) = integrated_target_oid
            && resolved_target_oid == Some(integrated_target_oid)
        {
            return Some(true);
        }

        Some(false)
    }

    pub fn transition_to_conflicted(
        &mut self,
        summary: String,
        completed_at: DateTime<Utc>,
    ) -> Result<(), ConvergenceTransitionError> {
        let (integration_workspace_id, input_target_commit_oid) =
            self.take_state().into_required_transition_context()?;
        self.state = ConvergenceState::Conflicted {
            integration_workspace_id,
            input_target_commit_oid,
            conflict_summary: summary,
            completed_at,
        };
        Ok(())
    }

    pub fn transition_to_prepared(
        &mut self,
        prepared_oid: CommitOid,
        completed_at: Option<DateTime<Utc>>,
    ) -> Result<(), ConvergenceTransitionError> {
        let (integration_workspace_id, input_target_commit_oid) =
            self.take_state().into_required_transition_context()?;
        self.state = ConvergenceState::Prepared {
            integration_workspace_id,
            input_target_commit_oid,
            prepared_commit_oid: prepared_oid,
            completed_at,
        };
        Ok(())
    }

    pub fn transition_to_finalized(
        &mut self,
        final_oid: CommitOid,
        checkout_adoption: FinalizedCheckoutAdoption,
        completed_at: DateTime<Utc>,
    ) -> Result<(), ConvergenceTransitionError> {
        self.state = match self.take_state() {
            ConvergenceState::Prepared {
                integration_workspace_id,
                input_target_commit_oid,
                prepared_commit_oid,
                completed_at: _,
            } => ConvergenceState::Finalized {
                integration_workspace_id: Some(integration_workspace_id),
                input_target_commit_oid,
                prepared_commit_oid,
                final_target_commit_oid: final_oid,
                checkout_adoption,
                completed_at,
            },
            other => {
                let (integration_workspace_id, input_target_commit_oid) =
                    other.into_required_transition_context()?;
                ConvergenceState::Finalized {
                    integration_workspace_id: Some(integration_workspace_id),
                    input_target_commit_oid,
                    prepared_commit_oid: final_oid.clone(),
                    final_target_commit_oid: final_oid,
                    checkout_adoption,
                    completed_at,
                }
            }
        };
        Ok(())
    }

    pub fn transition_finalized_checkout_adoption(
        &mut self,
        checkout_adoption: FinalizedCheckoutAdoption,
    ) -> Result<(), ConvergenceTransitionError> {
        match &mut self.state {
            ConvergenceState::Finalized {
                checkout_adoption: existing,
                ..
            } => {
                *existing = checkout_adoption;
                Ok(())
            }
            _ => Err(ConvergenceTransitionError::NotFinalized),
        }
    }

    pub fn transition_to_failed(&mut self, summary: Option<String>, completed_at: DateTime<Utc>) {
        let (ws, oid) = self.take_state().into_optional_transition_context();
        self.state = ConvergenceState::Failed {
            integration_workspace_id: ws,
            input_target_commit_oid: oid,
            conflict_summary: summary,
            completed_at,
        };
    }

    pub fn transition_to_cancelled(&mut self, completed_at: DateTime<Utc>) {
        let (ws, oid) = self.take_state().into_optional_transition_context();
        self.state = ConvergenceState::Cancelled {
            integration_workspace_id: ws,
            input_target_commit_oid: oid,
            completed_at,
        };
    }

    fn take_state(&mut self) -> ConvergenceState {
        std::mem::replace(&mut self.state, ConvergenceState::Queued)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ConvergenceWire {
    pub id: ConvergenceId,
    pub project_id: ProjectId,
    pub item_id: ItemId,
    pub item_revision_id: ItemRevisionId,
    pub source_workspace_id: WorkspaceId,
    pub integration_workspace_id: Option<WorkspaceId>,
    pub source_head_commit_oid: CommitOid,
    pub target_ref: GitRef,
    pub strategy: ConvergenceStrategy,
    pub status: ConvergenceStatus,
    pub input_target_commit_oid: Option<CommitOid>,
    pub prepared_commit_oid: Option<CommitOid>,
    pub final_target_commit_oid: Option<CommitOid>,
    pub checkout_adoption_state: Option<CheckoutAdoptionState>,
    pub checkout_adoption_message: Option<String>,
    pub checkout_adoption_updated_at: Option<DateTime<Utc>>,
    pub checkout_adoption_synced_at: Option<DateTime<Utc>>,
    pub target_head_valid: Option<bool>,
    pub conflict_summary: Option<String>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

impl TryFrom<ConvergenceWire> for Convergence {
    type Error = String;

    fn try_from(w: ConvergenceWire) -> Result<Self, Self::Error> {
        let state = ConvergenceState::from_parts(
            w.status,
            ConvergenceStateParts {
                integration_workspace_id: w.integration_workspace_id,
                input_target_commit_oid: w.input_target_commit_oid,
                prepared_commit_oid: w.prepared_commit_oid,
                final_target_commit_oid: w.final_target_commit_oid,
                checkout_adoption_state: w.checkout_adoption_state,
                checkout_adoption_message: w.checkout_adoption_message,
                checkout_adoption_updated_at: w.checkout_adoption_updated_at,
                checkout_adoption_synced_at: w.checkout_adoption_synced_at,
                conflict_summary: w.conflict_summary,
                completed_at: w.completed_at,
            },
        )?;

        Ok(Convergence {
            id: w.id,
            project_id: w.project_id,
            item_id: w.item_id,
            item_revision_id: w.item_revision_id,
            source_workspace_id: w.source_workspace_id,
            source_head_commit_oid: w.source_head_commit_oid,
            target_ref: w.target_ref,
            strategy: w.strategy,
            created_at: w.created_at,
            target_head_valid: w.target_head_valid,
            state,
        })
    }
}

impl From<Convergence> for ConvergenceWire {
    fn from(c: Convergence) -> Self {
        let status = c.state.status();
        let integration_workspace_id = c.state.integration_workspace_id();
        let input_target_commit_oid = c.state.input_target_commit_oid().cloned();
        let prepared_commit_oid = c.state.prepared_commit_oid().cloned();
        let final_target_commit_oid = c.state.final_target_commit_oid().cloned();
        let checkout_adoption_state = c.state.checkout_adoption_state();
        let checkout_adoption_message = c.state.checkout_adoption_message().map(ToOwned::to_owned);
        let checkout_adoption_updated_at = c.state.checkout_adoption_updated_at();
        let checkout_adoption_synced_at = c.state.checkout_adoption_synced_at();
        let conflict_summary = c.state.conflict_summary().map(ToOwned::to_owned);
        let completed_at = c.state.completed_at();

        ConvergenceWire {
            id: c.id,
            project_id: c.project_id,
            item_id: c.item_id,
            item_revision_id: c.item_revision_id,
            source_workspace_id: c.source_workspace_id,
            integration_workspace_id,
            source_head_commit_oid: c.source_head_commit_oid,
            target_ref: c.target_ref,
            strategy: c.strategy,
            status,
            input_target_commit_oid,
            prepared_commit_oid,
            final_target_commit_oid,
            checkout_adoption_state,
            checkout_adoption_message,
            checkout_adoption_updated_at,
            checkout_adoption_synced_at,
            target_head_valid: c.target_head_valid,
            conflict_summary,
            created_at: c.created_at,
            completed_at,
        }
    }
}

/// Used by `fail_prepare_convergence_attempt` to distinguish Conflicted vs Failed transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrepareFailureKind {
    Conflicted,
    Failed,
}

#[cfg(test)]
mod tests {
    use crate::test_support::{ConvergenceBuilder, default_timestamp};

    use super::*;
    use crate::ids::*;

    fn base_convergence(state: ConvergenceState) -> Convergence {
        let mut convergence =
            ConvergenceBuilder::new(ProjectId::new(), ItemId::new(), ItemRevisionId::new()).build();
        convergence.state = state;
        convergence
    }

    fn running_state() -> ConvergenceState {
        ConvergenceState::Running {
            integration_workspace_id: WorkspaceId::new(),
            input_target_commit_oid: "base".into(),
        }
    }

    #[test]
    fn deserialize_rejects_running_without_integration_workspace_id() {
        let convergence = base_convergence(running_state());
        let mut value = serde_json::to_value(convergence).expect("serialize");
        value
            .as_object_mut()
            .unwrap()
            .remove("integration_workspace_id");

        let error = serde_json::from_value::<Convergence>(value)
            .expect_err("missing integration_workspace_id");
        assert!(
            error.to_string().contains("integration_workspace_id"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn deserialize_rejects_prepared_without_prepared_commit_oid() {
        let convergence = base_convergence(ConvergenceState::Prepared {
            integration_workspace_id: WorkspaceId::new(),
            input_target_commit_oid: "base".into(),
            prepared_commit_oid: "prep".into(),
            completed_at: Some(default_timestamp()),
        });
        let mut value = serde_json::to_value(convergence).expect("serialize");
        value.as_object_mut().unwrap().remove("prepared_commit_oid");

        let error =
            serde_json::from_value::<Convergence>(value).expect_err("missing prepared_commit_oid");
        assert!(
            error.to_string().contains("prepared_commit_oid"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn deserialize_rejects_finalized_without_final_target_commit_oid() {
        let convergence = base_convergence(ConvergenceState::Finalized {
            integration_workspace_id: Some(WorkspaceId::new()),
            input_target_commit_oid: "base".into(),
            prepared_commit_oid: "prep".into(),
            final_target_commit_oid: "final".into(),
            checkout_adoption: FinalizedCheckoutAdoption::pending(default_timestamp()),
            completed_at: default_timestamp(),
        });
        let mut value = serde_json::to_value(convergence).expect("serialize");
        value
            .as_object_mut()
            .unwrap()
            .remove("final_target_commit_oid");

        let error = serde_json::from_value::<Convergence>(value)
            .expect_err("missing final_target_commit_oid");
        assert!(
            error.to_string().contains("final_target_commit_oid"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn deserialize_rejects_conflicted_without_conflict_summary() {
        let convergence = base_convergence(ConvergenceState::Conflicted {
            integration_workspace_id: WorkspaceId::new(),
            input_target_commit_oid: "base".into(),
            conflict_summary: "oops".into(),
            completed_at: default_timestamp(),
        });
        let mut value = serde_json::to_value(convergence).expect("serialize");
        value.as_object_mut().unwrap().remove("conflict_summary");

        let error =
            serde_json::from_value::<Convergence>(value).expect_err("missing conflict_summary");
        assert!(
            error.to_string().contains("conflict_summary"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn deserialize_rejects_failed_without_completed_at() {
        let convergence = base_convergence(ConvergenceState::Failed {
            integration_workspace_id: None,
            input_target_commit_oid: None,
            conflict_summary: None,
            completed_at: default_timestamp(),
        });
        let mut value = serde_json::to_value(convergence).expect("serialize");
        value
            .as_object_mut()
            .unwrap()
            .insert("completed_at".into(), serde_json::Value::Null);

        let error = serde_json::from_value::<Convergence>(value).expect_err("missing completed_at");
        assert!(
            error.to_string().contains("completed_at"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn transition_to_prepared_rejects_queued_state() {
        let mut convergence = base_convergence(ConvergenceState::Queued);

        let error = convergence
            .transition_to_prepared("prep".into(), Some(default_timestamp()))
            .expect_err("queued convergence should not transition directly to prepared");

        assert_eq!(error, ConvergenceTransitionError::QueuedMissingContext);
    }

    #[test]
    fn transition_to_finalized_rejects_failed_state_without_workspace() {
        let mut convergence = base_convergence(ConvergenceState::Failed {
            integration_workspace_id: None,
            input_target_commit_oid: Some("base".into()),
            conflict_summary: None,
            completed_at: default_timestamp(),
        });

        let error = convergence
            .transition_to_finalized(
                "final".into(),
                FinalizedCheckoutAdoption::pending(default_timestamp()),
                default_timestamp(),
            )
            .expect_err("failed convergence without workspace should not finalize");

        assert_eq!(error, ConvergenceTransitionError::FailedMissingWorkspace);
    }

    #[test]
    fn round_trip_preserves_all_variants() {
        let variants = vec![
            base_convergence(ConvergenceState::Queued),
            base_convergence(running_state()),
            base_convergence(ConvergenceState::Conflicted {
                integration_workspace_id: WorkspaceId::new(),
                input_target_commit_oid: "base".into(),
                conflict_summary: "conflict!".into(),
                completed_at: default_timestamp(),
            }),
            base_convergence(ConvergenceState::Prepared {
                integration_workspace_id: WorkspaceId::new(),
                input_target_commit_oid: "base".into(),
                prepared_commit_oid: "prep".into(),
                completed_at: Some(default_timestamp()),
            }),
            base_convergence(ConvergenceState::Finalized {
                integration_workspace_id: Some(WorkspaceId::new()),
                input_target_commit_oid: "base".into(),
                prepared_commit_oid: "prep".into(),
                final_target_commit_oid: "final".into(),
                checkout_adoption: FinalizedCheckoutAdoption::pending(default_timestamp()),
                completed_at: default_timestamp(),
            }),
            base_convergence(ConvergenceState::Failed {
                integration_workspace_id: Some(WorkspaceId::new()),
                input_target_commit_oid: Some("base".into()),
                conflict_summary: Some("fail".into()),
                completed_at: default_timestamp(),
            }),
            base_convergence(ConvergenceState::Cancelled {
                integration_workspace_id: Some(WorkspaceId::new()),
                input_target_commit_oid: Some("base".into()),
                completed_at: default_timestamp(),
            }),
        ];

        for original in variants {
            let expected_status = original.state.status();
            let json = serde_json::to_value(&original).expect("serialize");
            let roundtripped: Convergence = serde_json::from_value(json).expect("deserialize");
            assert_eq!(roundtripped.state.status(), expected_status);
            assert_eq!(roundtripped.id, original.id);
            assert_eq!(
                roundtripped.state.integration_workspace_id(),
                original.state.integration_workspace_id()
            );
            assert_eq!(
                roundtripped.state.input_target_commit_oid(),
                original.state.input_target_commit_oid()
            );
            assert_eq!(
                roundtripped.state.prepared_commit_oid(),
                original.state.prepared_commit_oid()
            );
            assert_eq!(
                roundtripped.state.final_target_commit_oid(),
                original.state.final_target_commit_oid()
            );
            assert_eq!(
                roundtripped.state.conflict_summary(),
                original.state.conflict_summary()
            );
        }
    }
}
