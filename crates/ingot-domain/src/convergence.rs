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

impl ConvergenceState {
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

    fn into_required_transition_context(self) -> (WorkspaceId, CommitOid) {
        match self {
            Self::Running {
                integration_workspace_id,
                input_target_commit_oid,
            } => (integration_workspace_id, input_target_commit_oid),
            Self::Prepared {
                integration_workspace_id,
                input_target_commit_oid,
                ..
            } => (integration_workspace_id, input_target_commit_oid),
            Self::Finalized {
                integration_workspace_id,
                input_target_commit_oid,
                ..
            } => (
                integration_workspace_id
                    .expect("finalized convergence missing workspace for transition"),
                input_target_commit_oid,
            ),
            Self::Conflicted {
                integration_workspace_id,
                input_target_commit_oid,
                ..
            } => (integration_workspace_id, input_target_commit_oid),
            Self::Failed {
                integration_workspace_id,
                input_target_commit_oid,
                ..
            } => (
                integration_workspace_id
                    .expect("failed convergence missing workspace for transition"),
                input_target_commit_oid
                    .expect("failed convergence missing input_target for transition"),
            ),
            Self::Cancelled {
                integration_workspace_id,
                input_target_commit_oid,
                ..
            } => (
                integration_workspace_id
                    .expect("cancelled convergence missing workspace for transition"),
                input_target_commit_oid
                    .expect("cancelled convergence missing input_target for transition"),
            ),
            Self::Queued => panic!("cannot extract running fields from Queued state"),
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

    pub fn transition_to_conflicted(&mut self, summary: String, completed_at: DateTime<Utc>) {
        let (integration_workspace_id, input_target_commit_oid) =
            self.take_state().into_required_transition_context();
        self.state = ConvergenceState::Conflicted {
            integration_workspace_id,
            input_target_commit_oid,
            conflict_summary: summary,
            completed_at,
        };
    }

    pub fn transition_to_prepared(
        &mut self,
        prepared_oid: CommitOid,
        completed_at: Option<DateTime<Utc>>,
    ) {
        let (integration_workspace_id, input_target_commit_oid) =
            self.take_state().into_required_transition_context();
        self.state = ConvergenceState::Prepared {
            integration_workspace_id,
            input_target_commit_oid,
            prepared_commit_oid: prepared_oid,
            completed_at,
        };
    }

    pub fn transition_to_finalized(&mut self, final_oid: CommitOid, completed_at: DateTime<Utc>) {
        self.state = match self.take_state() {
            ConvergenceState::Prepared {
                integration_workspace_id,
                input_target_commit_oid,
                prepared_commit_oid,
                completed_at: existing_completed_at,
            } => ConvergenceState::Finalized {
                integration_workspace_id: Some(integration_workspace_id),
                input_target_commit_oid,
                prepared_commit_oid,
                final_target_commit_oid: final_oid,
                completed_at: existing_completed_at.unwrap_or(completed_at),
            },
            other => {
                let (integration_workspace_id, input_target_commit_oid) =
                    other.into_required_transition_context();
                ConvergenceState::Finalized {
                    integration_workspace_id: Some(integration_workspace_id),
                    input_target_commit_oid,
                    prepared_commit_oid: final_oid.clone(),
                    final_target_commit_oid: final_oid,
                    completed_at,
                }
            }
        };
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
    pub target_head_valid: Option<bool>,
    pub conflict_summary: Option<String>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

fn required_convergence_field<T>(field: &'static str, value: Option<T>) -> Result<T, String> {
    value.ok_or_else(|| format!("convergence {field} is required for this status"))
}

impl TryFrom<ConvergenceWire> for Convergence {
    type Error = String;

    fn try_from(w: ConvergenceWire) -> Result<Self, Self::Error> {
        let state = match w.status {
            ConvergenceStatus::Queued => ConvergenceState::Queued,
            ConvergenceStatus::Running => ConvergenceState::Running {
                integration_workspace_id: required_convergence_field(
                    "integration_workspace_id",
                    w.integration_workspace_id,
                )?,
                input_target_commit_oid: required_convergence_field(
                    "input_target_commit_oid",
                    w.input_target_commit_oid,
                )?,
            },
            ConvergenceStatus::Conflicted => ConvergenceState::Conflicted {
                integration_workspace_id: required_convergence_field(
                    "integration_workspace_id",
                    w.integration_workspace_id,
                )?,
                input_target_commit_oid: required_convergence_field(
                    "input_target_commit_oid",
                    w.input_target_commit_oid,
                )?,
                conflict_summary: required_convergence_field(
                    "conflict_summary",
                    w.conflict_summary,
                )?,
                completed_at: required_convergence_field("completed_at", w.completed_at)?,
            },
            ConvergenceStatus::Prepared => ConvergenceState::Prepared {
                integration_workspace_id: required_convergence_field(
                    "integration_workspace_id",
                    w.integration_workspace_id,
                )?,
                input_target_commit_oid: required_convergence_field(
                    "input_target_commit_oid",
                    w.input_target_commit_oid,
                )?,
                prepared_commit_oid: required_convergence_field(
                    "prepared_commit_oid",
                    w.prepared_commit_oid,
                )?,
                completed_at: w.completed_at,
            },
            ConvergenceStatus::Finalized => ConvergenceState::Finalized {
                integration_workspace_id: w.integration_workspace_id,
                input_target_commit_oid: required_convergence_field(
                    "input_target_commit_oid",
                    w.input_target_commit_oid,
                )?,
                prepared_commit_oid: required_convergence_field(
                    "prepared_commit_oid",
                    w.prepared_commit_oid,
                )?,
                final_target_commit_oid: required_convergence_field(
                    "final_target_commit_oid",
                    w.final_target_commit_oid,
                )?,
                completed_at: required_convergence_field("completed_at", w.completed_at)?,
            },
            ConvergenceStatus::Failed => ConvergenceState::Failed {
                integration_workspace_id: w.integration_workspace_id,
                input_target_commit_oid: w.input_target_commit_oid,
                conflict_summary: w.conflict_summary,
                completed_at: required_convergence_field("completed_at", w.completed_at)?,
            },
            ConvergenceStatus::Cancelled => ConvergenceState::Cancelled {
                integration_workspace_id: w.integration_workspace_id,
                input_target_commit_oid: w.input_target_commit_oid,
                completed_at: required_convergence_field("completed_at", w.completed_at)?,
            },
        };

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
