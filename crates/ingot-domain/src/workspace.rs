use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::commit_oid::CommitOid;
use crate::git_ref::GitRef;
use crate::ids::{ItemRevisionId, JobId, ProjectId, WorkspaceId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceKind {
    Authoring,
    Review,
    Integration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceStrategy {
    Worktree,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetentionPolicy {
    Ephemeral,
    RetainUntilDebug,
    Persistent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceStatus {
    Provisioning,
    Ready,
    Busy,
    Stale,
    RetainedForDebug,
    Abandoned,
    Error,
    Removing,
}

/// Git commit OIDs that identify a workspace's position. Present once provisioned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceCommitState {
    pub base_commit_oid: CommitOid,
    pub head_commit_oid: CommitOid,
}

impl WorkspaceCommitState {
    #[must_use]
    pub fn new(base_commit_oid: CommitOid, head_commit_oid: CommitOid) -> Self {
        Self {
            base_commit_oid,
            head_commit_oid,
        }
    }

    #[must_use]
    pub fn from_option_parts(
        base_commit_oid: Option<CommitOid>,
        head_commit_oid: Option<CommitOid>,
    ) -> Option<Self> {
        match (base_commit_oid, head_commit_oid) {
            (Some(base_commit_oid), Some(head_commit_oid)) => {
                Some(Self::new(base_commit_oid, head_commit_oid))
            }
            _ => None,
        }
    }

    #[must_use]
    pub fn with_head_commit_oid(mut self, head_commit_oid: CommitOid) -> Self {
        self.head_commit_oid = head_commit_oid;
        self
    }
}

/// Lifecycle state of a Workspace, replacing `status` + `current_job_id` +
/// conditional commit OID fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceState {
    /// Workspace record created, filesystem provisioning not yet confirmed.
    Provisioning {
        commits: Option<WorkspaceCommitState>,
    },

    /// Workspace is provisioned and idle. No job attached.
    Ready { commits: WorkspaceCommitState },

    /// Workspace is attached to a running job.
    Busy {
        commits: WorkspaceCommitState,
        current_job_id: JobId,
    },

    /// Workspace contents are stale (e.g., target ref moved, heartbeat expiry).
    Stale {
        commits: Option<WorkspaceCommitState>,
    },

    /// Retained for post-mortem debugging. No job attached.
    RetainedForDebug { commits: WorkspaceCommitState },

    /// Partially provisioned or in an error condition.
    Error {
        commits: Option<WorkspaceCommitState>,
    },

    /// Transitional: filesystem cleanup in progress.
    Removing {
        commits: Option<WorkspaceCommitState>,
    },

    /// Terminal: workspace is abandoned.
    Abandoned {
        commits: Option<WorkspaceCommitState>,
    },
}

impl WorkspaceState {
    pub fn from_parts(
        status: WorkspaceStatus,
        commits: Option<WorkspaceCommitState>,
        current_job_id: Option<JobId>,
    ) -> Result<Self, String> {
        match status {
            WorkspaceStatus::Provisioning => Ok(Self::Provisioning { commits }),
            WorkspaceStatus::Ready => Ok(Self::Ready {
                commits: required_workspace_field(
                    status,
                    "base_commit_oid/head_commit_oid",
                    commits,
                )?,
            }),
            WorkspaceStatus::Busy => Ok(Self::Busy {
                commits: required_workspace_field(
                    status,
                    "base_commit_oid/head_commit_oid",
                    commits,
                )?,
                current_job_id: required_workspace_field(status, "current_job_id", current_job_id)?,
            }),
            WorkspaceStatus::Stale => Ok(Self::Stale { commits }),
            WorkspaceStatus::RetainedForDebug => Ok(Self::RetainedForDebug {
                commits: required_workspace_field(
                    status,
                    "base_commit_oid/head_commit_oid",
                    commits,
                )?,
            }),
            WorkspaceStatus::Error => Ok(Self::Error { commits }),
            WorkspaceStatus::Removing => Ok(Self::Removing { commits }),
            WorkspaceStatus::Abandoned => Ok(Self::Abandoned { commits }),
        }
    }

    #[must_use]
    pub fn status(&self) -> WorkspaceStatus {
        match self {
            Self::Provisioning { .. } => WorkspaceStatus::Provisioning,
            Self::Ready { .. } => WorkspaceStatus::Ready,
            Self::Busy { .. } => WorkspaceStatus::Busy,
            Self::Stale { .. } => WorkspaceStatus::Stale,
            Self::RetainedForDebug { .. } => WorkspaceStatus::RetainedForDebug,
            Self::Error { .. } => WorkspaceStatus::Error,
            Self::Removing { .. } => WorkspaceStatus::Removing,
            Self::Abandoned { .. } => WorkspaceStatus::Abandoned,
        }
    }

    #[must_use]
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            Self::Provisioning { .. }
                | Self::Ready { .. }
                | Self::Busy { .. }
                | Self::Stale { .. }
                | Self::RetainedForDebug { .. }
        )
    }

    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Abandoned { .. })
    }

    #[must_use]
    pub fn current_job_id(&self) -> Option<JobId> {
        match self {
            Self::Busy { current_job_id, .. } => Some(*current_job_id),
            _ => None,
        }
    }

    #[must_use]
    pub fn base_commit_oid(&self) -> Option<&CommitOid> {
        self.commits().map(|c| &c.base_commit_oid)
    }

    #[must_use]
    pub fn head_commit_oid(&self) -> Option<&CommitOid> {
        self.commits().map(|c| &c.head_commit_oid)
    }

    #[must_use]
    pub fn commits(&self) -> Option<&WorkspaceCommitState> {
        match self {
            Self::Ready { commits }
            | Self::Busy { commits, .. }
            | Self::RetainedForDebug { commits } => Some(commits),
            Self::Stale { commits }
            | Self::Provisioning { commits }
            | Self::Error { commits }
            | Self::Removing { commits }
            | Self::Abandoned { commits } => commits.as_ref(),
        }
    }

    // --- Transition methods ---

    /// Transition to Ready with confirmed commit state.
    #[must_use]
    pub fn into_ready(self, commits: WorkspaceCommitState) -> Self {
        Self::Ready { commits }
    }

    /// Transition Ready → Busy by attaching a job.
    #[must_use]
    pub fn into_busy(self, job_id: JobId) -> Self {
        match self {
            Self::Ready { commits } => Self::Busy {
                commits,
                current_job_id: job_id,
            },
            other => Self::Busy {
                commits: other
                    .into_commits()
                    .expect("into_busy requires commit state"),
                current_job_id: job_id,
            },
        }
    }

    /// Busy → Ready: release the job, preserve commits.
    #[must_use]
    pub fn into_released(self) -> Self {
        match self {
            Self::Busy { commits, .. } => Self::Ready { commits },
            other => other,
        }
    }

    /// Busy → Ready with updated head commit OID.
    #[must_use]
    pub fn into_released_with_head(self, head_commit_oid: CommitOid) -> Self {
        match self {
            Self::Busy { mut commits, .. } => {
                commits.head_commit_oid = head_commit_oid;
                Self::Ready { commits }
            }
            other => other,
        }
    }

    /// Any active → Stale.
    #[must_use]
    pub fn into_stale(self) -> Self {
        Self::Stale {
            commits: self.into_commits(),
        }
    }

    /// Any → Error.
    #[must_use]
    pub fn into_error(self) -> Self {
        Self::Error {
            commits: self.into_commits(),
        }
    }

    /// Any → Removing.
    #[must_use]
    pub fn into_removing(self) -> Self {
        Self::Removing {
            commits: self.into_commits(),
        }
    }

    /// Any → Abandoned.
    #[must_use]
    pub fn into_abandoned(self) -> Self {
        Self::Abandoned {
            commits: self.into_commits(),
        }
    }

    /// Update head_commit_oid in place (preserving current variant).
    #[must_use]
    pub fn with_head_commit_oid(self, head_commit_oid: CommitOid) -> Self {
        match self {
            Self::Ready { commits } => Self::Ready {
                commits: commits.with_head_commit_oid(head_commit_oid),
            },
            Self::Busy {
                commits,
                current_job_id,
            } => Self::Busy {
                commits: commits.with_head_commit_oid(head_commit_oid),
                current_job_id,
            },
            Self::Stale { commits } => Self::Stale {
                commits: commits.map(|commits| commits.with_head_commit_oid(head_commit_oid)),
            },
            Self::RetainedForDebug { commits } => Self::RetainedForDebug {
                commits: commits.with_head_commit_oid(head_commit_oid),
            },
            Self::Provisioning { commits } => Self::Provisioning {
                commits: Some(
                    commits
                        .unwrap_or_else(|| {
                            WorkspaceCommitState::new(
                                head_commit_oid.clone(),
                                head_commit_oid.clone(),
                            )
                        })
                        .with_head_commit_oid(head_commit_oid),
                ),
            },
            Self::Error { commits } => Self::Error {
                commits: Some(
                    commits
                        .unwrap_or_else(|| {
                            WorkspaceCommitState::new(
                                head_commit_oid.clone(),
                                head_commit_oid.clone(),
                            )
                        })
                        .with_head_commit_oid(head_commit_oid),
                ),
            },
            Self::Removing { commits } => Self::Removing {
                commits: Some(
                    commits
                        .unwrap_or_else(|| {
                            WorkspaceCommitState::new(
                                head_commit_oid.clone(),
                                head_commit_oid.clone(),
                            )
                        })
                        .with_head_commit_oid(head_commit_oid),
                ),
            },
            Self::Abandoned { commits } => Self::Abandoned {
                commits: Some(
                    commits
                        .unwrap_or_else(|| {
                            WorkspaceCommitState::new(
                                head_commit_oid.clone(),
                                head_commit_oid.clone(),
                            )
                        })
                        .with_head_commit_oid(head_commit_oid),
                ),
            },
        }
    }

    /// Extract owned commits, consuming the state.
    fn into_commits(self) -> Option<WorkspaceCommitState> {
        match self {
            Self::Ready { commits }
            | Self::Busy { commits, .. }
            | Self::RetainedForDebug { commits } => Some(commits),
            Self::Stale { commits }
            | Self::Provisioning { commits }
            | Self::Error { commits }
            | Self::Removing { commits }
            | Self::Abandoned { commits } => commits,
        }
    }
}

