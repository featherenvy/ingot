use ingot_domain::convergence::{Convergence, ConvergenceStatus};
use ingot_domain::finding::{Finding, FindingTriageState};
use ingot_domain::item::{ApprovalState, EscalationState, Item, LifecycleState, ParkingState};
use ingot_domain::job::{Job, JobStatus, OutcomeClass, PhaseKind};
use ingot_domain::revision::{ApprovalPolicy, ItemRevision};

use crate::graph::{TransitionTarget, WorkflowGraph};
use crate::step::{self, ClosureRelevance};

const ACTION_NONE: &str = "none";
const ACTION_APPROVAL_APPROVE: &str = "approval_approve";
const ACTION_OPERATOR_INTERVENTION: &str = "operator_intervention";
const ACTION_FINALIZE_PREPARED_CONVERGENCE: &str = "finalize_prepared_convergence";
const ACTION_INVALIDATE_PREPARED_CONVERGENCE: &str = "invalidate_prepared_convergence";
const ACTION_TRIAGE_FINDINGS: &str = "triage_findings";

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
    pub attention_badges: Vec<String>,
    pub current_step_id: Option<String>,
    pub current_phase_kind: Option<String>,
    pub phase_status: Option<String>,
    pub next_recommended_action: String,
    pub dispatchable_step_id: Option<String>,
    pub auxiliary_dispatchable_step_ids: Vec<String>,
    pub allowed_actions: Vec<String>,
    pub terminal_readiness: bool,
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone)]
struct IdleProjection {
    current_step_id: Option<String>,
    phase_status: &'static str,
    next_recommended_action: String,
    dispatchable_step_id: Option<String>,
    allowed_actions: Vec<String>,
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

        if item.escalation_state == EscalationState::OperatorRequired {
            attention_badges.push("escalated".into());
        }
        if item.parking_state == ParkingState::Deferred {
            attention_badges.push("deferred".into());
        }

