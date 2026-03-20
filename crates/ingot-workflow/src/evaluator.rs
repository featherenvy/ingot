use std::fmt;

use ingot_domain::convergence::{Convergence, ConvergenceStatus};
use ingot_domain::finding::{Finding, FindingTriageState};
use ingot_domain::item::{ApprovalState, Item, ParkingState};
use ingot_domain::job::{Job, JobStatus, OutcomeClass, PhaseKind};
use ingot_domain::revision::{ApprovalPolicy, ItemRevision};

use crate::graph::{TransitionTarget, WorkflowGraph};
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecommendedAction {
    None,
    ApprovalApprove,
    OperatorIntervention,
    FinalizePreparedConvergence,
    InvalidatePreparedConvergence,
    TriageFindings,
    PrepareConvergence,
    AwaitConvergenceLane,
    ResolveCheckoutSync,
    /// Catch-all: any step ID that doesn't match a named action above.
    /// `From<String>` routes unrecognized strings here.
    DispatchStep(String),
}

impl RecommendedAction {
    pub fn dispatch(step: &str) -> Self {
        Self::DispatchStep(step.into())
    }

    fn as_str(&self) -> &str {
        match self {
            Self::None => "none",
            Self::ApprovalApprove => "approval_approve",
            Self::OperatorIntervention => "operator_intervention",
            Self::FinalizePreparedConvergence => "finalize_prepared_convergence",
            Self::InvalidatePreparedConvergence => "invalidate_prepared_convergence",
            Self::TriageFindings => "triage_findings",
            Self::PrepareConvergence => "prepare_convergence",
            Self::AwaitConvergenceLane => "await_convergence_lane",
            Self::ResolveCheckoutSync => "resolve_checkout_sync",
            Self::DispatchStep(step_id) => step_id.as_str(),
        }
    }
}

impl fmt::Display for RecommendedAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<RecommendedAction> for String {
    fn from(action: RecommendedAction) -> Self {
        action.as_str().to_owned()
    }
}

impl From<&str> for RecommendedAction {
    fn from(action: &str) -> Self {
        match action {
            "none" => Self::None,
            "approval_approve" => Self::ApprovalApprove,
            "operator_intervention" => Self::OperatorIntervention,
            "finalize_prepared_convergence" => Self::FinalizePreparedConvergence,
            "invalidate_prepared_convergence" => Self::InvalidatePreparedConvergence,
            "triage_findings" => Self::TriageFindings,
            "prepare_convergence" => Self::PrepareConvergence,
            "await_convergence_lane" => Self::AwaitConvergenceLane,
            "resolve_checkout_sync" => Self::ResolveCheckoutSync,
            _ => Self::DispatchStep(action.to_owned()),
        }
    }
}

impl From<String> for RecommendedAction {
    fn from(action: String) -> Self {
        Self::from(action.as_str())
    }
}

impl serde::Serialize for RecommendedAction {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for RecommendedAction {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(Self::from(s))
    }
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
    pub current_step_id: Option<String>,
    pub current_phase_kind: Option<PhaseKind>,
    pub phase_status: Option<PhaseStatus>,
    pub next_recommended_action: RecommendedAction,
    pub dispatchable_step_id: Option<String>,
    pub auxiliary_dispatchable_step_ids: Vec<String>,
    pub allowed_actions: Vec<AllowedAction>,
    pub terminal_readiness: bool,
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone)]
struct IdleProjection {
    current_step_id: Option<String>,
    phase_status: PhaseStatus,
    next_recommended_action: RecommendedAction,
    dispatchable_step_id: Option<String>,
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

        let closure_terminal_jobs = closure_terminal_jobs(&current_revision_jobs);
        let has_terminal_closure_job = !closure_terminal_jobs.is_empty();
        let latest_closure_job = latest_terminal_job(&closure_terminal_jobs);

