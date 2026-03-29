use std::fmt;

use ingot_domain::step_id::{ParseStepIdError, StepId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecommendedAction {
    None,
    Named(NamedRecommendedAction),
    /// Catch-all: any step ID that doesn't match a named action above.
    /// `From<String>` routes unrecognized strings here.
    DispatchStep(StepId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NamedRecommendedAction {
    ApprovalApprove,
    OperatorIntervention,
    FinalizePreparedConvergence,
    InvalidatePreparedConvergence,
    TriageFindings,
    PrepareConvergence,
    AwaitConvergenceLane,
    ResolveCheckoutSync,
}

impl NamedRecommendedAction {
    #[cfg(test)]
    const ALL: [Self; 8] = [
        Self::ApprovalApprove,
        Self::OperatorIntervention,
        Self::FinalizePreparedConvergence,
        Self::InvalidatePreparedConvergence,
        Self::TriageFindings,
        Self::PrepareConvergence,
        Self::AwaitConvergenceLane,
        Self::ResolveCheckoutSync,
    ];

    fn from_step_alias(step: StepId) -> Option<Self> {
        match step {
            StepId::PrepareConvergence => Some(Self::PrepareConvergence),
            _ => None,
        }
    }

    fn is_daemon_owned(self) -> bool {
        matches!(
            self,
            Self::PrepareConvergence
                | Self::FinalizePreparedConvergence
                | Self::InvalidatePreparedConvergence
        )
    }

    fn parse(action: &str) -> Option<Self> {
        match action {
            "approval_approve" => Some(Self::ApprovalApprove),
            "operator_intervention" => Some(Self::OperatorIntervention),
            "finalize_prepared_convergence" => Some(Self::FinalizePreparedConvergence),
            "invalidate_prepared_convergence" => Some(Self::InvalidatePreparedConvergence),
            "triage_findings" => Some(Self::TriageFindings),
            "prepare_convergence" => Some(Self::PrepareConvergence),
            "await_convergence_lane" => Some(Self::AwaitConvergenceLane),
            "resolve_checkout_sync" => Some(Self::ResolveCheckoutSync),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::ApprovalApprove => "approval_approve",
            Self::OperatorIntervention => "operator_intervention",
            Self::FinalizePreparedConvergence => "finalize_prepared_convergence",
            Self::InvalidatePreparedConvergence => "invalidate_prepared_convergence",
            Self::TriageFindings => "triage_findings",
            Self::PrepareConvergence => "prepare_convergence",
            Self::AwaitConvergenceLane => "await_convergence_lane",
            Self::ResolveCheckoutSync => "resolve_checkout_sync",
        }
    }
}

impl RecommendedAction {
    pub fn dispatch(step: StepId) -> Self {
        Self::DispatchStep(step)
    }

    pub fn named(action: NamedRecommendedAction) -> Self {
        Self::Named(action)
    }

    pub(crate) fn from_step(step: StepId) -> Self {
        NamedRecommendedAction::from_step_alias(step)
            .map(Self::named)
            .unwrap_or_else(|| Self::DispatchStep(step))
    }

    pub(crate) fn system_action(action: &str) -> Result<Self, String> {
        NamedRecommendedAction::parse(action)
            .map(Self::named)
            .ok_or_else(|| format!("unknown internal recommended action: {action}"))
    }

    pub(crate) fn is_daemon_owned(self) -> bool {
        match self {
            Self::Named(action) => action.is_daemon_owned(),
            _ => false,
        }
    }

    fn parse(action: &str) -> Result<Self, String> {
        if action == "none" {
            return Ok(Self::None);
        }

        if let Some(action) = NamedRecommendedAction::parse(action) {
            return Ok(Self::named(action));
        }

        action
            .parse()
            .map(Self::DispatchStep)
            .map_err(|error: ParseStepIdError| error.to_string())
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Named(action) => action.as_str(),
            Self::DispatchStep(step_id) => step_id.as_str(),
        }
    }
}

impl fmt::Display for RecommendedAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str((*self).as_str())
    }
}

impl From<RecommendedAction> for String {
    fn from(action: RecommendedAction) -> Self {
        action.as_str().to_owned()
    }
}

impl serde::Serialize for RecommendedAction {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str((*self).as_str())
    }
}

impl<'de> serde::Deserialize<'de> for RecommendedAction {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Self::parse(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use ingot_domain::step_id::StepId;

    use super::{NamedRecommendedAction, RecommendedAction};

    #[test]
    fn recommended_actions_round_trip_named_and_step_actions() {
        for named_action in NamedRecommendedAction::ALL {
            assert_eq!(
                RecommendedAction::parse(named_action.as_str()).expect("named action"),
                RecommendedAction::named(named_action)
            );
            assert_eq!(
                RecommendedAction::named(named_action).as_str(),
                named_action.as_str()
            );
        }

        assert_eq!(
            RecommendedAction::parse(StepId::ReviewIncrementalInitial.as_str())
                .expect("step action"),
            RecommendedAction::dispatch(StepId::ReviewIncrementalInitial)
        );
    }

    #[test]
    fn prepare_convergence_step_uses_named_action_metadata() {
        assert_eq!(
            RecommendedAction::from_step(StepId::PrepareConvergence),
            RecommendedAction::named(NamedRecommendedAction::PrepareConvergence)
        );
        assert!(
            RecommendedAction::named(NamedRecommendedAction::PrepareConvergence).is_daemon_owned()
        );
    }
}
