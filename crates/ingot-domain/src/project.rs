use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::branch_name::BranchName;
use crate::finding::FindingSeverity;
use crate::ids::ProjectId;
use crate::job::PhaseKind;

#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "sqlx", sqlx(rename_all = "snake_case"))]
pub enum ExecutionMode {
    #[default]
    Manual,
    Autopilot,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRouting {
    pub author: Option<String>,
    pub review: Option<String>,
    pub investigate: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoTriageDecision {
    FixNow,
    Backlog,
    Skip,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoTriagePolicy {
    pub critical: AutoTriageDecision,
    pub high: AutoTriageDecision,
    pub medium: AutoTriageDecision,
    pub low: AutoTriageDecision,
}

impl Default for AutoTriagePolicy {
    fn default() -> Self {
        Self {
            critical: AutoTriageDecision::FixNow,
            high: AutoTriageDecision::FixNow,
            medium: AutoTriageDecision::FixNow,
            low: AutoTriageDecision::Backlog,
        }
    }
}

impl AutoTriagePolicy {
    pub fn decision_for(&self, severity: FindingSeverity) -> AutoTriageDecision {
        match severity {
            FindingSeverity::Critical => self.critical,
            FindingSeverity::High => self.high,
            FindingSeverity::Medium => self.medium,
            FindingSeverity::Low => self.low,
        }
    }
}

impl AgentRouting {
    #[must_use]
    pub fn preferred_slug(&self, phase_kind: PhaseKind) -> Option<&str> {
        match phase_kind {
            PhaseKind::Author => self.author.as_deref(),
            PhaseKind::Review => self.review.as_deref(),
            PhaseKind::Investigate => self.investigate.as_deref(),
            PhaseKind::Validate | PhaseKind::System => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: ProjectId,
    pub name: String,
    pub path: PathBuf,
    pub default_branch: BranchName,
    pub color: String,
    pub execution_mode: ExecutionMode,
    pub agent_routing: Option<AgentRouting>,
    pub auto_triage_policy: Option<AutoTriagePolicy>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferred_slug_returns_matching_phase() {
        let routing = AgentRouting {
            author: Some("claude-code".into()),
            review: Some("codex".into()),
            investigate: None,
        };
        assert_eq!(
            routing.preferred_slug(PhaseKind::Author),
            Some("claude-code")
        );
        assert_eq!(routing.preferred_slug(PhaseKind::Review), Some("codex"));
        assert_eq!(routing.preferred_slug(PhaseKind::Investigate), None);
    }

    #[test]
    fn preferred_slug_returns_none_for_validate_and_system() {
        let routing = AgentRouting {
            author: Some("claude-code".into()),
            review: Some("codex".into()),
            investigate: Some("claude-code".into()),
        };
        assert_eq!(routing.preferred_slug(PhaseKind::Validate), None);
        assert_eq!(routing.preferred_slug(PhaseKind::System), None);
    }

    #[test]
    fn default_routing_has_no_preferences() {
        let routing = AgentRouting::default();
        assert_eq!(routing.preferred_slug(PhaseKind::Author), None);
        assert_eq!(routing.preferred_slug(PhaseKind::Review), None);
        assert_eq!(routing.preferred_slug(PhaseKind::Investigate), None);
    }
}
