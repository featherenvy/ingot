use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::commit_oid::CommitOid;
use crate::git_ref::GitRef;
use crate::ids::{GitOperationId, ProjectId, WorkspaceId};

#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "sqlx", sqlx(rename_all = "snake_case"))]
pub enum OperationKind {
    CreateJobCommit,
    PrepareConvergenceCommit,
    FinalizeTargetRef,
    CreateInvestigationRef,
    RemoveInvestigationRef,
    ResetWorkspace,
    RemoveWorkspaceRef,
}

#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "sqlx", sqlx(rename_all = "snake_case"))]
pub enum GitEntityType {
    Job,
    Convergence,
    Workspace,
    ItemRevision,
}

#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "sqlx", sqlx(rename_all = "snake_case"))]
pub enum GitOperationStatus {
    Planned,
    Applied,
    Reconciled,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConvergenceReplayMetadata {
    pub source_commit_oids: Vec<CommitOid>,
    pub prepared_commit_oids: Vec<CommitOid>,
}

#[derive(Debug, Clone)]
pub enum OperationPayload {
    CreateJobCommit {
        workspace_id: WorkspaceId,
        ref_name: GitRef,
        expected_old_oid: CommitOid,
        new_oid: Option<CommitOid>,
        commit_oid: Option<CommitOid>,
    },
    PrepareConvergenceCommit {
        workspace_id: WorkspaceId,
        ref_name: Option<GitRef>,
        expected_old_oid: CommitOid,
        new_oid: Option<CommitOid>,
        commit_oid: Option<CommitOid>,
        replay_metadata: Option<ConvergenceReplayMetadata>,
    },
    FinalizeTargetRef {
        workspace_id: Option<WorkspaceId>,
        ref_name: GitRef,
        expected_old_oid: CommitOid,
        new_oid: CommitOid,
        commit_oid: Option<CommitOid>,
    },
    CreateInvestigationRef {
        ref_name: GitRef,
        new_oid: CommitOid,
        commit_oid: Option<CommitOid>,
    },
    RemoveInvestigationRef {
        ref_name: GitRef,
        expected_old_oid: CommitOid,
    },
    ResetWorkspace {
        workspace_id: WorkspaceId,
        ref_name: Option<GitRef>,
        expected_old_oid: Option<CommitOid>,
        new_oid: CommitOid,
    },
    RemoveWorkspaceRef {
        workspace_id: WorkspaceId,
        ref_name: GitRef,
        expected_old_oid: CommitOid,
    },
}

impl OperationPayload {
    #[must_use]
    pub fn operation_kind(&self) -> OperationKind {
        match self {
            Self::CreateJobCommit { .. } => OperationKind::CreateJobCommit,
            Self::PrepareConvergenceCommit { .. } => OperationKind::PrepareConvergenceCommit,
            Self::FinalizeTargetRef { .. } => OperationKind::FinalizeTargetRef,
            Self::CreateInvestigationRef { .. } => OperationKind::CreateInvestigationRef,
            Self::RemoveInvestigationRef { .. } => OperationKind::RemoveInvestigationRef,
            Self::ResetWorkspace { .. } => OperationKind::ResetWorkspace,
            Self::RemoveWorkspaceRef { .. } => OperationKind::RemoveWorkspaceRef,
        }
    }

    #[must_use]
    pub fn entity_type(&self) -> GitEntityType {
        match self {
            Self::CreateJobCommit { .. } => GitEntityType::Job,
            Self::PrepareConvergenceCommit { .. } | Self::FinalizeTargetRef { .. } => {
                GitEntityType::Convergence
            }
            Self::CreateInvestigationRef { .. } | Self::RemoveInvestigationRef { .. } => {
                GitEntityType::Job
            }
            Self::ResetWorkspace { .. } | Self::RemoveWorkspaceRef { .. } => {
                GitEntityType::Workspace
            }
        }
    }

