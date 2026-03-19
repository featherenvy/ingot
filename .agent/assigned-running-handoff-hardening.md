# Harden job launch handoff and remove duplicate supervisor launches

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows `.agent/PLANS.md` and must be maintained in accordance with that file.

## Purpose / Big Picture

After this change, one queued agent-backed job will produce at most one live runner process whether it is executed through the inline `tick()` path or the background `run_forever()` supervisor. The visible effect is not just quieter logs. The daemon must stop launching duplicate Codex subprocesses for the same `job_id`, stop leaving a queued job blocked behind a busy workspace when launch preparation fails, and stop retrying terminal mutations after the task has already lost ownership of the database row.

You will see the fix in three ways. First, a new runtime regression will prove that the supervisor cannot launch the same queued job twice even when execution is paused just before spawn. Second, a cleanup regression will prove that a prepared-but-unclaimed job releases its workspace instead of stranding the row in `Queued` with a `Busy` workspace. Third, startup reconciliation will still repair legacy `Assigned` rows so persisted state from older binaries remains recoverable.

This is not a logging-only change. The root bug is that the live agent path currently writes `status=assigned` in `prepare_run()`, but ordinary maintenance in `reconcile_active_jobs()` treats `Assigned` as abandoned work and requeues it immediately. The comprehensive fix is to stop persisting live agent work in `Assigned`, claim `Queued -> Running` atomically at the store boundary, keep `Assigned` only for legacy and daemon-only recovery paths, and make every runtime cleanup path preserve the same revision, lease, and workspace invariants.

## Progress

- [x] (2026-03-19 20:44Z) Re-read `.agent/PLANS.md`, inspected the runtime launch, heartbeat, completion, and reconciliation paths in `crates/ingot-agent-runtime/src/lib.rs`, and confirmed that the production symptom is a duplicate launch bug rather than operator cancellation.
- [x] (2026-03-19 20:45Z) Queried `~/.ingot/ingot.db` and `~/.ingot/logs/daemon.log` for `job_019d07cc767b74b1aa2353b33a6e490e` and confirmed two separate `prepared job execution` plus `job entered running state` sequences for the same `job_id` with different `workspace_id` values.
- [x] (2026-03-19 20:46Z) Confirmed the immediate trigger: `reconcile_active_jobs()` requeues every `Assigned` job during steady-state maintenance, which races with the JoinSet supervisor’s `prepare_run()` to create a second launch window.
- [x] (2026-03-19 20:47Z) Confirmed the architectural root cause: the persistent `Assigned` state means two incompatible things at once, “freshly claimed for launch” and “orphaned launch residue that recovery may requeue.”
- [x] (2026-03-19 20:48Z) Created and claimed beads issue `ingot-6l0` for this bug and recorded the production evidence there.
- [x] (2026-03-19 20:49Z) Authored this ExecPlan in `.agent/assigned-running-handoff-hardening.md`.
- [x] (2026-03-19 20:48Z) Deep-read every file already referenced by this plan plus adjacent state-mutating paths in `crates/ingot-domain/src/ports.rs`, `crates/ingot-domain/src/workspace.rs`, `crates/ingot-usecases/src/job.rs`, `crates/ingot-usecases/src/reconciliation.rs`, `crates/ingot-agent-runtime/tests/common/mod.rs`, and the runtime crate-private tests in `crates/ingot-agent-runtime/src/lib.rs`.
- [ ] Implement an agent-specific store claim that moves `Queued -> Running` and persists assignment metadata atomically, without broadening the generic `JobRepository::start_execution` trait.
- [ ] Refactor the agent runtime call paths (`tick()`, `execute_prepared_agent_job()`, `run_with_heartbeats()`, `launch_supervised_jobs()`, and `cleanup_supervised_task()`) so prepared work that never reaches the claim point still releases its workspace safely.
- [ ] Keep startup repair for legacy `Assigned` rows, stop steady-state maintenance from requeueing live agent launch handoffs, and leave the daemon-only validation path on its current `Assigned -> Running` flow until a dedicated follow-up changes it.
- [ ] Add store, runtime, and reconciliation regressions for atomic claim semantics, duplicate-launch prevention, queued-workspace cleanup, ownership-loss handling, and legacy assigned-row startup recovery.
- [ ] Run the focused test commands, then the broader Rust validation commands, update this plan with outcomes, and close `ingot-6l0` only if the implementation and validation both land cleanly.

## Surprises & Discoveries

