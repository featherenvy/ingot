use ingot_domain::convergence::{Convergence, ConvergenceStatus};
use ingot_domain::item::{ApprovalState, EscalationState, Item, LifecycleState, ParkingState};
use ingot_domain::job::{Job, JobStatus, OutcomeClass};
use ingot_domain::revision::{ApprovalPolicy, ItemRevision};

use crate::graph::{TransitionTarget, WorkflowGraph};
use crate::step;

/// Board column for UI rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BoardStatus {
    Inbox,
    Working,
    Approval,
    Done,
}

/// The recommended next action for an item.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NextAction {
    DispatchStep(String),
    PrepareConvergence,
    FinalizeConvergence,
    InvalidateConvergence,
    ApprovalApprove,
    ApprovalReject,
    OperatorIntervention,
    None,
}

/// Pure read-side projection of item state.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Evaluation {
    pub board_status: BoardStatus,
    pub attention_badges: Vec<String>,
    pub current_step_id: Option<String>,
    pub current_phase_kind: Option<String>,
    pub phase_status: Option<String>,
    pub next_recommended_action: NextAction,
    pub dispatchable_step_id: Option<String>,
    pub allowed_actions: Vec<String>,
    pub terminal_readiness: bool,
    pub diagnostics: Vec<String>,
}

pub struct Evaluator {
    graph: WorkflowGraph,
}

impl Evaluator {
    pub fn new() -> Self {
        Self {
            graph: WorkflowGraph::delivery_v1(),
        }
    }

