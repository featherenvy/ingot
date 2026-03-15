# Projected Review Recovery On Degraded HTTP Dispatch

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with [.agent/PLANS.md](/Users/aa/Documents/ingot/.agent/PLANS.md).

## Purpose / Big Picture

After this change, the daemon still recovers projected review work even when an HTTP route completed its primary mutation, failed the immediate auto-dispatch follow-up, and the next dispatcher tick spends its main unit of work on convergence system actions. A contributor can prove the behavior by running focused route and runtime tests: the degraded HTTP path remains warning-only, and the follow-on recovery now still queues the missing review job on a system-action tick.

## Progress

- [x] (2026-03-15 09:45Z) Reviewed the current HTTP hooks, runtime tick ordering, and existing happy-path/recovery tests.
- [x] (2026-03-15 09:55Z) Chose the minimal fix: keep the warning-only HTTP behavior and patch `JobDispatcher::tick` so fallback recovery also runs on system-action ticks.
- [x] (2026-03-15 10:10Z) Added route-level regression coverage for degraded `resume` behavior and for `complete` plus later recovery on a system-action tick.
- [x] (2026-03-15 10:15Z) Patched `crates/ingot-agent-runtime/src/lib.rs` so system-action ticks also run projected-review recovery.
- [x] (2026-03-15 10:20Z) Ran focused route and runtime tests for the new regressions plus nearby happy paths.

## Surprises & Discoveries

- Observation: `resume_item` calls `load_item_detail(...)` after the warning-only auto-dispatch hook, so breaking mirror refresh is not a good way to test degraded `resume`; the route would fail later while loading detail.
  Evidence: `crates/ingot-http-api/src/router/items.rs` calls `refresh_project_mirror(...)` inside both `auto_dispatch_projected_review_job_locked(...)` and `load_item_detail(...)`.

- Observation: `complete_job` is easier to drive through a recoverable degraded state because it returns a small JSON response and does not call `load_item_detail(...)` after the warning-only hook.
  Evidence: `crates/ingot-http-api/src/router/jobs.rs` returns `CompleteJobResponse` immediately after the warning-only `auto_dispatch_projected_review_job(...)` call.

- Observation: Breaking the project path was still too early for the `complete` route because the route refreshes the project mirror before completing the job and again while rebuilding revision context.
  Evidence: `crates/ingot-http-api/src/router/jobs.rs` calls `refresh_project_mirror(...)` before `complete_job_service.execute(...)` and again inside `refresh_revision_context_for_job(...)`.

## Decision Log

- Decision: Do not change `ConvergenceService::tick_system_actions(...)`.
  Rationale: It intentionally performs one system action per call. The bug is that `JobDispatcher::tick()` returned before invoking projected-review recovery; fixing that call site is smaller and safer than changing convergence scheduling semantics.
  Date/Author: 2026-03-15 / Codex

- Decision: Keep the HTTP routes' warning-only behavior and add regressions around it instead of returning an error after the primary mutation is already committed.
  Rationale: `resume` and `complete` mutate durable state before the follow-on dispatch attempt. Returning an HTTP error after that point would still leave partial success and create a different ambiguity for callers.
  Date/Author: 2026-03-15 / Codex

## Outcomes & Retrospective

The final patch kept the HTTP contract unchanged and fixed the runtime starvation gap instead. The new `resume` regression proves the warning-only degraded path directly by checking that the route returns `200 OK`, leaves the item active, and would still hit `incomplete candidate subject` if auto-dispatch were retried against the same state. The new `complete` regression goes further: it drives a real HTTP completion through the warning-only failure, repairs the missing subject inputs, then proves that a dispatcher tick which also performs a convergence invalidation now still queues the missing projected review.

## Context and Orientation

`crates/ingot-http-api/src/router/items.rs` resumes deferred items and then best-effort auto-dispatches the next projected review. `crates/ingot-http-api/src/router/jobs.rs` completes a job and then best-effort auto-dispatches any follow-on projected review. `crates/ingot-agent-runtime/src/lib.rs` contains the background dispatcher loop; its `tick()` method runs maintenance, then at most one convergence system action, then at most one runnable job, and finally a projected-review recovery sweep.

A projected review is an automatically queued review step inferred from the current workflow state. A degraded HTTP dispatch path is the case where the primary mutation succeeds, the immediate projected-review dispatch fails, the route logs a warning, and the system relies on later background recovery to queue the missing review.

## Plan of Work