- Observation: the observed `job_not_active` heartbeat warnings were not caused by another daemon or by operator cancellation.
  Evidence: `~/.ingot/logs/daemon.log` shows two `job entered running state` lines for `job_019d07cc767b74b1aa2353b33a6e490e`, both with `lease_owner_id=ingotd:65958`, and `~/.ingot/ingot.db` shows the final row as `completed/findings`, not `cancelled` or `expired`.

- Observation: the duplicate launch happened within one supervisor loop, not across a restart boundary.
  Evidence: the first `prepared job execution` for the job appears at `2026-03-19T20:32:13.683859Z`, and the second appears at `2026-03-19T20:32:13.883829Z`, well before either runner finished.

- Observation: `reconcile_active_jobs()` currently treats every `Assigned` row as something to repair immediately, even during ordinary steady-state operation.
  Evidence: `crates/ingot-agent-runtime/src/lib.rs` calls `reconcile_assigned_job(job)` for every `JobStatus::Assigned` in `reconcile_active_jobs()`, and `reconcile_assigned_job()` sets the job back to `Queued` and releases the workspace.

- Observation: the JoinSet supervisor has no secondary protection against duplicate launch after `prepare_run()` returns.
  Evidence: `launch_supervised_jobs()` scans `list_queued_jobs(32)`, calls `prepare_run(job.clone())`, and inserts metadata keyed only by Tokio `TaskId`; it does not track “this `job_id` is already prepared or running in this process.”

- Observation: the persistence boundary is the real leverage point, because the current race exists before the child process has even meaningfully started work.
  Evidence: `prepare_run()` mutates the workspace row, assembles the prompt, writes `status=assigned` via `update_job()`, and only later does `run_with_heartbeats()` call `start_job_execution(...)` to flip the row to `running`.

- Observation: removing live `Assigned` from the agent path exposes a second bug that the original plan did not call out: `prepare_run()` persists a busy workspace before it persists `Assigned`, while `cleanup_supervised_task()` only knows how to recover rows that are still `Assigned` or `Running`.
  Evidence: `prepare_run()` calls `workspace.attach_job(job.id, now)` and persists the workspace before `job.assign(...)`; `cleanup_supervised_task()` in `crates/ingot-agent-runtime/src/lib.rs` branches only on `JobStatus::Assigned` and `JobStatus::Running`. If the job remains `Queued`, the current cleanup does nothing.

- Observation: the daemon-only validation path still uses a persisted `Assigned -> Running` handoff, but it does so while holding the project mutation lock all the way through `start_job_execution()`.
  Evidence: `prepare_harness_validation()` in `crates/ingot-agent-runtime/src/lib.rs` updates the workspace, calls `job.assign(JobAssignment::new(workspace.id))`, persists the job, and immediately calls `start_job_execution(...)` before returning. This is a distinct path from the reproduced agent-launch race and constrains how much of the generic store API should be changed in the first patch.

- Observation: report-job completion and commit-job completion already use different seams, so “completion hardening” must refer to the real call sites instead of one imagined helper.
  Evidence: authoring commit jobs call `Database::apply_job_completion(...)` directly in `complete_commit_run()`, while report-producing jobs go through `CompleteJobService::execute(...)` in `crates/ingot-usecases/src/job.rs`, which already maps `RepositoryError::Conflict("job_not_active")` to `UseCaseError::JobNotActive`.

## Decision Log

- Decision: make the primary fix an atomic database claim into `Running` immediately before spawn, rather than trying to make steady-state `Assigned` safe with more timing checks.
  Rationale: the duplicate launch exists because the live path exposes `Assigned` to a concurrent recovery loop. If the runtime persists `Running` as the first launch-state transition, the database itself becomes the serialization point: at most one process can win the claim, and no steady-state maintenance path needs to reason about a live “about to spawn” row.
  Date/Author: 2026-03-19 / Codex

- Decision: keep `Assigned` in the domain and reconciliation code for rollout compatibility, but stop using it in the ordinary agent-backed launch path.
  Rationale: removing the enum variant and every recovery path in the same patch would broaden scope unnecessarily and would break existing startup tests that explicitly cover stale assigned-row cleanup. The safer first step is “no new live uses,” with cleanup of legacy assigned rows preserved.
  Date/Author: 2026-03-19 / Codex