    /// Evaluate the current state of an item.
    ///
    /// This is pure read-side logic. It MUST NOT mutate durable state.
    pub fn evaluate(
        &self,
        item: &Item,
        revision: &ItemRevision,
        jobs: &[Job],
        convergences: &[Convergence],
    ) -> Evaluation {
        let mut diagnostics = Vec::new();
        let mut attention_badges = Vec::new();

        // 1. Terminal items
        if item.lifecycle_state == LifecycleState::Done {
            return Evaluation {
                board_status: BoardStatus::Done,
                attention_badges,
                current_step_id: None,
                current_phase_kind: None,
                phase_status: Some("done".into()),
                next_recommended_action: NextAction::None,
                dispatchable_step_id: None,
                allowed_actions: vec![],
                terminal_readiness: false,
                diagnostics,
            };
        }

        // Compute attention badges
        if item.escalation_state == EscalationState::OperatorRequired {
            attention_badges.push("escalated".into());
        }
        if item.parking_state == ParkingState::Deferred {
            attention_badges.push("deferred".into());
        }

        // 2. Deferred items
        if item.parking_state == ParkingState::Deferred {
            return Evaluation {
                board_status: BoardStatus::Working,
                attention_badges,
                current_step_id: None,
                current_phase_kind: None,
                phase_status: Some("deferred".into()),
                next_recommended_action: NextAction::None,
                dispatchable_step_id: None,
                allowed_actions: vec!["resume".into()],
                terminal_readiness: false,
                diagnostics,
            };
        }

        // 3. Active job or convergence
        let active_job = jobs.iter().find(|j| j.status.is_active());
        let active_convergence = convergences.iter().find(|c| {
            matches!(
                c.status,
                ConvergenceStatus::Queued | ConvergenceStatus::Running
            )
        });

        if let Some(job) = active_job {
            return Evaluation {
                board_status: BoardStatus::Working,
                attention_badges,
                current_step_id: Some(job.step_id.clone()),
                current_phase_kind: Some(format!("{:?}", job.phase_kind).to_lowercase()),
                phase_status: Some("running".into()),
                next_recommended_action: NextAction::None,
                dispatchable_step_id: None,
                allowed_actions: vec!["cancel_job".into()],
                terminal_readiness: false,
                diagnostics,
            };
        }

        if active_convergence.is_some() {
            return Evaluation {
                board_status: BoardStatus::Working,
                attention_badges,
                current_step_id: Some(step::PREPARE_CONVERGENCE.into()),
                current_phase_kind: Some("system".into()),
                phase_status: Some("running".into()),
                next_recommended_action: NextAction::None,
                dispatchable_step_id: None,
                allowed_actions: vec![],
                terminal_readiness: false,
                diagnostics,
            };
        }

        // 4. Determine workflow position from terminal jobs and convergence state
        let prepared_convergence = convergences
            .iter()
            .find(|c| c.status == ConvergenceStatus::Prepared);

        // Check for stale prepared convergence
        // Note: actual target_ref head comparison happens at the use-case layer
        // The evaluator signals the need for invalidation when told the convergence is stale

        // 5. Escalated items
        if item.escalation_state == EscalationState::OperatorRequired {
            return Evaluation {
                board_status: BoardStatus::Working,
                attention_badges,
                current_step_id: None,
                current_phase_kind: None,
                phase_status: Some("escalated".into()),
                next_recommended_action: NextAction::OperatorIntervention,
                dispatchable_step_id: None,
                allowed_actions: vec![
                    "revise".into(),
                    "dismiss".into(),
                    "invalidate".into(),
                    "defer".into(),
                ],
                terminal_readiness: false,
                diagnostics,
            };
        }

        // 6. Approval gate
        if item.approval_state == ApprovalState::Pending {
            return Evaluation {
                board_status: BoardStatus::Approval,
                attention_badges,
                current_step_id: Some(step::VALIDATE_INTEGRATED.into()),
                current_phase_kind: None,
                phase_status: Some("pending_approval".into()),
                next_recommended_action: NextAction::ApprovalApprove,
                dispatchable_step_id: None,
                allowed_actions: vec!["approval_approve".into(), "approval_reject".into()],
                terminal_readiness: false,
                diagnostics,
            };
        }

        // 7. Find the latest non-superseded terminal job for the current revision
        let terminal_jobs: Vec<&Job> = jobs
            .iter()
            .filter(|j| {
                j.item_revision_id == item.current_revision_id
                    && j.status.is_terminal()
                    && j.status != JobStatus::Superseded
            })
            .collect();

        let latest_terminal = terminal_jobs.iter().max_by_key(|j| j.ended_at);

        if let Some(last_job) = latest_terminal {
            let outcome = last_job.outcome_class.unwrap_or(OutcomeClass::Clean);

            // After validate_integrated with clean outcome
            if last_job.step_id == step::VALIDATE_INTEGRATED && outcome == OutcomeClass::Clean {
                if prepared_convergence.is_some() {
                    match revision.approval_policy {
                        ApprovalPolicy::Required => {
                            // approval_state should already be pending via job-completion handler
                            diagnostics.push(
                                "validate_integrated clean but approval_state not pending".into(),
                            );
                        }
                        ApprovalPolicy::NotRequired => {
                            return Evaluation {
                                board_status: BoardStatus::Working,
                                attention_badges,
                                current_step_id: Some(step::VALIDATE_INTEGRATED.into()),
                                current_phase_kind: None,
                                phase_status: Some("finalization_ready".into()),
                                next_recommended_action: NextAction::FinalizeConvergence,
                                dispatchable_step_id: None,
                                allowed_actions: vec![],
                                terminal_readiness: true,
                                diagnostics,
                            };
                        }
                    }
                }
            }

            // Follow the graph for the next step
            if let Some(target) = self.graph.next_step(&last_job.step_id, &outcome) {
                match target {
                    TransitionTarget::Step(next_step) => {
                        let contract = step::find_step(next_step);
                        if let Some(contract) = contract {
                            if contract.is_system_step {
                                return Evaluation {
                                    board_status: BoardStatus::Working,
                                    attention_badges,
                                    current_step_id: Some(last_job.step_id.clone()),
                                    current_phase_kind: None,
                                    phase_status: Some("awaiting_convergence".into()),
                                    next_recommended_action: NextAction::PrepareConvergence,
                                    dispatchable_step_id: None,
                                    allowed_actions: vec!["prepare_convergence".into()],
                                    terminal_readiness: false,
                                    diagnostics,
                                };
                            }

                            return Evaluation {
                                board_status: BoardStatus::Working,
                                attention_badges,
                                current_step_id: Some(last_job.step_id.clone()),
                                current_phase_kind: None,
                                phase_status: Some("idle".into()),
                                next_recommended_action: NextAction::DispatchStep(
                                    next_step.to_string(),
                                ),
                                dispatchable_step_id: Some(next_step.to_string()),
                                allowed_actions: vec!["dispatch".into()],
                                terminal_readiness: false,
                                diagnostics,
                            };
                        }
                    }
                    TransitionTarget::SystemAction(action) => {
                        diagnostics.push(format!("system action: {action}"));
                    }
                    TransitionTarget::Escalation(reason) => {
                        diagnostics.push(format!("escalation expected: {reason}"));
                    }
                }
            }
        }

        // No terminal jobs yet — item is in INBOX, ready for first dispatch
        let has_any_terminal_job = !terminal_jobs.is_empty();

        if !has_any_terminal_job {
            return Evaluation {
                board_status: BoardStatus::Inbox,
                attention_badges,
                current_step_id: None,
                current_phase_kind: None,
                phase_status: Some("new".into()),
                next_recommended_action: NextAction::DispatchStep(step::AUTHOR_INITIAL.to_string()),
                dispatchable_step_id: Some(step::AUTHOR_INITIAL.to_string()),
                allowed_actions: vec!["dispatch".into(), "defer".into(), "dismiss".into()],
                terminal_readiness: false,
                diagnostics,
            };
        }

        // Fallback
        Evaluation {
            board_status: BoardStatus::Working,
            attention_badges,
            current_step_id: None,
            current_phase_kind: None,
            phase_status: Some("unknown".into()),
            next_recommended_action: NextAction::OperatorIntervention,
            dispatchable_step_id: None,
            allowed_actions: vec![],
            terminal_readiness: false,
            diagnostics: vec!["could not determine workflow position".into()],
        }
    }
}