        if item.lifecycle_state == LifecycleState::Done {
            return Evaluation {
                board_status: BoardStatus::Done,
                attention_badges,
                current_step_id: None,
                current_phase_kind: None,
                phase_status: Some("done".into()),
                next_recommended_action: ACTION_NONE.into(),
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
            .find(|job| job.status.is_active());
        let active_convergence = current_revision_convergences.iter().copied().find(|conv| {
            matches!(
                conv.status,
                ConvergenceStatus::Queued | ConvergenceStatus::Running
            )
        });
        let prepared_convergence = current_revision_convergences
            .iter()
            .copied()
            .find(|conv| conv.status == ConvergenceStatus::Prepared);

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
                        current_phase_kind: Some(phase_kind_name(job.phase_kind).into()),
                        phase_status: Some("running".into()),
                        next_recommended_action: ACTION_NONE.into(),
                        dispatchable_step_id: None,
                        auxiliary_dispatchable_step_ids: vec![],
                        allowed_actions: vec!["cancel_job".into()],
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
                    current_phase_kind: Some(phase_kind_name(job.phase_kind).into()),
                    phase_status: Some("running".into()),
                    next_recommended_action: ACTION_NONE.into(),
                    dispatchable_step_id: None,
                    auxiliary_dispatchable_step_ids: vec![],
                    allowed_actions: vec!["cancel_job".into()],
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
                    current_phase_kind: Some(phase_kind_name(PhaseKind::System).into()),
                    phase_status: Some("running".into()),
                    next_recommended_action: ACTION_NONE.into(),
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
                phase_status: Some(base.phase_status.into()),
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

        if item.escalation_state == EscalationState::OperatorRequired {
            return IdleProjection {
                current_step_id,
                phase_status: "escalated",
                next_recommended_action: ACTION_OPERATOR_INTERVENTION.into(),
                dispatchable_step_id: None,
                allowed_actions: vec![
                    "revise".into(),
                    "dismiss".into(),
                    "invalidate".into(),
                    "defer".into(),
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
                phase_status: "idle",
                next_recommended_action: ACTION_INVALIDATE_PREPARED_CONVERGENCE.into(),
                dispatchable_step_id: None,
                allowed_actions: vec![],
                terminal_readiness: false,
            };
        }

        if item.parking_state == ParkingState::Deferred {
            return IdleProjection {
                current_step_id,
                phase_status: "deferred",
                next_recommended_action: ACTION_NONE.into(),
                dispatchable_step_id: None,
                allowed_actions: vec!["resume".into()],
                terminal_readiness: false,
            };
        }

        if let Some(last_job) = latest_closure_job {
            let Some(outcome) = last_job.outcome_class else {
                diagnostics.push(format!(
                    "last closure job {} has no outcome_class despite terminal status {}",
                    last_job.step_id,
                    job_status_name(last_job.status)
                ));

                return IdleProjection {
                    current_step_id,
                    phase_status: "unknown",
                    next_recommended_action: ACTION_OPERATOR_INTERVENTION.into(),
                    dispatchable_step_id: None,
                    allowed_actions: vec![],
                    terminal_readiness: false,
                };
            };

            if last_job.step_id == step::VALIDATE_INTEGRATED && outcome == OutcomeClass::Clean {
                if prepared_convergence.is_none() {
                    if item.approval_state == ApprovalState::Granted {
                        return IdleProjection {
                            current_step_id,
                            phase_status: "awaiting_convergence",
                            next_recommended_action: step::PREPARE_CONVERGENCE.into(),
                            dispatchable_step_id: None,
                            allowed_actions: vec![],
                            terminal_readiness: false,
                        };
                    }
                    diagnostics.push(
                        "validate_integrated clean but no prepared convergence exists".into(),
                    );
                    return IdleProjection {
                        current_step_id,
                        phase_status: "unknown",
                        next_recommended_action: ACTION_OPERATOR_INTERVENTION.into(),
                        dispatchable_step_id: None,
                        allowed_actions: vec![],
                        terminal_readiness: false,
                    };
                } else if item.approval_state == ApprovalState::Pending {
                    return IdleProjection {
                        current_step_id,
                        phase_status: "pending_approval",
                        next_recommended_action: ACTION_APPROVAL_APPROVE.into(),
                        dispatchable_step_id: None,
                        allowed_actions: vec!["approval_approve".into(), "approval_reject".into()],
                        terminal_readiness: false,
                    };
                } else if revision.approval_policy == ApprovalPolicy::NotRequired {
                    return IdleProjection {
                        current_step_id,
                        phase_status: "finalization_ready",
                        next_recommended_action: ACTION_FINALIZE_PREPARED_CONVERGENCE.into(),
                        dispatchable_step_id: None,
                        allowed_actions: vec![],
                        terminal_readiness: true,
                    };
                } else {
                    diagnostics
                        .push("validate_integrated clean but approval_state is not pending".into());
                    return IdleProjection {
                        current_step_id,
                        phase_status: "unknown",
                        next_recommended_action: ACTION_OPERATOR_INTERVENTION.into(),
                        dispatchable_step_id: None,
                        allowed_actions: vec![],
                        terminal_readiness: false,
                    };
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
            let Some(outcome) = last_job.outcome_class else {
                diagnostics.push(format!(
                    "last closure job {} has no outcome_class despite terminal status {}",
                    last_job.step_id,
                    job_status_name(last_job.status)
                ));

                return IdleProjection {
                    current_step_id,
                    phase_status: "unknown",
                    next_recommended_action: ACTION_OPERATOR_INTERVENTION.into(),
                    dispatchable_step_id: None,
                    allowed_actions: vec![],
                    terminal_readiness: false,
                };
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
                                        "idle",
                                        step::VALIDATE_INTEGRATED,
                                    );
                                }

                                if let Some(contract) = step::find_step(next_step) {
                                    if contract.execution_permission
                                        == ingot_domain::job::ExecutionPermission::DaemonOnly
                                    {
                                        return IdleProjection {
                                            current_step_id: Some(last_job.step_id.clone()),
                                            phase_status: "awaiting_convergence",
                                            next_recommended_action: (*next_step).into(),
                                            dispatchable_step_id: None,
                                            allowed_actions: vec!["prepare_convergence".into()],
                                            terminal_readiness: false,
                                        };
                                    }
                                }

                                return dispatchable_projection(
                                    Some(last_job.step_id.clone()),
                                    "idle",
                                    next_step,
                                );
                            }
                            TransitionTarget::SystemAction(action) => {
                                return IdleProjection {
                                    current_step_id: Some(last_job.step_id.clone()),
                                    phase_status: "awaiting_convergence",
                                    next_recommended_action: (*action).into(),
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
                        phase_status: "idle",
                        next_recommended_action: ACTION_NONE.into(),
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
                "idle",
                step::VALIDATE_INTEGRATED,
            );
        }

        if latest_closure_job.is_none() {
            return dispatchable_projection(None, "new", step::AUTHOR_INITIAL);
        }

        IdleProjection {
            current_step_id,
            phase_status: "unknown",
            next_recommended_action: ACTION_OPERATOR_INTERVENTION.into(),
            dispatchable_step_id: None,
            allowed_actions: vec![],
            terminal_readiness: false,
        }
    }

    fn finish_evaluation(
        &self,
        item: &Item,
        has_terminal_closure_job: bool,
        attention_badges: Vec<String>,
        mut evaluation: Evaluation,
    ) -> Evaluation {
        evaluation.attention_badges = attention_badges;

        evaluation.board_status = if item.lifecycle_state == LifecycleState::Done {
            BoardStatus::Done
        } else if item.approval_state == ApprovalState::Pending
            && evaluation.next_recommended_action != ACTION_INVALIDATE_PREPARED_CONVERGENCE
        {
            BoardStatus::Approval
        } else if evaluation.phase_status.as_deref() == Some("running") || has_terminal_closure_job
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
            job.status.is_terminal()
                && job.status != JobStatus::Superseded
                && is_closure_relevant_step(&job.step_id)
        })
        .collect()
}

fn latest_terminal_job<'a>(jobs: &'a [&'a Job]) -> Option<&'a Job> {
    jobs.iter()
        .copied()
        .max_by_key(|job| (job.ended_at, job.created_at))
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

    if job_findings.iter().any(|finding| {
        matches!(
            finding.triage_state,
            FindingTriageState::Untriaged | FindingTriageState::NeedsInvestigation
        )
    }) {
        return Some(IdleProjection {
            current_step_id: Some(latest_closure_job.step_id.clone()),
            phase_status: "triaging",
            next_recommended_action: ACTION_TRIAGE_FINDINGS.into(),
            dispatchable_step_id: None,
            allowed_actions: vec![],
            terminal_readiness: false,
        });
    }

    if job_findings
        .iter()
        .any(|finding| finding.triage_state == FindingTriageState::FixNow)
    {
        return triaged_findings_repair_projection(latest_closure_job);
    }

    triaged_findings_clean_projection(item, revision, latest_closure_job, prepared_convergence)
}

fn triaged_findings_repair_projection(latest_closure_job: &Job) -> Option<IdleProjection> {
    graph_target_projection(
        Some(latest_closure_job.step_id.clone()),
        "idle",
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
            if item.approval_state == ApprovalState::Granted {
                return Some(IdleProjection {
                    current_step_id: Some(step::VALIDATE_INTEGRATED.into()),
                    phase_status: "awaiting_convergence",
                    next_recommended_action: step::PREPARE_CONVERGENCE.into(),
                    dispatchable_step_id: None,
                    allowed_actions: vec![],
                    terminal_readiness: false,
                });
            }
            return Some(IdleProjection {
                current_step_id: Some(step::VALIDATE_INTEGRATED.into()),
                phase_status: "unknown",
                next_recommended_action: ACTION_OPERATOR_INTERVENTION.into(),
                dispatchable_step_id: None,
                allowed_actions: vec![],
                terminal_readiness: false,
            });
        }

        if revision.approval_policy == ApprovalPolicy::Required {
            if item.approval_state == ApprovalState::Pending {
                return Some(IdleProjection {
                    current_step_id: Some(step::VALIDATE_INTEGRATED.into()),
                    phase_status: "pending_approval",
                    next_recommended_action: ACTION_APPROVAL_APPROVE.into(),
                    dispatchable_step_id: None,
                    allowed_actions: vec!["approval_approve".into(), "approval_reject".into()],
                    terminal_readiness: false,
                });
            }

            return Some(IdleProjection {
                current_step_id: Some(step::VALIDATE_INTEGRATED.into()),
                phase_status: "unknown",
                next_recommended_action: ACTION_OPERATOR_INTERVENTION.into(),
                dispatchable_step_id: None,
                allowed_actions: vec![],
                terminal_readiness: false,
            });
        }

        return Some(IdleProjection {
            current_step_id: Some(step::VALIDATE_INTEGRATED.into()),
            phase_status: "finalization_ready",
            next_recommended_action: ACTION_FINALIZE_PREPARED_CONVERGENCE.into(),
            dispatchable_step_id: None,
            allowed_actions: vec![],
            terminal_readiness: true,
        });
    }

    graph_target_projection(
        Some(latest_closure_job.step_id.clone()),
        "idle",
        WorkflowGraph::delivery_v1().next_step(&latest_closure_job.step_id, &OutcomeClass::Clean),
    )
}

fn graph_target_projection(
    current_step_id: Option<String>,
    phase_status: &'static str,
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
            phase_status: "awaiting_convergence",
            next_recommended_action: (*action).into(),
            dispatchable_step_id: None,
            allowed_actions: vec![],
            terminal_readiness: false,
        }),
        TransitionTarget::Escalation(_) => None,
    }
}

