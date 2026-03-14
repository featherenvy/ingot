# Auto-dispatch all review steps

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with [.agent/PLANS.md](/Users/aa/Documents/ingot/.agent/PLANS.md).

## Purpose / Big Picture

After this change, every closure-relevant `review_*` step in `delivery:v1` is created automatically by the daemon as soon as it becomes the next legal workflow step. Operators should no longer need to press Dispatch for `review_candidate_initial`, `review_candidate_repair`, or `review_after_integration_repair`, and the same rule must hold when a triage decision advances the workflow onto a review step.

The behavior is observable by running targeted runtime tests: a clean `review_incremental_initial` completion should immediately queue `review_candidate_initial`, a clean `review_incremental_repair` completion should immediately queue `review_candidate_repair`, and triaging incremental review findings into a non-blocking disposition should also leave the next candidate review job queued automatically.

## Progress

- [x] (2026-03-14 20:55Z) Investigated the evaluator, dispatch use case, runtime, HTTP triage flow, and SPEC language for review dispatch semantics.
- [ ] Implement the code changes that generalize review auto-dispatch and cover the triage path.
- [ ] Update `SPEC.md` so it matches the new default behavior.
- [ ] Run focused regression tests and record the results.

## Surprises & Discoveries

- Observation: `dispatch_job` already computes the correct step and review subject for every review stage; the missing behavior is automatic job creation, not step selection.
  Evidence: `crates/ingot-usecases/src/job.rs` `dispatch_job` delegates to `Evaluator::evaluate` and `input_commits_for_step`.

- Observation: the current runtime only auto-dispatches the three incremental review steps, and only after commit success or reconciled commit adoption.
  Evidence: `crates/ingot-agent-runtime/src/lib.rs` `auto_dispatch_incremental_review` plus its two call sites in `complete_commit_run` and `adopt_create_job_commit`.

- Observation: triage can legally advance onto another review step without any job finishing, so report-completion hooks alone are insufficient.
  Evidence: `crates/ingot-workflow/src/evaluator.rs` `triaged_findings_clean_projection` follows the clean edge of the source step, which for incremental review points to candidate review.

## Decision Log

- Decision: keep `Evaluator` and `dispatch_job` semantics intact and implement automatic review dispatch in orchestration code.
  Rationale: workflow projection and review subject calculation are already correct and shared by manual dispatch; changing them would broaden scope without adding value.
  Date/Author: 2026-03-14 / Codex

- Decision: treat every closure-relevant `review_*` step as auto-dispatchable, not just incremental review.
  Rationale: that matches the requested behavior change and still leaves validation, approval, repair, convergence prepare, and auxiliary report-only work under their existing rules.
  Date/Author: 2026-03-14 / Codex

- Decision: cover the triage-to-review transition explicitly.
  Rationale: otherwise the system would still require an operator dispatch in one legitimate path to a review step, violating the requested “all review_* jobs” rule.
  Date/Author: 2026-03-14 / Codex

## Outcomes & Retrospective

Pending implementation.

## Context and Orientation

The workflow projection lives in `crates/ingot-workflow/src/evaluator.rs`. It returns `dispatchable_step_id` when the item is idle and a closure-relevant job is legal to dispatch next. The use case that turns that projection into a queued `Job` row lives in `crates/ingot-usecases/src/job.rs` as `dispatch_job`.

The daemon runtime loop lives in `crates/ingot-agent-runtime/src/lib.rs`. It runs queued jobs, marks them complete, refreshes revision context, and in some cases triggers follow-up work. Right now it only auto-creates incremental review jobs via `auto_dispatch_incremental_review`.

The operator command surface lives in `crates/ingot-http-api/src/router.rs`. The item dispatch endpoint creates jobs manually via `dispatch_job`. The finding triage endpoint mutates `Finding` rows and can change workflow projection because the evaluator uses triage state to decide whether the workflow remains blocked, goes to repair, or follows the clean edge.

The spec language that currently contradicts the requested behavior is in `SPEC.md` section 8.5, which says review and validation stages are “automatic” only as projected `dispatchable_step_id` values and do not require daemon execution.