    #[must_use]
    pub fn workspace_id(&self) -> Option<WorkspaceId> {
        match self {
            Self::CreateJobCommit { workspace_id, .. }
            | Self::PrepareConvergenceCommit { workspace_id, .. }
            | Self::ResetWorkspace { workspace_id, .. }
            | Self::RemoveWorkspaceRef { workspace_id, .. } => Some(*workspace_id),
            Self::FinalizeTargetRef { workspace_id, .. } => *workspace_id,
            Self::CreateInvestigationRef { .. } | Self::RemoveInvestigationRef { .. } => None,
        }
    }

    #[must_use]
    pub fn ref_name(&self) -> Option<&GitRef> {
        match self {
            Self::CreateJobCommit { ref_name, .. }
            | Self::FinalizeTargetRef { ref_name, .. }
            | Self::CreateInvestigationRef { ref_name, .. }
            | Self::RemoveInvestigationRef { ref_name, .. }
            | Self::RemoveWorkspaceRef { ref_name, .. } => Some(ref_name),
            Self::PrepareConvergenceCommit { ref_name, .. }
            | Self::ResetWorkspace { ref_name, .. } => ref_name.as_ref(),
        }
    }

    #[must_use]
    pub fn expected_old_oid(&self) -> Option<&CommitOid> {
        match self {
            Self::CreateJobCommit {
                expected_old_oid, ..
            }
            | Self::PrepareConvergenceCommit {
                expected_old_oid, ..
            }
            | Self::FinalizeTargetRef {
                expected_old_oid, ..
            }
            | Self::RemoveInvestigationRef {
                expected_old_oid, ..
            }
            | Self::RemoveWorkspaceRef {
                expected_old_oid, ..
            } => Some(expected_old_oid),
            Self::ResetWorkspace {
                expected_old_oid, ..
            } => expected_old_oid.as_ref(),
            Self::CreateInvestigationRef { .. } => None,
        }
    }

    #[must_use]
    pub fn new_oid(&self) -> Option<&CommitOid> {
        match self {
            Self::CreateJobCommit { new_oid, .. }
            | Self::PrepareConvergenceCommit { new_oid, .. } => new_oid.as_ref(),
            Self::FinalizeTargetRef { new_oid, .. }
            | Self::CreateInvestigationRef { new_oid, .. }
            | Self::ResetWorkspace { new_oid, .. } => Some(new_oid),
            Self::RemoveInvestigationRef { .. } | Self::RemoveWorkspaceRef { .. } => None,
        }
    }

    #[must_use]
    pub fn commit_oid(&self) -> Option<&CommitOid> {
        match self {
            Self::CreateJobCommit { commit_oid, .. }
            | Self::PrepareConvergenceCommit { commit_oid, .. }
            | Self::FinalizeTargetRef { commit_oid, .. }
            | Self::CreateInvestigationRef { commit_oid, .. } => commit_oid.as_ref(),
            Self::RemoveInvestigationRef { .. }
            | Self::ResetWorkspace { .. }
            | Self::RemoveWorkspaceRef { .. } => None,
        }
    }

    #[must_use]
    pub fn effective_commit_oid(&self) -> Option<&CommitOid> {
        self.commit_oid().or_else(|| self.new_oid())
    }

    #[must_use]
    pub fn replay_metadata(&self) -> Option<&ConvergenceReplayMetadata> {
        match self {
            Self::PrepareConvergenceCommit {
                replay_metadata, ..
            } => replay_metadata.as_ref(),
            _ => None,
        }
    }

    pub fn set_job_commit_result(&mut self, oid: CommitOid) {
        match self {
            Self::CreateJobCommit {
                new_oid,
                commit_oid,
                ..
            } => {
                *new_oid = Some(oid.clone());
                *commit_oid = Some(oid);
            }
            _ => panic!("set_job_commit_result called on non-CreateJobCommit payload"),
        }
    }

    pub fn set_convergence_commit_result(&mut self, tip_oid: CommitOid) {
        match self {
            Self::PrepareConvergenceCommit {
                new_oid,
                commit_oid,
                ..
            } => {
                *new_oid = Some(tip_oid.clone());
                *commit_oid = Some(tip_oid);
            }
            _ => panic!(
                "set_convergence_commit_result called on non-PrepareConvergenceCommit payload"
            ),
        }
    }

