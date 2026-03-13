use ingot_domain::job::OutcomeClass;

use crate::step::*;

/// Represents a transition edge in the workflow graph.
#[derive(Debug, Clone)]
pub struct Transition {
    pub from_step: StepId,
    pub outcome: TransitionOutcome,
    pub to: TransitionTarget,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransitionOutcome {
    Clean,
    Findings,
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
                Transition {
                    from_step: AUTHOR_INITIAL,
                    outcome: TransitionOutcome::Clean,
                    to: TransitionTarget::Step(REVIEW_INCREMENTAL_INITIAL),
                },
                // review_incremental_initial
                Transition {
                    from_step: REVIEW_INCREMENTAL_INITIAL,
                    outcome: TransitionOutcome::Clean,
                    to: TransitionTarget::Step(REVIEW_CANDIDATE_INITIAL),
                },
                Transition {
                    from_step: REVIEW_INCREMENTAL_INITIAL,
                    outcome: TransitionOutcome::Findings,
                    to: TransitionTarget::Step(REPAIR_CANDIDATE),
                },
                // review_candidate_initial
                Transition {
                    from_step: REVIEW_CANDIDATE_INITIAL,
                    outcome: TransitionOutcome::Clean,
                    to: TransitionTarget::Step(VALIDATE_CANDIDATE_INITIAL),
                },
                Transition {
                    from_step: REVIEW_CANDIDATE_INITIAL,
                    outcome: TransitionOutcome::Findings,
                    to: TransitionTarget::Step(REPAIR_CANDIDATE),
                },
                // validate_candidate_initial
                Transition {
                    from_step: VALIDATE_CANDIDATE_INITIAL,
                    outcome: TransitionOutcome::Clean,
                    to: TransitionTarget::Step(PREPARE_CONVERGENCE),
                },
                Transition {
                    from_step: VALIDATE_CANDIDATE_INITIAL,
                    outcome: TransitionOutcome::Findings,
                    to: TransitionTarget::Step(REPAIR_CANDIDATE),
                },
                // repair_candidate -> review_incremental_repair
                Transition {
                    from_step: REPAIR_CANDIDATE,
                    outcome: TransitionOutcome::Clean,
                    to: TransitionTarget::Step(REVIEW_INCREMENTAL_REPAIR),
                },
                Transition {
                    from_step: REVIEW_INCREMENTAL_REPAIR,
                    outcome: TransitionOutcome::Clean,
                    to: TransitionTarget::Step(REVIEW_CANDIDATE_REPAIR),
                },
                Transition {
                    from_step: REVIEW_INCREMENTAL_REPAIR,
                    outcome: TransitionOutcome::Findings,
                    to: TransitionTarget::Step(REPAIR_CANDIDATE),
                },
                // review_candidate_repair
                Transition {
                    from_step: REVIEW_CANDIDATE_REPAIR,
                    outcome: TransitionOutcome::Clean,
                    to: TransitionTarget::Step(VALIDATE_CANDIDATE_REPAIR),
                },
                Transition {
                    from_step: REVIEW_CANDIDATE_REPAIR,
                    outcome: TransitionOutcome::Findings,
                    to: TransitionTarget::Step(REPAIR_CANDIDATE),
                },
                // validate_candidate_repair
                Transition {
                    from_step: VALIDATE_CANDIDATE_REPAIR,
                    outcome: TransitionOutcome::Clean,
                    to: TransitionTarget::Step(PREPARE_CONVERGENCE),
                },
                Transition {
                    from_step: VALIDATE_CANDIDATE_REPAIR,
                    outcome: TransitionOutcome::Findings,
                    to: TransitionTarget::Step(REPAIR_CANDIDATE),
                },
                // validate_integrated - clean goes to approval gate (handled by evaluator)
                Transition {
                    from_step: VALIDATE_INTEGRATED,
                    outcome: TransitionOutcome::Findings,
                    to: TransitionTarget::Step(REPAIR_AFTER_INTEGRATION),
                },
                // repair_after_integration
                Transition {
                    from_step: REPAIR_AFTER_INTEGRATION,
                    outcome: TransitionOutcome::Clean,
                    to: TransitionTarget::Step(REVIEW_INCREMENTAL_AFTER_INTEGRATION_REPAIR),
                },
                Transition {
                    from_step: REVIEW_INCREMENTAL_AFTER_INTEGRATION_REPAIR,
                    outcome: TransitionOutcome::Clean,
                    to: TransitionTarget::Step(REVIEW_AFTER_INTEGRATION_REPAIR),
                },
                Transition {
                    from_step: REVIEW_INCREMENTAL_AFTER_INTEGRATION_REPAIR,
                    outcome: TransitionOutcome::Findings,
                    to: TransitionTarget::Step(REPAIR_AFTER_INTEGRATION),
                },
                // review_after_integration_repair
                Transition {
                    from_step: REVIEW_AFTER_INTEGRATION_REPAIR,
                    outcome: TransitionOutcome::Clean,
                    to: TransitionTarget::Step(VALIDATE_AFTER_INTEGRATION_REPAIR),
                },
                Transition {
                    from_step: REVIEW_AFTER_INTEGRATION_REPAIR,
                    outcome: TransitionOutcome::Findings,
                    to: TransitionTarget::Step(REPAIR_AFTER_INTEGRATION),
                },
                // validate_after_integration_repair
                Transition {
                    from_step: VALIDATE_AFTER_INTEGRATION_REPAIR,
                    outcome: TransitionOutcome::Clean,
                    to: TransitionTarget::Step(PREPARE_CONVERGENCE),
                },
                Transition {
                    from_step: VALIDATE_AFTER_INTEGRATION_REPAIR,
                    outcome: TransitionOutcome::Findings,
                    to: TransitionTarget::Step(REPAIR_AFTER_INTEGRATION),
                },
            ],
        }
    }

    /// Find the next step given a completed step and its outcome.
    pub fn next_step(&self, from_step: &str, outcome: &OutcomeClass) -> Option<&TransitionTarget> {
        let transition_outcome = match outcome {
            OutcomeClass::Clean => TransitionOutcome::Clean,
            OutcomeClass::Findings => TransitionOutcome::Findings,
            _ => return None,
        };

        self.transitions
            .iter()
            .find(|t| t.from_step == from_step && t.outcome == transition_outcome)
            .map(|t| &t.to)
    }
}
