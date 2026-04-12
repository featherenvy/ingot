mod investigation;
mod projection;
#[cfg(test)]
mod tests;

use ingot_domain::convergence::{CheckoutAdoptionState, Convergence, ConvergenceStatus};
use ingot_domain::finding::Finding;
use ingot_domain::item::{ApprovalState, Item, ParkingState, WorkflowVersion};
use ingot_domain::job::{Job, PhaseKind};
use ingot_domain::revision::ItemRevision;
use ingot_domain::step_id::StepId;

use crate::graph::WorkflowGraph;
use crate::recommended_action::{NamedRecommendedAction, RecommendedAction};
use crate::step::{self, ClosureRelevance};

use self::projection::{
    auxiliary_steps, evaluate_idle_projection, latest_closure_terminal_job, merge_allowed_actions,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseStatus {
    New,
    Done,
    Running,
    Idle,
    Escalated,
    Deferred,
    PendingApproval,
    AwaitingConvergence,
    AwaitingCheckoutSync,
    Triaging,
    FinalizationReady,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AllowedAction {
    Dispatch,
    CancelJob,
    ApprovalApprove,
    ApprovalReject,
    PrepareConvergence,
    Resume,
    Revise,
    Dismiss,
    Invalidate,
    Defer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionBadge {
    Escalated,
    Deferred,
}

/// Board column for UI rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BoardStatus {
    Inbox,
    Working,
    Approval,
    Done,
}

/// Pure read-side projection of item state.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Evaluation {
    pub board_status: BoardStatus,
    pub attention_badges: Vec<AttentionBadge>,
    pub current_step_id: Option<StepId>,
    pub current_phase_kind: Option<PhaseKind>,
    pub phase_status: Option<PhaseStatus>,
    pub next_recommended_action: RecommendedAction,
    pub dispatchable_step_id: Option<StepId>,
    pub auxiliary_dispatchable_step_ids: Vec<StepId>,
    pub allowed_actions: Vec<AllowedAction>,
    pub terminal_readiness: bool,
    pub diagnostics: Vec<String>,
}

pub struct Evaluator {
    delivery_graph: WorkflowGraph,
    investigation_graph: WorkflowGraph,
}

impl Default for Evaluator {
    fn default() -> Self {
        Self::new()
    }
}

impl Evaluator {
    pub fn new() -> Self {
        Self {
            delivery_graph: WorkflowGraph::delivery_v1(),
            investigation_graph: WorkflowGraph::investigation_v1(),
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
        findings: &[Finding],
        convergences: &[Convergence],
    ) -> Evaluation {
        let mut diagnostics = Vec::new();
        let mut attention_badges = Vec::new();

        if item.escalation.is_escalated() {
            attention_badges.push(AttentionBadge::Escalated);
        }
        if item.parking_state == ParkingState::Deferred {
            attention_badges.push(AttentionBadge::Deferred);
        }

        if item.lifecycle.is_done() {
            return Evaluation {
                board_status: BoardStatus::Done,
                attention_badges,
                current_step_id: None,
                current_phase_kind: None,
                phase_status: Some(PhaseStatus::Done),
                next_recommended_action: RecommendedAction::None,
                dispatchable_step_id: None,
                auxiliary_dispatchable_step_ids: vec![],
                allowed_actions: vec![],
                terminal_readiness: false,
                diagnostics,
            };
        }

        if item.workflow_version == WorkflowVersion::InvestigationV1 {
            return investigation::evaluate_investigation(
                &self.investigation_graph,
                item,
                revision,
                jobs,
                findings,
                attention_badges,
                diagnostics,
            );
        }

        let current_revision_jobs: Vec<&Job> = jobs
            .iter()
            .filter(|job| job.item_revision_id == item.current_revision_id)
            .collect();
        let current_revision_findings: Vec<&Finding> = findings
            .iter()
            .filter(|finding| finding.source_item_revision_id == item.current_revision_id)
            .collect();
        let current_revision_convergences: Vec<&Convergence> = convergences
            .iter()
            .filter(|conv| conv.item_revision_id == item.current_revision_id)
            .collect();

        let active_job = current_revision_jobs
            .iter()
            .copied()
            .find(|job| job.state.is_active());
        let active_convergence = current_revision_convergences.iter().copied().find(|conv| {
            matches!(
                conv.state.status(),
                ConvergenceStatus::Queued | ConvergenceStatus::Running
            )
        });
        let prepared_convergence = current_revision_convergences
            .iter()
            .copied()
            .find(|conv| conv.state.status() == ConvergenceStatus::Prepared);
        let awaiting_checkout_sync = current_revision_convergences.iter().copied().find(|conv| {
            conv.state.status() == ConvergenceStatus::Finalized
                && matches!(
                    conv.state.checkout_adoption_state(),
                    Some(CheckoutAdoptionState::Pending | CheckoutAdoptionState::Blocked)
                )
        });

        let latest_closure_job = latest_closure_terminal_job(&current_revision_jobs);
        let has_terminal_closure_job = latest_closure_job.is_some();

        if awaiting_checkout_sync.is_some() {
            return self.finish_evaluation(
                item,
                has_terminal_closure_job,
                attention_badges,
                Evaluation {
                    board_status: BoardStatus::Working,
                    attention_badges: vec![],
                    current_step_id: Some(StepId::PrepareConvergence),
                    current_phase_kind: None,
                    phase_status: Some(PhaseStatus::AwaitingCheckoutSync),
                    next_recommended_action: RecommendedAction::named(
                        NamedRecommendedAction::ResolveCheckoutSync,
                    ),
                    dispatchable_step_id: None,
                    auxiliary_dispatchable_step_ids: vec![],
                    allowed_actions: vec![],
                    terminal_readiness: false,
                    diagnostics,
                },
            );
        }

        if let Some(job) = active_job {
            let contract = step::find_step(job.step_id);
            let is_report_only = contract.closure_relevance == ClosureRelevance::ReportOnly;

            if is_report_only {
                let base = evaluate_idle_projection(
                    &self.delivery_graph,
                    item,
                    revision,
                    latest_closure_job,
                    &current_revision_findings,
                    prepared_convergence,
                    &mut diagnostics,
                );

                return self.finish_evaluation(
                    item,
                    has_terminal_closure_job,
                    attention_badges,
                    Evaluation {
                        board_status: BoardStatus::Working,
                        attention_badges: vec![],
                        current_step_id: base.current_step_id,
                        current_phase_kind: Some(job.phase_kind),
                        phase_status: Some(PhaseStatus::Running),
                        next_recommended_action: RecommendedAction::None,
                        dispatchable_step_id: None,
                        auxiliary_dispatchable_step_ids: vec![],
                        allowed_actions: vec![AllowedAction::CancelJob],
                        terminal_readiness: false,
                        diagnostics,
                    },
                );
            }

            return self.finish_evaluation(
                item,
                has_terminal_closure_job,
                attention_badges,
                Evaluation {
                    board_status: BoardStatus::Working,
                    attention_badges: vec![],
                    current_step_id: Some(job.step_id),
                    current_phase_kind: Some(job.phase_kind),
                    phase_status: Some(PhaseStatus::Running),
                    next_recommended_action: RecommendedAction::None,
                    dispatchable_step_id: None,
                    auxiliary_dispatchable_step_ids: vec![],
                    allowed_actions: vec![AllowedAction::CancelJob],
                    terminal_readiness: false,
                    diagnostics,
                },
            );
        }

        if active_convergence.is_some() {
            return self.finish_evaluation(
                item,
                has_terminal_closure_job,
                attention_badges,
                Evaluation {
                    board_status: BoardStatus::Working,
                    attention_badges: vec![],
                    current_step_id: Some(StepId::PrepareConvergence),
                    current_phase_kind: Some(PhaseKind::System),
                    phase_status: Some(PhaseStatus::Running),
                    next_recommended_action: RecommendedAction::None,
                    dispatchable_step_id: None,
                    auxiliary_dispatchable_step_ids: vec![],
                    allowed_actions: vec![],
                    terminal_readiness: false,
                    diagnostics,
                },
            );
        }

        let base = evaluate_idle_projection(
            &self.delivery_graph,
            item,
            revision,
            latest_closure_job,
            &current_revision_findings,
            prepared_convergence,
            &mut diagnostics,
        );
        let auxiliary_dispatchable_step_ids =
            auxiliary_steps(item, &base.next_recommended_action, base.phase_status);
        let allowed_actions =
            merge_allowed_actions(base.allowed_actions, &auxiliary_dispatchable_step_ids);

        self.finish_evaluation(
            item,
            has_terminal_closure_job,
            attention_badges,
            Evaluation {
                board_status: BoardStatus::Working,
                attention_badges: vec![],
                current_step_id: base.current_step_id,
                current_phase_kind: None,
                phase_status: Some(base.phase_status),
                next_recommended_action: base.next_recommended_action,
                dispatchable_step_id: base.dispatchable_step_id,
                auxiliary_dispatchable_step_ids,
                allowed_actions,
                terminal_readiness: base.terminal_readiness,
                diagnostics,
            },
        )
    }

    fn finish_evaluation(
        &self,
        item: &Item,
        has_terminal_closure_job: bool,
        attention_badges: Vec<AttentionBadge>,
        mut evaluation: Evaluation,
    ) -> Evaluation {
        evaluation.attention_badges = attention_badges;

        evaluation.board_status = if item.lifecycle.is_done() {
            BoardStatus::Done
        } else if item.approval_state == ApprovalState::Pending
            && evaluation.next_recommended_action
                != RecommendedAction::named(NamedRecommendedAction::InvalidatePreparedConvergence)
        {
            BoardStatus::Approval
        } else if evaluation.phase_status == Some(PhaseStatus::Running) || has_terminal_closure_job
        {
            BoardStatus::Working
        } else {
            BoardStatus::Inbox
        };

        evaluation
    }
}
