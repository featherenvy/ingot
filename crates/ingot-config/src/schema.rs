use ingot_domain::revision::ApprovalPolicy;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IngotConfig {
    #[serde(default)]
    pub defaults: DefaultsConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefaultsConfig {
    pub candidate_rework_budget: u32,
    pub integration_rework_budget: u32,
    pub approval_policy: ApprovalPolicy,
    pub overflow_strategy: String,
}

impl Default for DefaultsConfig {
    fn default() -> Self {
        Self {
            candidate_rework_budget: 2,
            integration_rework_budget: 2,
            approval_policy: ApprovalPolicy::Required,
            overflow_strategy: "truncate".into(),
        }
    }
}
