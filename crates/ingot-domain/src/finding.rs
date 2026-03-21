use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::commit_oid::CommitOid;
use crate::ids::{FindingId, ItemId, ItemRevisionId, JobId, ProjectId};
use crate::step_id::StepId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingSubjectKind {
    Candidate,
    Integrated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingSeverity {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingTriageState {
    Untriaged,
    FixNow,
    WontFix,
    Backlog,
    Duplicate,
    DismissedInvalid,
    NeedsInvestigation,
}

impl FindingTriageState {
    #[must_use]
    pub fn is_unresolved(self) -> bool {
        matches!(self, Self::Untriaged | Self::NeedsInvestigation)
    }

    #[must_use]
    pub fn blocks_closure(self) -> bool {
        matches!(
            self,
            Self::Untriaged | Self::FixNow | Self::NeedsInvestigation
        )
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Untriaged => "untriaged",
            Self::FixNow => "fix_now",
            Self::WontFix => "wont_fix",
            Self::Backlog => "backlog",
            Self::Duplicate => "duplicate",
            Self::DismissedInvalid => "dismissed_invalid",
            Self::NeedsInvestigation => "needs_investigation",
        }
    }
}

/// Finding triage lifecycle. Encodes SPEC §4.13 conditional requirements:
/// - `linked_item_id` required iff `backlog|duplicate`
/// - `triage_note` required iff `wont_fix|dismissed_invalid|needs_investigation`
/// - `triaged_at` null while `untriaged`, required otherwise
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FindingTriage {
    Untriaged,

    FixNow {
        triaged_at: DateTime<Utc>,
    },

    WontFix {
        triage_note: String,
        triaged_at: DateTime<Utc>,
    },

    Backlog {
        linked_item_id: ItemId,
        triage_note: Option<String>,
        triaged_at: DateTime<Utc>,
    },

    Duplicate {
        linked_item_id: ItemId,
        triage_note: Option<String>,
        triaged_at: DateTime<Utc>,
    },

    DismissedInvalid {
        triage_note: String,
        triaged_at: DateTime<Utc>,
    },

    NeedsInvestigation {
        triage_note: String,
        triaged_at: DateTime<Utc>,
    },
}

impl FindingTriage {
    pub fn try_from_parts<E, F>(
        state: FindingTriageState,
        linked_item_id: Option<ItemId>,
        triage_note: Option<String>,
        triaged_at: Option<DateTime<Utc>>,
        mut missing_field: F,
    ) -> Result<Self, E>
    where
        F: FnMut(FindingTriageState, &'static str) -> E,
    {
        match state {
            FindingTriageState::Untriaged => Ok(Self::Untriaged),
            FindingTriageState::FixNow => Ok(Self::FixNow {
                triaged_at: required_triage_field(
                    state,
                    "triaged_at",
                    triaged_at,
                    &mut missing_field,
                )?,
            }),
            FindingTriageState::WontFix => Ok(Self::WontFix {
                triage_note: required_triage_field(
                    state,
                    "triage_note",
                    triage_note,
                    &mut missing_field,
                )?,
                triaged_at: required_triage_field(
                    state,
                    "triaged_at",
                    triaged_at,
                    &mut missing_field,
                )?,
            }),
            FindingTriageState::Backlog => Ok(Self::Backlog {
                linked_item_id: required_triage_field(
                    state,
                    "linked_item_id",
                    linked_item_id,
                    &mut missing_field,
                )?,
                triage_note,
                triaged_at: required_triage_field(
                    state,
                    "triaged_at",
                    triaged_at,
                    &mut missing_field,
                )?,
            }),
            FindingTriageState::Duplicate => Ok(Self::Duplicate {
                linked_item_id: required_triage_field(
                    state,
                    "linked_item_id",
                    linked_item_id,
                    &mut missing_field,
                )?,
                triage_note,
                triaged_at: required_triage_field(
                    state,
                    "triaged_at",
                    triaged_at,
                    &mut missing_field,
                )?,
            }),
            FindingTriageState::DismissedInvalid => Ok(Self::DismissedInvalid {
                triage_note: required_triage_field(
                    state,
                    "triage_note",
                    triage_note,
                    &mut missing_field,
                )?,
                triaged_at: required_triage_field(
                    state,
                    "triaged_at",
                    triaged_at,
                    &mut missing_field,
                )?,
            }),
            FindingTriageState::NeedsInvestigation => Ok(Self::NeedsInvestigation {
                triage_note: required_triage_field(
                    state,
                    "triage_note",
                    triage_note,
                    &mut missing_field,
                )?,
                triaged_at: required_triage_field(
                    state,
                    "triaged_at",
                    triaged_at,
                    &mut missing_field,
                )?,
            }),
        }
    }

    #[must_use]
    pub fn state(&self) -> FindingTriageState {
        match self {
            Self::Untriaged => FindingTriageState::Untriaged,
            Self::FixNow { .. } => FindingTriageState::FixNow,
            Self::WontFix { .. } => FindingTriageState::WontFix,
            Self::Backlog { .. } => FindingTriageState::Backlog,
            Self::Duplicate { .. } => FindingTriageState::Duplicate,
            Self::DismissedInvalid { .. } => FindingTriageState::DismissedInvalid,
            Self::NeedsInvestigation { .. } => FindingTriageState::NeedsInvestigation,
        }
    }

    #[must_use]
    pub fn is_unresolved(&self) -> bool {
        self.state().is_unresolved()
    }

    #[must_use]
    pub fn blocks_closure(&self) -> bool {
        self.state().blocks_closure()
    }

    #[must_use]
    pub fn linked_item_id(&self) -> Option<ItemId> {
        match self {
            Self::Backlog { linked_item_id, .. } | Self::Duplicate { linked_item_id, .. } => {
                Some(*linked_item_id)
            }
            _ => None,
        }
    }

    #[must_use]
    pub fn triage_note(&self) -> Option<&str> {
        match self {
            Self::WontFix { triage_note, .. }
            | Self::DismissedInvalid { triage_note, .. }
            | Self::NeedsInvestigation { triage_note, .. } => Some(triage_note.as_str()),
            Self::Backlog { triage_note, .. } | Self::Duplicate { triage_note, .. } => {
                triage_note.as_deref()
            }
            Self::Untriaged | Self::FixNow { .. } => None,
        }
    }

    #[must_use]
    pub fn triaged_at(&self) -> Option<DateTime<Utc>> {
        match self {
            Self::Untriaged => None,
            Self::FixNow { triaged_at }
            | Self::WontFix { triaged_at, .. }
            | Self::Backlog { triaged_at, .. }
            | Self::Duplicate { triaged_at, .. }
            | Self::DismissedInvalid { triaged_at, .. }
            | Self::NeedsInvestigation { triaged_at, .. } => Some(*triaged_at),
        }
    }
}

fn required_triage_field<T, E, F>(
    state: FindingTriageState,
    field: &'static str,
    value: Option<T>,
    missing_field: &mut F,
) -> Result<T, E>
where
    F: FnMut(FindingTriageState, &'static str) -> E,
{
    value.ok_or_else(|| missing_field(state, field))
}

// --- Serde: backward-compatible JSON via FindingWire ---

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FindingWire {
    pub id: FindingId,
    pub project_id: ProjectId,
    pub source_item_id: ItemId,
    pub source_item_revision_id: ItemRevisionId,
    pub source_job_id: JobId,
    pub source_step_id: StepId,
    pub source_report_schema_version: String,
    pub source_finding_key: String,
    pub source_subject_kind: FindingSubjectKind,
    pub source_subject_base_commit_oid: Option<CommitOid>,
    pub source_subject_head_commit_oid: CommitOid,
    pub code: String,
    pub severity: FindingSeverity,
    pub summary: String,
    pub paths: Vec<String>,
    pub evidence: serde_json::Value,
    pub triage_state: FindingTriageState,
    pub linked_item_id: Option<ItemId>,
    pub triage_note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub triaged_at: Option<DateTime<Utc>>,
}

impl TryFrom<FindingWire> for Finding {
    type Error = String;

    fn try_from(w: FindingWire) -> Result<Self, Self::Error> {
        let triage = FindingTriage::try_from_parts(
            w.triage_state,
            w.linked_item_id,
            w.triage_note,
            w.triaged_at,
            |_state, field| format!("finding {field} is required for this triage_state"),
        )?;

        Ok(Finding {
            id: w.id,
            project_id: w.project_id,
            source_item_id: w.source_item_id,
            source_item_revision_id: w.source_item_revision_id,
            source_job_id: w.source_job_id,
            source_step_id: w.source_step_id,
            source_report_schema_version: w.source_report_schema_version,
            source_finding_key: w.source_finding_key,
            source_subject_kind: w.source_subject_kind,
            source_subject_base_commit_oid: w.source_subject_base_commit_oid,
            source_subject_head_commit_oid: w.source_subject_head_commit_oid,
            code: w.code,
            severity: w.severity,
            summary: w.summary,
            paths: w.paths,
            evidence: w.evidence,
            created_at: w.created_at,
            triage,
        })
    }
}

impl From<Finding> for FindingWire {
    fn from(f: Finding) -> Self {
        let triage_state = f.triage.state();
        let linked_item_id = f.triage.linked_item_id();
        let triage_note = f.triage.triage_note().map(ToOwned::to_owned);
        let triaged_at = f.triage.triaged_at();

        FindingWire {
            id: f.id,
            project_id: f.project_id,
            source_item_id: f.source_item_id,
            source_item_revision_id: f.source_item_revision_id,
            source_job_id: f.source_job_id,
            source_step_id: f.source_step_id,
            source_report_schema_version: f.source_report_schema_version,
            source_finding_key: f.source_finding_key,
            source_subject_kind: f.source_subject_kind,
            source_subject_base_commit_oid: f.source_subject_base_commit_oid,
            source_subject_head_commit_oid: f.source_subject_head_commit_oid,
            code: f.code,
            severity: f.severity,
            summary: f.summary,
            paths: f.paths,
            evidence: f.evidence,
            triage_state,
            linked_item_id,
            triage_note,
            created_at: f.created_at,
            triaged_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(try_from = "FindingWire", into = "FindingWire")]
pub struct Finding {
    pub id: FindingId,
    pub project_id: ProjectId,
    pub source_item_id: ItemId,
    pub source_item_revision_id: ItemRevisionId,
    pub source_job_id: JobId,
    pub source_step_id: StepId,
    pub source_report_schema_version: String,
    pub source_finding_key: String,
    pub source_subject_kind: FindingSubjectKind,
    pub source_subject_base_commit_oid: Option<CommitOid>,
    pub source_subject_head_commit_oid: CommitOid,
    pub code: String,
    pub severity: FindingSeverity,
    pub summary: String,
    pub paths: Vec<String>,
    pub evidence: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub triage: FindingTriage,
}

#[cfg(test)]
mod tests {
    use crate::test_support::{FindingBuilder, default_timestamp};

    use super::*;

    fn base_finding(triage: FindingTriage) -> Finding {
        let mut finding = FindingBuilder::new(
            ProjectId::new(),
            ItemId::new(),
            ItemRevisionId::new(),
            JobId::new(),
        )
        .build();
        finding.triage = triage;
        finding
    }

    #[test]
    fn deserialize_rejects_fix_now_without_triaged_at() {
        let finding = base_finding(FindingTriage::FixNow {
            triaged_at: default_timestamp(),
        });
        let mut value = serde_json::to_value(finding).expect("serialize");
        value
            .as_object_mut()
            .unwrap()
            .insert("triaged_at".into(), serde_json::Value::Null);

        let error = serde_json::from_value::<Finding>(value).expect_err("missing triaged_at");
        assert!(
            error.to_string().contains("triaged_at"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn deserialize_rejects_wont_fix_without_triage_note() {
        let finding = base_finding(FindingTriage::WontFix {
            triage_note: "reason".into(),
            triaged_at: default_timestamp(),
        });
        let mut value = serde_json::to_value(finding).expect("serialize");
        value
            .as_object_mut()
            .unwrap()
            .insert("triage_note".into(), serde_json::Value::Null);

        let error = serde_json::from_value::<Finding>(value).expect_err("missing triage_note");
        assert!(
            error.to_string().contains("triage_note"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn deserialize_rejects_backlog_without_linked_item_id() {
        let finding = base_finding(FindingTriage::Backlog {
            linked_item_id: ItemId::new(),
            triage_note: None,
            triaged_at: default_timestamp(),
        });
        let mut value = serde_json::to_value(finding).expect("serialize");
        value
            .as_object_mut()
            .unwrap()
            .insert("linked_item_id".into(), serde_json::Value::Null);

        let error = serde_json::from_value::<Finding>(value).expect_err("missing linked_item_id");
        assert!(
            error.to_string().contains("linked_item_id"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn deserialize_rejects_duplicate_without_linked_item_id() {
        let finding = base_finding(FindingTriage::Duplicate {
            linked_item_id: ItemId::new(),
            triage_note: None,
            triaged_at: default_timestamp(),
        });
        let mut value = serde_json::to_value(finding).expect("serialize");
        value
            .as_object_mut()
            .unwrap()
            .insert("linked_item_id".into(), serde_json::Value::Null);

        let error = serde_json::from_value::<Finding>(value).expect_err("missing linked_item_id");
        assert!(
            error.to_string().contains("linked_item_id"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn deserialize_rejects_needs_investigation_without_triage_note() {
        let finding = base_finding(FindingTriage::NeedsInvestigation {
            triage_note: "investigate".into(),
            triaged_at: default_timestamp(),
        });
        let mut value = serde_json::to_value(finding).expect("serialize");
        value
            .as_object_mut()
            .unwrap()
            .insert("triage_note".into(), serde_json::Value::Null);

        let error = serde_json::from_value::<Finding>(value).expect_err("missing triage_note");
        assert!(
            error.to_string().contains("triage_note"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn round_trip_preserves_all_variants() {
        let item_id = ItemId::new();
        let variants = vec![
            base_finding(FindingTriage::Untriaged),
            base_finding(FindingTriage::FixNow {
                triaged_at: default_timestamp(),
            }),
            base_finding(FindingTriage::WontFix {
                triage_note: "reason".into(),
                triaged_at: default_timestamp(),
            }),
            base_finding(FindingTriage::Backlog {
                linked_item_id: item_id,
                triage_note: Some("note".into()),
                triaged_at: default_timestamp(),
            }),
            base_finding(FindingTriage::Backlog {
                linked_item_id: item_id,
                triage_note: None,
                triaged_at: default_timestamp(),
            }),
            base_finding(FindingTriage::Duplicate {
                linked_item_id: item_id,
                triage_note: None,
                triaged_at: default_timestamp(),
            }),
            base_finding(FindingTriage::DismissedInvalid {
                triage_note: "invalid".into(),
                triaged_at: default_timestamp(),
            }),
            base_finding(FindingTriage::NeedsInvestigation {
                triage_note: "investigate".into(),
                triaged_at: default_timestamp(),
            }),
        ];

        for original in variants {
            let expected_state = original.triage.state();
            let json = serde_json::to_value(&original).expect("serialize");
            let roundtripped: Finding = serde_json::from_value(json).expect("deserialize");
            assert_eq!(roundtripped.triage.state(), expected_state);
            assert_eq!(roundtripped.id, original.id);
            assert_eq!(
                roundtripped.triage.linked_item_id(),
                original.triage.linked_item_id()
            );
            assert_eq!(
                roundtripped.triage.triage_note(),
                original.triage.triage_note()
            );
            assert_eq!(
                roundtripped.triage.triaged_at().is_some(),
                original.triage.triaged_at().is_some()
            );
        }
    }

    #[test]
    fn untriaged_has_no_associated_data() {
        let finding = base_finding(FindingTriage::Untriaged);
        assert_eq!(finding.triage.state(), FindingTriageState::Untriaged);
        assert_eq!(finding.triage.linked_item_id(), None);
        assert_eq!(finding.triage.triage_note(), None);
        assert_eq!(finding.triage.triaged_at(), None);
        assert!(finding.triage.is_unresolved());
        assert!(finding.triage.blocks_closure());
    }
}