- Decision: add a supervisor-local `job_id` in-flight set even after the database transition is hardened.
  Rationale: atomic DB claiming prevents the production race, but local dedupe is a cheap defense-in-depth measure that turns future lifecycle drift into a skipped launch rather than a second subprocess.
  Date/Author: 2026-03-19 / Codex

- Decision: steady-state maintenance must stop requeueing `Assigned` jobs during normal runtime ticks; assigned-row repair belongs to startup recovery and explicit task-cleanup paths only.
  Rationale: even if a future code path still creates `Assigned`, the steady-state supervisor should not compete with its own launch handoff. Recovery of abandoned rows should happen only when there is evidence that launch ownership was lost, not on every maintenance pass.
  Date/Author: 2026-03-19 / Codex

- Decision: harden the heartbeat loop so `job_not_active` is treated as a terminal execution-state mismatch, not as an endless warning stream.
  Rationale: once a runner can prove that its DB row is no longer active, continued heartbeats add noise and obscure the first real error. This is not the primary fix, but it prevents future lifecycle mismatches from degenerating into minute-long warning spam.
  Date/Author: 2026-03-19 / Codex

- Decision: add a new agent-specific store helper instead of changing `JobRepository::start_execution` semantics for every caller.
  Rationale: `JobDispatcher` talks to a concrete `Database`, while daemon-only validation still legitimately uses `start_job_execution()` after persisting `Assigned`. A new helper keeps the trait and the use-case fakes stable, and it lets the first patch harden only the path that reproduced the bug.
  Date/Author: 2026-03-19 / Codex

- Decision: explicitly clean up prepared workspaces when agent execution never reaches the claim point.
  Rationale: once `prepare_run()` stops writing `Assigned`, the existing `cleanup_supervised_task()` logic is no longer sufficient. The workspace attachment becomes an invariant of the prepared runtime path, not of the persisted job row, so the cleanup plan must name that release explicitly.
  Date/Author: 2026-03-19 / Codex

- Decision: keep daemon-only validation on its existing launch path in the first implementation, but call out the asymmetry in the plan and tests.
  Rationale: `prepare_harness_validation()` currently reaches `Running` while still holding the project lock, so it is not the reproduced same-daemon race. Folding it into the first patch would change more code than the investigation justifies. The plan must still mention it so a future implementation does not accidentally break `start_job_execution()` compatibility.
  Date/Author: 2026-03-19 / Codex

- Decision: replace the agent runtime’s “generic `AgentError` plus fallback `fail_run()`” handling with an explicit ownership-loss branch.
  Rationale: both `tick()` and `execute_prepared_agent_job()` currently treat any non-timeout runtime failure as something to terminally fail again unless the job is already `Cancelled`. When `run_with_heartbeats()` loses ownership because another task already completed the row, those branches should stop cleanly instead of calling `fail_run()` and generating an extra `job_not_active` conflict.
  Date/Author: 2026-03-19 / Codex

## Outcomes & Retrospective

No implementation has landed yet. The investigation changed the understanding of the bug in two important ways. First, the repeated heartbeat warnings are not the root problem; they are the tail symptom of two live runners sharing one `job_id`. Second, the problem is not just “reconciliation is too eager.” The deeper issue is that the current lifecycle lets a live launch inhabit a recovery-only state (`Assigned`) long enough for the maintenance loop to treat it as abandoned.

The deeper audit in this revision uncovered two concrete execution details that the first draft of this plan did not spell out. One is that queued jobs can strand a `Busy` workspace if the plan removes live `Assigned` without adding a new cleanup path. The other is that daemon-only validation still depends on the generic `start_job_execution()` helper and therefore constrains how aggressively the store API can be rewritten in the first patch.

The recommended implementation strategy is therefore broader and more exact than a one-line guard in `reconcile_active_jobs()`. The fix should add an agent-specific atomic claim helper, reshape runtime cleanup around prepared workspaces, preserve startup repair for legacy assigned rows, leave daemon-only validation behavior stable, add in-process dedupe, and explicitly test that one queued agent job produces one runner process.

## Context and Orientation

The daemon entry point in `apps/ingot-daemon/src/main.rs` constructs one `JobDispatcher` and runs `run_forever()` in the background. The runtime implementation lives in `crates/ingot-agent-runtime/src/lib.rs`. There are two execution modes for agent-backed jobs in that file.

The inline path is `tick()`. It calls `drive_non_job_work()`, selects one queued job with `next_runnable_job()`, prepares it with `prepare_run()`, and then calls `run_with_heartbeats()` directly before finishing or failing the run in the same task.

