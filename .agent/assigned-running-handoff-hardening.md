# Harden job launch handoff and remove duplicate supervisor launches

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows `.agent/PLANS.md` and must be maintained in accordance with that file.

## Purpose / Big Picture

After this change, one queued job will produce at most one live runner process, even under the background JoinSet supervisor. The user-visible result is that the daemon no longer starts duplicate Codex subprocesses for the same `job_id`, no longer spams `job heartbeat update failed error=Conflict("job_not_active")` while a losing duplicate keeps running, and no longer leaves duplicate review or authoring work competing against the same database row. You will be able to see the fix by running focused runtime tests that fail before the change and pass after it: one regression should prove that `run_forever()` launches a queued job only once, and another should prove that no post-terminal heartbeat warnings are emitted for the same job after successful completion.

This is not just a logging cleanup. The root bug is a lifecycle inconsistency between the state machine, the supervisor loop, and the recovery loop. The live runtime currently persists `Assigned` as an intermediate state, but the steady-state reconciliation path treats `Assigned` as abandoned work and requeues it immediately. The comprehensive fix is to make the live handoff from “launchable” to “running” atomic from the database’s perspective, keep recovery logic for truly abandoned rows, and add a second line of defense in the supervisor so a future lifecycle bug cannot spawn the same `job_id` twice.

## Progress

- [x] (2026-03-19 20:44Z) Re-read `.agent/PLANS.md`, inspected the runtime launch, heartbeat, completion, and reconciliation paths in `crates/ingot-agent-runtime/src/lib.rs`, and confirmed that the production symptom is a duplicate launch bug rather than operator cancellation.
- [x] (2026-03-19 20:45Z) Queried `~/.ingot/ingot.db` and `~/.ingot/logs/daemon.log` for `job_019d07cc767b74b1aa2353b33a6e490e` and confirmed two separate `prepared job execution` plus `job entered running state` sequences for the same `job_id` with different `workspace_id` values.
- [x] (2026-03-19 20:46Z) Confirmed the immediate trigger: `reconcile_active_jobs()` requeues every `Assigned` job during steady-state maintenance, which races with the JoinSet supervisor’s `prepare_run()` to create a second launch window.
- [x] (2026-03-19 20:47Z) Confirmed the architectural root cause: the persistent `Assigned` state means two incompatible things at once, “freshly claimed for launch” and “orphaned launch residue that recovery may requeue.”
- [x] (2026-03-19 20:48Z) Created and claimed beads issue `ingot-6l0` for this bug and recorded the production evidence there.
- [x] (2026-03-19 20:49Z) Authored this ExecPlan in `.agent/assigned-running-handoff-hardening.md`.
- [ ] Implement the store and runtime refactor that removes the live `Queued -> Assigned -> Running` handoff window in favor of an atomic `Queued -> Running` claim just before spawn.
- [ ] Keep a narrow recovery path for legacy or abandoned `Assigned` rows so startup reconciliation remains safe during rollout.
- [ ] Add supervisor-local duplicate-launch guards keyed by `job_id`, plus targeted regressions for duplicate spawn, post-completion heartbeat silence, and startup recovery of legacy assigned rows.
- [ ] Run the focused runtime suites, then the broader Rust validation commands, update this plan with outcomes, and close `ingot-6l0` if the implementation lands cleanly.

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

- Observation: the existing `Assigned` state is still useful as a recovery concept for old rows or partially rolled out daemons, but it is a poor steady-state launch state.
  Evidence: `crates/ingot-agent-runtime/tests/reconciliation.rs` already contains `reconcile_startup_handles_mixed_inflight_states_conservatively()`, which expects startup reconciliation to repair stale `Assigned` rows.

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

## Outcomes & Retrospective

No implementation has landed yet. The investigation changed the understanding of the bug in two important ways. First, the repeated heartbeat warnings are not the root problem; they are the tail symptom of two live runners sharing one `job_id`. Second, the problem is not just “reconciliation is too eager.” The deeper issue is that the current lifecycle lets a live launch inhabit a recovery-only state (`Assigned`) long enough for the maintenance loop to treat it as abandoned.

The recommended implementation strategy is therefore broader than a one-line guard in `reconcile_active_jobs()`. The fix should make the persisted launch handoff atomic, keep startup repair for legacy assigned rows, add in-process dedupe, and explicitly test that one queued job produces one runner process.

## Context and Orientation

The daemon entry point in `apps/ingot-daemon/src/main.rs` constructs one `JobDispatcher` and runs `run_forever()` in the background. The background loop lives in `crates/ingot-agent-runtime/src/lib.rs`. That file contains four runtime areas that matter for this bug:

