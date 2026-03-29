use ingot_domain::convergence::{Convergence, ConvergenceStatus};
use ingot_domain::finding::{Finding, FindingTriageState};
use ingot_domain::item::{ApprovalState, Item, ParkingState};
use ingot_domain::job::{Job, JobStatus, OutcomeClass, PhaseKind};
use ingot_domain::revision::{ApprovalPolicy, ItemRevision};
use ingot_domain::step_id::StepId;

use crate::graph::{TransitionTarget, WorkflowGraph};
use crate::recommended_action::{NamedRecommendedAction, RecommendedAction};
use crate::step::{self, ClosureRelevance};

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

#[derive(Debug, Clone)]
struct IdleProjection {
    current_step_id: Option<StepId>,
    phase_status: PhaseStatus,
    next_recommended_action: RecommendedAction,
    dispatchable_step_id: Option<StepId>,
    allowed_actions: Vec<AllowedAction>,
    terminal_readiness: bool,
}

pub struct Evaluator {
    graph: WorkflowGraph,
}

impl Default for Evaluator {
    fn default() -> Self {
        Self::new()
    }
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

        let latest_closure_job = latest_closure_terminal_job(&current_revision_jobs);
        let has_terminal_closure_job = latest_closure_job.is_some();

        if let Some(job) = active_job {
            let contract = step::find_step(job.step_id);
            let is_report_only = contract.closure_relevance == ClosureRelevance::ReportOnly;

            if is_report_only {
                let base = self.evaluate_idle_projection(
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

        let base = self.evaluate_idle_projection(
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

    fn evaluate_idle_projection(
        &self,
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
                    diagnostics.push(
                        "validate_integrated clean but no prepared convergence exists".into(),
                    );
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
                    if let Some(target) = self.graph.next_step(last_job.step_id, &outcome) {
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

fn is_terminal_closure_job(job: &Job) -> bool {
    job.state.is_terminal()
        && job.state.status() != JobStatus::Superseded
        && is_closure_relevant_step(job.step_id)
}

fn latest_closure_terminal_job<'a>(jobs: &'a [&'a Job]) -> Option<&'a Job> {
    jobs.iter()
        .copied()
        .filter(|job| is_terminal_closure_job(job))
        .max_by_key(|job| (job.state.ended_at(), job.created_at))
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
        return triaged_findings_repair_projection(latest_closure_job, diagnostics);
    }

    triaged_findings_clean_projection(
        item,
        revision,
        latest_closure_job,
        prepared_convergence,
        diagnostics,
    )
}

fn triaged_findings_repair_projection(
    latest_closure_job: &Job,
    diagnostics: &mut Vec<String>,
) -> Option<IdleProjection> {
    graph_target_projection(
        Some(latest_closure_job.step_id),
        PhaseStatus::Idle,
        WorkflowGraph::delivery_v1().next_step(latest_closure_job.step_id, &OutcomeClass::Findings),
        diagnostics,
    )
}

fn triaged_findings_clean_projection(
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
        WorkflowGraph::delivery_v1().next_step(latest_closure_job.step_id, &OutcomeClass::Clean),
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

fn system_action_projection(
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

fn auxiliary_steps(
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

fn merge_allowed_actions(
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
    match outcome_class {
        OutcomeClass::Clean => "clean",
        OutcomeClass::Findings => "findings",
        OutcomeClass::TransientFailure => "transient_failure",
        OutcomeClass::TerminalFailure => "terminal_failure",
        OutcomeClass::ProtocolViolation => "protocol_violation",
        OutcomeClass::Cancelled => "cancelled",
    }
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

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use ingot_domain::finding::FindingTriageState;
    use ingot_domain::ids::{ItemId, ItemRevisionId, ProjectId};
    use ingot_domain::item::ApprovalState;
    use ingot_domain::job::{
        Job, JobInput, JobStatus, OutcomeClass, OutputArtifactKind, PhaseKind,
    };
    use ingot_domain::revision::ApprovalPolicy;
    use ingot_domain::step_id::StepId;
    use ingot_domain::test_support::{
        ConvergenceBuilder, FindingBuilder, JobBuilder, RevisionBuilder, nil_item,
    };
    use uuid::Uuid;

    use super::{BoardStatus, Evaluator, PhaseStatus};
    use crate::{NamedRecommendedAction, RecommendedAction};

    #[test]
    fn unknown_system_actions_degrade_to_operator_intervention() {
        let mut diagnostics = Vec::new();

        let projection = super::system_action_projection(
            Some(StepId::ValidateCandidateInitial),
            "unknown_internal_action",
            &mut diagnostics,
        );

        assert_eq!(projection.phase_status, PhaseStatus::Unknown);
        assert_eq!(
            projection.next_recommended_action,
            RecommendedAction::named(NamedRecommendedAction::OperatorIntervention)
        );
        assert_eq!(
            diagnostics,
            vec!["unknown internal recommended action: unknown_internal_action".to_owned()]
        );
    }

    #[test]
    fn report_only_jobs_keep_the_closure_position_visible() {
        let evaluator = Evaluator::new();
        let item = nil_item();
        let revision = test_revision(ApprovalPolicy::Required);
        let jobs = vec![
            test_job(
                StepId::ReviewIncrementalInitial,
                PhaseKind::Review,
                JobStatus::Completed,
                Some(OutcomeClass::Clean),
            ),
            test_job(
                StepId::InvestigateItem,
                PhaseKind::Investigate,
                JobStatus::Running,
                None,
            ),
        ];

        let evaluation = evaluator.evaluate(&item, &revision, &jobs, &[], &[]);

        assert_eq!(
            evaluation.current_step_id.map(StepId::as_str),
            Some(StepId::ReviewIncrementalInitial.as_str())
        );
        assert_eq!(evaluation.current_phase_kind, Some(PhaseKind::Investigate));
        assert_eq!(evaluation.phase_status, Some(PhaseStatus::Running));
        assert_eq!(evaluation.board_status, BoardStatus::Working);
    }

    #[test]
    fn idle_items_expose_investigation_as_auxiliary_dispatch() {
        let evaluator = Evaluator::new();
        let item = nil_item();
        let revision = test_revision(ApprovalPolicy::Required);
        let jobs = vec![test_job(
            StepId::ReviewIncrementalInitial,
            PhaseKind::Review,
            JobStatus::Completed,
            Some(OutcomeClass::Clean),
        )];

        let evaluation = evaluator.evaluate(&item, &revision, &jobs, &[], &[]);

        assert_eq!(
            evaluation.dispatchable_step_id.map(StepId::as_str),
            Some(StepId::ReviewCandidateInitial.as_str())
        );
        assert_eq!(
            evaluation.auxiliary_dispatchable_step_ids,
            vec![StepId::InvestigateItem]
        );
    }

    #[test]
    fn clean_authoring_commits_flow_into_incremental_review() {
        let evaluator = Evaluator::new();
        let item = nil_item();
        let revision = test_revision(ApprovalPolicy::Required);
        let jobs = vec![test_job(
            StepId::AuthorInitial,
            PhaseKind::Author,
            JobStatus::Completed,
            Some(OutcomeClass::Clean),
        )];

        let evaluation = evaluator.evaluate(&item, &revision, &jobs, &[], &[]);

        assert_eq!(
            evaluation.dispatchable_step_id.map(StepId::as_str),
            Some(StepId::ReviewIncrementalInitial.as_str())
        );
        assert_eq!(
            evaluation.next_recommended_action,
            RecommendedAction::dispatch(StepId::ReviewIncrementalInitial)
        );
    }

    #[test]
    fn clean_whole_candidate_review_flows_to_candidate_validation() {
        let evaluator = Evaluator::new();
        let item = nil_item();
        let revision = test_revision(ApprovalPolicy::Required);
        let jobs = vec![test_job(
            StepId::ReviewCandidateInitial,
            PhaseKind::Review,
            JobStatus::Completed,
            Some(OutcomeClass::Clean),
        )];

        let evaluation = evaluator.evaluate(&item, &revision, &jobs, &[], &[]);

        assert_eq!(
            evaluation.dispatchable_step_id.map(StepId::as_str),
            Some(StepId::ValidateCandidateInitial.as_str())
        );
        assert_eq!(
            evaluation.next_recommended_action,
            RecommendedAction::dispatch(StepId::ValidateCandidateInitial)
        );
    }

    #[test]
    fn daemon_only_next_steps_are_not_projected_as_dispatchable_jobs() {
        let evaluator = Evaluator::new();
        let item = nil_item();
        let revision = test_revision(ApprovalPolicy::Required);
        let jobs = vec![test_job(
            StepId::ValidateCandidateInitial,
            PhaseKind::Validate,
            JobStatus::Completed,
            Some(OutcomeClass::Clean),
        )];

        let evaluation = evaluator.evaluate(&item, &revision, &jobs, &[], &[]);

        assert_eq!(
            evaluation.next_recommended_action,
            RecommendedAction::named(NamedRecommendedAction::PrepareConvergence)
        );
        assert_eq!(evaluation.dispatchable_step_id, None);
        assert_eq!(
            evaluation.phase_status,
            Some(PhaseStatus::AwaitingConvergence)
        );
    }

    #[test]
    fn stale_prepared_convergences_project_invalidation() {
        let evaluator = Evaluator::new();
        let mut item = nil_item();
        item.approval_state = ApprovalState::Pending;

        let revision = test_revision(ApprovalPolicy::Required);
        let jobs = vec![test_job(
            StepId::ValidateIntegrated,
            PhaseKind::Validate,
            JobStatus::Completed,
            Some(OutcomeClass::Clean),
        )];
        let convergences = vec![test_prepared_convergence(false)];

        let evaluation = evaluator.evaluate(&item, &revision, &jobs, &[], &convergences);

        assert_eq!(
            evaluation.next_recommended_action,
            RecommendedAction::named(NamedRecommendedAction::InvalidatePreparedConvergence)
        );
        assert_eq!(evaluation.board_status, BoardStatus::Working);
        assert!(evaluation.allowed_actions.is_empty());
    }

    #[test]
    fn integrated_validation_findings_follow_graph_to_repair() {
        let evaluator = Evaluator::new();
        let item = nil_item();
        let revision = test_revision(ApprovalPolicy::Required);
        let job = test_job(
            StepId::ValidateIntegrated,
            PhaseKind::Validate,
            JobStatus::Completed,
            Some(OutcomeClass::Findings),
        );
        let jobs = vec![job.clone()];
        let findings = vec![test_finding(&job, FindingTriageState::FixNow)];
        let convergences = vec![test_prepared_convergence(true)];

        let evaluation = evaluator.evaluate(&item, &revision, &jobs, &findings, &convergences);

        assert_eq!(
            evaluation.dispatchable_step_id.map(StepId::as_str),
            Some(StepId::RepairAfterIntegration.as_str())
        );
        assert_eq!(
            evaluation.next_recommended_action,
            RecommendedAction::dispatch(StepId::RepairAfterIntegration)
        );
    }

    #[test]
    fn untriaged_findings_block_dispatch_in_triage_state() {
        let evaluator = Evaluator::new();
        let item = nil_item();
        let revision = test_revision(ApprovalPolicy::Required);
        let job = test_job(
            StepId::ReviewCandidateInitial,
            PhaseKind::Review,
            JobStatus::Completed,
            Some(OutcomeClass::Findings),
        );
        let jobs = vec![job.clone()];
        let findings = vec![test_finding(&job, FindingTriageState::Untriaged)];

        let evaluation = evaluator.evaluate(&item, &revision, &jobs, &findings, &[]);

        assert_eq!(evaluation.phase_status, Some(PhaseStatus::Triaging));
        assert_eq!(
            evaluation.next_recommended_action,
            RecommendedAction::named(NamedRecommendedAction::TriageFindings)
        );
        assert_eq!(evaluation.dispatchable_step_id, None);
    }

    #[test]
    fn non_blocking_triaged_findings_follow_clean_edge() {
        let evaluator = Evaluator::new();
        let item = nil_item();
        let revision = test_revision(ApprovalPolicy::Required);
        let job = test_job(
            StepId::ReviewCandidateInitial,
            PhaseKind::Review,
            JobStatus::Completed,
            Some(OutcomeClass::Findings),
        );
        let jobs = vec![job.clone()];
        let findings = vec![test_finding(&job, FindingTriageState::WontFix)];

        let evaluation = evaluator.evaluate(&item, &revision, &jobs, &findings, &[]);

        assert_eq!(
            evaluation.dispatchable_step_id.map(StepId::as_str),
            Some(StepId::ValidateCandidateInitial.as_str())
        );
    }

    #[test]
    fn post_integration_repairs_reenter_incremental_review() {
        let evaluator = Evaluator::new();
        let item = nil_item();
        let revision = test_revision(ApprovalPolicy::Required);
        let jobs = vec![test_job(
            StepId::RepairAfterIntegration,
            PhaseKind::Author,
            JobStatus::Completed,
            Some(OutcomeClass::Clean),
        )];

        let evaluation = evaluator.evaluate(&item, &revision, &jobs, &[], &[]);

        assert_eq!(
            evaluation.dispatchable_step_id.map(StepId::as_str),
            Some(StepId::ReviewIncrementalAfterIntegrationRepair.as_str())
        );
        assert_eq!(
            evaluation.next_recommended_action,
            RecommendedAction::dispatch(StepId::ReviewIncrementalAfterIntegrationRepair)
        );
    }

    #[test]
    fn terminal_jobs_without_outcomes_do_not_advance_workflow() {
        let evaluator = Evaluator::new();
        let item = nil_item();
        let revision = test_revision(ApprovalPolicy::Required);
        let jobs = vec![test_job(
            StepId::ValidateCandidateInitial,
            PhaseKind::Validate,
            JobStatus::Expired,
            None,
        )];

        let evaluation = evaluator.evaluate(&item, &revision, &jobs, &[], &[]);

        assert_eq!(evaluation.dispatchable_step_id, None);
        assert_eq!(evaluation.phase_status, Some(PhaseStatus::Unknown));
        assert!(
            evaluation
                .diagnostics
                .iter()
                .any(|value| value.contains("has no outcome_class"))
        );
    }

    #[test]
    fn cancelled_jobs_do_not_auto_redispatch() {
        let evaluator = Evaluator::new();
        let item = nil_item();
        let revision = test_revision(ApprovalPolicy::Required);
        let jobs = vec![test_job(
            StepId::ValidateCandidateInitial,
            PhaseKind::Validate,
            JobStatus::Cancelled,
            Some(OutcomeClass::Cancelled),
        )];

        let evaluation = evaluator.evaluate(&item, &revision, &jobs, &[], &[]);

        assert_eq!(evaluation.dispatchable_step_id, None);
        assert_eq!(evaluation.next_recommended_action, RecommendedAction::None);
        assert_eq!(
            evaluation.current_step_id.map(StepId::as_str),
            Some(StepId::ValidateCandidateInitial.as_str())
        );
    }

    #[test]
    fn transient_failures_do_not_auto_redispatch_without_retry_policy() {
        let evaluator = Evaluator::new();
        let item = nil_item();
        let revision = test_revision(ApprovalPolicy::Required);
        let jobs = vec![test_job(
            StepId::ValidateCandidateInitial,
            PhaseKind::Validate,
            JobStatus::Failed,
            Some(OutcomeClass::TransientFailure),
        )];

        let evaluation = evaluator.evaluate(&item, &revision, &jobs, &[], &[]);

        assert_eq!(evaluation.dispatchable_step_id, None);
        assert_eq!(evaluation.next_recommended_action, RecommendedAction::None);
        assert_eq!(
            evaluation.current_step_id.map(StepId::as_str),
            Some(StepId::ValidateCandidateInitial.as_str())
        );
    }

    fn test_revision(approval_policy: ApprovalPolicy) -> ingot_domain::revision::ItemRevision {
        RevisionBuilder::nil()
            .approval_policy(approval_policy)
            .explicit_seed("abc123")
            .seed_target_commit_oid(Some("def456"))
            .build()
    }

    fn test_job(
        step_id: StepId,
        phase_kind: PhaseKind,
        status: JobStatus,
        outcome_class: Option<OutcomeClass>,
    ) -> Job {
        let nil = Uuid::nil();
        let output_artifact_kind = match step_id {
            StepId::InvestigateItem => OutputArtifactKind::FindingReport,
            StepId::AuthorInitial | StepId::RepairCandidate | StepId::RepairAfterIntegration => {
                OutputArtifactKind::Commit
            }
            StepId::ValidateIntegrated
            | StepId::ValidateCandidateInitial
            | StepId::ValidateCandidateRepair
            | StepId::ValidateAfterIntegrationRepair => OutputArtifactKind::ValidationReport,
            _ => OutputArtifactKind::ReviewReport,
        };
        let mut builder = JobBuilder::new(
            ProjectId::from_uuid(nil),
            ItemId::from_uuid(nil),
            ItemRevisionId::from_uuid(nil),
            step_id,
        )
        .status(status)
        .phase_kind(phase_kind)
        .execution_permission(ingot_domain::job::ExecutionPermission::MustNotMutate)
        .job_input(JobInput::candidate_subject("base".into(), "head".into()))
        .output_artifact_kind(output_artifact_kind)
        .ended_at(Utc::now());
        if let Some(oc) = outcome_class {
            builder = builder.outcome_class(oc);
        }
        builder.build()
    }

    fn test_prepared_convergence(
        target_head_valid: bool,
    ) -> ingot_domain::convergence::Convergence {
        ConvergenceBuilder::new(
            ProjectId::from_uuid(Uuid::nil()),
            ItemId::from_uuid(Uuid::nil()),
            ItemRevisionId::from_uuid(Uuid::nil()),
        )
        .target_head_valid(target_head_valid)
        .build()
    }

    fn test_finding(job: &Job, triage_state: FindingTriageState) -> ingot_domain::finding::Finding {
        let mut builder = FindingBuilder::new(
            ProjectId::from_uuid(Uuid::nil()),
            ItemId::from_uuid(Uuid::nil()),
            ItemRevisionId::from_uuid(Uuid::nil()),
            job.id,
        )
        .source_step_id(job.step_id)
        .source_finding_key("finding-1")
        .triage_state(triage_state);
        if triage_state == FindingTriageState::WontFix {
            builder = builder.triage_note("accepted");
        }
        if triage_state != FindingTriageState::Untriaged {
            builder = builder.triaged_at(Utc::now());
        }
        builder.build()
    }
}