    pub fn set_replay_metadata(&mut self, metadata: ConvergenceReplayMetadata) {
        match self {
            Self::PrepareConvergenceCommit {
                replay_metadata, ..
            } => {
                *replay_metadata = Some(metadata);
            }
            _ => panic!("set_replay_metadata called on non-PrepareConvergenceCommit payload"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct GitOperation {
    pub id: GitOperationId,
    pub project_id: ProjectId,
    pub entity_id: String,
    pub payload: OperationPayload,
    pub status: GitOperationStatus,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

impl GitOperation {
    #[must_use]
    pub fn operation_kind(&self) -> OperationKind {
        self.payload.operation_kind()
    }

    #[must_use]
    pub fn entity_type(&self) -> GitEntityType {
        self.payload.entity_type()
    }

    #[must_use]
    pub fn workspace_id(&self) -> Option<WorkspaceId> {
        self.payload.workspace_id()
    }

    #[must_use]
    pub fn ref_name(&self) -> Option<&GitRef> {
        self.payload.ref_name()
    }

    #[must_use]
    pub fn expected_old_oid(&self) -> Option<&CommitOid> {
        self.payload.expected_old_oid()
    }

    #[must_use]
    pub fn new_oid(&self) -> Option<&CommitOid> {
        self.payload.new_oid()
    }

    #[must_use]
    pub fn commit_oid(&self) -> Option<&CommitOid> {
        self.payload.commit_oid()
    }

    #[must_use]
    pub fn effective_commit_oid(&self) -> Option<&CommitOid> {
        self.payload.effective_commit_oid()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitOperationWire {
    pub id: GitOperationId,
    pub project_id: ProjectId,
    pub operation_kind: OperationKind,
    pub entity_type: GitEntityType,
    pub entity_id: String,
    pub workspace_id: Option<WorkspaceId>,
    pub ref_name: Option<GitRef>,
    pub expected_old_oid: Option<CommitOid>,
    pub new_oid: Option<CommitOid>,
    pub commit_oid: Option<CommitOid>,
    pub status: GitOperationStatus,
    pub metadata: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

impl TryFrom<GitOperationWire> for GitOperation {
    type Error = String;

    fn try_from(w: GitOperationWire) -> Result<Self, Self::Error> {
        let replay_metadata = w
            .metadata
            .as_ref()
            .map(|v| serde_json::from_value::<ConvergenceReplayMetadata>(v.clone()))
            .transpose()
            .map_err(|e| format!("invalid replay metadata: {e}"))?;

        let payload = match w.operation_kind {
            OperationKind::CreateJobCommit => {
                let workspace_id = w
                    .workspace_id
                    .ok_or("CreateJobCommit requires workspace_id")?;
                let ref_name = w.ref_name.ok_or("CreateJobCommit requires ref_name")?;
                let expected_old_oid = w
                    .expected_old_oid
                    .ok_or("CreateJobCommit requires expected_old_oid")?;
                OperationPayload::CreateJobCommit {
                    workspace_id,
                    ref_name,
                    expected_old_oid,
                    new_oid: w.new_oid,
                    commit_oid: w.commit_oid,
                }
            }
            OperationKind::PrepareConvergenceCommit => {
                let workspace_id = w
                    .workspace_id
                    .ok_or("PrepareConvergenceCommit requires workspace_id")?;
                let expected_old_oid = w
                    .expected_old_oid
                    .ok_or("PrepareConvergenceCommit requires expected_old_oid")?;
                OperationPayload::PrepareConvergenceCommit {
                    workspace_id,
                    ref_name: w.ref_name,
                    expected_old_oid,
                    new_oid: w.new_oid,
                    commit_oid: w.commit_oid,
                    replay_metadata,
                }
            }
            OperationKind::FinalizeTargetRef => {
                let ref_name = w.ref_name.ok_or("FinalizeTargetRef requires ref_name")?;
                let expected_old_oid = w
                    .expected_old_oid
                    .ok_or("FinalizeTargetRef requires expected_old_oid")?;
                let new_oid = w.new_oid.ok_or("FinalizeTargetRef requires new_oid")?;
                OperationPayload::FinalizeTargetRef {
                    workspace_id: w.workspace_id,
                    ref_name,
                    expected_old_oid,
                    new_oid,
                    commit_oid: w.commit_oid,
                }
            }
            OperationKind::CreateInvestigationRef => {
                let ref_name = w
                    .ref_name
                    .ok_or("CreateInvestigationRef requires ref_name")?;
                let new_oid = w.new_oid.ok_or("CreateInvestigationRef requires new_oid")?;
                OperationPayload::CreateInvestigationRef {
                    ref_name,
                    new_oid,
                    commit_oid: w.commit_oid,
                }
            }
            OperationKind::RemoveInvestigationRef => {
                let ref_name = w
                    .ref_name
                    .ok_or("RemoveInvestigationRef requires ref_name")?;
                let expected_old_oid = w
                    .expected_old_oid
                    .ok_or("RemoveInvestigationRef requires expected_old_oid")?;
                OperationPayload::RemoveInvestigationRef {
                    ref_name,
                    expected_old_oid,
                }
            }
            OperationKind::ResetWorkspace => {
                let workspace_id = w
                    .workspace_id
                    .ok_or("ResetWorkspace requires workspace_id")?;
                let new_oid = w.new_oid.ok_or("ResetWorkspace requires new_oid")?;
                OperationPayload::ResetWorkspace {
                    workspace_id,
                    ref_name: w.ref_name,
                    expected_old_oid: w.expected_old_oid,
                    new_oid,
                }
            }
            OperationKind::RemoveWorkspaceRef => {
                let workspace_id = w
                    .workspace_id
                    .ok_or("RemoveWorkspaceRef requires workspace_id")?;
                let ref_name = w.ref_name.ok_or("RemoveWorkspaceRef requires ref_name")?;
                let expected_old_oid = w
                    .expected_old_oid
                    .ok_or("RemoveWorkspaceRef requires expected_old_oid")?;
                OperationPayload::RemoveWorkspaceRef {
                    workspace_id,
                    ref_name,
                    expected_old_oid,
                }
            }
        };

        Ok(GitOperation {
            id: w.id,
            project_id: w.project_id,
            entity_id: w.entity_id,
            payload,
            status: w.status,
            created_at: w.created_at,
            completed_at: w.completed_at,
        })
    }
}

impl From<&GitOperation> for GitOperationWire {
    fn from(op: &GitOperation) -> Self {
        GitOperationWire {
            id: op.id,
            project_id: op.project_id,
            operation_kind: op.operation_kind(),
            entity_type: op.entity_type(),
            entity_id: op.entity_id.clone(),
            workspace_id: op.workspace_id(),
            ref_name: op.ref_name().cloned(),
            expected_old_oid: op.expected_old_oid().cloned(),
            new_oid: op.new_oid().cloned(),
            commit_oid: op.commit_oid().cloned(),
            status: op.status,
            metadata: op
                .payload
                .replay_metadata()
                .map(|metadata| serde_json::to_value(metadata).expect("serialize replay metadata")),
            created_at: op.created_at,
            completed_at: op.completed_at,
        }
    }
}

impl From<GitOperation> for GitOperationWire {
    fn from(op: GitOperation) -> Self {
        Self::from(&op)
    }
}

impl Serialize for GitOperation {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        GitOperationWire::from(self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for GitOperation {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = GitOperationWire::deserialize(deserializer)?;
        GitOperation::try_from(wire).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use crate::test_support::{GitOperationBuilder, default_timestamp};

    use super::*;

    fn test_project_id() -> ProjectId {
        "01234567-89ab-cdef-0123-456789abcdef".parse().unwrap()
    }

    fn test_workspace_id() -> WorkspaceId {
        "fedcba98-7654-3210-fedc-ba9876543210".parse().unwrap()
    }

    #[test]
    fn create_job_commit_accessors() {
        let payload = OperationPayload::CreateJobCommit {
            workspace_id: test_workspace_id(),
            ref_name: GitRef::new("refs/ingot/workspaces/auth"),
            expected_old_oid: "aaa".into(),
            new_oid: None,
            commit_oid: None,
        };
        assert_eq!(payload.operation_kind(), OperationKind::CreateJobCommit);
        assert_eq!(payload.entity_type(), GitEntityType::Job);
        assert_eq!(payload.workspace_id(), Some(test_workspace_id()));
        assert_eq!(
            payload.ref_name(),
            Some(&GitRef::new("refs/ingot/workspaces/auth"))
        );
        assert_eq!(payload.expected_old_oid(), Some(&CommitOid::from("aaa")));
        assert_eq!(payload.new_oid(), None);
        assert_eq!(payload.commit_oid(), None);
        assert_eq!(payload.effective_commit_oid(), None);
    }

    #[test]
    fn effective_commit_oid_prefers_commit_oid() {
        let payload = OperationPayload::CreateJobCommit {
            workspace_id: test_workspace_id(),
            ref_name: GitRef::new("ref"),
            expected_old_oid: "old".into(),
            new_oid: Some("new".into()),
            commit_oid: Some("commit".into()),
        };
        assert_eq!(
            payload.effective_commit_oid(),
            Some(&CommitOid::from("commit"))
        );
    }

    #[test]
    fn effective_commit_oid_falls_back_to_new_oid() {
        let payload = OperationPayload::FinalizeTargetRef {
            workspace_id: None,
            ref_name: GitRef::new("ref"),
            expected_old_oid: "old".into(),
            new_oid: "new".into(),
            commit_oid: None,
        };
        assert_eq!(
            payload.effective_commit_oid(),
            Some(&CommitOid::from("new"))
        );
    }

    #[test]
    fn set_job_commit_result_sets_both_fields() {
        let mut payload = OperationPayload::CreateJobCommit {
            workspace_id: test_workspace_id(),
            ref_name: GitRef::new("ref"),
            expected_old_oid: "old".into(),
            new_oid: None,
            commit_oid: None,
        };
        payload.set_job_commit_result("abc123".into());
        assert_eq!(payload.new_oid(), Some(&CommitOid::from("abc123")));
        assert_eq!(payload.commit_oid(), Some(&CommitOid::from("abc123")));
    }

    #[test]
    fn set_convergence_commit_result_sets_both_fields() {
        let mut payload = OperationPayload::PrepareConvergenceCommit {
            workspace_id: test_workspace_id(),
            ref_name: None,
            expected_old_oid: "old".into(),
            new_oid: None,
            commit_oid: None,
            replay_metadata: None,
        };
        payload.set_convergence_commit_result("tip123".into());
        assert_eq!(payload.new_oid(), Some(&CommitOid::from("tip123")));
        assert_eq!(payload.commit_oid(), Some(&CommitOid::from("tip123")));
    }

    #[test]
    fn set_replay_metadata_works() {
        let mut payload = OperationPayload::PrepareConvergenceCommit {
            workspace_id: test_workspace_id(),
            ref_name: None,
            expected_old_oid: "old".into(),
            new_oid: None,
            commit_oid: None,
            replay_metadata: None,
        };
        payload.set_replay_metadata(ConvergenceReplayMetadata {
            source_commit_oids: vec![CommitOid::from("s1")],
            prepared_commit_oids: vec![CommitOid::from("p1")],
        });
        match &payload {
            OperationPayload::PrepareConvergenceCommit {
                replay_metadata, ..
            } => {
                let rm = replay_metadata.as_ref().unwrap();
                assert_eq!(rm.source_commit_oids, vec![CommitOid::from("s1")]);
                assert_eq!(rm.prepared_commit_oids, vec![CommitOid::from("p1")]);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn entity_type_derivation() {
        let cases: Vec<(OperationPayload, GitEntityType)> = vec![
            (
                OperationPayload::CreateJobCommit {
                    workspace_id: test_workspace_id(),
                    ref_name: GitRef::new("r"),
                    expected_old_oid: "o".into(),
                    new_oid: None,
                    commit_oid: None,
                },
                GitEntityType::Job,
            ),
            (
                OperationPayload::PrepareConvergenceCommit {
                    workspace_id: test_workspace_id(),
                    ref_name: None,
                    expected_old_oid: "o".into(),
                    new_oid: None,
                    commit_oid: None,
                    replay_metadata: None,
                },
                GitEntityType::Convergence,
            ),
            (
                OperationPayload::FinalizeTargetRef {
                    workspace_id: None,
                    ref_name: GitRef::new("r"),
                    expected_old_oid: "o".into(),
                    new_oid: "n".into(),
                    commit_oid: None,
                },
                GitEntityType::Convergence,
            ),
            (
                OperationPayload::CreateInvestigationRef {
                    ref_name: GitRef::new("r"),
                    new_oid: "n".into(),
                    commit_oid: None,
                },
                GitEntityType::Job,
            ),
            (
                OperationPayload::RemoveInvestigationRef {
                    ref_name: GitRef::new("r"),
                    expected_old_oid: "o".into(),
                },
                GitEntityType::Job,
            ),
            (
                OperationPayload::ResetWorkspace {
                    workspace_id: test_workspace_id(),
                    ref_name: None,
                    expected_old_oid: None,
                    new_oid: "n".into(),
                },
                GitEntityType::Workspace,
            ),
            (
                OperationPayload::RemoveWorkspaceRef {
                    workspace_id: test_workspace_id(),
                    ref_name: GitRef::new("r"),
                    expected_old_oid: "o".into(),
                },
                GitEntityType::Workspace,
            ),
        ];

        for (payload, expected) in cases {
            assert_eq!(
                payload.entity_type(),
                expected,
                "{:?}",
                payload.operation_kind()
            );
        }
    }

    #[test]
    fn wire_round_trip_create_job_commit() {
        let op = GitOperationBuilder::new(
            test_project_id(),
            OperationKind::CreateJobCommit,
            GitEntityType::Job,
            "job-1",
        )
        .workspace_id(test_workspace_id())
        .ref_name("refs/ingot/workspaces/auth")
        .expected_old_oid("aaa")
        .new_oid("bbb")
        .commit_oid("bbb")
        .created_at(default_timestamp())
        .build();

        let json = serde_json::to_string(&op).unwrap();
        let restored: GitOperation = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.operation_kind(), OperationKind::CreateJobCommit);
        assert_eq!(
            restored.payload.ref_name(),
            Some(&GitRef::new("refs/ingot/workspaces/auth"))
        );
        assert_eq!(restored.payload.new_oid(), Some(&CommitOid::from("bbb")));
    }

    #[test]
    fn wire_round_trip_prepare_convergence_with_metadata() {
        let op = GitOperationBuilder::new(
            test_project_id(),
            OperationKind::PrepareConvergenceCommit,
            GitEntityType::Convergence,
            "conv-1",
        )
        .workspace_id(test_workspace_id())
        .ref_name("refs/ingot/workspaces/int")
        .expected_old_oid("old")
        .new_oid("tip")
        .commit_oid("tip")
        .metadata(serde_json::json!({
            "source_commit_oids": ["s1", "s2"],
            "prepared_commit_oids": ["p1", "p2"]
        }))
        .created_at(default_timestamp())
        .build();

        let json = serde_json::to_string(&op).unwrap();
        let restored: GitOperation = serde_json::from_str(&json).unwrap();
        assert_eq!(
            restored.operation_kind(),
            OperationKind::PrepareConvergenceCommit
        );
        match &restored.payload {
            OperationPayload::PrepareConvergenceCommit {
                replay_metadata, ..
            } => {
                let rm = replay_metadata.as_ref().unwrap();
                assert_eq!(
                    rm.source_commit_oids,
                    vec![CommitOid::from("s1"), CommitOid::from("s2")]
                );
                assert_eq!(
                    rm.prepared_commit_oids,
                    vec![CommitOid::from("p1"), CommitOid::from("p2")]
                );
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn wire_rejects_missing_required_fields() {
        let wire = GitOperationWire {
            id: GitOperationId::new(),
            project_id: test_project_id(),
            operation_kind: OperationKind::CreateJobCommit,
            entity_type: GitEntityType::Job,
            entity_id: "j1".into(),
            workspace_id: None, // required!
            ref_name: Some(GitRef::new("r")),
            expected_old_oid: Some("o".into()),
            new_oid: None,
            commit_oid: None,
            status: GitOperationStatus::Planned,
            metadata: None,
            created_at: default_timestamp(),
            completed_at: None,
        };
        assert!(GitOperation::try_from(wire).is_err());
    }
}
