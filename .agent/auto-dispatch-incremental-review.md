# Auto-dispatch projected review jobs

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with [.agent/PLANS.md](/Users/aa/Documents/ingot/.agent/PLANS.md).

## Purpose / Big Picture

After this change, any closure-relevant `review_*` workflow step will queue automatically without requiring the operator to press Dispatch. A user can see the behavior by running the runtime tests: after an authoring job finishes, the database should already contain a queued incremental review job; after a clean incremental review finishes, the database should already contain a queued whole-candidate review job; and after non-blocking triage clears an incremental review finding set, the daemon should recover and queue the next candidate review on its own.

## Progress

- [x] (2026-03-13 19:18Z) Investigated the stalled item in the live database, job artifacts, worktree, and runtime code path.
- [x] (2026-03-13 19:22Z) Identified the seam: successful commit jobs update state and revision context, but successor jobs are only created through explicit dispatch APIs.
- [x] (2026-03-13 19:30Z) Implemented runtime auto-dispatch for the next incremental review step after successful commit jobs.
- [x] (2026-03-13 19:34Z) Added and updated runtime tests to prove initial authoring and repair flows queue incremental review automatically.
- [x] (2026-03-13 19:36Z) Ran targeted and full `ingot-agent-runtime` Rust tests successfully.
- [x] (2026-03-13 19:44Z) Extended startup reconciliation for adopted `create_job_commit` operations so crash recovery also auto-queues incremental review.
- [x] (2026-03-13 19:46Z) Strengthened the startup adoption test to assert the queued review job and reran the full runtime suite.
- [x] (2026-03-14 19:20Z) Expanded the runtime helper from incremental-only review to any projected closure-relevant `review_*` step.
- [x] (2026-03-14 19:27Z) Added a tick-time review auto-dispatch sweep so triage completion, resume, and recovery states self-heal even when no commit just finished.
- [x] (2026-03-14 19:34Z) Updated runtime tests to cover automatic candidate review after clean review completion and after non-blocking incremental-review triage.
- [x] (2026-03-14 19:40Z) Ran targeted review auto-dispatch tests and the full `cargo test -p ingot-agent-runtime` suite successfully.

## Surprises & Discoveries

- Observation: The workflow evaluator already treats `review_incremental_initial` as the next dispatchable step after a clean `author_initial`; the missing behavior is job creation, not workflow evaluation.
  Evidence: `crates/ingot-workflow/src/evaluator.rs` contains `clean_authoring_commits_flow_into_incremental_review`.

- Observation: The runtime completion path for commit jobs stops after marking the job complete, updating the workspace, and rebuilding revision context.
  Evidence: `crates/ingot-agent-runtime/src/lib.rs` `complete_commit_run` does not call `dispatch_job` or `create_job`.

- Observation: One existing runtime test encoded the old contract by expecting review to be merely dispatchable after a successful authoring retry.
  Evidence: `successful_authoring_retry_clears_escalation_and_reopens_review_dispatch` failed until it was updated to assert that `review_incremental_initial` is already queued.

- Observation: Startup reconciliation used a separate adoption path for already-applied `create_job_commit` operations, so the first implementation still left a crash window where review was not auto-queued.
  Evidence: `adopt_create_job_commit` completed the recovered job and refreshed revision context, but did not call the runtime auto-dispatch helper until this follow-up fix.

- Observation: Broadening auto-dispatch from incremental review to all `review_*` steps exposed another seam: an item can become eligible for candidate review after finding triage or resume without any new job completion event.
  Evidence: `crates/ingot-http-api/src/router.rs` updates finding triage state but does not create successor jobs, and `POST /items/:id/resume` only clears `parking_state`.

## Decision Log

- Decision: Keep evaluator semantics unchanged and implement the feature in the runtime orchestration layer.
  Rationale: The evaluator already projects the correct next step. Changing it would broaden the behavior change and alter the UI contract unnecessarily.
  Date/Author: 2026-03-13 / Codex

