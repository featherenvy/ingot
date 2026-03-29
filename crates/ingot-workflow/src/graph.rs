use ingot_domain::job::OutcomeClass;
use ingot_domain::step_id::StepId;

/// Represents a transition edge in the workflow graph.
#[derive(Debug, Clone)]
pub struct Transition {
    pub from_step: StepId,
    pub outcome: TransitionOutcome,
    pub to: TransitionTarget,
}

impl Transition {
    const fn step(from_step: StepId, outcome: TransitionOutcome, to_step: StepId) -> Self {
        Self {
            from_step,
            outcome,
            to: TransitionTarget::Step(to_step),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransitionOutcome {
    Clean,
    Findings,
}

impl TransitionOutcome {
    fn from_job_outcome(outcome: &OutcomeClass) -> Option<Self> {
        match outcome {
            OutcomeClass::Clean => Some(Self::Clean),
            OutcomeClass::Findings => Some(Self::Findings),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransitionTarget {
    Step(StepId),
    SystemAction(&'static str),
    Escalation(&'static str),
}

pub struct WorkflowGraph {
    transitions: Vec<Transition>,
}

impl WorkflowGraph {
    /// Build the delivery:v1 workflow graph.
    pub fn delivery_v1() -> Self {
        Self {
            transitions: vec![
                // author_initial -> review_incremental_initial
                Transition::step(
                    StepId::AuthorInitial,
                    TransitionOutcome::Clean,
                    StepId::ReviewIncrementalInitial,
                ),
                // review_incremental_initial
                Transition::step(
                    StepId::ReviewIncrementalInitial,
                    TransitionOutcome::Clean,
                    StepId::ReviewCandidateInitial,
                ),
                Transition::step(
                    StepId::ReviewIncrementalInitial,
                    TransitionOutcome::Findings,
                    StepId::RepairCandidate,
                ),
                // review_candidate_initial
                Transition::step(
                    StepId::ReviewCandidateInitial,
                    TransitionOutcome::Clean,
                    StepId::ValidateCandidateInitial,
                ),
                Transition::step(
                    StepId::ReviewCandidateInitial,
                    TransitionOutcome::Findings,
                    StepId::RepairCandidate,
                ),
                // validate_candidate_initial
                Transition::step(
                    StepId::ValidateCandidateInitial,
                    TransitionOutcome::Clean,
                    StepId::PrepareConvergence,
                ),
                Transition::step(
                    StepId::ValidateCandidateInitial,
                    TransitionOutcome::Findings,
                    StepId::RepairCandidate,
                ),
                // repair_candidate -> review_incremental_repair
                Transition::step(
                    StepId::RepairCandidate,
                    TransitionOutcome::Clean,
                    StepId::ReviewIncrementalRepair,
                ),
                Transition::step(
                    StepId::ReviewIncrementalRepair,
                    TransitionOutcome::Clean,
                    StepId::ReviewCandidateRepair,
                ),
                Transition::step(
                    StepId::ReviewIncrementalRepair,
                    TransitionOutcome::Findings,
                    StepId::RepairCandidate,
                ),
                // review_candidate_repair
                Transition::step(
                    StepId::ReviewCandidateRepair,
                    TransitionOutcome::Clean,
                    StepId::ValidateCandidateRepair,
                ),
                Transition::step(
                    StepId::ReviewCandidateRepair,
                    TransitionOutcome::Findings,
                    StepId::RepairCandidate,
                ),
                // validate_candidate_repair
                Transition::step(
                    StepId::ValidateCandidateRepair,
                    TransitionOutcome::Clean,
                    StepId::PrepareConvergence,
                ),
                Transition::step(
                    StepId::ValidateCandidateRepair,
                    TransitionOutcome::Findings,
                    StepId::RepairCandidate,
                ),
                // validate_integrated - clean goes to approval gate (handled by evaluator)
                Transition::step(
                    StepId::ValidateIntegrated,
                    TransitionOutcome::Findings,
                    StepId::RepairAfterIntegration,
                ),
                // repair_after_integration
                Transition::step(
                    StepId::RepairAfterIntegration,
                    TransitionOutcome::Clean,
                    StepId::ReviewIncrementalAfterIntegrationRepair,
                ),
                Transition::step(
                    StepId::ReviewIncrementalAfterIntegrationRepair,
                    TransitionOutcome::Clean,
                    StepId::ReviewAfterIntegrationRepair,
                ),
                Transition::step(
                    StepId::ReviewIncrementalAfterIntegrationRepair,
                    TransitionOutcome::Findings,
                    StepId::RepairAfterIntegration,
                ),
                // review_after_integration_repair
                Transition::step(
                    StepId::ReviewAfterIntegrationRepair,
                    TransitionOutcome::Clean,
                    StepId::ValidateAfterIntegrationRepair,
                ),
                Transition::step(
                    StepId::ReviewAfterIntegrationRepair,
                    TransitionOutcome::Findings,
                    StepId::RepairAfterIntegration,
                ),
                // validate_after_integration_repair
                Transition::step(
                    StepId::ValidateAfterIntegrationRepair,
                    TransitionOutcome::Clean,
                    StepId::PrepareConvergence,
                ),
                Transition::step(
                    StepId::ValidateAfterIntegrationRepair,
                    TransitionOutcome::Findings,
                    StepId::RepairAfterIntegration,
                ),
            ],
        }
    }

    /// Build the investigation:v1 workflow graph.
    pub fn investigation_v1() -> Self {
        Self {
            transitions: vec![
                Transition::step(
                    StepId::InvestigateProject,
                    TransitionOutcome::Findings,
                    StepId::ReinvestigateProject,
                ),
                Transition::step(
                    StepId::ReinvestigateProject,
                    TransitionOutcome::Findings,
                    StepId::ReinvestigateProject,
                ),
            ],
        }
    }

    /// Find the next step given a completed step and its outcome.
    pub fn next_step(
        &self,
        from_step: StepId,
        outcome: &OutcomeClass,
    ) -> Option<&TransitionTarget> {
        let transition_outcome = TransitionOutcome::from_job_outcome(outcome)?;

        self.transitions
            .iter()
            .find(|t| t.from_step == from_step && t.outcome == transition_outcome)
            .map(|t| &t.to)
    }
}
