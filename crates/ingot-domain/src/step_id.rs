use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Closed set of workflow step identifiers used across the delivery workflow.
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "sqlx", sqlx(rename_all = "snake_case"))]
pub enum StepId {
    AuthorInitial,
    ReviewIncrementalInitial,
    ReviewCandidateInitial,
    ValidateCandidateInitial,
    RepairCandidate,
    ReviewIncrementalRepair,
    ReviewCandidateRepair,
    ValidateCandidateRepair,
    InvestigateItem,
    PrepareConvergence,
    ValidateIntegrated,
    RepairAfterIntegration,
    ReviewIncrementalAfterIntegrationRepair,
    ReviewAfterIntegrationRepair,
    ValidateAfterIntegrationRepair,
    InvestigateProject,
    ReinvestigateProject,
}

impl StepId {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AuthorInitial => "author_initial",
            Self::ReviewIncrementalInitial => "review_incremental_initial",
            Self::ReviewCandidateInitial => "review_candidate_initial",
            Self::ValidateCandidateInitial => "validate_candidate_initial",
            Self::RepairCandidate => "repair_candidate",
            Self::ReviewIncrementalRepair => "review_incremental_repair",
            Self::ReviewCandidateRepair => "review_candidate_repair",
            Self::ValidateCandidateRepair => "validate_candidate_repair",
            Self::InvestigateItem => "investigate_item",
            Self::PrepareConvergence => "prepare_convergence",
            Self::ValidateIntegrated => "validate_integrated",
            Self::RepairAfterIntegration => "repair_after_integration",
            Self::ReviewIncrementalAfterIntegrationRepair => {
                "review_incremental_after_integration_repair"
            }
            Self::ReviewAfterIntegrationRepair => "review_after_integration_repair",
            Self::ValidateAfterIntegrationRepair => "validate_after_integration_repair",
            Self::InvestigateProject => "investigate_project",
            Self::ReinvestigateProject => "reinvestigate_project",
        }
    }
}

impl fmt::Display for StepId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl AsRef<str> for StepId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl From<StepId> for String {
    fn from(step_id: StepId) -> Self {
        step_id.as_str().to_owned()
    }
}

impl TryFrom<&str> for StepId {
    type Error = ParseStepIdError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl TryFrom<String> for StepId {
    type Error = ParseStepIdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl FromStr for StepId {
    type Err = ParseStepIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "author_initial" => Ok(Self::AuthorInitial),
            "review_incremental_initial" => Ok(Self::ReviewIncrementalInitial),
            "review_candidate_initial" => Ok(Self::ReviewCandidateInitial),
            "validate_candidate_initial" => Ok(Self::ValidateCandidateInitial),
            "repair_candidate" => Ok(Self::RepairCandidate),
            "review_incremental_repair" => Ok(Self::ReviewIncrementalRepair),
            "review_candidate_repair" => Ok(Self::ReviewCandidateRepair),
            "validate_candidate_repair" => Ok(Self::ValidateCandidateRepair),
            "investigate_item" => Ok(Self::InvestigateItem),
            "prepare_convergence" => Ok(Self::PrepareConvergence),
            "validate_integrated" => Ok(Self::ValidateIntegrated),
            "repair_after_integration" => Ok(Self::RepairAfterIntegration),
            "review_incremental_after_integration_repair" => {
                Ok(Self::ReviewIncrementalAfterIntegrationRepair)
            }
            "review_after_integration_repair" => Ok(Self::ReviewAfterIntegrationRepair),
            "validate_after_integration_repair" => Ok(Self::ValidateAfterIntegrationRepair),
            "investigate_project" => Ok(Self::InvestigateProject),
            "reinvestigate_project" => Ok(Self::ReinvestigateProject),
            _ => Err(ParseStepIdError {
                value: value.to_owned(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown workflow step id: {value}")]
pub struct ParseStepIdError {
    value: String,
}

#[cfg(test)]
mod tests {
    use super::StepId;

    #[test]
    fn serde_round_trips_known_step_ids() {
        let serialized =
            serde_json::to_string(&StepId::ReviewIncrementalAfterIntegrationRepair).expect("json");
        assert_eq!(
            serialized,
            "\"review_incremental_after_integration_repair\""
        );

        let deserialized: StepId =
            serde_json::from_str("\"validate_integrated\"").expect("deserialize");
        assert_eq!(deserialized, StepId::ValidateIntegrated);
    }

    #[test]
    fn serde_rejects_unknown_step_ids() {
        let error =
            serde_json::from_str::<StepId>("\"not_a_real_step\"").expect_err("invalid step");

        assert!(error.to_string().contains("unknown variant"));
    }

    #[test]
    fn from_str_rejects_unknown_step_ids() {
        let error = "not_a_real_step"
            .parse::<StepId>()
            .expect_err("invalid step");
        assert_eq!(
            error.to_string(),
            "unknown workflow step id: not_a_real_step"
        );
    }
}
