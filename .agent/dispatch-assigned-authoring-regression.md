# Align authoring dispatch with the queued-to-running runtime contract

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows `.agent/PLANS.md` and must be maintained in accordance with that file.

## Purpose / Big Picture

After this change, dispatching an authoring job will no longer strand the item in a fake in-flight state. The user-visible behavior is simple: dispatching `author_initial` still provisions or reuses the authoring workspace, but the `jobs` row remains `queued` until the runtime actually claims it and turns it `running`. Existing broken rows created by the old dispatch path will also recover without requiring the operator to notice the problem and restart the daemon by hand.

You will see the fix in three places. First, the dispatch route will return a queued job instead of an assigned one for agent-backed authoring work. Second, the runtime will still claim queued work to running atomically, preserving the handoff hardening from `2a56e40` instead of bypassing it. Third, a targeted maintenance recovery will requeue already-stranded dispatch residue that matches the exact broken signature observed in the local database on 2026-03-20.

## Progress

- [x] (2026-03-20 09:28Z) Re-read `.agent/PLANS.md` and the earlier handoff plan in `.agent/assigned-running-handoff-hardening.md` to keep this follow-up aligned with the runtime contract introduced in commit `2a56e40`.
- [x] (2026-03-20 09:31Z) Queried `~/.ingot/ingot.db` and `~/.ingot/logs/daemon.log` and confirmed the live symptom is a stranded `assigned` row, not a long-running author process.
- [x] (2026-03-20 09:34Z) Located the concrete write path in `crates/ingot-http-api/src/router/dispatch.rs` where authoring dispatch still calls `job.assign(...)` before any runtime claim.
- [x] (2026-03-20 09:37Z) Confirmed that `Database::list_queued_jobs()` only selects `status = 'queued'` while `JobDispatcher::reconcile_active_jobs()` intentionally ignores `assigned` during steady-state operation, which explains why the broken row becomes invisible until restart.
- [x] (2026-03-20 10:04Z) Created and claimed beads issue `ingot-1kw` to track the bug and attach the production evidence.
- [x] (2026-03-20 10:08Z) Inspected the route tests, workflow evaluator, and UI tests to determine whether the fix should change persistence only or also change workflow/UI semantics.
- [x] (2026-03-20 10:10Z) Authored this ExecPlan in `.agent/dispatch-assigned-authoring-regression.md`.
- [ ] Implement the dispatch contract change for initial dispatch and retry dispatch.
- [ ] Add targeted maintenance recovery for already-stranded dispatch-created `assigned` rows.
- [ ] Update tests to encode the new contract and preserve the existing evaluator semantics for queued work.
- [ ] Validate with focused backend tests, then broader checks, and finish the required `bd dolt push` and `git push` workflow.

## Surprises & Discoveries

- Observation: the stuck row in the local database was created after the daemon had already started, so startup-only reconciliation could never repair it.
  Evidence: `ingotd` started at `2026-03-20T06:22:17Z`, but the broken job row `job_019d0a49a57f7b83ab2824287d15acb8` was created at `2026-03-20T08:08:11.903782+00:00`.

- Observation: the local row is not merely old; it has an impossible signature for a truly running authoring job.
  Evidence: the row is `status=assigned` with `workspace_id=wrk_019d0a49a5ff71b1b4ac56d784203ae8` but `agent_id`, `process_pid`, `lease_owner_id`, `heartbeat_at`, `lease_expires_at`, `started_at`, and `ended_at` are all `NULL`, while the linked workspace is already `ready` with `current_job_id = NULL`.

- Observation: the broken write is in the HTTP dispatch layer, not in the runtime launch path that was hardened yesterday.
  Evidence: `crates/ingot-http-api/src/router/dispatch.rs` still routes authoring workspace creation through `link_job_to_workspace_or_cleanup(...)`, which calls `job.assign(JobAssignment::new(workspace.id))` and then `state.db.update_job(job)`.

- Observation: the current route tests actively preserve the bad contract.
  Evidence: `crates/ingot-http-api/tests/dispatch_routes.rs` asserts `json["status"] == "assigned"` for `dispatch_item_job_route_creates_queued_author_initial_job_and_workspace`.

- Observation: the evaluator intentionally treats queued work as active, so the presence of `phase_status = running` is not, by itself, the defect this plan should fix.
  Evidence: `crates/ingot-workflow/src/evaluator.rs` selects the first `job.state.is_active()` row, and `JobState::is_active()` includes `Queued`, `Assigned`, and `Running`.