- Decision: Auto-dispatch only the incremental review steps (`review_incremental_initial`, `review_incremental_repair`, `review_incremental_after_integration_repair`).
  Rationale: That matches the user request and keeps manual operator control for candidate review, validation, approval, and other non-incremental steps.
  Date/Author: 2026-03-13 / Codex

- Decision: Replace the incremental-only rule with auto-dispatch for every projected closure-relevant `review_*` step, while leaving authoring, validation, approval, and convergence progression unchanged.
  Rationale: The updated user requirement is that all review jobs are unnecessary operator work. Restricting the automation to review steps preserves the existing control points for the rest of the workflow.
  Date/Author: 2026-03-14 / Codex

- Decision: Keep immediate post-completion auto-dispatch hooks and add a tick-time sweep over idle items.
  Rationale: The immediate hooks avoid a needless poll delay after successful commit and review jobs, while the sweep closes the gaps for triage-completed, resumed, or recovered items that become review-eligible without a fresh completion callback.
  Date/Author: 2026-03-14 / Codex

## Outcomes & Retrospective

The runtime now auto-queues any projected closure-relevant `review_*` step. That includes the original incremental review handoffs after successful commit-producing jobs, the whole-candidate review handoffs after clean incremental review jobs, and idle-item recovery cases where non-blocking triage or resume makes a review step legal again. Validation, approval, and convergence behavior remain unchanged.

The tests now confirm the behavior in four ways: a clean `author_initial` run leaves a queued incremental review job with the expected review subject commits; startup reconciliation still proves that recovering an already-applied authoring commit also queues review; the repaired candidate loop reaches `prepare_convergence` while both incremental and candidate review steps auto-queue; and a new idle-item test proves that non-blocking triage on an incremental review findings set queues the next candidate review without any manual dispatch call. The full `ingot-agent-runtime` test suite also passed after the change, which lowers the risk that older maintenance or convergence paths were relying on manual review dispatch.

## Context and Orientation

The relevant runtime loop lives in `crates/ingot-agent-runtime/src/lib.rs`. A "commit-producing job" is a workflow job whose output artifact is a Git commit, such as `author_initial` or `repair_candidate`. Those jobs mutate an authoring workspace, and the daemon later creates the canonical commit in the project repository. The function `complete_commit_run` is where the daemon persists job success and refreshes revision context after such a job.

The workflow evaluator lives in `crates/ingot-workflow/src/evaluator.rs`. It does not create jobs; it only inspects the current item, revision, jobs, and convergences and reports which step is next. The dispatch use case lives in `crates/ingot-usecases/src/job.rs`, and the HTTP route in `crates/ingot-http-api/src/router.rs` uses it to create a new queued job when an operator presses Dispatch.

The gap is that the runtime historically only performed that successor dispatch after a successful commit job, and only for incremental review. Items that became eligible for candidate review after clean review completion, non-blocking triage, or resume still waited for manual operator dispatch. The implementation should reuse the existing dispatch use case under the project mutation lock so the behavior remains consistent with operator-triggered dispatch.

## Plan of Work

Add a small helper in `crates/ingot-agent-runtime/src/lib.rs` that reloads the current item state under the project lock, evaluates the workflow using the latest jobs and hydrated convergences, and, when the next dispatchable step is any closure-relevant `review_*` step, calls `dispatch_job` and persists the resulting queued job. It should also append the same `job_dispatched` activity record used by the HTTP API so the timeline remains consistent.

Keep the helper narrow. It should auto-dispatch only closure-relevant review steps. It should not auto-dispatch validation, approval, or convergence work. It should not change the evaluator. It should rely on the existing use case for computing input base and head commits, so the review subject remains identical to manual dispatch.