The background path is `run_forever()`. It loops through `run_supervisor_iteration()`, which calls `drive_non_job_work()`, launches new work in `launch_supervised_jobs()`, and reaps finished tasks with `handle_supervised_join_result()`. A JoinSet is Tokio’s container for many concurrently running tasks. In this repository, it is the background supervisor’s job pool.

The state machine for jobs lives in `crates/ingot-domain/src/job.rs`. The important statuses are `Queued`, `Assigned`, and `Running`. `Queued` means launchable. `Assigned` means a workspace and optionally an agent, prompt snapshot, and template digest were recorded in the row. `Running` adds the execution lease: `lease_owner_id`, `heartbeat_at`, `lease_expires_at`, and `started_at`. The wire encoding and decoding for these fields also lives in `crates/ingot-domain/src/job.rs`, so any change in when those fields are written must still round-trip through `JobWire`.

The matching workspace state lives in `crates/ingot-domain/src/workspace.rs`. A workspace becomes `Busy` when `Workspace::attach_job(job_id, now)` is called, and it is released through `Workspace::release_to(...)` or marked stale through `Workspace::mark_stale(...)`. This matters because the runtime persists workspace attachment before it persists `Assigned`.

The SQLite job store lives in `crates/ingot-store-sqlite/src/store/job.rs`. The current `start_job_execution(StartJobExecutionParams)` helper moves a row to `Running` from either `Queued` or `Assigned`, requires a workspace binding, and guards against stale item revisions by checking that `items.current_revision_id` still matches `expected_item_revision_id`. `heartbeat_job_execution(...)` refreshes the lease only when the row is still `Running` and owned by the same `lease_owner_id`. `finish_job_non_success(...)` can terminate jobs from any active status. `list_active_jobs()` and `find_active_job_for_revision()` both still treat `Queued`, `Assigned`, and `Running` as active.

Job completion uses a different store file, `crates/ingot-store-sqlite/src/store/job_completion.rs`. Authoring jobs call `apply_job_completion(...)` directly from `complete_commit_run()` in the runtime. Report jobs call `CompleteJobService::execute(...)` in `crates/ingot-usecases/src/job.rs`, which loads the current job, validates it, extracts findings, and then calls the same store mutation. That use case already maps `RepositoryError::Conflict("job_not_active")` into `UseCaseError::JobNotActive`.

Maintenance and startup reconciliation are defined one layer up in `crates/ingot-usecases/src/reconciliation.rs`. `ReconciliationService::reconcile_startup()` and `tick_maintenance()` both call the runtime port methods in this order: git operations, active jobs, active convergences, workspace retention. The runtime implementation of the active-job stage is `JobDispatcher::reconcile_active_jobs()` in `crates/ingot-agent-runtime/src/lib.rs`. Today it scans `Database::list_active_jobs()`, ignores queued jobs, requeues every assigned job via `reconcile_assigned_job()`, and expires stale or foreign-owner running jobs via `reconcile_running_job()`.

The runtime tests are split across three places. End-to-end dispatch tests live in `crates/ingot-agent-runtime/tests/dispatch.rs` and use helpers from `crates/ingot-agent-runtime/tests/common/mod.rs`, including `BlockingRunner`, `wait_for_job_status(...)`, and `wait_for_running_jobs(...)`. Startup and maintenance reconciliation tests live in `crates/ingot-agent-runtime/tests/reconciliation.rs`. Crate-private tests for pre-spawn pause hooks and low-level runtime behavior live inside `crates/ingot-agent-runtime/src/lib.rs`, where private types like `PreSpawnPauseHook` are accessible.

The store re-export surface in `crates/ingot-store-sqlite/src/lib.rs` currently only re-exports `Database`, `FinishJobNonSuccessParams`, and `StartJobExecutionParams`. If this plan adds a new agent-specific claim params type that the runtime imports from the crate root, that file must also be updated.

### Invariant-bearing fields

The durable guard for job staleness is `job.item_revision_id`, passed through the runtime as `expected_item_revision_id`. The runtime checks it in `prepare_run()` and `prepare_harness_validation()` by comparing it to `item.current_revision_id`. The store re-checks it in `start_job_execution(...)`, `heartbeat_job_execution(...)`, `finish_job_non_success(...)`, and `apply_job_completion(...)`. Any new claim helper must preserve the same guard.