        if let Some(job) = active_job {
            let contract = step::find_step(&job.step_id);
            let is_report_only = matches!(
                contract.map(|contract| contract.closure_relevance),
                Some(ClosureRelevance::ReportOnly)
            );

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
                    current_step_id: Some(job.step_id.clone()),
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
                    current_step_id: Some(step::PREPARE_CONVERGENCE.into()),
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
                next_recommended_action: RecommendedAction::OperatorIntervention,
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
                next_recommended_action: RecommendedAction::InvalidatePreparedConvergence,
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

            if last_job.step_id == step::VALIDATE_INTEGRATED && outcome == OutcomeClass::Clean {
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

            if outcome == OutcomeClass::Findings && is_closure_relevant_step(&last_job.step_id) {
                if let Some(triage_projection) =
                    triage_projection(item, revision, last_job, findings, prepared_convergence)
                {
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
                    if let Some(target) = self.graph.next_step(&last_job.step_id, &outcome) {
                        match target {
                            TransitionTarget::Step(next_step) => {
                                if *next_step == step::PREPARE_CONVERGENCE
                                    && prepared_convergence.is_some()
                                {
                                    return dispatchable_projection(
                                        Some(step::PREPARE_CONVERGENCE.into()),
                                        PhaseStatus::Idle,
                                        step::VALIDATE_INTEGRATED,
                                    );
                                }

                                if let Some(contract) = step::find_step(next_step) {
                                    if !contract.is_dispatchable_job() {
                                        return IdleProjection {
                                            current_step_id: Some(last_job.step_id.clone()),
                                            phase_status: PhaseStatus::AwaitingConvergence,
                                            next_recommended_action: RecommendedAction::from(
                                                *next_step,
                                            ),
                                            dispatchable_step_id: None,
                                            allowed_actions: vec![
                                                AllowedAction::PrepareConvergence,
                                            ],
                                            terminal_readiness: false,
                                        };
                                    }
                                }

                                return dispatchable_projection(
                                    Some(last_job.step_id.clone()),
                                    PhaseStatus::Idle,
                                    next_step,
                                );
                            }
                            TransitionTarget::SystemAction(action) => {
                                return IdleProjection {
                                    current_step_id: Some(last_job.step_id.clone()),
                                    phase_status: PhaseStatus::AwaitingConvergence,
                                    next_recommended_action: RecommendedAction::from(*action),
                                    dispatchable_step_id: None,
                                    allowed_actions: vec![],
                                    terminal_readiness: false,
                                };
                            }
                            TransitionTarget::Escalation(reason) => {
                                diagnostics.push(format!("escalation expected: {reason}"));
                            }
                        }
                    }
                }
                OutcomeClass::Cancelled | OutcomeClass::TransientFailure => {
                    return IdleProjection {
                        current_step_id: Some(last_job.step_id.clone()),
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
                Some(step::PREPARE_CONVERGENCE.into()),
                PhaseStatus::Idle,
                step::VALIDATE_INTEGRATED,
            );
        }

        if latest_closure_job.is_none() {
            return dispatchable_projection(None, PhaseStatus::New, step::AUTHOR_INITIAL);
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
                != RecommendedAction::InvalidatePreparedConvergence
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

fn closure_terminal_jobs<'a>(jobs: &'a [&'a Job]) -> Vec<&'a Job> {
    jobs.iter()
        .copied()
        .filter(|job| {
            job.state.is_terminal()
                && job.state.status() != JobStatus::Superseded
                && is_closure_relevant_step(&job.step_id)
        })
        .collect()
}

fn latest_terminal_job<'a>(jobs: &'a [&'a Job]) -> Option<&'a Job> {
    jobs.iter()
        .copied()
        .max_by_key(|job| (job.state.ended_at(), job.created_at))
}

fn current_closure_step_id(
    latest_closure_job: Option<&Job>,
    prepared_convergence: Option<&Convergence>,
) -> Option<String> {
    if let Some(last_job) = latest_closure_job {
        if last_job.step_id == step::VALIDATE_INTEGRATED {
            return Some(step::VALIDATE_INTEGRATED.into());
        }
    }

    if prepared_convergence.is_some() {
        return Some(step::PREPARE_CONVERGENCE.into());
    }

    latest_closure_job.map(|job| job.step_id.clone())
}

fn triage_projection(
    item: &Item,
    revision: &ItemRevision,
    latest_closure_job: &Job,
    findings: &[&Finding],
    prepared_convergence: Option<&Convergence>,
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
            current_step_id: Some(latest_closure_job.step_id.clone()),
            phase_status: PhaseStatus::Triaging,
            next_recommended_action: RecommendedAction::TriageFindings,
            dispatchable_step_id: None,
            allowed_actions: vec![],
            terminal_readiness: false,
        });
    }

    if job_findings
        .iter()
        .any(|finding| finding.triage.state() == FindingTriageState::FixNow)
    {
        return triaged_findings_repair_projection(latest_closure_job);
    }

    triaged_findings_clean_projection(item, revision, latest_closure_job, prepared_convergence)
}

fn triaged_findings_repair_projection(latest_closure_job: &Job) -> Option<IdleProjection> {
    graph_target_projection(
        Some(latest_closure_job.step_id.clone()),
        PhaseStatus::Idle,
        WorkflowGraph::delivery_v1()
            .next_step(&latest_closure_job.step_id, &OutcomeClass::Findings),
    )
}

fn triaged_findings_clean_projection(
    item: &Item,
    revision: &ItemRevision,
    latest_closure_job: &Job,
    prepared_convergence: Option<&Convergence>,
) -> Option<IdleProjection> {
    if latest_closure_job.step_id == step::VALIDATE_INTEGRATED {
        if prepared_convergence.is_none() {
            return Some(IdleProjection {
                current_step_id: Some(step::VALIDATE_INTEGRATED.into()),
                phase_status: PhaseStatus::Unknown,
                next_recommended_action: RecommendedAction::OperatorIntervention,
                dispatchable_step_id: None,
                allowed_actions: vec![],
                terminal_readiness: false,
            });
        }

        if revision.approval_policy == ApprovalPolicy::Required {
            if item.approval_state == ApprovalState::Pending {
                return Some(pending_approval_projection(Some(
                    step::VALIDATE_INTEGRATED.into(),
                )));
            }

            return Some(operator_intervention_projection(Some(
                step::VALIDATE_INTEGRATED.into(),
            )));
        }

        return Some(finalization_ready_projection(Some(
            step::VALIDATE_INTEGRATED.into(),
        )));
    }

    graph_target_projection(
        Some(latest_closure_job.step_id.clone()),
        PhaseStatus::Idle,
        WorkflowGraph::delivery_v1().next_step(&latest_closure_job.step_id, &OutcomeClass::Clean),
    )
}

fn graph_target_projection(
    current_step_id: Option<String>,
    phase_status: PhaseStatus,
    target: Option<&TransitionTarget>,
) -> Option<IdleProjection> {
    match target? {
        TransitionTarget::Step(next_step) => Some(dispatchable_projection(
            current_step_id,
            phase_status,
            next_step,
        )),
        TransitionTarget::SystemAction(action) => Some(IdleProjection {
            current_step_id,
            phase_status: PhaseStatus::AwaitingConvergence,
            next_recommended_action: RecommendedAction::from(*action),
            dispatchable_step_id: None,
            allowed_actions: vec![],
            terminal_readiness: false,
        }),
        TransitionTarget::Escalation(_) => None,
    }
}

fn dispatchable_projection(
    current_step_id: Option<String>,
    phase_status: PhaseStatus,
    next_step: &'static str,
) -> IdleProjection {
    IdleProjection {
        current_step_id,
        phase_status,
        next_recommended_action: RecommendedAction::dispatch(next_step),
        dispatchable_step_id: Some(next_step.into()),
        allowed_actions: vec![AllowedAction::Dispatch],
        terminal_readiness: false,
    }
}

fn auxiliary_steps(
    item: &Item,
    next_action: &RecommendedAction,
    phase_status: PhaseStatus,
) -> Vec<String> {
    if !item.lifecycle.is_open()
        || item.parking_state != ParkingState::Active
        || item.approval_state == ApprovalState::Pending
        || item.escalation.is_escalated()
        || !matches!(phase_status, PhaseStatus::New | PhaseStatus::Idle)
        || is_daemon_action(next_action)
    {
        return vec![];
    }

    vec![step::INVESTIGATE_ITEM.into()]
}

fn merge_allowed_actions(
    mut allowed_actions: Vec<AllowedAction>,
    auxiliary_dispatchable_step_ids: &[String],
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

fn operator_intervention_projection(current_step_id: Option<String>) -> IdleProjection {
    IdleProjection {
        current_step_id,
        phase_status: PhaseStatus::Unknown,
        next_recommended_action: RecommendedAction::OperatorIntervention,
        dispatchable_step_id: None,
        allowed_actions: vec![],
        terminal_readiness: false,
    }
}

fn pending_approval_projection(current_step_id: Option<String>) -> IdleProjection {
    IdleProjection {
        current_step_id,
        phase_status: PhaseStatus::PendingApproval,
        next_recommended_action: RecommendedAction::ApprovalApprove,
        dispatchable_step_id: None,
        allowed_actions: vec![
            AllowedAction::ApprovalApprove,
            AllowedAction::ApprovalReject,
        ],
        terminal_readiness: false,
    }
}

fn finalization_ready_projection(current_step_id: Option<String>) -> IdleProjection {
    IdleProjection {
        current_step_id,
        phase_status: PhaseStatus::FinalizationReady,
        next_recommended_action: RecommendedAction::FinalizePreparedConvergence,
        dispatchable_step_id: None,
        allowed_actions: vec![],
        terminal_readiness: true,
    }
}

fn is_daemon_action(action: &RecommendedAction) -> bool {
    matches!(
        action,
        RecommendedAction::PrepareConvergence
            | RecommendedAction::FinalizePreparedConvergence
            | RecommendedAction::InvalidatePreparedConvergence
    )
}

fn is_closure_relevant_step(step_id: &str) -> bool {
    matches!(
        step::find_step(step_id).map(|contract| contract.closure_relevance),
        Some(ClosureRelevance::ClosureRelevant)
    )
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
    use ingot_test_support::fixtures::{
        ConvergenceBuilder, FindingBuilder, JobBuilder, RevisionBuilder, nil_item,
    };
    use uuid::Uuid;

    use super::{BoardStatus, Evaluator, PhaseStatus, RecommendedAction};
    use crate::step;

    #[test]
    fn report_only_jobs_keep_the_closure_position_visible() {
        let evaluator = Evaluator::new();
        let item = nil_item();
        let revision = test_revision(ApprovalPolicy::Required);
        let jobs = vec![
            test_job(
                step::REVIEW_INCREMENTAL_INITIAL,
                PhaseKind::Review,
                JobStatus::Completed,
                Some(OutcomeClass::Clean),
            ),
            test_job(
                step::INVESTIGATE_ITEM,
                PhaseKind::Investigate,
                JobStatus::Running,
                None,
            ),
        ];

        let evaluation = evaluator.evaluate(&item, &revision, &jobs, &[], &[]);

        assert_eq!(
            evaluation.current_step_id.as_deref(),
            Some(step::REVIEW_INCREMENTAL_INITIAL)
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
            step::REVIEW_INCREMENTAL_INITIAL,
            PhaseKind::Review,
            JobStatus::Completed,
            Some(OutcomeClass::Clean),
        )];

        let evaluation = evaluator.evaluate(&item, &revision, &jobs, &[], &[]);

        assert_eq!(
            evaluation.dispatchable_step_id.as_deref(),
            Some(step::REVIEW_CANDIDATE_INITIAL)
        );
        assert_eq!(
            evaluation.auxiliary_dispatchable_step_ids,
            vec![step::INVESTIGATE_ITEM.to_string()]
        );
    }

    #[test]
    fn clean_authoring_commits_flow_into_incremental_review() {
        let evaluator = Evaluator::new();
        let item = nil_item();
        let revision = test_revision(ApprovalPolicy::Required);
        let jobs = vec![test_job(
            step::AUTHOR_INITIAL,
            PhaseKind::Author,
            JobStatus::Completed,
            Some(OutcomeClass::Clean),
        )];

        let evaluation = evaluator.evaluate(&item, &revision, &jobs, &[], &[]);

        assert_eq!(
            evaluation.dispatchable_step_id.as_deref(),
            Some(step::REVIEW_INCREMENTAL_INITIAL)
        );
        assert_eq!(
            evaluation.next_recommended_action,
            RecommendedAction::dispatch(step::REVIEW_INCREMENTAL_INITIAL)
        );
    }

    #[test]
    fn clean_whole_candidate_review_flows_to_candidate_validation() {
        let evaluator = Evaluator::new();
        let item = nil_item();
        let revision = test_revision(ApprovalPolicy::Required);
        let jobs = vec![test_job(
            step::REVIEW_CANDIDATE_INITIAL,
            PhaseKind::Review,
            JobStatus::Completed,
            Some(OutcomeClass::Clean),
        )];

        let evaluation = evaluator.evaluate(&item, &revision, &jobs, &[], &[]);

        assert_eq!(
            evaluation.dispatchable_step_id.as_deref(),
            Some(step::VALIDATE_CANDIDATE_INITIAL)
        );
        assert_eq!(
            evaluation.next_recommended_action,
            RecommendedAction::dispatch(step::VALIDATE_CANDIDATE_INITIAL)
        );
    }

    #[test]
    fn daemon_only_next_steps_are_not_projected_as_dispatchable_jobs() {
        let evaluator = Evaluator::new();
        let item = nil_item();
        let revision = test_revision(ApprovalPolicy::Required);
        let jobs = vec![test_job(
            step::VALIDATE_CANDIDATE_INITIAL,
            PhaseKind::Validate,
            JobStatus::Completed,
            Some(OutcomeClass::Clean),
        )];

        let evaluation = evaluator.evaluate(&item, &revision, &jobs, &[], &[]);

        assert_eq!(
            evaluation.next_recommended_action,
            RecommendedAction::PrepareConvergence
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
            step::VALIDATE_INTEGRATED,
            PhaseKind::Validate,
            JobStatus::Completed,
            Some(OutcomeClass::Clean),
        )];
        let convergences = vec![test_prepared_convergence(false)];

        let evaluation = evaluator.evaluate(&item, &revision, &jobs, &[], &convergences);

        assert_eq!(
            evaluation.next_recommended_action,
            RecommendedAction::InvalidatePreparedConvergence
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
            step::VALIDATE_INTEGRATED,
            PhaseKind::Validate,
            JobStatus::Completed,
            Some(OutcomeClass::Findings),
        );
        let jobs = vec![job.clone()];
        let findings = vec![test_finding(&job, FindingTriageState::FixNow)];
        let convergences = vec![test_prepared_convergence(true)];

        let evaluation = evaluator.evaluate(&item, &revision, &jobs, &findings, &convergences);

        assert_eq!(
            evaluation.dispatchable_step_id.as_deref(),
            Some(step::REPAIR_AFTER_INTEGRATION)
        );
        assert_eq!(
            evaluation.next_recommended_action,
            RecommendedAction::dispatch(step::REPAIR_AFTER_INTEGRATION)
        );
    }

    #[test]
    fn untriaged_findings_block_dispatch_in_triage_state() {
        let evaluator = Evaluator::new();
        let item = nil_item();
        let revision = test_revision(ApprovalPolicy::Required);
        let job = test_job(
            step::REVIEW_CANDIDATE_INITIAL,
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
            RecommendedAction::TriageFindings
        );
        assert_eq!(evaluation.dispatchable_step_id, None);
    }

    #[test]
    fn non_blocking_triaged_findings_follow_clean_edge() {
        let evaluator = Evaluator::new();
        let item = nil_item();
        let revision = test_revision(ApprovalPolicy::Required);
        let job = test_job(
            step::REVIEW_CANDIDATE_INITIAL,
            PhaseKind::Review,
            JobStatus::Completed,
            Some(OutcomeClass::Findings),
        );
        let jobs = vec![job.clone()];
        let findings = vec![test_finding(&job, FindingTriageState::WontFix)];

        let evaluation = evaluator.evaluate(&item, &revision, &jobs, &findings, &[]);

        assert_eq!(
            evaluation.dispatchable_step_id.as_deref(),
            Some(step::VALIDATE_CANDIDATE_INITIAL)
        );
    }

    #[test]
    fn post_integration_repairs_reenter_incremental_review() {
        let evaluator = Evaluator::new();
        let item = nil_item();
        let revision = test_revision(ApprovalPolicy::Required);
        let jobs = vec![test_job(
            step::REPAIR_AFTER_INTEGRATION,
            PhaseKind::Author,
            JobStatus::Completed,
            Some(OutcomeClass::Clean),
        )];

        let evaluation = evaluator.evaluate(&item, &revision, &jobs, &[], &[]);

        assert_eq!(
            evaluation.dispatchable_step_id.as_deref(),
            Some(step::REVIEW_INCREMENTAL_AFTER_INTEGRATION_REPAIR)
        );
        assert_eq!(
            evaluation.next_recommended_action,
            RecommendedAction::dispatch(step::REVIEW_INCREMENTAL_AFTER_INTEGRATION_REPAIR)
        );
    }

    #[test]
    fn terminal_jobs_without_outcomes_do_not_advance_workflow() {
        let evaluator = Evaluator::new();
        let item = nil_item();
        let revision = test_revision(ApprovalPolicy::Required);
        let jobs = vec![test_job(
            step::VALIDATE_CANDIDATE_INITIAL,
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
            step::VALIDATE_CANDIDATE_INITIAL,
            PhaseKind::Validate,
            JobStatus::Cancelled,
            Some(OutcomeClass::Cancelled),
        )];

        let evaluation = evaluator.evaluate(&item, &revision, &jobs, &[], &[]);

        assert_eq!(evaluation.dispatchable_step_id, None);
        assert_eq!(evaluation.next_recommended_action, RecommendedAction::None);
        assert_eq!(
            evaluation.current_step_id.as_deref(),
            Some(step::VALIDATE_CANDIDATE_INITIAL)
        );
    }

    #[test]
    fn transient_failures_do_not_auto_redispatch_without_retry_policy() {
        let evaluator = Evaluator::new();
        let item = nil_item();
        let revision = test_revision(ApprovalPolicy::Required);
        let jobs = vec![test_job(
            step::VALIDATE_CANDIDATE_INITIAL,
            PhaseKind::Validate,
            JobStatus::Failed,
            Some(OutcomeClass::TransientFailure),
        )];

        let evaluation = evaluator.evaluate(&item, &revision, &jobs, &[], &[]);

        assert_eq!(evaluation.dispatchable_step_id, None);
        assert_eq!(evaluation.next_recommended_action, RecommendedAction::None);
        assert_eq!(
            evaluation.current_step_id.as_deref(),
            Some(step::VALIDATE_CANDIDATE_INITIAL)
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
        step_id: &str,
        phase_kind: PhaseKind,
        status: JobStatus,
        outcome_class: Option<OutcomeClass>,
    ) -> Job {
        let nil = Uuid::nil();
        let output_artifact_kind = match step_id {
            step::INVESTIGATE_ITEM => OutputArtifactKind::FindingReport,
            step::AUTHOR_INITIAL | step::REPAIR_CANDIDATE | step::REPAIR_AFTER_INTEGRATION => {
                OutputArtifactKind::Commit
            }
            step::VALIDATE_INTEGRATED
            | step::VALIDATE_CANDIDATE_INITIAL
            | step::VALIDATE_CANDIDATE_REPAIR
            | step::VALIDATE_AFTER_INTEGRATION_REPAIR => OutputArtifactKind::ValidationReport,
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
        .source_step_id(job.step_id.clone())
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
