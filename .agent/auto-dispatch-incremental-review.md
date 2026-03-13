# Auto-dispatch incremental review after commit jobs

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with [.agent/PLANS.md](/Users/aa/Documents/ingot/.agent/PLANS.md).

## Purpose / Big Picture

After this change, a clean commit-producing workflow step such as `author_initial` or `repair_candidate` will immediately queue the next incremental review job without requiring the operator to press Dispatch again. A user can see the behavior by running the runtime tests: after the authoring job finishes, the database should already contain a queued `review_incremental_initial` job, and after a repair job finishes, it should already contain a queued `review_incremental_repair` job.

## Progress

- [x] (2026-03-13 19:18Z) Investigated the stalled item in the live database, job artifacts, worktree, and runtime code path.
- [x] (2026-03-13 19:22Z) Identified the seam: successful commit jobs update state and revision context, but successor jobs are only created through explicit dispatch APIs.
- [x] (2026-03-13 19:30Z) Implemented runtime auto-dispatch for the next incremental review step after successful commit jobs.
- [x] (2026-03-13 19:34Z) Added and updated runtime tests to prove initial authoring and repair flows queue incremental review automatically.
- [x] (2026-03-13 19:36Z) Ran targeted and full `ingot-agent-runtime` Rust tests successfully.
- [x] (2026-03-13 19:44Z) Extended startup reconciliation for adopted `create_job_commit` operations so crash recovery also auto-queues incremental review.
- [x] (2026-03-13 19:46Z) Strengthened the startup adoption test to assert the queued review job and reran the full runtime suite.

## Surprises & Discoveries

- Observation: The workflow evaluator already treats `review_incremental_initial` as the next dispatchable step after a clean `author_initial`; the missing behavior is job creation, not workflow evaluation.
  Evidence: `crates/ingot-workflow/src/evaluator.rs` contains `clean_authoring_commits_flow_into_incremental_review`.

- Observation: The runtime completion path for commit jobs stops after marking the job complete, updating the workspace, and rebuilding revision context.
  Evidence: `crates/ingot-agent-runtime/src/lib.rs` `complete_commit_run` does not call `dispatch_job` or `create_job`.

- Observation: One existing runtime test encoded the old contract by expecting review to be merely dispatchable after a successful authoring retry.
  Evidence: `successful_authoring_retry_clears_escalation_and_reopens_review_dispatch` failed until it was updated to assert that `review_incremental_initial` is already queued.

- Observation: Startup reconciliation used a separate adoption path for already-applied `create_job_commit` operations, so the first implementation still left a crash window where review was not auto-queued.
  Evidence: `adopt_create_job_commit` completed the recovered job and refreshed revision context, but did not call the runtime auto-dispatch helper until this follow-up fix.

## Decision Log

- Decision: Keep evaluator semantics unchanged and implement the feature in the runtime orchestration layer.
  Rationale: The evaluator already projects the correct next step. Changing it would broaden the behavior change and alter the UI contract unnecessarily.
  Date/Author: 2026-03-13 / Codex

- Decision: Auto-dispatch only the incremental review steps (`review_incremental_initial`, `review_incremental_repair`, `review_incremental_after_integration_repair`).
  Rationale: That matches the user request and keeps manual operator control for candidate review, validation, approval, and other non-incremental steps.
  Date/Author: 2026-03-13 / Codex

## Outcomes & Retrospective

The runtime now auto-queues `review_incremental_initial`, `review_incremental_repair`, and `review_incremental_after_integration_repair` immediately after successful commit-producing jobs when those steps are the next legal workflow action. This keeps the workflow moving without changing evaluator semantics or broadening auto-dispatch to candidate review, validation, approval, or convergence work.

The tests confirm the intended behavior in three ways: a new test proves a clean `author_initial` run leaves a queued incremental review job with the expected review subject commits, the startup reconciliation test now proves that recovering an already-applied authoring commit also queues review, and the repaired candidate loop still reaches `prepare_convergence` while relying on auto-dispatched incremental review steps. A pre-existing retry test was updated because the queue now advances one step further than before.

## Context and Orientation

The relevant runtime loop lives in `crates/ingot-agent-runtime/src/lib.rs`. A "commit-producing job" is a workflow job whose output artifact is a Git commit, such as `author_initial` or `repair_candidate`. Those jobs mutate an authoring workspace, and the daemon later creates the canonical commit in the project repository. The function `complete_commit_run` is where the daemon persists job success and refreshes revision context after such a job.

The workflow evaluator lives in `crates/ingot-workflow/src/evaluator.rs`. It does not create jobs; it only inspects the current item, revision, jobs, and convergences and reports which step is next. The dispatch use case lives in `crates/ingot-usecases/src/job.rs`, and the HTTP route in `crates/ingot-http-api/src/router.rs` uses it to create a new queued job when an operator presses Dispatch.

The gap is that the runtime never performs that same dispatch step after a successful commit job, even when the evaluator says the next step is an incremental review. The implementation should reuse the existing dispatch use case under the project mutation lock so the behavior remains consistent with operator-triggered dispatch.

## Plan of Work

Add a small helper in `crates/ingot-agent-runtime/src/lib.rs` that runs after `complete_commit_run` persists the successful commit. The helper should reload the current item state under the project lock, evaluate the workflow using the latest jobs and hydrated convergences, and, when the next dispatchable step is one of the incremental review steps, call `dispatch_job` and persist the resulting queued job. It should also append the same `job_dispatched` activity record used by the HTTP API so the timeline remains consistent.

Keep the helper narrow. It should not auto-dispatch candidate review, validation, approval, or convergence work. It should not change the evaluator or the HTTP API. It should rely on the existing use case for computing input base and head commits, so the review subject remains identical to manual dispatch.

Update runtime tests in `crates/ingot-agent-runtime/src/lib.rs` to prove the new behavior. One test should verify that a successful `author_initial` run leaves behind a queued `review_incremental_initial` job with the expected base and head commit inputs. Another test should cover the repair loop and show that a successful `repair_candidate` run leaves behind a queued `review_incremental_repair` job automatically before candidate review is dispatched manually.

## Concrete Steps

From `/Users/aa/Documents/ingot`:

1. Edit `crates/ingot-agent-runtime/src/lib.rs` to add the auto-dispatch helper and invoke it from `complete_commit_run`.
2. Edit the runtime tests in the same file so they assert queued incremental review jobs exist after commit-producing steps finish.
3. Run targeted tests:

       cargo test -p ingot-agent-runtime auto_dispatch
       cargo test -p ingot-agent-runtime candidate_repair_loop_advances_to_prepare_convergence

4. If those pass, run the full crate tests:

       cargo test -p ingot-agent-runtime

## Validation and Acceptance

Acceptance is satisfied when these behaviors are observable:

1. After a clean `author_initial` runtime execution, the database contains a completed author job and a queued `review_incremental_initial` job without any explicit dispatch call between them.
2. The queued incremental review job uses the expected review subject, with `input_base_commit_oid` equal to the seed or previous authoring head and `input_head_commit_oid` equal to the newly created authoring commit.
3. After a clean repair commit, the database contains a queued `review_incremental_repair` job automatically, and the existing repair loop test still reaches `prepare_convergence`.

## Idempotence and Recovery

The runtime helper must run under the existing project mutation lock so it cannot race a manual dispatch for the same item. If the helper does not find an auto-dispatchable incremental review step, it should return without side effects. If a test fails partway through, it can be rerun safely because each runtime test uses a temporary repository and database.

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