The durable launch metadata for agent-backed jobs is `workspace_id`, `agent_id`, `prompt_snapshot`, and `phase_template_digest`. Today `prepare_run()` creates that metadata in memory and then persists it through `job.assign(...)` and `update_job(&job)`. If `prepare_run()` stops writing `Assigned`, those same fields still need to be persisted in the first successful claim to `Running`, because terminal rows and serde rely on them.

The durable execution lease is `lease_owner_id`, `heartbeat_at`, `lease_expires_at`, and `started_at`. Those fields are created by `start_job_execution(...)`, refreshed by `heartbeat_job_execution(...)`, and consumed by `reconcile_running_job()`. The plan must preserve the same lease semantics and keep `process_pid` optional, because the current agent claim happens before child spawn and there is no existing post-spawn PID update path.

The workspace-side guard is `WorkspaceState::Busy { current_job_id }`. It is created by `Workspace::attach_job(...)` during preparation and cleared by `Workspace::release_to(...)` or replaced by `Workspace::mark_stale(...)`. If a prepared job never reaches the atomic claim, the workspace still has to be released explicitly because startup reconciliation does not repair `Queued` jobs with busy workspaces.

## Plan of Work

The implementation should happen in five passes that correspond to the real seams in the code.

First, add an agent-specific atomic claim helper in the SQLite store. Do this in `crates/ingot-store-sqlite/src/store/job.rs`, and re-export any new params type from `crates/ingot-store-sqlite/src/lib.rs` only if the runtime needs crate-root imports. Do not replace `StartJobExecutionParams` or the `JobRepository::start_execution` trait method in this first patch. The new helper should update a row only when it is still `Queued`, persist `workspace_id`, `agent_id`, `prompt_snapshot`, `phase_template_digest`, `lease_owner_id`, `heartbeat_at`, `lease_expires_at`, and `started_at`, and continue to guard on `items.current_revision_id = expected_item_revision_id`. It should not require `process_pid`, because that value is not available until after spawn and the current runtime has no separate PID update step.

Second, refactor the agent preparation path so `prepare_run()` computes launch metadata but does not persist `Assigned`. In `crates/ingot-agent-runtime/src/lib.rs`, keep the current project lock, item revision check, workspace provisioning, and prompt assembly, but stop calling `job.assign(...)` followed by `self.db.update_job(&job)`. Instead, store the needed assignment metadata directly on `PreparedRun`, either as a `JobAssignment` field or as an equivalent small launch-metadata struct, because the later claim helper still needs `workspace_id`, `agent_id`, `prompt_snapshot`, and `phase_template_digest`.

Third, move the live claim boundary and repair the cleanup path around it. The atomic claim must happen in the shared agent execution path used by both `tick()` and the background supervisor, not only in `launch_supervised_jobs()`, because both execution modes call `prepare_run()` and then `run_with_heartbeats()`. `run_with_heartbeats()` should claim the queued row before the spawn pause hook and before the cancellation check, because “queued but prepared” is the state that must disappear from the live launch path. Once this is true, `cleanup_supervised_task()` also needs a new branch for the case where the current job is still `Queued` but the prepared workspace is `Busy` with `current_job_id = prepared.job.id`; that branch must release the workspace to `Ready` so a failed prepare or failed claim does not strand the queue. The same cleanup logic should be available to any direct error path in `prepare_run()` after `workspace.attach_job(...)` has already been persisted.

Fourth, narrow steady-state reconciliation without deleting legacy recovery. In `crates/ingot-agent-runtime/src/lib.rs`, remove the `JobStatus::Assigned` branch from ordinary `reconcile_active_jobs()` so `tick_maintenance()` and `run_forever()` stop requeueing their own live handoff state. Keep `reconcile_assigned_job()` for `reconcile_startup()` and for explicit cleanup paths that are truly dealing with abandoned persisted rows. Do not change `prepare_harness_validation()` or the generic `start_job_execution()` helper in the same patch, because that daemon-only path still intentionally persists `Assigned` and immediately transitions to `Running` while holding the project lock.

Fifth, add defense in depth and ownership-loss handling around the runtime result paths. `launch_supervised_jobs()` should keep a `HashSet<JobId>` next to `running_meta`, insert the `job_id` when a task is spawned, skip any queued snapshot row already in that set, and remove the `job_id` in `handle_supervised_join_result()`. In addition, `run_with_heartbeats()` should stop returning an undifferentiated generic `AgentError` for lost ownership. Introduce a runtime-local result or error type that distinguishes timeout, cancellation, generic launch/process failures, and “job ownership lost.” Then update both call sites that currently duplicate response handling, `tick()` and `execute_prepared_agent_job()`, so they do not call `fail_run()` when the row is already terminal or otherwise no longer owned by this task. This is the actual way to eliminate the trailing `job_not_active` noise after the winner has already completed the job.