`prepare_run()` reads the queued job, loads item and revision context, provisions or creates the workspace, assembles the prompt, assigns the job to a workspace and agent, and persists the full row back to the database. Today it ends by calling `job.assign(...)` and `self.db.update_job(&job)`, which leaves the row in `status=assigned`.

`run_with_heartbeats()` is the agent-backed execution loop. It is called after `prepare_run()` succeeds. It persists `status=running` by calling `start_job_execution(...)`, spawns the agent runner, watches for completion, timeout, cancellation notifications, and heartbeat ticks, and refreshes the running lease through `heartbeat_job_execution(...)`.

`launch_supervised_jobs()` is the JoinSet-based launcher used by `run_forever()`. It iterates over `self.db.list_queued_jobs(32)`, tries to prepare each job, and spawns Tokio tasks for prepared runs. It currently tracks spawned work only in `running_meta`, keyed by Tokio `TaskId`.

`reconcile_active_jobs()` is part of the background maintenance pass. It scans `self.db.list_active_jobs()`. For `Assigned` jobs it calls `reconcile_assigned_job()`, which immediately requeues the job and releases the workspace. For `Running` jobs it may expire stale leases.

The persistence layer lives in `crates/ingot-store-sqlite/src/store/job.rs` and `crates/ingot-store-sqlite/src/store/job_completion.rs`. `start_job_execution(...)` already performs a compare-and-swap update from `status IN ('queued', 'assigned')` to `status='running'` while setting workspace, agent, process, and lease fields. `heartbeat_job_execution(...)` succeeds only while the row remains `status='running'`. `apply_job_completion(...)` moves `queued`, `assigned`, or `running` jobs to `completed`, which means a losing duplicate can still finish if it gets there first.

The lifecycle model is defined in `crates/ingot-domain/src/job.rs`. `JobState::Assigned` currently stores only `JobAssignment`, while `JobState::Running` adds a `JobLease`. The current design makes `Assigned` look like a live state, but the runtime’s steady-state maintenance treats it as restart residue.

The runtime tests live in `crates/ingot-agent-runtime/tests/dispatch.rs` and `crates/ingot-agent-runtime/tests/reconciliation.rs`. `dispatch.rs` already contains JoinSet and heartbeat regressions such as `run_forever_starts_next_job_on_joinset_completion()`, `run_forever_starts_next_job_after_running_job_cancellation()`, and `run_forever_refreshes_heartbeat_while_job_is_running()`. `reconciliation.rs` already contains assigned-row startup repair coverage in `reconcile_startup_handles_mixed_inflight_states_conservatively()`.

## Plan of Work

The implementation should proceed in four linked passes.

First, narrow the meaning of `Assigned` in the runtime. In `crates/ingot-agent-runtime/src/lib.rs`, change the live agent-backed launch path so `prepare_run()` no longer persists `job.assign(...)` through the generic `update_job()` call. It should still provision the workspace, assemble the prompt, and return a fully populated `PreparedRun`, but the first database state transition for a newly launched agent-backed job should happen in one compare-and-swap call immediately before process spawn. That compare-and-swap should move the row from `Queued` to `Running` while persisting the workspace binding, agent binding, prompt snapshot, phase-template digest, lease owner, and initial lease timestamps. The cleanest implementation is either to extend `start_job_execution(...)` to accept the assignment metadata that `prepare_run()` currently writes, or to replace it with a new store method such as `claim_and_start_job_execution(...)` that captures all of those fields atomically. The winner of that DB update is the only code path that may spawn the child process.

Second, restrict recovery behavior so it cannot fight the live launch path. In `crates/ingot-agent-runtime/src/lib.rs`, remove `JobStatus::Assigned` handling from steady-state `reconcile_active_jobs()`. Keep `reconcile_assigned_job()` for two narrower call sites: startup reconciliation of legacy or abandoned rows, and panic/error cleanup when a supervised task dies before it ever reaches `Running`. This preserves the repository’s existing conservative recovery behavior without letting the maintenance tick race normal launch. Update the startup reconciliation tests to make that scope explicit: startup should still repair assigned rows, but normal `run_forever()` maintenance should not requeue a freshly claimed launch.

Third, add supervisor-local duplicate-launch guards. In the JoinSet supervisor code, introduce a small in-memory set keyed by `JobId` that records jobs which have been prepared and spawned but have not yet been reaped. Filter `launch_supervised_jobs()` against that set before preparing more queued jobs, and remove the entry when `handle_supervised_join_result()` reaps the task. This is not the primary serialization mechanism; the database claim is. The point is that a future code path that accidentally re-exposes a job as queued or assigned inside the same process should still be contained locally.