- Observation: dispatch currently uses `Assigned` as a proxy for “job knows its workspace” because the domain model has no separate queued-with-workspace state.
  Evidence: `JobWire.workspace_id` is derived from `job.state.workspace_id()`, which is `None` for `JobState::Queued`, and the only public helper for attaching a workspace to a non-terminal job is `Job::assign(...)`.

## Decision Log

- Decision: treat the regression as a contract mismatch between dispatch and runtime, not as a generic scheduler bug.
  Rationale: the runtime now correctly claims `queued -> running` atomically in `run_with_heartbeats()`. The remaining failure happens because the dispatch route writes `assigned` before the runtime ever sees the row.
  Date/Author: 2026-03-20 / Codex

- Decision: do not change the evaluator’s “queued work counts as active” semantics in the same patch.
  Rationale: the local bug is persistence-level invisibility, not the high-level board model. Changing evaluator semantics at the same time would widen the patch and make it harder to verify that the root cause is fixed.
  Date/Author: 2026-03-20 / Codex

- Decision: authoring dispatch should provision or reuse the authoring workspace without binding that workspace onto the queued job row.
  Rationale: the runtime can already rediscover the authoring workspace by `created_for_revision_id` during `prepare_run()`. There is no need to encode that relationship by misusing `JobStatus::Assigned`.
  Date/Author: 2026-03-20 / Codex

- Decision: add a narrow steady-state recovery for already-stranded dispatch residue instead of restoring broad `assigned` repair in ordinary maintenance.
  Rationale: startup-only repair is insufficient for rows created after boot, but broad steady-state repair would re-open the race that `2a56e40` intentionally removed. The recovery must target only the observed inert signature of broken authoring dispatch rows.
  Date/Author: 2026-03-20 / Codex

- Decision: keep daemon-only validation on its existing `assigned -> running` path.
  Rationale: `prepare_harness_validation()` still uses a tightly coupled, lock-held handoff to `start_job_execution(...)`. This plan should not conflate that path with the broken HTTP authoring dispatch contract.
  Date/Author: 2026-03-20 / Codex

## Outcomes & Retrospective

This plan is not implemented yet. The repository now has a precise diagnosis, a tracked bug (`ingot-1kw`), and a scoped implementation plan that keeps the runtime hardening intact while fixing the dispatch contract and recovering already-bad rows. The main remaining risk is over-broad recovery logic that accidentally touches legitimate transient `assigned` rows, so the implementation must keep the recovery predicate narrow and heavily tested.

## Context and Orientation

The core authoring dispatch path lives in `crates/ingot-http-api/src/router/dispatch.rs`. Both the initial dispatch route and the retry route create a `Job`, optionally ensure an authoring workspace exists, and then currently call `link_job_to_workspace_or_cleanup(...)`. That helper persists `JobStatus::Assigned` solely to put `workspace_id` onto the job row.

The runtime launch path lives in `crates/ingot-agent-runtime/src/lib.rs`. The important functions are `prepare_run()`, which provisions and attaches the real workspace, `run_with_heartbeats()`, which now claims queued work to running atomically, `reconcile_active_jobs()`, which only repairs running rows during steady-state maintenance, and `reconcile_startup_assigned_jobs()`, which repairs legacy assigned rows only during startup reconciliation.

The SQLite store logic for job selection and lifecycle transitions lives in `crates/ingot-store-sqlite/src/store/job.rs`. `list_queued_jobs()` selects only `status = 'queued'`. That is the key reason a dispatch-created `assigned` row becomes invisible to ordinary job launch.

The workflow evaluator lives in `crates/ingot-workflow/src/evaluator.rs`. It treats any active job as “running” from the board’s perspective, where “active” includes queued, assigned, and running. This plan preserves that high-level workflow interpretation.

The route tests that currently encode the broken contract live in `crates/ingot-http-api/tests/dispatch_routes.rs`. Shared helpers for route-test fixtures live in `crates/ingot-http-api/tests/common/mod.rs`. Some UI tests in `ui/src/test/item-detail-page.test.tsx` already understand queued jobs, which is useful because it means the frontend is not relying exclusively on `assigned`.

The local reproduction that motivated this plan is in `~/.ingot/ingot.db`, where job `job_019d0a49a57f7b83ab2824287d15acb8` for item `001 — Scaffold the project` was left `assigned` with no lease or runner metadata, while its workspace was already `ready`. That exact signature should be encoded into recovery tests so the bug never returns silently.

## Plan of Work

