use ingot_domain::finding::Finding;
use ingot_domain::item::{Item, ParkingState};
use ingot_domain::job::{Job, OutcomeClass};
use ingot_domain::revision::ItemRevision;
use ingot_domain::step_id::StepId;

use crate::graph::WorkflowGraph;
use crate::recommended_action::{NamedRecommendedAction, RecommendedAction};
use crate::step;

use super::projection::latest_closure_terminal_job;
use super::{AllowedAction, AttentionBadge, BoardStatus, Evaluation, PhaseStatus};

pub(super) fn evaluate_investigation(
    graph: &WorkflowGraph,
    item: &Item,
    _revision: &ItemRevision,
    jobs: &[Job],
    findings: &[Finding],
    attention_badges: Vec<AttentionBadge>,
    mut diagnostics: Vec<String>,
) -> Evaluation {
    let current_revision_jobs: Vec<&Job> = jobs
        .iter()
        .filter(|job| job.item_revision_id == item.current_revision_id)
        .collect();
    let current_revision_findings: Vec<&Finding> = findings
        .iter()
        .filter(|finding| finding.source_item_revision_id == item.current_revision_id)
        .collect();

    let active_job = current_revision_jobs
        .iter()
        .copied()
        .find(|job| job.state.is_active());

    let latest_closure_job = latest_closure_terminal_job(&current_revision_jobs);

    if item.escalation.is_escalated() {
        return finish(
            item,
            attention_badges,
            Evaluation {
                board_status: BoardStatus::Working,
                attention_badges: vec![],
                current_step_id: latest_closure_job.map(|j| j.step_id),
                current_phase_kind: None,
                phase_status: Some(PhaseStatus::Escalated),
                next_recommended_action: RecommendedAction::named(
                    NamedRecommendedAction::OperatorIntervention,
                ),
                dispatchable_step_id: None,
                auxiliary_dispatchable_step_ids: vec![],
                allowed_actions: vec![
                    AllowedAction::Revise,
                    AllowedAction::Dismiss,
                    AllowedAction::Invalidate,
                    AllowedAction::Defer,
                ],
                terminal_readiness: false,
                diagnostics,
            },
        );
    }

    if item.parking_state == ParkingState::Deferred {
        return finish(
            item,
            attention_badges,
            Evaluation {
                board_status: BoardStatus::Inbox,
                attention_badges: vec![],
                current_step_id: latest_closure_job.map(|j| j.step_id),
                current_phase_kind: None,
                phase_status: Some(PhaseStatus::Deferred),
                next_recommended_action: RecommendedAction::None,
                dispatchable_step_id: None,
                auxiliary_dispatchable_step_ids: vec![],
                allowed_actions: vec![AllowedAction::Resume],
                terminal_readiness: false,
                diagnostics,
            },
        );
    }

    if let Some(job) = active_job {
        return finish(
            item,
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

    let Some(last_job) = latest_closure_job else {
        return finish(
            item,
            attention_badges,
            Evaluation {
                board_status: BoardStatus::Inbox,
                attention_badges: vec![],
                current_step_id: None,
                current_phase_kind: None,
                phase_status: Some(PhaseStatus::New),
                next_recommended_action: RecommendedAction::dispatch(StepId::InvestigateProject),
                dispatchable_step_id: Some(StepId::InvestigateProject),
                auxiliary_dispatchable_step_ids: vec![],
                allowed_actions: vec![AllowedAction::Dispatch],
                terminal_readiness: false,
                diagnostics,
            },
        );
    };

    let Some(outcome) = last_job.state.outcome_class() else {
        diagnostics.push(format!(
            "investigation job {} has no outcome_class despite terminal status",
            last_job.step_id,
        ));
        return finish(
            item,
            attention_badges,
            Evaluation {
                board_status: BoardStatus::Working,
                attention_badges: vec![],
                current_step_id: Some(last_job.step_id),
                current_phase_kind: None,
                phase_status: Some(PhaseStatus::Unknown),
                next_recommended_action: RecommendedAction::named(
                    NamedRecommendedAction::OperatorIntervention,
                ),
                dispatchable_step_id: None,
                auxiliary_dispatchable_step_ids: vec![],
                allowed_actions: vec![],
                terminal_readiness: false,
                diagnostics,
            },
        );
    };

    if outcome == OutcomeClass::Clean {
        return finish(
            item,
            attention_badges,
            Evaluation {
                board_status: BoardStatus::Working,
                attention_badges: vec![],
                current_step_id: Some(last_job.step_id),
                current_phase_kind: None,
                phase_status: Some(PhaseStatus::Idle),
                next_recommended_action: RecommendedAction::None,
                dispatchable_step_id: None,
                auxiliary_dispatchable_step_ids: vec![],
                allowed_actions: vec![],
                terminal_readiness: true,
                diagnostics,
            },
        );
    }

    if outcome == OutcomeClass::Findings {
        let job_findings: Vec<&&Finding> = current_revision_findings
            .iter()
            .filter(|finding| finding.source_job_id == last_job.id)
            .collect();

        if job_findings.iter().any(|f| f.triage.is_unresolved()) {
            return finish(
                item,
                attention_badges,
                Evaluation {
                    board_status: BoardStatus::Working,
                    attention_badges: vec![],
                    current_step_id: Some(last_job.step_id),
                    current_phase_kind: None,
                    phase_status: Some(PhaseStatus::Triaging),
                    next_recommended_action: RecommendedAction::named(
                        NamedRecommendedAction::TriageFindings,
                    ),
                    dispatchable_step_id: None,
                    auxiliary_dispatchable_step_ids: vec![],
                    allowed_actions: vec![],
                    terminal_readiness: false,
                    diagnostics,
                },
            );
        }

        let has_fix_now = job_findings
            .iter()
            .any(|f| f.triage.state() == ingot_domain::finding::FindingTriageState::FixNow);
        let has_needs_investigation = job_findings.iter().any(|f| {
            f.triage.state() == ingot_domain::finding::FindingTriageState::NeedsInvestigation
        });

        if has_fix_now || has_needs_investigation {
            if let Some(crate::graph::TransitionTarget::Step(next_step)) =
                graph.next_step(last_job.step_id, &OutcomeClass::Findings)
            {
                let contract = step::find_step(*next_step);
                if contract.is_dispatchable_job() {
                    return finish(
                        item,
                        attention_badges,
                        Evaluation {
                            board_status: BoardStatus::Working,
                            attention_badges: vec![],
                            current_step_id: Some(last_job.step_id),
                            current_phase_kind: None,
                            phase_status: Some(PhaseStatus::Idle),
                            next_recommended_action: RecommendedAction::dispatch(*next_step),
                            dispatchable_step_id: Some(*next_step),
                            auxiliary_dispatchable_step_ids: vec![],
                            allowed_actions: vec![AllowedAction::Dispatch],
                            terminal_readiness: false,
                            diagnostics,
                        },
                    );
                }
            }
        }

        return finish(
            item,
            attention_badges,
            Evaluation {
                board_status: BoardStatus::Working,
                attention_badges: vec![],
                current_step_id: Some(last_job.step_id),
                current_phase_kind: None,
                phase_status: Some(PhaseStatus::Idle),
                next_recommended_action: RecommendedAction::None,
                dispatchable_step_id: None,
                auxiliary_dispatchable_step_ids: vec![],
                allowed_actions: vec![],
                terminal_readiness: true,
                diagnostics,
            },
        );
    }

    // Terminal/transient failure — needs operator intervention
    finish(
        item,
        attention_badges,
        Evaluation {
            board_status: BoardStatus::Working,
            attention_badges: vec![],
            current_step_id: Some(last_job.step_id),
            current_phase_kind: None,
            phase_status: Some(PhaseStatus::Idle),
            next_recommended_action: RecommendedAction::None,
            dispatchable_step_id: None,
            auxiliary_dispatchable_step_ids: vec![],
            allowed_actions: vec![],
            terminal_readiness: false,
            diagnostics,
        },
    )
}

fn finish(
    item: &Item,
    attention_badges: Vec<AttentionBadge>,
    mut evaluation: Evaluation,
) -> Evaluation {
    evaluation.attention_badges = attention_badges;
    evaluation.board_status = if item.lifecycle.is_done() {
        BoardStatus::Done
    } else if evaluation.phase_status == Some(PhaseStatus::Running)
        || evaluation.dispatchable_step_id.is_some()
        || evaluation.terminal_readiness
    {
        BoardStatus::Working
    } else {
        evaluation.board_status
    };
    evaluation
}
