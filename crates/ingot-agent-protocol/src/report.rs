//! Canonical agent report contract: schema versions, JSON schemas, prompt
//! suffixes, outcome parsing, and v1 deserialization types.
//!
//! Every report-producing step in the delivery workflow references this module
//! as the single source of truth for the wire contract between the daemon and
//! the agent subprocess.

use ingot_domain::commit_oid::CommitOid;
use ingot_domain::finding::FindingSeverity;
use ingot_domain::job::{OutcomeClass, OutputArtifactKind};
use serde::Deserialize;

// ── Schema version constants ────────────────────────────────────────────────

pub const VALIDATION_REPORT_V1: &str = "validation_report:v1";
pub const REVIEW_REPORT_V1: &str = "review_report:v1";
pub const FINDING_REPORT_V1: &str = "finding_report:v1";
pub const INVESTIGATION_REPORT_V1: &str = "investigation_report:v1";

// ── Schema version lookup ───────────────────────────────────────────────────

/// Map an output-artifact kind to its schema-version string, if the kind
/// produces a report.
pub fn schema_version(kind: OutputArtifactKind) -> Option<&'static str> {
    match kind {
        OutputArtifactKind::ValidationReport => Some(VALIDATION_REPORT_V1),
        OutputArtifactKind::ReviewReport => Some(REVIEW_REPORT_V1),
        OutputArtifactKind::FindingReport => Some(FINDING_REPORT_V1),
        OutputArtifactKind::InvestigationReport => Some(INVESTIGATION_REPORT_V1),
        _ => None,
    }
}

// ── Output JSON schemas ─────────────────────────────────────────────────────

/// Return the JSON Schema that the agent must conform to for the given
/// output-artifact kind.
pub fn output_schema(kind: OutputArtifactKind) -> Option<serde_json::Value> {
    match kind {
        OutputArtifactKind::Commit => Some(commit_summary_schema()),
        OutputArtifactKind::ValidationReport => Some(validation_report_schema()),
        OutputArtifactKind::ReviewReport => Some(review_report_schema()),
        OutputArtifactKind::FindingReport => Some(finding_report_schema()),
        OutputArtifactKind::InvestigationReport => Some(investigation_report_schema()),
        OutputArtifactKind::None => None,
    }
}

pub fn commit_summary_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "summary": { "type": "string" },
            "validation": {
                "type": ["string", "null"]
            }
        },
        "required": ["summary", "validation"],
        "additionalProperties": false
    })
}

pub fn commit_summary_payload(summary: &str, validation: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "summary": summary,
        "validation": validation
    })
}

pub fn finding_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "finding_key": { "type": "string" },
            "code": { "type": "string" },
            "severity": { "type": "string", "enum": ["low", "medium", "high", "critical"] },
            "summary": { "type": "string" },
            "paths": {
                "type": "array",
                "items": { "type": "string" }
            },
            "evidence": {
                "type": "array",
                "items": { "type": "string" }
            }
        },
        "required": ["finding_key", "code", "severity", "summary", "paths", "evidence"],
        "additionalProperties": false
    })
}

pub fn nullable_closed_extensions_schema() -> serde_json::Value {
    serde_json::json!({
        "anyOf": [
            {
                "type": "object",
                "additionalProperties": false
            },
            {
                "type": "null"
            }
        ]
    })
}

pub fn clean_validation_report_payload(summary: &str) -> serde_json::Value {
    serde_json::json!({
        "outcome": "clean",
        "summary": summary,
        "checks": [],
        "findings": [],
        "extensions": null
    })
}

pub fn clean_review_report_payload(
    base_commit_oid: &str,
    head_commit_oid: &str,
) -> serde_json::Value {
    serde_json::json!({
        "outcome": "clean",
        "summary": "No issues found",
        "review_subject": {
            "base_commit_oid": base_commit_oid,
            "head_commit_oid": head_commit_oid
        },
        "overall_risk": "low",
        "findings": [],
        "extensions": null
    })
}