### Paths that must stay aligned

The plan must keep these mutating paths aligned on the same invariants.

`prepare_run()`, `run_with_heartbeats()`, `tick()`, and `execute_prepared_agent_job()` are one logical launch-and-finish pipeline even though they are split across several functions. Any new ownership-loss branch or claim helper must be wired through both the inline and supervised execution paths.

`cleanup_supervised_task()` and `reconcile_assigned_job()` are different kinds of recovery. The first repairs work that was already prepared in this process and then failed. The second repairs persisted rows that look abandoned. After removing live `Assigned`, those responsibilities cannot stay implicit.

`start_job_execution(...)`, `finish_job_non_success(...)`, and `apply_job_completion(...)` all mutate the same `jobs` row and all rely on the item revision guard. The new claim helper must preserve that same guard rather than creating an unguarded side path.

`prepare_harness_validation()` is intentionally different from `prepare_run()`. The revised implementation must mention that difference so a novice does not “simplify” both paths into the same helper and accidentally break daemon-only validation semantics.

## Concrete Steps

Work from `/Users/aa/Documents/ingot`.

1. Add the new store claim helper and its tests.

   Read:

       crates/ingot-store-sqlite/src/store/job.rs
       crates/ingot-store-sqlite/src/lib.rs
       crates/ingot-domain/src/ports.rs
       crates/ingot-domain/src/job.rs

   Implement:

       - a new SQLite helper for agent-backed launch claims that only succeeds from `Queued`
       - persistence of assignment metadata (`workspace_id`, `agent_id`, `prompt_snapshot`, `phase_template_digest`) in that same claim
       - no change to the existing `JobRepository::start_execution` trait method or to the daemon-only validation caller

   Add tests in `crates/ingot-store-sqlite/src/store/job.rs` with stable names:

       - `claim_queued_agent_job_execution_persists_assignment_and_running_lease`
       - `claim_queued_agent_job_execution_rejects_rows_that_left_queued`

2. Refactor `PreparedRun` and the agent launch pipeline.

   Read:

       crates/ingot-agent-runtime/src/lib.rs
       crates/ingot-domain/src/workspace.rs

   Implement:

       - `prepare_run()` computes assignment metadata but no longer persists `Assigned`
       - `PreparedRun` stores the metadata needed by the new claim helper
       - `run_with_heartbeats()` performs the atomic claim before the spawn pause hook and before the cancellation-before-spawn check
       - `tick()` and `execute_prepared_agent_job()` share one ownership-aware result-handling path rather than duplicating slightly different error branches

3. Repair cleanup and reconciliation boundaries.

   Read:

       crates/ingot-agent-runtime/src/lib.rs
       crates/ingot-agent-runtime/tests/reconciliation.rs
       crate-private tests in crates/ingot-agent-runtime/src/lib.rs near the existing pre-spawn cases

   Implement:

       - a cleanup path for prepared agent work that never reached the claim point and therefore still has `status=queued`
       - steady-state `reconcile_active_jobs()` no longer requeues `Assigned`
       - startup reconciliation still requeues legacy `Assigned` rows
       - no behavior change to `prepare_harness_validation()` in this patch, beyond comments or small clarifying test updates if needed

4. Add duplicate-launch and ownership-loss regressions where the current harnesses can actually express them.

   Use crate-private tests in `crates/ingot-agent-runtime/src/lib.rs` for behaviors that need `PreSpawnPauseHook`, and keep `crates/ingot-agent-runtime/tests/dispatch.rs` and `crates/ingot-agent-runtime/tests/reconciliation.rs` for the broader integration checks.

   Add or update tests with stable names:

       - `supervisor_does_not_launch_same_job_twice_during_pre_spawn_pause`
       - `cleanup_supervised_task_releases_workspace_for_unclaimed_prepared_agent_job`
       - `run_with_heartbeats_stops_after_job_row_becomes_inactive`
       - `run_with_heartbeats_does_not_launch_runner_when_job_is_cancelled_before_spawn`
       - `reconcile_startup_handles_mixed_inflight_states_conservatively`

   Keep the existing dispatch regressions green, especially:

       - `run_forever_starts_next_job_on_joinset_completion`
       - `run_forever_starts_next_job_after_running_job_cancellation`
       - `run_forever_refreshes_heartbeat_while_job_is_running`