fn dispatchable_projection(
    current_step_id: Option<String>,
    phase_status: &'static str,
    next_step: &'static str,
) -> IdleProjection {
    dispatchable_projection_owned(current_step_id, phase_status, next_step.to_string())
}

fn dispatchable_projection_owned(
    current_step_id: Option<String>,
    phase_status: &'static str,
    next_step: String,
) -> IdleProjection {
    IdleProjection {
        current_step_id,
        phase_status,
        next_recommended_action: next_step.clone(),
        dispatchable_step_id: Some(next_step),
        allowed_actions: vec!["dispatch".into()],
        terminal_readiness: false,
    }
}

fn auxiliary_steps(item: &Item, next_action: &str, phase_status: &str) -> Vec<String> {
    if item.lifecycle_state != LifecycleState::Open
        || item.parking_state != ParkingState::Active
        || item.approval_state == ApprovalState::Pending
        || item.escalation_state == EscalationState::OperatorRequired
        || !matches!(phase_status, "new" | "idle")
        || is_daemon_action(next_action)
    {
        return vec![];
    }

    vec![step::INVESTIGATE_ITEM.into()]
}

fn merge_allowed_actions(
    mut allowed_actions: Vec<String>,
    auxiliary_dispatchable_step_ids: &[String],
) -> Vec<String> {
    if !auxiliary_dispatchable_step_ids.is_empty()
        && !allowed_actions.iter().any(|action| action == "dispatch")
    {
        allowed_actions.push("dispatch".into());
    }

    allowed_actions
}