pub fn findings_review_report_payload(
    base_commit_oid: &str,
    head_commit_oid: &str,
    summary: &str,
    overall_risk: &str,
    findings: Vec<serde_json::Value>,
) -> serde_json::Value {
    serde_json::json!({
        "outcome": "findings",
        "summary": summary,
        "review_subject": {
            "base_commit_oid": base_commit_oid,
            "head_commit_oid": head_commit_oid
        },
        "overall_risk": overall_risk,
        "findings": findings,
        "extensions": null
    })
}

pub fn validation_report_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "outcome": { "type": "string", "enum": ["clean", "findings"] },
            "summary": { "type": "string" },
            "checks": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "status": { "type": "string", "enum": ["pass", "fail", "skip"] },
                        "summary": { "type": "string" }
                    },
                    "required": ["name", "status", "summary"],
                    "additionalProperties": false
                }
            },
            "findings": {
                "type": "array",
                "items": finding_schema()
            },
            "extensions": nullable_closed_extensions_schema()
        },
        "required": ["outcome", "summary", "checks", "findings", "extensions"],
        "additionalProperties": false
    })
}

pub fn review_report_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "outcome": { "type": "string", "enum": ["clean", "findings"] },
            "summary": { "type": "string" },
            "review_subject": {
                "type": "object",
                "properties": {
                    "base_commit_oid": { "type": "string" },
                    "head_commit_oid": { "type": "string" }
                },
                "required": ["base_commit_oid", "head_commit_oid"],
                "additionalProperties": false
            },
            "overall_risk": { "type": "string", "enum": ["low", "medium", "high"] },
            "findings": {
                "type": "array",
                "items": finding_schema()
            },
            "extensions": nullable_closed_extensions_schema()
        },
        "required": ["outcome", "summary", "review_subject", "overall_risk", "findings", "extensions"],
        "additionalProperties": false
    })
}

pub fn finding_report_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "outcome": { "type": "string", "enum": ["clean", "findings"] },
            "summary": { "type": "string" },
            "findings": {
                "type": "array",
                "items": finding_schema()
            },
            "extensions": nullable_closed_extensions_schema()
        },
        "required": ["outcome", "summary", "findings", "extensions"],
        "additionalProperties": false
    })
}

pub fn investigation_report_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "outcome": { "type": "string", "enum": ["clean", "findings"] },
            "summary": { "type": "string" },
            "scope": {
                "type": "object",
                "properties": {
                    "description": { "type": "string" },
                    "paths_examined": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "methodology": { "type": "string" }
                },
                "required": ["description", "paths_examined", "methodology"],
                "additionalProperties": false
            },
            "findings": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "finding_key": { "type": "string" },
                        "code": { "type": "string" },
                        "severity": { "type": "string", "enum": ["low", "medium", "high", "critical"] },
                        "summary": { "type": "string" },
                        "paths": {
                            "type": "array",
                            "items": { "type": "string" }
                        },
                        "evidence": {
                            "type": "array",
                            "items": { "type": "string" }
                        },
                        "promotion": {
                            "type": "object",
                            "properties": {
                                "title": { "type": "string", "maxLength": 120 },
                                "description": { "type": "string" },
                                "acceptance_criteria": { "type": "string" },
                                "classification": { "type": "string", "enum": ["change", "bug"] },
                                "estimated_scope": { "type": "string", "enum": ["small", "medium", "large"] }
                            },
                            "required": ["title", "description", "acceptance_criteria", "classification", "estimated_scope"],
                            "additionalProperties": false
                        },
                        "group_key": { "type": ["string", "null"] }
                    },
                    "required": ["finding_key", "code", "severity", "summary", "paths", "evidence", "promotion"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["outcome", "summary", "scope", "findings"],
        "additionalProperties": false
    })
}

// ── Prompt suffix ───────────────────────────────────────────────────────────