First, fix the HTTP dispatch contract for authoring jobs in `crates/ingot-http-api/src/router/dispatch.rs`. Replace the current helper that mutates the job into `Assigned` with a helper whose responsibility is only “ensure the authoring workspace exists or clean it up on failure.” The initial dispatch route and the retry route should continue to create or reuse the authoring workspace, but they must leave the new authoring job row `Queued`. The job returned from the API will therefore no longer carry `workspace_id` immediately, because queued jobs do not persist assignment metadata in the current domain model.

Second, update the route tests to codify the new dispatch contract. In `crates/ingot-http-api/tests/dispatch_routes.rs`, rewrite `dispatch_item_job_route_creates_queued_author_initial_job_and_workspace` so it expects `status = queued`, keeps verifying that the workspace exists in item detail, and stops asserting that the job itself is already assigned to that workspace. Review the retry-route tests in `crates/ingot-http-api/tests/job_routes.rs` and any helper assumptions in `crates/ingot-http-api/tests/common/mod.rs` so authoring retries also encode queued-after-dispatch semantics.

Third, add a narrow maintenance repair path in `crates/ingot-agent-runtime/src/lib.rs` for already-broken rows created by the old dispatch code. The repair should apply only to authoring jobs that still have `status = assigned` but clearly never entered the runtime claim path: no lease metadata, no agent metadata, and a linked authoring workspace that is already `ready` with no `current_job_id`. Those rows should be requeued during steady-state maintenance so a long-running daemon can recover them without restart. The existing broad startup-only `assigned` repair must remain in place for older binaries and non-authoring leftovers.

Fourth, add targeted runtime tests that pin down the inert-assigned recovery predicate. One test should seed the exact broken signature from the local database and prove that `reconcile_active_jobs()` requeues it and leaves the workspace usable. Another test should prove that legitimate runtime states are not affected, especially daemon-only validation and already-running jobs. The implementation should prefer small, explicit predicates over fuzzy time-based heuristics.

Fifth, preserve the runtime hardening boundary introduced in `2a56e40`. This patch must not change `run_with_heartbeats()` back to accepting live `assigned` rows for ordinary agent-backed work. The contract after this patch is: HTTP authoring dispatch creates `queued`, the runtime claims `queued -> running`, and maintenance only repairs authoring `assigned` rows that are demonstrably dispatch residue rather than real in-flight launches.

## Concrete Steps

Work from `/Users/aa/Documents/ingot`.

1. Update the authoring dispatch helper in `crates/ingot-http-api/src/router/dispatch.rs`.

   Read:

       crates/ingot-http-api/src/router/dispatch.rs
       crates/ingot-domain/src/job.rs

   Implement:

       - remove the `job.assign(...)` write from the authoring dispatch helper path
       - rename the helper if needed so its name matches its new responsibility
       - apply the same change to both initial dispatch and retry dispatch
       - keep workspace creation and cleanup behavior intact

2. Update backend route tests to reflect the new contract.

   Read:

       crates/ingot-http-api/tests/dispatch_routes.rs
       crates/ingot-http-api/tests/job_routes.rs
       crates/ingot-http-api/tests/common/mod.rs

   Implement:

       - change the initial authoring dispatch test to expect `queued`
       - keep the assertions that the authoring workspace exists and is usable
       - add or tighten a retry-route test so authoring retry dispatch also remains queued
       - remove any helper assumptions that queued authoring rows always have `workspace_id`

3. Add targeted runtime recovery for inert dispatch residue.

   Read:

       crates/ingot-agent-runtime/src/lib.rs
       crates/ingot-agent-runtime/tests/reconciliation.rs
       crates/ingot-store-sqlite/src/store/job.rs

   Implement:

       - a predicate for “dispatch-created inert assigned authoring row”
       - a steady-state repair branch that requeues only rows matching that predicate
       - preservation of the existing startup-only broad assigned recovery
       - no change to `run_with_heartbeats()` or the queued-to-running claim contract for agent-backed jobs

4. Add regression coverage for the observed root-cause signature.

   Add tests with stable names:

       - `dispatch_item_job_route_creates_queued_author_initial_job_and_workspace`
         Update this existing test rather than renaming it.
       - `retry_route_requeues_authoring_job_without_persisting_assigned_state`
       - `reconcile_active_jobs_repairs_inert_assigned_authoring_dispatch_residue`
       - `reconcile_active_jobs_does_not_repair_daemon_validation_assigned_handoff`