Add one route-level regression in `crates/ingot-http-api/tests/item_routes.rs` that proves `resume` still returns success and commits its parking-state mutation when projected review auto-dispatch cannot derive a complete candidate subject. Add a second regression in `crates/ingot-http-api/tests/job_routes.rs` that drives the degraded `complete` path through a recoverable mirror failure, then runs the background dispatcher on a tick that also performs a convergence invalidation system action. That second test must prove both halves of the bug: before the patch, the system-action tick would not queue the missing review; after the patch, the same tick invalidates the stale convergence and also recovers the candidate review.

Patch `crates/ingot-agent-runtime/src/lib.rs` in `JobDispatcher::tick()` so that when `tick_system_actions()` returns `true`, the dispatcher still calls `recover_projected_review_jobs()` before returning `Ok(true)`. Keep the single-system-action-per-tick behavior intact.

## Concrete Steps

From `/Users/aa/Documents/ingot`:

1. Edit `.agent/projected-review-recovery-execplan.md` as implementation progresses.
2. Edit `crates/ingot-http-api/Cargo.toml` only if the new cross-layer regression needs a dev-dependency on `ingot-agent-runtime`.
3. Add the `resume` degraded-path regression to `crates/ingot-http-api/tests/item_routes.rs`.
4. Add the `complete` plus later-recovery regression to `crates/ingot-http-api/tests/job_routes.rs`.
5. Edit `crates/ingot-agent-runtime/src/lib.rs` so system-action ticks also run projected-review recovery before returning.
6. Run the narrowest affected tests first, then record the observed commands and outcomes here.

## Validation and Acceptance

Acceptance means:

- `resume` still returns `200 OK` when the follow-on projected-review dispatch fails, and the item still becomes active.
- `complete` still returns `200 OK` when immediate projected-review dispatch fails for a recoverable reason.
- a later dispatcher tick that performs a convergence system action in the same pass still queues the missing projected review.

The minimum verification set is:

    cargo test -p ingot-http-api --test item_routes resume_route_returns_success_when_projected_review_auto_dispatch_cannot_bind_subject -- --exact
    cargo test -p ingot-http-api --test job_routes complete_route_recovers_projected_review_after_warning_only_dispatch_failure_on_system_action_tick -- --exact

The second test must fail before the runtime patch and pass after it.

Observed verification:

    cargo test -p ingot-http-api --test item_routes resume_route_returns_success_when_projected_review_auto_dispatch_cannot_bind_subject -- --exact
    cargo test -p ingot-http-api --test job_routes complete_route_recovers_projected_review_after_warning_only_dispatch_failure_on_system_action_tick -- --exact
    cargo test -p ingot-http-api --test item_routes resume_route_auto_dispatches_projected_review_job -- --exact
    cargo test -p ingot-http-api --test job_routes complete_route_auto_dispatches_candidate_review_after_clean_incremental_review -- --exact
    cargo test -p ingot-agent-runtime --test convergence tick_invalidates_stale_prepared_convergence -- --exact
    cargo test -p ingot-agent-runtime --test auto_dispatch tick_recovers_idle_review_work_even_when_processing_other_queued_jobs -- --exact

## Idempotence and Recovery

The focused tests use temporary repositories, temporary SQLite databases, and isolated state roots. Re-running them is safe. If a test fails after partially mutating its temporary database, rerun the same test; the fixture setup recreates all state from scratch.

## Artifacts and Notes

Key discovery commands:

    rg -n "projected review auto-dispatch failed|recover_projected_review_jobs|tick_system_actions" crates
    cargo test -p ingot-http-api --test item_routes resume_route_auto_dispatches_projected_review_job -- --exact
    cargo test -p ingot-http-api --test job_routes complete_route_auto_dispatches_candidate_review_after_clean_incremental_review -- --exact
    cargo test -p ingot-agent-runtime --test auto_dispatch tick_recovers_idle_review_work_even_when_processing_other_queued_jobs -- --exact

## Interfaces and Dependencies

The runtime patch stays inside:

    impl JobDispatcher {
        pub async fn tick(&self) -> Result<bool, RuntimeError> { ... }
    }

The cross-layer route regression may require the `ingot-http-api` test target to depend on:

    ingot-agent-runtime = { workspace = true }

if the test uses `JobDispatcher::new(...)` or `JobDispatcher::with_runner(...)` directly from the route test crate.

Revision note: created during investigation to track the degraded projected-review dispatch fix and its regression coverage.
Revision note: updated after implementation to record the actual degraded-state injection and the focused verification results.