// --- Workspace convenience methods ---

impl Workspace {
    fn transition_state(
        &mut self,
        now: DateTime<Utc>,
        transition: impl FnOnce(WorkspaceState) -> WorkspaceState,
    ) {
        let previous =
            std::mem::replace(&mut self.state, WorkspaceState::Abandoned { commits: None });
        self.state = transition(previous);
        self.updated_at = now;
    }

    pub fn mark_ready(&mut self, commits: WorkspaceCommitState, now: DateTime<Utc>) {
        self.state = WorkspaceState::Ready { commits };
        self.updated_at = now;
    }

    pub fn mark_ready_with_head(&mut self, head_commit_oid: CommitOid, now: DateTime<Utc>) {
        let base_commit_oid = self
            .state
            .base_commit_oid()
            .cloned()
            .unwrap_or_else(|| head_commit_oid.clone());
        self.mark_ready(
            WorkspaceCommitState::new(base_commit_oid, head_commit_oid),
            now,
        );
    }

    /// Release workspace from Busy to `target_status`. No-op if not Busy.
    /// Preserves the existing conditional pattern from `release_workspace`.
    pub fn release_to(&mut self, target_status: WorkspaceStatus, now: DateTime<Utc>) {
        if self.state.status() != WorkspaceStatus::Busy {
            return;
        }

        self.transition_state(now, |state| match target_status {
            WorkspaceStatus::Ready => state.into_released(),
            WorkspaceStatus::Stale => state.into_stale(),
            WorkspaceStatus::Abandoned => state.into_abandoned(),
            WorkspaceStatus::Error => state.into_error(),
            _ => state.into_released(),
        });
    }