Invoke the helper immediately after successful commit and report completions, and add a small tick-time sweep over idle items so states reached through finding triage, resume, or recovery also self-dispatch review jobs. Update runtime tests in `crates/ingot-agent-runtime/src/lib.rs` to prove the broadened behavior. One test should verify that a successful `author_initial` run leaves behind a queued `review_incremental_initial` job with the expected base and head commit inputs. Another test should cover the repair loop and show that a clean `review_incremental_repair` run leaves behind a queued `review_candidate_repair` job automatically. A third test should cover an idle item whose incremental-review findings were triaged non-blocking and prove that the next candidate review job is auto-queued on the next dispatcher tick.

## Concrete Steps

From `/Users/aa/Documents/ingot`:

1. Edit `crates/ingot-agent-runtime/src/lib.rs` to generalize the review auto-dispatch helper, invoke it from successful commit and report completion paths, and add the idle-item sweep in `tick`.
2. Edit the runtime tests in the same file so they assert queued review jobs exist after commit-producing steps finish, after clean review completion, and after non-blocking triage recovery.
3. Run targeted tests:

       cargo test -p ingot-agent-runtime auto_dispatch
       cargo test -p ingot-agent-runtime idle_item_auto_dispatches_candidate_review_after_nonblocking_incremental_triage
       cargo test -p ingot-agent-runtime candidate_repair_loop_advances_to_prepare_convergence

4. If those pass, run the full crate tests:

       cargo test -p ingot-agent-runtime

## Validation and Acceptance

Acceptance is satisfied when these behaviors are observable:

1. After a clean `author_initial` runtime execution, the database contains a completed author job and a queued `review_incremental_initial` job without any explicit dispatch call between them.
2. After a clean incremental review execution, the database contains the next queued whole-candidate review job without any explicit dispatch call between them.
3. After non-blocking triage resolves the latest incremental-review findings set, the next dispatcher tick queues the successor candidate review job automatically.
4. Validation, approval, and convergence steps still require the same explicit commands or daemon-only rules they required before.

## Idempotence and Recovery

The runtime helper must run under the existing project mutation lock so it cannot race a manual dispatch for the same item. If the helper does not find an auto-dispatchable review step, it should return without side effects. The idle-item sweep should be safe to rerun because it only creates a job when the evaluator still projects a review step and no active execution already exists. If a test fails partway through, it can be rerun safely because each runtime test uses a temporary repository and database.

Startup reconciliation must follow the same rule. If the daemon restarts after creating the canonical commit but before queuing the next incremental review, the `adopt_create_job_commit` path must also invoke the helper so recovery closes that gap instead of reintroducing a manual-dispatch requirement.

## Artifacts and Notes

The live production-like reproduction that motivated this change showed:

    item evaluation:
      next_recommended_action = review_incremental_initial
      dispatchable_step_id = review_incremental_initial

    jobs:
      author_initial completed clean
      no queued review job present

That proves the evaluator was correct and the missing behavior was successor job creation.

## Interfaces and Dependencies

In `crates/ingot-agent-runtime/src/lib.rs`, the implementation should continue to use:

- `ingot_usecases::job::dispatch_job` and `DispatchJobCommand` to create successor jobs.
- `ingot_workflow::Evaluator` to inspect the current workflow state.
- `ProjectLocks` to guard against concurrent mutations.
- `Database::create_job` and `append_activity` for persistence and activity history.

Revision note: created this ExecPlan before implementation to capture the production investigation and keep the behavior change narrow to runtime auto-dispatch of incremental review steps.

Revision note: updated after implementation to record the new runtime helper, the revised retry expectation, and the passing targeted and full crate test runs.

Revision note: updated after a post-implementation code review found that startup reconciliation still bypassed auto-dispatch for recovered `create_job_commit` operations; the plan now records that fix and the added regression coverage.

Revision note: updated on 2026-03-14 to widen the behavior from incremental-only review auto-dispatch to all projected closure-relevant `review_*` steps, including idle-item recovery after non-blocking triage or resume.