## Plan of Work

First, update `SPEC.md` so the built-in workflow semantics clearly say that every `review_*` stage is daemon auto-dispatched by default when it becomes the sole legal closure-relevant next step. Adjust the dispatch endpoint semantics to keep manual dispatch for operator-driven and auxiliary steps while removing the old “projected only, not daemon-executed” language for review stages.

Next, generalize the runtime helper in `crates/ingot-agent-runtime/src/lib.rs` from incremental-only behavior to all review steps. The helper should continue to reload the current item state under the project mutation lock, inspect `Evaluator::evaluate`, and call `dispatch_job` only when the projected `dispatchable_step_id` is a review step. Rename the helper and its predicate accordingly, and invoke it after successful report completion as well as after successful commit completion and reconciled commit adoption.

Then, patch the finding triage path in `crates/ingot-http-api/src/router.rs` so after a triage mutation is persisted, the server re-evaluates the item and auto-dispatches a review job immediately when triage has advanced the workflow onto a review step. Reuse the same `dispatch_job` path and append the standard `job_dispatched` activity entry so history stays consistent.

Finally, add regression tests in `crates/ingot-agent-runtime/src/lib.rs` and `crates/ingot-http-api/src/router.rs` that prove review candidate steps are auto-queued after incremental review success and after triage clean-edge advancement. Update any existing runtime tests that still manually dispatch candidate review after a successful incremental review.

## Concrete Steps

From `/Users/aa/Documents/ingot`:

1. Edit `.agent/auto-dispatch-all-review.md` as implementation decisions solidify.
2. Edit `SPEC.md` to describe review auto-dispatch semantics.
3. Edit `crates/ingot-agent-runtime/src/lib.rs` to generalize review auto-dispatch and invoke it from all relevant completion and recovery paths.
4. Edit `crates/ingot-http-api/src/router.rs` so triage-triggered transitions into review steps also auto-dispatch.
5. Run focused tests:

       cargo test -p ingot-agent-runtime auto_dispatch
       cargo test -p ingot-agent-runtime candidate_repair_loop_advances_to_prepare_convergence
       cargo test -p ingot-http-api triage

## Validation and Acceptance

Acceptance is satisfied when:

1. A clean authoring completion still auto-queues the next incremental review job.
2. A clean incremental review completion auto-queues the next candidate review job without a manual dispatch command.
3. Triaging incremental review findings into a non-blocking disposition auto-queues the next candidate review job.
4. The item detail evaluation no longer surfaces a manual dispatch window for those review stages because a queued review job already exists.

## Idempotence and Recovery

The runtime helper must remain safe to call repeatedly. It should return without side effects when the projected next step is absent, is not a review step, or the item already has an active job. Reusing `dispatch_job` preserves that invariant because the use case rejects dispatch while active execution exists.

Crash recovery matters because the daemon can restart after persisting a successful job but before creating the next review job. Existing commit adoption already handles that case for commit jobs, and the new helper should be reachable from any startup or periodic path that needs to close a missing review-dispatch gap.

## Artifacts and Notes

Key code seams identified during investigation:

    crates/ingot-usecases/src/job.rs: dispatch_job selects whatever the evaluator projects.
    crates/ingot-agent-runtime/src/lib.rs: auto_dispatch_incremental_review only handles incremental review.
    crates/ingot-http-api/src/router.rs: triage_finding mutates findings but does not create follow-up jobs.
    SPEC.md: section 8.5 still says review stages are not daemon-executed automatically.

## Interfaces and Dependencies

The implementation should continue to use:

- `ingot_usecases::job::dispatch_job` and `DispatchJobCommand` for all auto-created review jobs.
- `ingot_workflow::Evaluator` to decide whether the current state projects a review step.
- `ActivityEventType::JobDispatched` so auto-created jobs appear in the normal activity history.
- `ProjectLocks` to serialize dispatch with other item mutations.

Revision note: created on 2026-03-14 to expand the earlier incremental-only auto-dispatch work into a complete all-review auto-dispatch rule, including the triage path and spec updates.
