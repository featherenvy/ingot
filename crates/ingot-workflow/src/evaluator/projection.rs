use ingot_domain::convergence::Convergence;
use ingot_domain::finding::{Finding, FindingTriageState};
use ingot_domain::item::{ApprovalState, Item, ParkingState};
use ingot_domain::job::{Job, JobStatus, OutcomeClass};
use ingot_domain::revision::{ApprovalPolicy, ItemRevision};
use ingot_domain::step_id::StepId;

use crate::graph::{TransitionTarget, WorkflowGraph};
use crate::recommended_action::{NamedRecommendedAction, RecommendedAction};
use crate::step::{self, ClosureRelevance};

use super::{AllowedAction, PhaseStatus};

#[derive(Debug, Clone)]
pub(super) struct IdleProjection {
    pub(super) current_step_id: Option<StepId>,
    pub(super) phase_status: PhaseStatus,
    pub(super) next_recommended_action: RecommendedAction,
    pub(super) dispatchable_step_id: Option<StepId>,
    pub(super) allowed_actions: Vec<AllowedAction>,
    pub(super) terminal_readiness: bool,
}

pub(super) fn latest_closure_terminal_job<'a>(jobs: &'a [&'a Job]) -> Option<&'a Job> {
    jobs.iter()
        .copied()
        .filter(|job| is_terminal_closure_job(job))
        .max_by_key(|job| (job.state.ended_at(), job.created_at))
}

pub(super) fn evaluate_idle_projection(
    graph: &WorkflowGraph,
    item: &Item,
    revision: &ItemRevision,
    latest_closure_job: Option<&Job>,
    findings: &[&Finding],
    prepared_convergence: Option<&Convergence>,
    diagnostics: &mut Vec<String>,
) -> IdleProjection {
    let current_step_id = current_closure_step_id(latest_closure_job, prepared_convergence);

    if item.escalation.is_escalated() {
        return IdleProjection {
            current_step_id,
            phase_status: PhaseStatus::Escalated,
            next_recommended_action: RecommendedAction::named(
                NamedRecommendedAction::OperatorIntervention,
            ),
            dispatchable_step_id: None,
            allowed_actions: vec![
                AllowedAction::Revise,
                AllowedAction::Dismiss,
                AllowedAction::Invalidate,
                AllowedAction::Defer,
            ],
            terminal_readiness: false,
        };
    }

    if prepared_convergence
        .and_then(|conv| conv.target_head_valid)
        .is_some_and(|valid| !valid)
    {
        return IdleProjection {
            current_step_id,
            phase_status: PhaseStatus::Idle,
            next_recommended_action: RecommendedAction::named(
                NamedRecommendedAction::InvalidatePreparedConvergence,
            ),
            dispatchable_step_id: None,
            allowed_actions: vec![],
            terminal_readiness: false,
        };
    }

    if item.parking_state == ParkingState::Deferred {
        return IdleProjection {
            current_step_id,
            phase_status: PhaseStatus::Deferred,
            next_recommended_action: RecommendedAction::None,
            dispatchable_step_id: None,
            allowed_actions: vec![AllowedAction::Resume],
            terminal_readiness: false,
        };
    }

    if let Some(last_job) = latest_closure_job {
        let Some(outcome) = closure_outcome(last_job, diagnostics) else {
            return operator_intervention_projection(current_step_id);
        };

        if last_job.step_id == StepId::ValidateIntegrated && outcome == OutcomeClass::Clean {
            if prepared_convergence.is_none() {
                diagnostics
                    .push("validate_integrated clean but no prepared convergence exists".into());
                return operator_intervention_projection(current_step_id);
            } else if item.approval_state == ApprovalState::Pending {
                return pending_approval_projection(current_step_id);
            } else if revision.approval_policy == ApprovalPolicy::NotRequired {
                return finalization_ready_projection(current_step_id);
            } else {
                diagnostics
                    .push("validate_integrated clean but approval_state is not pending".into());
                return operator_intervention_projection(current_step_id);
            }
        }

        if outcome == OutcomeClass::Findings && is_closure_relevant_step(last_job.step_id) {
            if let Some(triage_projection) = triage_projection(
                graph,
                item,
                revision,
                last_job,
                findings,
                prepared_convergence,
                diagnostics,
            ) {
                return triage_projection;
            }

            diagnostics.push(format!(
                "last closure job {} ended with findings but no durable findings were extracted",
                last_job.step_id
            ));
        }
    }

    if let Some(last_job) = latest_closure_job {
        let Some(outcome) = closure_outcome(last_job, diagnostics) else {
            return operator_intervention_projection(current_step_id);
        };
        match outcome {
            OutcomeClass::Clean | OutcomeClass::Findings => {
                if let Some(target) = graph.next_step(last_job.step_id, &outcome) {
                    match target {
                        TransitionTarget::Step(next_step) => {
                            if *next_step == StepId::PrepareConvergence
                                && prepared_convergence.is_some()
                            {
                                return dispatchable_projection(
                                    Some(StepId::PrepareConvergence),
                                    PhaseStatus::Idle,
                                    StepId::ValidateIntegrated,
                                );
                            }

                            let contract = step::find_step(*next_step);
                            if !contract.is_dispatchable_job() {
                                return IdleProjection {
                                    current_step_id: Some(last_job.step_id),
                                    phase_status: PhaseStatus::AwaitingConvergence,
                                    next_recommended_action: RecommendedAction::from_step(
                                        *next_step,
                                    ),
                                    dispatchable_step_id: None,
                                    allowed_actions: vec![AllowedAction::PrepareConvergence],
                                    terminal_readiness: false,
                                };
                            }

                            return dispatchable_projection(
                                Some(last_job.step_id),
                                PhaseStatus::Idle,
                                *next_step,
                            );
                        }
                        TransitionTarget::SystemAction(action) => {
                            return system_action_projection(
                                Some(last_job.step_id),
                                action,
                                diagnostics,
                            );
                        }
                        TransitionTarget::Escalation(reason) => {
                            diagnostics.push(format!("escalation expected: {reason}"));
                        }
                    }
                }
            }
            OutcomeClass::Cancelled | OutcomeClass::TransientFailure => {
                return IdleProjection {
                    current_step_id: Some(last_job.step_id),
                    phase_status: PhaseStatus::Idle,
                    next_recommended_action: RecommendedAction::None,
                    dispatchable_step_id: None,
                    allowed_actions: vec![],
                    terminal_readiness: false,
                };
            }
            OutcomeClass::TerminalFailure | OutcomeClass::ProtocolViolation => {
                diagnostics.push(format!(
                    "last closure job {} ended with {} and requires command-side handling",
                    last_job.step_id,
                    outcome_class_name(outcome)
                ));
            }
        }
    }

    if prepared_convergence.is_some() {
        return dispatchable_projection(
            Some(StepId::PrepareConvergence),
            PhaseStatus::Idle,
            StepId::ValidateIntegrated,
        );
    }

    if latest_closure_job.is_none() {
        return dispatchable_projection(None, PhaseStatus::New, StepId::AuthorInitial);
    }

    operator_intervention_projection(current_step_id)
}

pub(super) fn system_action_projection(
    current_step_id: Option<StepId>,
    action: &str,
    diagnostics: &mut Vec<String>,
) -> IdleProjection {
    match RecommendedAction::system_action(action) {
        Ok(next_recommended_action) => IdleProjection {
            current_step_id,
            phase_status: PhaseStatus::AwaitingConvergence,
            next_recommended_action,
            dispatchable_step_id: None,
            allowed_actions: vec![],
            terminal_readiness: false,
        },
        Err(error) => {
            diagnostics.push(error);
            operator_intervention_projection(current_step_id)
        }
    }
}

pub(super) fn auxiliary_steps(
    item: &Item,
    next_action: &RecommendedAction,
    phase_status: PhaseStatus,
) -> Vec<StepId> {
    if !item.lifecycle.is_open()
        || item.parking_state != ParkingState::Active
        || item.approval_state == ApprovalState::Pending
        || item.escalation.is_escalated()
        || !matches!(phase_status, PhaseStatus::New | PhaseStatus::Idle)
        || next_action.is_daemon_owned()
    {
        return vec![];
    }

    vec![StepId::InvestigateItem]
}

pub(super) fn merge_allowed_actions(
    mut allowed_actions: Vec<AllowedAction>,
    auxiliary_dispatchable_step_ids: &[StepId],
) -> Vec<AllowedAction> {
    if !auxiliary_dispatchable_step_ids.is_empty()
        && !allowed_actions.contains(&AllowedAction::Dispatch)
    {
        allowed_actions.push(AllowedAction::Dispatch);
    }

    allowed_actions
}

fn is_terminal_closure_job(job: &Job) -> bool {
    job.state.is_terminal()
        && job.state.status() != JobStatus::Superseded
        && is_closure_relevant_step(job.step_id)
}

fn current_closure_step_id(
    latest_closure_job: Option<&Job>,
    prepared_convergence: Option<&Convergence>,
) -> Option<StepId> {
    if let Some(last_job) = latest_closure_job {
        if last_job.step_id == StepId::ValidateIntegrated {
            return Some(StepId::ValidateIntegrated);
        }
    }

    if prepared_convergence.is_some() {
        return Some(StepId::PrepareConvergence);
    }

    latest_closure_job.map(|job| job.step_id)
}

fn triage_projection(
    graph: &WorkflowGraph,
    item: &Item,
    revision: &ItemRevision,
    latest_closure_job: &Job,
    findings: &[&Finding],
    prepared_convergence: Option<&Convergence>,
    diagnostics: &mut Vec<String>,
) -> Option<IdleProjection> {
    let job_findings = findings
        .iter()
        .copied()
        .filter(|finding| finding.source_job_id == latest_closure_job.id)
        .collect::<Vec<_>>();

    if job_findings.is_empty() {
        return None;
    }

    if job_findings
        .iter()
        .any(|finding| finding.triage.is_unresolved())
    {
        return Some(IdleProjection {
            current_step_id: Some(latest_closure_job.step_id),
            phase_status: PhaseStatus::Triaging,
            next_recommended_action: RecommendedAction::named(
                NamedRecommendedAction::TriageFindings,
            ),
            dispatchable_step_id: None,
            allowed_actions: vec![],
            terminal_readiness: false,
        });
    }

    if job_findings
        .iter()
        .any(|finding| finding.triage.state() == FindingTriageState::FixNow)
    {
        return triaged_findings_repair_projection(graph, latest_closure_job, diagnostics);
    }

    triaged_findings_clean_projection(
        graph,
        item,
        revision,
        latest_closure_job,
        prepared_convergence,
        diagnostics,
    )
}

fn triaged_findings_repair_projection(
    graph: &WorkflowGraph,
    latest_closure_job: &Job,
    diagnostics: &mut Vec<String>,
) -> Option<IdleProjection> {
    graph_target_projection(
        Some(latest_closure_job.step_id),
        PhaseStatus::Idle,
        graph.next_step(latest_closure_job.step_id, &OutcomeClass::Findings),
        diagnostics,
    )
}

fn triaged_findings_clean_projection(
    graph: &WorkflowGraph,
    item: &Item,
    revision: &ItemRevision,
    latest_closure_job: &Job,
    prepared_convergence: Option<&Convergence>,
    diagnostics: &mut Vec<String>,
) -> Option<IdleProjection> {
    if latest_closure_job.step_id == StepId::ValidateIntegrated {
        if prepared_convergence.is_none() {
            return Some(IdleProjection {
                current_step_id: Some(StepId::ValidateIntegrated),
                phase_status: PhaseStatus::Unknown,
                next_recommended_action: RecommendedAction::named(
                    NamedRecommendedAction::OperatorIntervention,
                ),
                dispatchable_step_id: None,
                allowed_actions: vec![],
                terminal_readiness: false,
            });
        }

        if revision.approval_policy == ApprovalPolicy::Required {
            if item.approval_state == ApprovalState::Pending {
                return Some(pending_approval_projection(Some(
                    StepId::ValidateIntegrated,
                )));
            }

            return Some(operator_intervention_projection(Some(
                StepId::ValidateIntegrated,
            )));
        }

        return Some(finalization_ready_projection(Some(
            StepId::ValidateIntegrated,
        )));
    }

    graph_target_projection(
        Some(latest_closure_job.step_id),
        PhaseStatus::Idle,
        graph.next_step(latest_closure_job.step_id, &OutcomeClass::Clean),
        diagnostics,
    )
}

fn graph_target_projection(
    current_step_id: Option<StepId>,
    phase_status: PhaseStatus,
    target: Option<&TransitionTarget>,
    diagnostics: &mut Vec<String>,
) -> Option<IdleProjection> {
    match target? {
        TransitionTarget::Step(next_step) => {
            let contract = step::find_step(*next_step);
            if !contract.is_dispatchable_job() {
                return Some(IdleProjection {
                    current_step_id,
                    phase_status: PhaseStatus::AwaitingConvergence,
                    next_recommended_action: RecommendedAction::from_step(*next_step),
                    dispatchable_step_id: None,
                    allowed_actions: vec![AllowedAction::PrepareConvergence],
                    terminal_readiness: false,
                });
            }

            Some(dispatchable_projection(
                current_step_id,
                phase_status,
                *next_step,
            ))
        }
        TransitionTarget::SystemAction(action) => Some(system_action_projection(
            current_step_id,
            action,
            diagnostics,
        )),
        TransitionTarget::Escalation(_) => None,
    }
}

fn dispatchable_projection(
    current_step_id: Option<StepId>,
    phase_status: PhaseStatus,
    next_step: StepId,
) -> IdleProjection {
    IdleProjection {
        current_step_id,
        phase_status,
        next_recommended_action: RecommendedAction::dispatch(next_step),
        dispatchable_step_id: Some(next_step),
        allowed_actions: vec![AllowedAction::Dispatch],
        terminal_readiness: false,
    }
}

fn closure_outcome(last_job: &Job, diagnostics: &mut Vec<String>) -> Option<OutcomeClass> {
    if let Some(outcome) = last_job.state.outcome_class() {
        return Some(outcome);
    }

    diagnostics.push(format!(
        "last closure job {} has no outcome_class despite terminal status {}",
        last_job.step_id,
        job_status_name(last_job.state.status())
    ));
    None
}

fn operator_intervention_projection(current_step_id: Option<StepId>) -> IdleProjection {
    IdleProjection {
        current_step_id,
        phase_status: PhaseStatus::Unknown,
        next_recommended_action: RecommendedAction::named(
            NamedRecommendedAction::OperatorIntervention,
        ),
        dispatchable_step_id: None,
        allowed_actions: vec![],
        terminal_readiness: false,
    }
}

fn pending_approval_projection(current_step_id: Option<StepId>) -> IdleProjection {
    IdleProjection {
        current_step_id,
        phase_status: PhaseStatus::PendingApproval,
        next_recommended_action: RecommendedAction::named(NamedRecommendedAction::ApprovalApprove),
        dispatchable_step_id: None,
        allowed_actions: vec![
            AllowedAction::ApprovalApprove,
            AllowedAction::ApprovalReject,
        ],
        terminal_readiness: false,
    }
}

fn finalization_ready_projection(current_step_id: Option<StepId>) -> IdleProjection {
    IdleProjection {
        current_step_id,
        phase_status: PhaseStatus::FinalizationReady,
        next_recommended_action: RecommendedAction::named(
            NamedRecommendedAction::FinalizePreparedConvergence,
        ),
        dispatchable_step_id: None,
        allowed_actions: vec![],
        terminal_readiness: true,
    }
}

fn is_closure_relevant_step(step_id: StepId) -> bool {
    step::find_step(step_id).closure_relevance == ClosureRelevance::ClosureRelevant
}

fn outcome_class_name(outcome_class: OutcomeClass) -> &'static str {
    outcome_class.as_str()
}

fn job_status_name(job_status: JobStatus) -> &'static str {
    match job_status {
        JobStatus::Queued => "queued",
        JobStatus::Assigned => "assigned",
        JobStatus::Running => "running",
        JobStatus::Completed => "completed",
        JobStatus::Failed => "failed",
        JobStatus::Cancelled => "cancelled",
        JobStatus::Expired => "expired",
        JobStatus::Superseded => "superseded",
    }
}