    /// Release workspace from Busy to Ready with updated head commit OID.
    pub fn release_with_head(&mut self, head_commit_oid: CommitOid, now: DateTime<Utc>) {
        self.transition_state(now, |state| state.into_released_with_head(head_commit_oid));
    }

    /// Attach a job: transition to Busy.
    pub fn attach_job(&mut self, job_id: JobId, now: DateTime<Utc>) {
        self.transition_state(now, |state| state.into_busy(job_id));
    }

    /// Mark workspace as Stale.
    pub fn mark_stale(&mut self, now: DateTime<Utc>) {
        self.transition_state(now, WorkspaceState::into_stale);
    }

    /// Mark workspace as Error.
    pub fn mark_error(&mut self, now: DateTime<Utc>) {
        self.transition_state(now, WorkspaceState::into_error);
    }

    /// Mark workspace as Removing.
    pub fn mark_removing(&mut self, now: DateTime<Utc>) {
        self.transition_state(now, WorkspaceState::into_removing);
    }

    /// Mark workspace as Abandoned.
    pub fn mark_abandoned(&mut self, now: DateTime<Utc>) {
        self.transition_state(now, WorkspaceState::into_abandoned);
    }

    /// Update head_commit_oid in place, preserving the current state variant.
    pub fn set_head_commit_oid(&mut self, head_commit_oid: CommitOid, now: DateTime<Utc>) {
        self.transition_state(now, |state| state.with_head_commit_oid(head_commit_oid));
    }
}