5. Validate incrementally, then broadly.

   Run:

       cd /Users/aa/Documents/ingot
       cargo test -p ingot-store-sqlite claim_queued_agent_job_execution_persists_assignment_and_running_lease -- --exact
       cargo test -p ingot-store-sqlite claim_queued_agent_job_execution_rejects_rows_that_left_queued -- --exact
       cargo test -p ingot-agent-runtime --lib supervisor_does_not_launch_same_job_twice_during_pre_spawn_pause -- --exact
       cargo test -p ingot-agent-runtime --lib cleanup_supervised_task_releases_workspace_for_unclaimed_prepared_agent_job -- --exact
       cargo test -p ingot-agent-runtime --lib run_with_heartbeats_stops_after_job_row_becomes_inactive -- --exact
       cargo test -p ingot-agent-runtime --lib run_with_heartbeats_does_not_launch_runner_when_job_is_cancelled_before_spawn -- --exact
       cargo test -p ingot-agent-runtime --test reconciliation reconcile_startup_handles_mixed_inflight_states_conservatively -- --exact
       cargo test -p ingot-agent-runtime --test dispatch
       cargo test -p ingot-agent-runtime --test auto_dispatch
       cargo check -p ingot-daemon
       make test

   If one of the exact-test commands changes because the implementation uses a different stable name, update this plan immediately before stopping.

## Validation and Acceptance

Acceptance is reached when all of the following are true.

1. The agent-backed runtime path cannot create a live persisted `Assigned` row as part of ordinary launch. The first durable transition for that path is `Queued -> Running`, and it persists the assignment metadata and lease together.

2. The new claim helper succeeds only while the row is still `Queued` and the item revision still matches. A second claimant, a stale item revision, or a row that already left `Queued` produces a conflict instead of a second launch.

3. Both execution entry points, the inline `tick()` path and the supervised `run_forever()` path, use the same claim boundary and the same ownership-loss handling. There must not be one hardened path and one leftover path that still writes `Assigned`.

4. If preparation attached a workspace but execution never reached the claim point, the workspace returns to `Ready` and the queued job remains launchable. The fix is not acceptable if it merely swaps duplicate launches for permanently busy workspaces.

5. Ordinary `tick_maintenance()` no longer requeues assigned jobs, but `reconcile_startup()` still converts legacy assigned rows back to `Queued` and releases their workspaces, as proved by `reconcile_startup_handles_mixed_inflight_states_conservatively`.

6. The supervisor keeps one in-memory `job_id` entry per spawned task and removes it when the task is reaped. A queued snapshot row already represented in that set is skipped instead of prepared a second time.

7. When a running task loses ownership and `heartbeat_job_execution(...)` returns `job_not_active`, the runtime stops heartbeating and exits through an explicit ownership-loss path. `tick()` and `execute_prepared_agent_job()` do not follow that path with a second `fail_run()` call.

8. The focused exact tests above pass, followed by `cargo test -p ingot-agent-runtime --test dispatch`, `cargo test -p ingot-agent-runtime --test auto_dispatch`, `cargo check -p ingot-daemon`, and `make test`, except for any unrelated pre-existing failures recorded in `Progress` and `Outcomes & Retrospective`.

The proof must be behavioral, not rhetorical. In particular, the duplicate-launch regression should demonstrate one launch count and one running row for one queued job under a forced pre-spawn pause, because that is the closest repository-native reproduction of the production race.

## Idempotence and Recovery

This change should not require a schema migration. It reuses existing job columns for assignment metadata and lease fields, and it leaves the `Assigned` variant in the domain model for compatibility and startup recovery.

The riskiest partial state during implementation is a queued job with a busy workspace. That state already exists today if `prepare_run()` fails after `workspace.attach_job(...)`, and removing live `Assigned` makes it easier to strand unless cleanup is updated deliberately. Do not consider the refactor complete until there is an automated regression for that case.

Because the first patch keeps daemon-only validation on the existing `Assigned -> Running` path, the generic `start_job_execution(...)` helper must continue to accept `Assigned` rows. Do not tighten its SQL predicate to only `Queued` unless `prepare_harness_validation()` is migrated in the same patch.