5. Validate incrementally, then broadly.

   Run:

       cd /Users/aa/Documents/ingot
       cargo test -p ingot-http-api dispatch_item_job_route_creates_queued_author_initial_job_and_workspace -- --exact
       cargo test -p ingot-http-api retry_route_requeues_authoring_job_without_persisting_assigned_state -- --exact
       cargo test -p ingot-agent-runtime reconcile_active_jobs_repairs_inert_assigned_authoring_dispatch_residue -- --exact
       cargo test -p ingot-agent-runtime reconcile_active_jobs_does_not_repair_daemon_validation_assigned_handoff -- --exact
       cargo test -p ingot-http-api
       cargo test -p ingot-agent-runtime
       cargo fmt --all --check

   If a stable test name changes during implementation, update this plan immediately before stopping.

## Validation and Acceptance

Acceptance is reached when all of the following are true.

1. Dispatching `author_initial` through the HTTP route creates a job row whose durable status is `queued`, not `assigned`, while the authoring workspace still exists for the current revision.

2. The runtime continues to launch ordinary agent-backed work only by claiming `queued -> running` in `run_with_heartbeats()`. There must be no new fallback path that treats `assigned` as launchable for ordinary authoring jobs.

3. A row matching the exact broken signature observed locally on 2026-03-20 can be repaired during ordinary maintenance without daemon restart: `status = assigned`, authoring workspace kind, no lease metadata, no agent metadata, workspace `ready`, and `current_job_id = NULL`.

4. Legitimate non-broken states are preserved. In particular, daemon-only validation must not be requeued by the new steady-state repair, and startup reconciliation must still handle broader legacy assigned rows.

5. The focused tests listed above pass, followed by the crate-level backend test suites. The updated route test must fail before the implementation and pass after it.

6. Manual spot-check of the local DB is consistent with the new contract. After dispatching a fresh authoring job in a dev environment, a query like the following should first show `queued`, then later `running` only after the runtime claims it:

       sqlite3 ~/.ingot/ingot.db "
         SELECT id, status, agent_id, lease_owner_id, heartbeat_at, started_at
         FROM jobs
         WHERE step_id = 'author_initial'
         ORDER BY created_at DESC
         LIMIT 3;
       "

## Idempotence and Recovery

This plan does not require a schema migration. The code changes are retry-safe because dispatching the same item again is already governed by the existing active-job checks, and the recovery logic only requeues rows that match a narrow inert signature. If implementation fails halfway, it is safe to rerun the focused tests and continue editing.

If a local environment already contains a broken row from the old code, the intended recovery after implementation is automatic maintenance repair. Until that code exists, restarting the daemon remains the operational fallback because startup reconciliation still requeues broad `assigned` residue. This fallback should be documented in the implementation notes but must not be the only fix.

## Artifacts and Notes

Key evidence gathered before implementation:

    sqlite> SELECT id, status, workspace_id, agent_id, process_pid, lease_owner_id, heartbeat_at, started_at
              FROM jobs
              WHERE id = 'job_019d0a49a57f7b83ab2824287d15acb8';
    job_019d0a49a57f7b83ab2824287d15acb8|assigned|wrk_019d0a49a5ff71b1b4ac56d784203ae8|||||

    sqlite> SELECT id, status, current_job_id
              FROM workspaces
              WHERE id = 'wrk_019d0a49a5ff71b1b4ac56d784203ae8';
    wrk_019d0a49a5ff71b1b4ac56d784203ae8|ready|

    daemon.log:
      2026-03-20T08:08:06.890079Z dispatcher woken by notification
      2026-03-20T08:08:12.069243Z dispatcher woken by notification

    No matching "prepared job execution" or "job entered running state" lines were present for that job.

## Interfaces and Dependencies

In `crates/ingot-http-api/src/router/dispatch.rs`, the authoring-dispatch helper at the end of the file must no longer require mutable job-state assignment as part of workspace provisioning. If renaming improves clarity, keep the replacement helper local to this module and make it explicit that it is about workspace persistence and cleanup, not job-state mutation.

In `crates/ingot-agent-runtime/src/lib.rs`, define a small, explicit predicate or helper function for the inert assigned authoring signature rather than scattering the checks inline. The predicate should inspect both the job row and, when needed, the linked workspace row so the recovery logic remains explainable and testable.

Do not change the public `JobRepository::start_execution` interface or the agent claim helper introduced by the earlier handoff hardening. This plan depends on preserving the existing runtime boundary: dispatch creates `queued`, the runtime claim writes assignment and lease metadata, and terminal lifecycle code continues to rely on the existing revision guard.

Revision note: created on 2026-03-20 to turn the local `author_initial` stuck-state investigation into an implementation-ready plan after confirming that the root cause is in HTTP dispatch, not in the already-hardened runtime claim path.