// --- Serde: backward-compatible JSON via WorkspaceWire ---

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WorkspaceWire {
    pub id: WorkspaceId,
    pub project_id: ProjectId,
    pub kind: WorkspaceKind,
    pub strategy: WorkspaceStrategy,
    pub path: String,
    pub created_for_revision_id: Option<ItemRevisionId>,
    pub parent_workspace_id: Option<WorkspaceId>,
    pub target_ref: Option<GitRef>,
    pub workspace_ref: Option<GitRef>,
    pub base_commit_oid: Option<CommitOid>,
    pub head_commit_oid: Option<CommitOid>,
    pub retention_policy: RetentionPolicy,
    pub status: WorkspaceStatus,
    pub current_job_id: Option<JobId>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

fn required_workspace_field<T>(
    status: WorkspaceStatus,
    field: &'static str,
    value: Option<T>,
) -> Result<T, String> {
    value.ok_or_else(|| format!("workspace {field} is required for {status:?}"))
}

impl TryFrom<WorkspaceWire> for Workspace {
    type Error = String;

    fn try_from(w: WorkspaceWire) -> Result<Self, Self::Error> {
        let state = WorkspaceState::from_parts(
            w.status,
            WorkspaceCommitState::from_option_parts(w.base_commit_oid, w.head_commit_oid),
            w.current_job_id,
        )?;

        Ok(Workspace {
            id: w.id,
            project_id: w.project_id,
            kind: w.kind,
            strategy: w.strategy,
            path: w.path,
            created_for_revision_id: w.created_for_revision_id,
            parent_workspace_id: w.parent_workspace_id,
            target_ref: w.target_ref,
            workspace_ref: w.workspace_ref,
            retention_policy: w.retention_policy,
            created_at: w.created_at,
            updated_at: w.updated_at,
            state,
        })
    }
}

impl From<Workspace> for WorkspaceWire {
    fn from(ws: Workspace) -> Self {
        let status = ws.state.status();
        let current_job_id = ws.state.current_job_id();
        let base_commit_oid = ws.state.base_commit_oid().cloned();
        let head_commit_oid = ws.state.head_commit_oid().cloned();

        WorkspaceWire {
            id: ws.id,
            project_id: ws.project_id,
            kind: ws.kind,
            strategy: ws.strategy,
            path: ws.path,
            created_for_revision_id: ws.created_for_revision_id,
            parent_workspace_id: ws.parent_workspace_id,
            target_ref: ws.target_ref,
            workspace_ref: ws.workspace_ref,
            base_commit_oid,
            head_commit_oid,
            retention_policy: ws.retention_policy,
            status,
            current_job_id,
            created_at: ws.created_at,
            updated_at: ws.updated_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(try_from = "WorkspaceWire", into = "WorkspaceWire")]
pub struct Workspace {
    // Immutable identity
    pub id: WorkspaceId,
    pub project_id: ProjectId,
    pub kind: WorkspaceKind,
    pub strategy: WorkspaceStrategy,
    pub created_for_revision_id: Option<ItemRevisionId>,
    pub parent_workspace_id: Option<WorkspaceId>,
    pub retention_policy: RetentionPolicy,
    pub created_at: DateTime<Utc>,

    // Mutable, not status-dependent
    pub path: String,
    pub target_ref: Option<GitRef>,
    pub workspace_ref: Option<GitRef>,
    pub updated_at: DateTime<Utc>,

    // Lifecycle state (replaces status + current_job_id + base/head commit OIDs)
    pub state: WorkspaceState,
}

#[cfg(test)]
mod tests {
    use crate::test_support::WorkspaceBuilder;

    use super::*;

    fn base_workspace(state: WorkspaceState) -> Workspace {
        let mut workspace = WorkspaceBuilder::new(ProjectId::new(), WorkspaceKind::Authoring)
            .path("/tmp/test")
            .workspace_ref("refs/ingot/workspaces/test")
            .build();
        workspace.state = state;
        workspace
    }

    fn ready_commits() -> WorkspaceCommitState {
        WorkspaceCommitState {
            base_commit_oid: CommitOid::new("abc123"),
            head_commit_oid: CommitOid::new("def456"),
        }
    }

    #[test]
    fn deserialize_rejects_ready_without_commits() {
        let ws = base_workspace(WorkspaceState::Ready {
            commits: ready_commits(),
        });
        let mut value = serde_json::to_value(ws).expect("serialize");
        value.as_object_mut().unwrap().remove("base_commit_oid");
        value.as_object_mut().unwrap().remove("head_commit_oid");

        let error = serde_json::from_value::<Workspace>(value).expect_err("missing commits");
        assert!(
            error
                .to_string()
                .contains("base_commit_oid/head_commit_oid"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn deserialize_rejects_busy_without_current_job_id() {
        let ws = base_workspace(WorkspaceState::Busy {
            commits: ready_commits(),
            current_job_id: JobId::new(),
        });
        let mut value = serde_json::to_value(ws).expect("serialize");
        value
            .as_object_mut()
            .unwrap()
            .insert("current_job_id".into(), serde_json::Value::Null);

        let error = serde_json::from_value::<Workspace>(value).expect_err("missing current_job_id");
        assert!(
            error.to_string().contains("current_job_id"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn deserialize_rejects_busy_without_commits() {
        let ws = base_workspace(WorkspaceState::Busy {
            commits: ready_commits(),
            current_job_id: JobId::new(),
        });
        let mut value = serde_json::to_value(ws).expect("serialize");
        value.as_object_mut().unwrap().remove("base_commit_oid");
        value.as_object_mut().unwrap().remove("head_commit_oid");

        let error = serde_json::from_value::<Workspace>(value).expect_err("missing commits");
        assert!(
            error
                .to_string()
                .contains("base_commit_oid/head_commit_oid"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn deserialize_allows_stale_without_commits() {
        let ws = base_workspace(WorkspaceState::Stale {
            commits: Some(ready_commits()),
        });
        let mut value = serde_json::to_value(ws).expect("serialize");
        value.as_object_mut().unwrap().remove("base_commit_oid");

        let roundtripped = serde_json::from_value::<Workspace>(value).expect("stale without commits");
        assert!(roundtripped.state.commits().is_none());
    }

    #[test]
    fn round_trip_preserves_all_variants() {
        let variants = vec![
            base_workspace(WorkspaceState::Provisioning { commits: None }),
            base_workspace(WorkspaceState::Provisioning {
                commits: Some(ready_commits()),
            }),
            base_workspace(WorkspaceState::Ready {
                commits: ready_commits(),
            }),
            base_workspace(WorkspaceState::Busy {
                commits: ready_commits(),
                current_job_id: JobId::new(),
            }),
            base_workspace(WorkspaceState::Stale {
                commits: Some(ready_commits()),
            }),
            base_workspace(WorkspaceState::RetainedForDebug {
                commits: ready_commits(),
            }),
            base_workspace(WorkspaceState::Error {
                commits: Some(ready_commits()),
            }),
            base_workspace(WorkspaceState::Error { commits: None }),
            base_workspace(WorkspaceState::Removing {
                commits: Some(ready_commits()),
            }),
            base_workspace(WorkspaceState::Removing { commits: None }),
            base_workspace(WorkspaceState::Abandoned {
                commits: Some(ready_commits()),
            }),
            base_workspace(WorkspaceState::Abandoned { commits: None }),
        ];

        for original in variants {
            let expected_status = original.state.status();
            let json = serde_json::to_value(&original).expect("serialize");
            let roundtripped: Workspace = serde_json::from_value(json).expect("deserialize");
            assert_eq!(roundtripped.state.status(), expected_status);
            assert_eq!(roundtripped.id, original.id);
            assert_eq!(
                roundtripped.state.current_job_id(),
                original.state.current_job_id()
            );
            assert_eq!(
                roundtripped.state.base_commit_oid(),
                original.state.base_commit_oid()
            );
            assert_eq!(
                roundtripped.state.head_commit_oid(),
                original.state.head_commit_oid()
            );
        }
    }

    #[test]
    fn busy_into_released_preserves_commits() {
        let state = WorkspaceState::Busy {
            commits: ready_commits(),
            current_job_id: JobId::new(),
        };
        let released = state.into_released();
        assert_eq!(released.status(), WorkspaceStatus::Ready);
        assert_eq!(
            released.head_commit_oid(),
            Some(&CommitOid::new("def456"))
        );
        assert!(released.current_job_id().is_none());
    }

    #[test]
    fn into_released_with_head_updates_head() {
        let state = WorkspaceState::Busy {
            commits: ready_commits(),
            current_job_id: JobId::new(),
        };
        let released = state.into_released_with_head(CommitOid::new("newhead"));
        assert_eq!(released.status(), WorkspaceStatus::Ready);
        assert_eq!(
            released.head_commit_oid(),
            Some(&CommitOid::new("newhead"))
        );
        assert_eq!(
            released.base_commit_oid(),
            Some(&CommitOid::new("abc123"))
        );
    }

    #[test]
    fn with_head_commit_oid_preserves_variant() {
        let state = WorkspaceState::Busy {
            commits: ready_commits(),
            current_job_id: JobId::new(),
        };
        let job_id = state.current_job_id();
        let updated = state.with_head_commit_oid(CommitOid::new("newhead"));
        assert_eq!(updated.status(), WorkspaceStatus::Busy);
        assert_eq!(
            updated.head_commit_oid(),
            Some(&CommitOid::new("newhead"))
        );
        assert_eq!(updated.current_job_id(), job_id);
    }
}