/// Instruction text appended to the agent prompt for report-producing jobs.
pub fn prompt_suffix(kind: OutputArtifactKind) -> &'static str {
    match kind {
        OutputArtifactKind::ValidationReport => {
            "Return JSON matching `validation_report:v1` with keys `outcome`, `summary`, `checks`, `findings`, and `extensions`. Set `extensions` to null when unused. Use `outcome=clean` only when there are no failed checks and no findings."
        }
        OutputArtifactKind::ReviewReport => {
            "Return JSON matching `review_report:v1` with keys `outcome`, `summary`, `review_subject`, `overall_risk`, `findings`, and `extensions`. Set `extensions` to null when unused. The `review_subject.base_commit_oid` and `review_subject.head_commit_oid` must exactly match the provided input commits."
        }
        OutputArtifactKind::FindingReport => {
            "Return JSON matching `finding_report:v1` with keys `outcome`, `summary`, `findings`, and `extensions`. Set `extensions` to null when unused."
        }
        OutputArtifactKind::InvestigationReport => {
            "Return JSON matching `investigation_report:v1` with keys `outcome`, `summary`, `scope`, and `findings`. The `scope` object must include `description`, `paths_examined`, and `methodology`. Each finding must include a `promotion` object with `title`, `description`, `acceptance_criteria`, `classification` (\"change\" or \"bug\"), and `estimated_scope` (\"small\", \"medium\", or \"large\"). Use `outcome=clean` only when there are no findings."
        }
        _ => "",
    }
}

// ── Outcome parsing ─────────────────────────────────────────────────────────

/// Parse the `outcome` field from a report payload into an `OutcomeClass`.
pub fn parse_outcome_class(result_payload: &serde_json::Value) -> Result<OutcomeClass, String> {
    match result_payload
        .get("outcome")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
    {
        "clean" => Ok(OutcomeClass::Clean),
        "findings" => Ok(OutcomeClass::Findings),
        other => Err(format!("unsupported report outcome `{other}`")),
    }
}

// ── v1 deserialization types ────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct FindingV1 {
    pub finding_key: String,
    pub code: String,
    pub severity: FindingSeverity,
    pub summary: String,
    pub paths: Vec<String>,
    pub evidence: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct ValidationReportV1 {
    pub outcome: String,
    pub summary: String,
    pub checks: Vec<ValidationCheckV1>,
    pub findings: Vec<FindingV1>,
}

#[derive(Debug, Deserialize)]
pub struct ReviewSubjectV1 {
    pub base_commit_oid: CommitOid,
    pub head_commit_oid: CommitOid,
}

#[derive(Debug, Deserialize)]
pub struct ReviewReportV1 {
    pub outcome: String,
    pub summary: String,
    pub review_subject: ReviewSubjectV1,
    pub overall_risk: ReviewOverallRisk,
    pub findings: Vec<FindingV1>,
}

#[derive(Debug, Deserialize)]
pub struct FindingReportV1 {
    pub outcome: String,
    pub summary: String,
    pub findings: Vec<FindingV1>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ValidationCheckV1 {
    pub name: String,
    pub status: ValidationCheckStatus,
    pub summary: String,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ValidationCheckStatus {
    Pass,
    Fail,
    Skip,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewOverallRisk {
    Low,
    Medium,
    High,
}

// ── Investigation report v1 types ──────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct InvestigationReportV1 {
    pub outcome: String,
    pub summary: String,
    pub scope: InvestigationScopeV1,
    pub findings: Vec<InvestigationFindingV1>,
}

#[derive(Debug, Deserialize)]
pub struct InvestigationScopeV1 {
    pub description: String,
    pub paths_examined: Vec<String>,
    pub methodology: String,
}

#[derive(Debug, Deserialize)]
pub struct InvestigationFindingV1 {
    pub finding_key: String,
    pub code: String,
    pub severity: FindingSeverity,
    pub summary: String,
    pub paths: Vec<String>,
    pub evidence: Vec<String>,
    pub promotion: InvestigationPromotionV1,
    pub group_key: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct InvestigationPromotionV1 {
    pub title: String,
    pub description: String,
    pub acceptance_criteria: String,
    pub classification: InvestigationClassification,
    pub estimated_scope: InvestigationEstimatedScope,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvestigationClassification {
    Change,
    Bug,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvestigationEstimatedScope {
    Small,
    Medium,
    Large,
}