fn is_daemon_action(action: &str) -> bool {
    matches!(
        action,
        step::PREPARE_CONVERGENCE
            | ACTION_FINALIZE_PREPARED_CONVERGENCE
            | ACTION_INVALIDATE_PREPARED_CONVERGENCE
    )
}

fn is_closure_relevant_step(step_id: &str) -> bool {
    matches!(
        step::find_step(step_id).map(|contract| contract.closure_relevance),
        Some(ClosureRelevance::ClosureRelevant)
    )
}

fn phase_kind_name(phase_kind: PhaseKind) -> &'static str {
    match phase_kind {
        PhaseKind::Author => "author",
        PhaseKind::Validate => "validate",
        PhaseKind::Review => "review",
        PhaseKind::Investigate => "investigate",
        PhaseKind::System => "system",
    }
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
    use ingot_domain::convergence::{Convergence, ConvergenceStatus, ConvergenceStrategy};
    use ingot_domain::finding::{Finding, FindingSeverity, FindingSubjectKind, FindingTriageState};
    use ingot_domain::ids::{
        ConvergenceId, FindingId, ItemId, ItemRevisionId, ProjectId, WorkspaceId,
    };
    use ingot_domain::item::{
        ApprovalState, Classification, EscalationState, Item, LifecycleState, OriginKind,
        ParkingState, Priority,
    };
    use ingot_domain::job::{
        ContextPolicy, ExecutionPermission, Job, JobStatus, OutcomeClass, OutputArtifactKind,
        PhaseKind,
    };
    use ingot_domain::revision::{ApprovalPolicy, ItemRevision};
    use ingot_domain::workspace::WorkspaceKind;
    use uuid::Uuid;

    use super::{BoardStatus, Evaluator};
    use crate::step;

    #[test]
    fn report_only_jobs_keep_the_closure_position_visible() {
        let evaluator = Evaluator::new();
        let item = test_item();
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
        assert_eq!(
            evaluation.current_phase_kind.as_deref(),
            Some("investigate")
        );
        assert_eq!(evaluation.phase_status.as_deref(), Some("running"));
        assert_eq!(evaluation.board_status, BoardStatus::Working);
    }

    #[test]
    fn idle_items_expose_investigation_as_auxiliary_dispatch() {
        let evaluator = Evaluator::new();
        let item = test_item();
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
        let item = test_item();
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
            step::REVIEW_INCREMENTAL_INITIAL
        );
    }

    #[test]
    fn clean_whole_candidate_review_flows_to_candidate_validation() {
        let evaluator = Evaluator::new();
        let item = test_item();
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
            step::VALIDATE_CANDIDATE_INITIAL
        );
    }

    #[test]
    fn daemon_only_next_steps_are_not_projected_as_dispatchable_jobs() {
        let evaluator = Evaluator::new();
        let item = test_item();
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
            step::PREPARE_CONVERGENCE
        );
        assert_eq!(evaluation.dispatchable_step_id, None);
        assert_eq!(
            evaluation.phase_status.as_deref(),
            Some("awaiting_convergence")
        );
    }

    #[test]
    fn stale_prepared_convergences_project_invalidation() {
        let evaluator = Evaluator::new();
        let mut item = test_item();
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
            "invalidate_prepared_convergence"
        );
        assert_eq!(evaluation.board_status, BoardStatus::Working);
        assert!(evaluation.allowed_actions.is_empty());
    }

    #[test]
    fn granted_without_prepared_convergence_requeues_prepare() {
        let evaluator = Evaluator::new();
        let mut item = test_item();
        item.approval_state = ApprovalState::Granted;

        let revision = test_revision(ApprovalPolicy::Required);
        let jobs = vec![test_job(
            step::VALIDATE_INTEGRATED,
            PhaseKind::Validate,
            JobStatus::Completed,
            Some(OutcomeClass::Clean),
        )];

        let evaluation = evaluator.evaluate(&item, &revision, &jobs, &[], &[]);

        assert_eq!(
            evaluation.next_recommended_action,
            step::PREPARE_CONVERGENCE
        );
        assert_eq!(
            evaluation.phase_status.as_deref(),
            Some("awaiting_convergence")
        );
        assert_eq!(evaluation.dispatchable_step_id, None);
    }

    #[test]
    fn integrated_validation_findings_follow_graph_to_repair() {
        let evaluator = Evaluator::new();
        let item = test_item();
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
            step::REPAIR_AFTER_INTEGRATION
        );
    }

    #[test]
    fn untriaged_findings_block_dispatch_in_triage_state() {
        let evaluator = Evaluator::new();
        let item = test_item();
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

        assert_eq!(evaluation.phase_status.as_deref(), Some("triaging"));
        assert_eq!(evaluation.next_recommended_action, "triage_findings");
        assert_eq!(evaluation.dispatchable_step_id, None);
    }

    #[test]
    fn non_blocking_triaged_findings_follow_clean_edge() {
        let evaluator = Evaluator::new();
        let item = test_item();
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
        let item = test_item();
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
            step::REVIEW_INCREMENTAL_AFTER_INTEGRATION_REPAIR
        );
    }

    #[test]
    fn terminal_jobs_without_outcomes_do_not_advance_workflow() {
        let evaluator = Evaluator::new();
        let item = test_item();
        let revision = test_revision(ApprovalPolicy::Required);
        let jobs = vec![test_job(
            step::VALIDATE_CANDIDATE_INITIAL,
            PhaseKind::Validate,
            JobStatus::Expired,
            None,
        )];

        let evaluation = evaluator.evaluate(&item, &revision, &jobs, &[], &[]);

        assert_eq!(evaluation.dispatchable_step_id, None);
        assert_eq!(evaluation.phase_status.as_deref(), Some("unknown"));
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
        let item = test_item();
        let revision = test_revision(ApprovalPolicy::Required);
        let jobs = vec![test_job(
            step::VALIDATE_CANDIDATE_INITIAL,
            PhaseKind::Validate,
            JobStatus::Cancelled,
            Some(OutcomeClass::Cancelled),
        )];

        let evaluation = evaluator.evaluate(&item, &revision, &jobs, &[], &[]);

        assert_eq!(evaluation.dispatchable_step_id, None);
        assert_eq!(evaluation.next_recommended_action, "none");
        assert_eq!(
            evaluation.current_step_id.as_deref(),
            Some(step::VALIDATE_CANDIDATE_INITIAL)
        );
    }

    #[test]
    fn transient_failures_do_not_auto_redispatch_without_retry_policy() {
        let evaluator = Evaluator::new();
        let item = test_item();
        let revision = test_revision(ApprovalPolicy::Required);
        let jobs = vec![test_job(
            step::VALIDATE_CANDIDATE_INITIAL,
            PhaseKind::Validate,
            JobStatus::Failed,
            Some(OutcomeClass::TransientFailure),
        )];

        let evaluation = evaluator.evaluate(&item, &revision, &jobs, &[], &[]);

        assert_eq!(evaluation.dispatchable_step_id, None);
        assert_eq!(evaluation.next_recommended_action, "none");
        assert_eq!(
            evaluation.current_step_id.as_deref(),
            Some(step::VALIDATE_CANDIDATE_INITIAL)
        );
    }

    fn test_item() -> Item {
        Item {
            id: ItemId::from_uuid(Uuid::nil()),
            project_id: ProjectId::from_uuid(Uuid::nil()),
            classification: Classification::Change,
            workflow_version: "delivery:v1".into(),
            lifecycle_state: LifecycleState::Open,
            parking_state: ParkingState::Active,
            done_reason: None,
            resolution_source: None,
            approval_state: ApprovalState::NotRequested,
            escalation_state: EscalationState::None,
            escalation_reason: None,
            current_revision_id: ItemRevisionId::from_uuid(Uuid::nil()),
            origin_kind: OriginKind::Manual,
            origin_finding_id: None,
            priority: Priority::Major,
            labels: vec![],
            operator_notes: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            closed_at: None,
        }
    }

    fn test_revision(approval_policy: ApprovalPolicy) -> ItemRevision {
        ItemRevision {
            id: ItemRevisionId::from_uuid(Uuid::nil()),
            item_id: ItemId::from_uuid(Uuid::nil()),
            revision_no: 1,
            title: "Title".into(),
            description: "Description".into(),
            acceptance_criteria: "AC".into(),
            target_ref: "refs/heads/main".into(),
            approval_policy,
            policy_snapshot: serde_json::json!({}),
            template_map_snapshot: serde_json::json!({}),
            seed_commit_oid: "abc123".into(),
            seed_target_commit_oid: Some("def456".into()),
            supersedes_revision_id: None,
            created_at: Utc::now(),
        }
    }

    fn test_job(
        step_id: &str,
        phase_kind: PhaseKind,
        status: JobStatus,
        outcome_class: Option<OutcomeClass>,
    ) -> Job {
        Job {
            id: ingot_domain::ids::JobId::from_uuid(Uuid::now_v7()),
            project_id: ProjectId::from_uuid(Uuid::nil()),
            item_id: ItemId::from_uuid(Uuid::nil()),
            item_revision_id: ItemRevisionId::from_uuid(Uuid::nil()),
            step_id: step_id.into(),
            semantic_attempt_no: 1,
            retry_no: 0,
            supersedes_job_id: None,
            status,
            outcome_class,
            phase_kind,
            workspace_id: Some(WorkspaceId::from_uuid(Uuid::now_v7())),
            workspace_kind: WorkspaceKind::Authoring,
            execution_permission: ExecutionPermission::MustNotMutate,
            context_policy: ContextPolicy::Fresh,
            phase_template_slug: "template".into(),
            phase_template_digest: None,
            prompt_snapshot: None,
            input_base_commit_oid: Some("base".into()),
            input_head_commit_oid: Some("head".into()),
            output_artifact_kind: match step_id {
                step::INVESTIGATE_ITEM => OutputArtifactKind::FindingReport,
                step::AUTHOR_INITIAL | step::REPAIR_CANDIDATE | step::REPAIR_AFTER_INTEGRATION => {
                    OutputArtifactKind::Commit
                }
                step::VALIDATE_INTEGRATED
                | step::VALIDATE_CANDIDATE_INITIAL
                | step::VALIDATE_CANDIDATE_REPAIR
                | step::VALIDATE_AFTER_INTEGRATION_REPAIR => OutputArtifactKind::ValidationReport,
                _ => OutputArtifactKind::ReviewReport,
            },
            output_commit_oid: None,
            result_schema_version: None,
            result_payload: None,
            agent_id: None,
            process_pid: None,
            lease_owner_id: None,
            heartbeat_at: None,
            lease_expires_at: None,
            error_code: None,
            error_message: None,
            created_at: Utc::now(),
            started_at: None,
            ended_at: Some(Utc::now()),
        }
    }

    fn test_prepared_convergence(target_head_valid: bool) -> Convergence {
        Convergence {
            id: ConvergenceId::from_uuid(Uuid::now_v7()),
            project_id: ProjectId::from_uuid(Uuid::nil()),
            item_id: ItemId::from_uuid(Uuid::nil()),
            item_revision_id: ItemRevisionId::from_uuid(Uuid::nil()),
            source_workspace_id: WorkspaceId::from_uuid(Uuid::now_v7()),
            integration_workspace_id: Some(WorkspaceId::from_uuid(Uuid::now_v7())),
            source_head_commit_oid: "head".into(),
            target_ref: "refs/heads/main".into(),
            strategy: ConvergenceStrategy::RebaseThenFastForward,
            status: ConvergenceStatus::Prepared,
            input_target_commit_oid: Some("base".into()),
            prepared_commit_oid: Some("prepared".into()),
            final_target_commit_oid: None,
            target_head_valid: Some(target_head_valid),
            conflict_summary: None,
            created_at: Utc::now(),
            completed_at: None,
        }
    }

    fn test_finding(job: &Job, triage_state: FindingTriageState) -> Finding {
        Finding {
            id: FindingId::from_uuid(Uuid::now_v7()),
            project_id: ProjectId::from_uuid(Uuid::nil()),
            source_item_id: ItemId::from_uuid(Uuid::nil()),
            source_item_revision_id: ItemRevisionId::from_uuid(Uuid::nil()),
            source_job_id: job.id,
            source_step_id: job.step_id.clone(),
            source_report_schema_version: "review_report:v1".into(),
            source_finding_key: "finding-1".into(),
            source_subject_kind: FindingSubjectKind::Candidate,
            source_subject_base_commit_oid: Some("base".into()),
            source_subject_head_commit_oid: "head".into(),
            code: "BUG001".into(),
            severity: FindingSeverity::High,
            summary: "summary".into(),
            paths: vec!["src/lib.rs".into()],
            evidence: serde_json::json!(["evidence"]),
            triage_state,
            linked_item_id: None,
            triage_note: match triage_state {
                FindingTriageState::WontFix => Some("accepted".into()),
                _ => None,
            },
            created_at: Utc::now(),
            triaged_at: if triage_state == FindingTriageState::Untriaged {
                None
            } else {
                Some(Utc::now())
            },
        }
    }
}
