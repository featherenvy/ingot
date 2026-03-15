use chrono::{DateTime, Utc};
use ingot_domain::finding::{Finding, FindingSeverity, FindingSubjectKind, FindingTriageState};
use ingot_domain::ids;
use serde_json::json;

use super::timestamps::default_timestamp;

pub struct FindingBuilder {
    id: ids::FindingId,
    project_id: ids::ProjectId,
    source_item_id: ids::ItemId,
    source_item_revision_id: ids::ItemRevisionId,
    source_job_id: ids::JobId,
    source_step_id: String,
    source_report_schema_version: String,
    source_finding_key: String,
    source_subject_kind: FindingSubjectKind,
    source_subject_base_commit_oid: Option<String>,
    source_subject_head_commit_oid: String,
    code: String,
    severity: FindingSeverity,
    summary: String,
    paths: Vec<String>,
    evidence: serde_json::Value,
    triage_state: FindingTriageState,
    linked_item_id: Option<ids::ItemId>,
    triage_note: Option<String>,
    created_at: DateTime<Utc>,
    triaged_at: Option<DateTime<Utc>>,
}

impl FindingBuilder {
    pub fn new(
        project_id: ids::ProjectId,
        item_id: ids::ItemId,
        revision_id: ids::ItemRevisionId,
        job_id: ids::JobId,
    ) -> Self {
        Self {
            id: ids::FindingId::new(),
            project_id,
            source_item_id: item_id,
            source_item_revision_id: revision_id,
            source_job_id: job_id,
            source_step_id: "review_candidate_initial".into(),
            source_report_schema_version: "review_report:v1".into(),
            source_finding_key: "f-1".into(),
            source_subject_kind: FindingSubjectKind::Candidate,
            source_subject_base_commit_oid: Some("base".into()),
            source_subject_head_commit_oid: "head".into(),
            code: "BUG001".into(),
            severity: FindingSeverity::High,
            summary: "summary".into(),
            paths: vec!["src/lib.rs".into()],
            evidence: json!(["evidence"]),
            triage_state: FindingTriageState::Untriaged,
            linked_item_id: None,
            triage_note: None,
            created_at: default_timestamp(),
            triaged_at: None,
        }
    }

    pub fn id(mut self, id: ids::FindingId) -> Self {
        self.id = id;
        self
    }

    pub fn source_step_id(mut self, step_id: impl Into<String>) -> Self {
        self.source_step_id = step_id.into();
        self
    }

    pub fn source_report_schema_version(mut self, version: impl Into<String>) -> Self {
        self.source_report_schema_version = version.into();
        self
    }

    pub fn source_finding_key(mut self, key: impl Into<String>) -> Self {
        self.source_finding_key = key.into();
        self
    }

    pub fn source_subject_kind(mut self, kind: FindingSubjectKind) -> Self {
        self.source_subject_kind = kind;
        self
    }

    pub fn source_subject_base_commit_oid(mut self, oid: Option<impl Into<String>>) -> Self {
        self.source_subject_base_commit_oid = oid.map(Into::into);
        self
    }

    pub fn source_subject_head_commit_oid(mut self, oid: impl Into<String>) -> Self {
        self.source_subject_head_commit_oid = oid.into();
        self
    }

    pub fn code(mut self, code: impl Into<String>) -> Self {
        self.code = code.into();
        self
    }

    pub fn severity(mut self, severity: FindingSeverity) -> Self {
        self.severity = severity;
        self
    }

    pub fn summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = summary.into();
        self
    }

    pub fn paths(mut self, paths: Vec<String>) -> Self {
        self.paths = paths;
        self
    }

    pub fn evidence(mut self, evidence: serde_json::Value) -> Self {
        self.evidence = evidence;
        self
    }

    pub fn triage_state(mut self, triage_state: FindingTriageState) -> Self {
        self.triage_state = triage_state;
        self
    }

    pub fn linked_item_id(mut self, item_id: ids::ItemId) -> Self {
        self.linked_item_id = Some(item_id);
        self
    }

    pub fn triage_note(mut self, note: impl Into<String>) -> Self {
        self.triage_note = Some(note.into());
        self
    }

    pub fn created_at(mut self, created_at: DateTime<Utc>) -> Self {
        self.created_at = created_at;
        self
    }

    pub fn triaged_at(mut self, triaged_at: DateTime<Utc>) -> Self {
        self.triaged_at = Some(triaged_at);
        self
    }

    pub fn build(self) -> Finding {
        Finding {
            id: self.id,
            project_id: self.project_id,
            source_item_id: self.source_item_id,
            source_item_revision_id: self.source_item_revision_id,
            source_job_id: self.source_job_id,
            source_step_id: self.source_step_id,
            source_report_schema_version: self.source_report_schema_version,
            source_finding_key: self.source_finding_key,
            source_subject_kind: self.source_subject_kind,
            source_subject_base_commit_oid: self.source_subject_base_commit_oid,
            source_subject_head_commit_oid: self.source_subject_head_commit_oid,
            code: self.code,
            severity: self.severity,
            summary: self.summary,
            paths: self.paths,
            evidence: self.evidence,
            triage_state: self.triage_state,
            linked_item_id: self.linked_item_id,
            triage_note: self.triage_note,
            created_at: self.created_at,
            triaged_at: self.triaged_at,
        }
    }
}