Fourth, harden execution mismatch handling so it fails fast instead of warning forever. In `run_with_heartbeats()`, treat `heartbeat_job_execution(...)` returning `Conflict("job_not_active")` as a terminal mismatch. Re-read the current row, then return a dedicated runtime error that tells the supervisor “this task lost ownership of the job row.” Do not continue the heartbeat loop after that point. Mirror the same principle in completion: if a task reaches report completion and the row is already terminal, surface one structured error rather than cascading warnings. The goal is to make future lifecycle regressions obvious and finite in logs.

The store layer needs one corresponding refactor. In `crates/ingot-store-sqlite/src/store/job.rs`, replace the current split between “generic `update_job()` writes the assigned row” and “`start_job_execution()` later claims running” with a store API that is explicit about launch claim semantics. That API should require `job_id`, `item_id`, `expected_item_revision_id`, `workspace_id`, `agent_id`, `prompt_snapshot`, `phase_template_digest`, `lease_owner_id`, and `lease_expires_at`. It should update only rows that are still `Queued`, not `Assigned`, because the runtime should no longer depend on a persisted assigned intermediate state for agent-backed launch. If the compare-and-swap fails, the caller must release or abandon any newly created workspace state before returning `NotPrepared` or a conflict error.

The domain layer may remain mostly stable for the first patch. `JobState::Assigned` can stay in `crates/ingot-domain/src/job.rs` for compatibility with existing tests and startup cleanup, but add comments clarifying that steady-state runtime launch no longer uses it. If the implementation later proves simpler by removing `Assigned` entirely, do that as a follow-up once the launch path and startup recovery tests are green.

## Concrete Steps

Work from `/Users/aa/Documents/ingot`.

1. Start with the store API and runtime handoff refactor.

    Read:

        crates/ingot-agent-runtime/src/lib.rs
        crates/ingot-store-sqlite/src/store/job.rs
        crates/ingot-domain/src/job.rs

    Implement:

        - a single atomic launch-claim store method for agent-backed jobs
        - runtime changes so `prepare_run()` no longer persists `Assigned`
        - spawn only after the launch claim succeeds

2. Tighten steady-state reconciliation.

    Read:

        crates/ingot-agent-runtime/src/lib.rs
        crates/ingot-agent-runtime/tests/reconciliation.rs

    Implement:

        - no `Assigned` repair inside ordinary `reconcile_active_jobs()`
        - keep `reconcile_assigned_job()` for startup and explicit cleanup paths

3. Add in-process duplicate-launch guards and finite mismatch handling.

    Read:

        crates/ingot-agent-runtime/src/lib.rs
        crates/ingot-agent-runtime/tests/dispatch.rs

    Implement:

        - an in-flight `JobId` set alongside `running_meta`
        - fast exit on `job_not_active` heartbeat conflicts

4. Add and update tests.

    The new or updated tests should include:

        - a `dispatch.rs` regression proving one queued job produces one runner launch under `run_forever()`
        - a `dispatch.rs` regression proving a successfully completed job does not continue producing heartbeat warnings or late `job_not_active` completion conflicts
        - a `reconciliation.rs` regression preserving startup cleanup of legacy assigned rows
        - updates to existing JoinSet and heartbeat tests so they still cover the current supervisor semantics after the refactor

5. Validate with focused commands first, then broader ones.

    Run:

        cd /Users/aa/Documents/ingot
        cargo test -p ingot-agent-runtime --test dispatch
        cargo test -p ingot-agent-runtime --test reconciliation
        cargo test -p ingot-agent-runtime --test auto_dispatch
        cargo check -p ingot-daemon
        make test

    If unrelated pre-existing lint failures remain, record them explicitly in this plan and in the final handoff rather than broadening scope silently.

## Validation and Acceptance

Acceptance is reached when all of the following are true:

1. A queued job launched under `run_forever()` causes exactly one runner launch for that `job_id`. The regression should fail before the patch by observing two launches or two distinct workspaces, and pass after the patch by observing only one.
2. The database claim for starting agent-backed execution is atomic from the perspective of other runtime iterations: if two code paths attempt to start the same queued row, only one wins and only that winner may spawn a child process.
3. `reconcile_active_jobs()` no longer requeues freshly claimed live work during ordinary runtime maintenance, but `reconcile_startup()` still repairs legacy or abandoned assigned rows as covered by `crates/ingot-agent-runtime/tests/reconciliation.rs`.
4. After a successful job completion, there is no continued stream of `job heartbeat update failed error=Conflict("job_not_active")` for that same `job_id`, and no losing duplicate task reports a late repository conflict for the same row.
5. Existing JoinSet, cancellation, and heartbeat regressions in `crates/ingot-agent-runtime/tests/dispatch.rs` and `crates/ingot-agent-runtime/tests/auto_dispatch.rs` still pass, proving the launch hardening did not break capacity release or lease refresh.
6. `cargo test -p ingot-agent-runtime --test dispatch`, `cargo test -p ingot-agent-runtime --test reconciliation`, `cargo test -p ingot-agent-runtime --test auto_dispatch`, `cargo check -p ingot-daemon`, and `make test` pass, except for any unrelated pre-existing failures documented in `Progress` and `Outcomes & Retrospective`.

The before/after proof matters. At least the duplicate-launch regression should fail on the pre-fix code and pass after the implementation. Otherwise the patch risks only moving the warnings around instead of fixing the ownership bug.

## Idempotence and Recovery

This change is internal to the runtime and SQLite store. It does not require a user-facing schema migration if the implementation reuses existing columns, but it does change launch semantics, so keep the rollout safe and incremental.

If you stop midway through the store refactor, the likely failure mode is that jobs remain `Queued` because the new claim API is not wired through the runtime yet, or that prepared workspaces are left attached after a failed claim. Prefer small commits and rerun the focused runtime tests after each stage. Do not use destructive git resets in a dirty worktree.

If the first implementation keeps `Assigned` in the type model but stops using it for live launch, make sure startup reconciliation tests still pass before attempting a second patch that removes the enum variant entirely. The safest recovery path is “preserve compatibility first, delete later.”

If a claim conflict happens after workspace preparation, release or mark the prepared workspace consistently before returning control to the supervisor. Do not leave a brand-new persistent workspace attached to a job that never actually started.

## Artifacts and Notes

When implementation lands, record the most useful evidence here. At minimum include:

    cargo test -p ingot-agent-runtime --test dispatch <new duplicate-launch test> -- --exact
    test <new duplicate-launch test> ... ok

    cargo test -p ingot-agent-runtime --test reconciliation reconcile_startup_handles_mixed_inflight_states_conservatively -- --exact
    test reconcile_startup_handles_mixed_inflight_states_conservatively ... ok

    cargo check -p ingot-daemon
    Finished `dev` profile ... target(s) in ...

Also record a short daemon-log excerpt from a manual or test run showing a single `job entered running state` for one `job_id` and no trailing `job_not_active` heartbeat spam after completion.

## Interfaces and Dependencies

At the end of this work, the runtime and store interfaces should make launch ownership explicit.

In `crates/ingot-store-sqlite/src/store/job.rs`, define or adapt one store entry point for the live launch claim, for example:

    pub async fn claim_and_start_job_execution(
        &self,
        params: StartJobExecutionParams,
    ) -> Result<(), RepositoryError>

with `StartJobExecutionParams` extended so it carries:

    - `job_id`
    - `item_id`
    - `expected_item_revision_id`
    - `workspace_id`
    - `agent_id`
    - `prompt_snapshot`
    - `phase_template_digest`
    - `lease_owner_id`
    - `process_pid`
    - `lease_expires_at`

The SQL update behind that API must claim only rows still in `status='queued'` for the steady-state runtime path. The method should persist both the assignment metadata and the initial running lease in one compare-and-swap update.

In `crates/ingot-agent-runtime/src/lib.rs`, `prepare_run()` should return owned `PreparedRun` data without persisting `Assigned`. `run_with_heartbeats()` should either call the new claim method itself before spawn or receive a prepared structure that has already been claimed, but there must be no second code path that can independently requeue or relaunch the same job between claim and spawn.

The supervisor should maintain an additional map or set keyed by `ingot_domain::ids::JobId` alongside `running_meta`. `TaskId` remains useful for JoinSet bookkeeping, but it is not sufficient to prevent same-job duplicate launch.

No new external dependency should be needed. The existing Tokio and SQLx primitives are enough. Keep the public surface changes narrow and localized to the runtime/store boundary.

Revision note: created on 2026-03-19 after investigating repeated `job_not_active` heartbeat warnings for `job_019d07cc767b74b1aa2353b33a6e490e`. The investigation showed that the true bug is duplicate launch during the `Assigned -> Running` handoff, so this plan targets atomic launch claiming, narrower recovery semantics, and explicit duplicate-launch regressions rather than log suppression.