If implementation stops midway, rerun the exact store and runtime tests before moving on. If a failed claim leaves a workspace busy, the safest local recovery is to fix the cleanup bug and rerun the tests; startup reconciliation will not repair a `Queued` row that still owns a busy workspace because `reconcile_active_jobs()` ignores queued rows.

If a new daemon binary is deployed while old `Assigned` rows already exist, startup reconciliation still repairs them. A mixed-version, no-restart environment could still leave legacy assigned rows waiting for that startup repair, so record that explicitly if it matters during rollout.

## Artifacts and Notes

When implementation lands, record the most useful evidence here. At minimum include:

    cargo test -p ingot-store-sqlite claim_queued_agent_job_execution_persists_assignment_and_running_lease -- --exact
    test claim_queued_agent_job_execution_persists_assignment_and_running_lease ... ok

    cargo test -p ingot-agent-runtime --lib supervisor_does_not_launch_same_job_twice_during_pre_spawn_pause -- --exact
    test supervisor_does_not_launch_same_job_twice_during_pre_spawn_pause ... ok

    cargo test -p ingot-agent-runtime --lib cleanup_supervised_task_releases_workspace_for_unclaimed_prepared_agent_job -- --exact
    test cleanup_supervised_task_releases_workspace_for_unclaimed_prepared_agent_job ... ok

    cargo test -p ingot-agent-runtime --test reconciliation reconcile_startup_handles_mixed_inflight_states_conservatively -- --exact
    test reconcile_startup_handles_mixed_inflight_states_conservatively ... ok

    cargo check -p ingot-daemon
    Finished `dev` profile ... target(s) in ...

If you reproduce the original bug manually after implementation, include a short daemon-log excerpt with one `prepared job execution`, one `job entered running state`, and no repeated `job heartbeat update failed` lines for the same `job_id` after completion.

## Interfaces and Dependencies

At the end of this work, the runtime and store interfaces should make it obvious which helper is for agent-backed launch and which helper remains for the daemon-only validation path.

In `crates/ingot-store-sqlite/src/store/job.rs`, add a new helper for agent-backed launch, for example:

    pub async fn claim_queued_agent_job_execution(
        &self,
        params: ClaimQueuedAgentJobExecutionParams,
    ) -> Result<(), RepositoryError>

The params type should include exactly the fields the current `prepare_run()` path already creates:

    - `job_id`
    - `item_id`
    - `expected_item_revision_id`
    - `workspace_id`
    - `agent_id`
    - `prompt_snapshot`
    - `phase_template_digest`
    - `lease_owner_id`
    - `lease_expires_at`

Do not include `process_pid` in this first helper, because the existing code claims the job before child spawn and there is no current post-spawn PID persistence step.

Keep `StartJobExecutionParams` in `crates/ingot-domain/src/ports.rs` and `Database::start_job_execution(...)` in `crates/ingot-store-sqlite/src/store/job.rs` for the daemon-only validation path. That path still assigns first and starts immediately while holding the project lock.

In `crates/ingot-agent-runtime/src/lib.rs`, `PreparedRun` should carry the claim metadata needed by the new store helper. `prepare_run()` should return that metadata without persisting it. `run_with_heartbeats()` should become the only place where the agent-backed job row transitions from queued to running. `tick()` and `execute_prepared_agent_job()` should both consume a shared runtime-local result type or helper that can distinguish:

    - successful agent response
    - timeout
    - operator cancellation
    - ownership lost / job no longer active
    - generic launch or process failure

The supervisor should maintain a `HashSet<ingot_domain::ids::JobId>` alongside `running_meta`. `TaskId` remains useful for JoinSet bookkeeping, but it is not sufficient to prevent the same `job_id` from being prepared twice in one process.

No new external dependency should be needed. Tokio, SQLx, the existing use-case layer, and the current test harnesses are sufficient.

Revision note: revised on 2026-03-19 after a deeper code audit of every referenced file and adjacent lifecycle path. This pass corrected a few inaccurate assumptions from the first draft: the atomic claim helper should be agent-specific rather than a blanket rewrite of `start_job_execution()`, `process_pid` cannot be part of the pre-spawn atomic claim, the inline `tick()` path must stay aligned with the supervised path, and removing live `Assigned` requires an explicit cleanup path for queued jobs that already attached a busy workspace. It also added concrete coverage for the daemon-only validation path, the completion/use-case seam, the new store tests, and the crate-private runtime tests needed to reproduce the race reliably.
