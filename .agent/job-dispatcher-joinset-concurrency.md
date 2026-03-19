# Introduce JoinSet-backed concurrent job supervision

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows `.agent/PLANS.md` and must be maintained in accordance with that file.

## Purpose / Big Picture

After this change, the daemon will be able to keep more than one job running at a time instead of blocking the entire dispatcher on whichever job it started first. The core mechanism is a Tokio `JoinSet`, which is Tokio’s built-in way to supervise multiple spawned async tasks as one collection, plus a Tokio `Semaphore`, which is a permit counter used to cap how many of those tasks may run concurrently. The user-visible result is simple: when several jobs are queued, up to the configured limit move to `running` together, and when any one job completes, the daemon can start the next queued job immediately without waiting for the fallback poll interval.

This is an internal runtime refactor, but it has observable behavior. A focused runtime test with three queued authoring jobs on three distinct item revisions should show two jobs entering `running` before either one finishes when concurrency is set to `2`. A second test should show that once one running job is released, the next queued job starts promptly even if `poll_interval` is set to a large value, proving that completion wakeups come from the `JoinSet` completion path rather than from polling.

## Progress

- [x] (2026-03-19 13:15Z) Re-read `.agent/PLANS.md`, inspected the daemon entry point, and mapped the current dispatcher loop in `crates/ingot-agent-runtime/src/lib.rs`.
- [x] (2026-03-19 13:15Z) Confirmed that `run_forever()` is single-job and that job execution is awaited inline through `tick()`, with no shared set of running tasks.
- [x] (2026-03-19 13:15Z) Identified an adjacent launch bottleneck: `next_runnable_job()` selects only the first supported queued job, so a stale or temporarily unlaunchable head job can prevent later queued jobs from starting.
- [x] (2026-03-19 13:15Z) Authored this ExecPlan in `.agent/job-dispatcher-joinset-concurrency.md`.
- [x] (2026-03-19 13:21Z) Deep-read the referenced runtime, store, notify, router, and test files; corrected the plan to reuse the existing queue/store APIs, account for current test harness limitations, and include the doc comments and HTTP test call sites that this refactor will affect.
- [x] (2026-03-19 13:24Z) Re-audited the referenced files for implementation-detail drift and corrected the plan again: only `crates/ingot-usecases/src/notify.rs` is definitely stale today, while `crates/ingot-http-api/src/router/mod.rs` is only an adjacent re-check, and `TestHarness::with_config()` needs explicit `state_root` alignment when custom configs are used.
- [x] (2026-03-19 13:25Z) Re-read startup bootstrap behavior and tightened the test plan so daemon-style `run_forever()` tests register their fake agents before calling `reconcile_startup()`, avoiding accidental default-agent bootstrap.
- [x] (2026-03-19 13:26Z) Re-checked dispatcher wakeup paths and test helpers; tightened the plan so background `run_forever()` tests explicitly call `DispatchNotify::notify()` after direct DB inserts and reuse the existing `TestHarness` agent-registration helpers.
- [x] (2026-03-19 13:37Z) Re-audited the runtime helpers, workspace preparation code, and bundled Tokio version; added the missing `Plan of Work` section, corrected the daemon-only validation side-effect assumptions, and made the supervisor design concrete about `join_next_with_id()`, empty-`JoinSet` waiting, panic cleanup metadata, and the need for distinct item revisions in concurrency tests.
- [x] (2026-03-19 13:49Z) Re-audited Tokio spawn/semaphore APIs and the existing daemon-only validation tests; tightened the plan so task metadata is keyed from `AbortHandle::id()`, the launch loop uses `try_acquire_owned()`, and the extraction milestone explicitly re-runs the current harness-validation tests that already guard clean, findings, and invalid-profile behavior.
- [x] (2026-03-19 14:02Z) Re-audited queued-job creation, workspace-busy behavior, and daemon-only lease ownership; tightened the plan so `run_forever()` treats supervisor-local `WorkspaceError::Busy` as “not launchable yet” instead of aborting the scan, added the existing daemon-validation workspace-resync tests to the extraction milestone, and corrected the recovery notes to distinguish agent-backed leases from daemon-only `"daemon"` leases.
- [x] (2026-03-19 14:09Z) Re-checked the exact `WorkspaceError::Busy` call sites and narrowed the plan again: that downgrade applies only to the authoring workspace path, including daemon-only validation jobs that use authoring workspaces, not to integration workspace preparation.
- [x] (2026-03-19 14:01Z) Deep-read the remaining referenced and adjacent files, including `bootstrap.rs`, `crates/ingot-domain/src/ports.rs`, `crates/ingot-agent-runtime/tests/escalation.rs`, and `crates/ingot-agent-runtime/tests/convergence.rs`; tightened the plan so `run_forever()` preserves the maintenance/system-action/projected-review work it currently inherits from `tick()`, and broadened regression coverage to the existing `dispatch`, `escalation`, and `convergence` test binaries.
- [ ] Add bounded-concurrency configuration to `DispatcherConfig` and document the chosen default.
- [ ] Refactor `run_forever()` into a supervisor loop that owns a `JoinSet`, a sidecar task-metadata map, and semaphore permits.
- [ ] Extract shared execution helpers so agent-backed jobs and daemon-only validation jobs can be launched as supervised tasks without changing their current durable side effects.
- [ ] Replace the single-candidate launcher in `run_forever()` with an ordered scan over the existing `Database::list_queued_jobs(32)` result so available permits are filled even when the oldest queued job is not launchable.
- [ ] Update the implementation-detail comment in `crates/ingot-usecases/src/notify.rs` so it no longer claims the dispatcher drains work specifically by looping on `tick()`, and re-read the adjacent `crates/ingot-http-api/src/router/mod.rs` middleware comment to confirm it still matches the final implementation.
- [ ] Extend the runtime test harness in `crates/ingot-agent-runtime/tests/common/mod.rs` so it keeps the actual `DispatchNotify`, stores the dispatcher’s real `state_root`, and provides bounded waiting support for deterministic `run_forever()` tests.
- [ ] Add focused runtime tests for concurrent launch, completion-driven wakeups, and concurrency bounds.
- [ ] Run the runtime-focused and repository-wide validation commands listed below.

## Surprises & Discoveries

- Observation: the current dispatcher is not merely missing a `JoinSet`; it also awaits job completion inline, so the background loop cannot observe any second queued job until the first job fully returns.
  Evidence: `crates/ingot-agent-runtime/src/lib.rs` calls `self.run_with_heartbeats(&prepared, request).await` inside `tick()`, and `apps/ingot-daemon/src/main.rs` runs exactly one `dispatcher.run_forever()` task.

- Observation: the current queue launcher can starve runnable work behind one stale or temporarily unlaunchable queued job.
  Evidence: `next_runnable_job()` reads up to 32 queued rows and returns only the first supported candidate, and `tick()` stops after `PrepareRunOutcome::NotPrepared` instead of scanning the rest of that queued window.

- Observation: many existing runtime tests call `dispatcher.tick()` directly and expect one deterministic unit of work.
  Evidence: `crates/ingot-agent-runtime/tests/dispatch.rs`, `crates/ingot-agent-runtime/tests/auto_dispatch.rs`, `crates/ingot-agent-runtime/tests/convergence.rs`, and `crates/ingot-agent-runtime/tests/reconciliation.rs` all exercise `tick()` as a synchronous helper.

- Observation: the existing runtime test harness does not expose the `DispatchNotify` instance used to build the dispatcher, and it has no helper for waiting until background state changes are visible in SQLite.
  Evidence: `crates/ingot-agent-runtime/tests/common/mod.rs` stores only `db`, `dispatcher`, `project`, `state_root`, and `repo_path`; the `DispatchNotify::default()` passed to `JobDispatcher::with_runner` is not retained anywhere.

- Observation: `TestHarness::with_config()` can report the wrong `state_root` when the caller passes a custom `DispatcherConfig`.
  Evidence: `crates/ingot-agent-runtime/tests/common/mod.rs` creates a local `state_root = unique_temp_path("ingot-runtime-state")`, then replaces the dispatcher config with the caller-provided value, but still stores the local `state_root` field in the returned harness.

- Observation: only the `DispatchNotify` documentation is definitely tied to the old `tick()` loop. The adjacent HTTP router comment is implementation-agnostic today.
  Evidence: `crates/ingot-usecases/src/notify.rs` says the dispatcher drains work by “looping while `tick()` returns progress,” while `crates/ingot-http-api/src/router/mod.rs` only says spurious wakeups are harmless because the dispatcher “drains until idle.”

- Observation: the runtime has no persisted or in-memory “busy” agent state, so concurrent launch will continue to select the same `Available` agent for multiple jobs unless this change explicitly broadens scope.
  Evidence: `crates/ingot-domain/src/agent.rs` defines only `Available`, `Unavailable`, and `Probing`, and `select_agent()` in `crates/ingot-agent-runtime/src/lib.rs` sorts available agents by slug and returns the first matching entry without reserving it.

- Observation: daemon-style tests that call `reconcile_startup()` on an empty agent registry will bootstrap a default agent before any queued jobs run.
  Evidence: `crates/ingot-agent-runtime/src/bootstrap.rs` creates a default Codex agent when `db.list_agents()` is empty, and `JobDispatcher::reconcile_startup()` in `crates/ingot-agent-runtime/src/lib.rs` always calls `bootstrap::ensure_default_agent(&self.db).await?` first.

- Observation: direct database inserts do not wake the background dispatcher in tests; the only automatic notifier in tree is the HTTP write middleware.
  Evidence: `crates/ingot-http-api/src/router/mod.rs` calls `state.dispatch_notify.notify()` after successful write requests, and no runtime or HTTP tests currently call `dispatch_notify.notify()` directly after creating jobs in SQLite.

- Observation: daemon-only validation jobs do not currently share the agent-backed artifact, heartbeat, or timeout path.
  Evidence: `execute_harness_validation()` in `crates/ingot-agent-runtime/src/lib.rs` calls `start_job_execution(...)`, runs harness commands, and completes through `complete_job_service()`, but it never calls `write_prompt_artifact()`, `write_response_artifacts()`, or `heartbeat_job_execution()`.

- Observation: authoring workspace reuse already rejects a second concurrent attachment to the same workspace, so concurrency tests must not accidentally queue multiple authoring jobs against one revision.
  Evidence: `ensure_authoring_workspace_state()` in `crates/ingot-workspace/src/lib.rs` returns `WorkspaceError::Busy` when an existing authoring workspace is already `Busy`, and `prepare_run()` in `crates/ingot-agent-runtime/src/lib.rs` propagates that error rather than converting it to `PrepareRunOutcome::NotPrepared`.

- Observation: the bundled Tokio version already exposes the exact APIs needed to avoid both empty-`JoinSet` busy loops and panic-metadata loss.
  Evidence: `Cargo.lock` pins `tokio` 1.48.0, and `/Users/aa/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.48.0/src/task/join_set.rs` provides `try_join_next_with_id()` and `join_next_with_id()`, while documenting that `join_next()` returns `None` when the set is empty.

- Observation: `JoinSet::spawn()` does not hide the task ID problem; the returned `AbortHandle` exposes `id()`, which is the concrete way to key supervisor metadata at launch time.
  Evidence: `/Users/aa/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.48.0/src/task/join_set.rs` shows `JoinSet::spawn()` returning `AbortHandle`, and `/Users/aa/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.48.0/src/runtime/task/abort.rs` defines `AbortHandle::id() -> tokio::task::Id`.

- Observation: the runtime runner knows about both Codex and ClaudeCode adapters, but runtime job selection still hard-filters to Codex agents today.
  Evidence: `CliAgentRunner` in `crates/ingot-agent-runtime/src/lib.rs` dispatches on `AdapterKind::{Codex, ClaudeCode}`, while `select_agent()` in the same file filters `agent.adapter_kind == AdapterKind::Codex` before `supports_job(...)`.

- Observation: the store allows multiple queued job rows for the same revision, and a conflicting queued job that goes through the authoring workspace path can currently surface as `WorkspaceError::Busy` during workspace preparation.
  Evidence: `Database::create_job()` in `crates/ingot-store-sqlite/src/store/job.rs` inserts rows without checking for duplicate queued work, while `ensure_authoring_workspace_state()` in `crates/ingot-workspace/src/lib.rs` returns `WorkspaceError::Busy`; both `prepare_run()` and `execute_harness_validation()` reach that helper when `job.workspace_kind == WorkspaceKind::Authoring`.

- Observation: daemon-only running jobs use a different lease owner string than agent-backed running jobs.
  Evidence: `run_with_heartbeats()` in `crates/ingot-agent-runtime/src/lib.rs` writes `lease_owner_id: self.lease_owner_id.clone()`, while `execute_harness_validation()` writes `lease_owner_id: "daemon".into()`, and `reconcile_running_job()` treats any lease owner different from `self.lease_owner_id` as `foreign_owner`.

- Observation: the daemon currently gets all of its non-job background work only by repeatedly calling `tick()`, so a `run_forever()` refactor that only supervises job launches would silently stop maintenance, convergence system actions, and projected-review recovery.
  Evidence: `JobDispatcher::run_forever()` in `crates/ingot-agent-runtime/src/lib.rs` does nothing except call `self.tick().await` in a loop, while `tick()` itself calls `ReconciliationService::tick_maintenance()`, `ConvergenceService::tick_system_actions()`, and `recover_projected_review_jobs()`.

## Decision Log

- Decision: keep `tick()` as a deterministic, synchronous helper for focused tests, and concentrate concurrent supervision inside `run_forever()`.
  Rationale: `tick()` is already heavily used in tests and in a few HTTP/runtime call sites. Preserving its “one unit of progress” shape keeps the test surface stable while still fixing the real daemon behavior in `run_forever()`.
  Date/Author: 2026-03-19 / Codex

- Decision: create the `JoinSet` and semaphore as supervisor-local state inside `run_forever()` instead of storing them on `JobDispatcher`.
  Rationale: `JobDispatcher` is `Clone` and is passed widely by value. Local supervisor state avoids mutex-heavy shared mutable state and matches the fact that the daemon starts exactly one long-lived dispatcher loop in `apps/ingot-daemon/src/main.rs`.
  Date/Author: 2026-03-19 / Codex

- Decision: extract the non-job work inside `tick()` into a shared helper that both `tick()` and the new `run_forever()` supervisor call before trying to launch queued jobs.
  Rationale: today `run_forever()` has no other path to maintenance, convergence system actions, or projected-review recovery. Sharing that control-plane helper preserves current daemon behavior while letting `run_forever()` replace only the inline single-job execution portion of `tick()`.
  Date/Author: 2026-03-19 / Codex

- Decision: add `max_concurrent_jobs` to `DispatcherConfig` and give it a conservative default of `2`.
  Rationale: the change should produce concurrent behavior in the default daemon path, but the repo has no per-agent busy tracking yet. A small default limit captures the benefit without opening the door to unbounded local CLI fan-out.
  Date/Author: 2026-03-19 / Codex

- Decision: fix queue-head starvation as part of this change instead of preserving the current “first queued row only” launcher.
  Rationale: a `JoinSet` only helps if the dispatcher can actually fill newly freed permits. Keeping the single-candidate launcher would leave permits idle whenever the oldest queued job is stale, lacks a compatible agent, or is otherwise not launchable.
  Date/Author: 2026-03-19 / Codex

- Decision: reuse `Database::list_queued_jobs(32)` in `crates/ingot-store-sqlite/src/store/job.rs` and scan that ordered window inside the runtime rather than inventing a new queue-reader abstraction.
  Rationale: the store already exposes the exact ordered queued-job window the runtime needs. Reusing it keeps the refactor localized to the runtime and avoids an unnecessary database API change.
  Date/Author: 2026-03-19 / Codex

- Decision: include a small test-harness update in `crates/ingot-agent-runtime/tests/common/mod.rs` so `run_forever()` tests can wake the dispatcher and wait for observed state transitions without long sleeps.
  Rationale: the current harness hides the `DispatchNotify` handle and offers no polling helper, which would make concurrent-supervisor tests brittle and timing-dependent.
  Date/Author: 2026-03-19 / Codex

- Decision: fix the existing `TestHarness::with_config()` `state_root` mismatch as part of the same helper update.
  Rationale: concurrency tests are the first likely consumers of custom dispatcher configs plus artifact-path assertions, and the current helper would point those assertions at the wrong directory.
  Date/Author: 2026-03-19 / Codex

- Decision: keep agent selection semantics unchanged for this plan and do not add a “busy agent” reservation model.
  Rationale: the current codebase has no busy-agent status or lease table, and introducing one would broaden the change well beyond `JoinSet`-backed supervision. This plan therefore improves bounded concurrency at the dispatcher level only.
  Date/Author: 2026-03-19 / Codex

- Decision: require daemon-style `run_forever()` tests to register their fake agents before calling `reconcile_startup()`.
  Rationale: `reconcile_startup()` bootstraps a default agent when the registry is empty. Registering the intended test agent first keeps the startup path realistic without letting bootstrap inject an extra agent that could change selection behavior.
  Date/Author: 2026-03-19 / Codex

- Decision: require background `run_forever()` tests to call the retained `DispatchNotify::notify()` after inserting queued work directly into SQLite.
  Rationale: unlike HTTP write routes, direct test inserts do not trigger any middleware notification. Requiring an explicit notify keeps the tests event-driven and avoids turning them into poll-interval races.
  Date/Author: 2026-03-19 / Codex

- Decision: use `JoinSet::try_join_next_with_id()` for the non-blocking reap phase and `JoinSet::join_next_with_id()` for the blocking wait path, guarded by `!running.is_empty()`.
  Rationale: Tokio 1.48.0 already exposes task IDs for completed and failed `JoinSet` entries. Using the `*_with_id` APIs lets the supervisor correlate panics with sidecar metadata, and guarding the blocking wait avoids the empty-set case where `join_next*()` returns `None` immediately and would otherwise starve the sleep branch.
  Date/Author: 2026-03-19 / Codex

- Decision: key the supervisor’s sidecar metadata from `AbortHandle::id()` immediately after each `JoinSet::spawn(...)`.
  Rationale: `JoinSet::spawn(...)` returns `AbortHandle`, and `AbortHandle::id()` is available in the bundled Tokio 1.48.0 API. Capturing that ID at spawn time is the concrete way to correlate later `join_next_with_id()` panics with the prepared job metadata needed for cleanup.
  Date/Author: 2026-03-19 / Codex

- Decision: use `Arc<Semaphore>::try_acquire_owned()` in the launch scan instead of manually checking `available_permits()`.
  Rationale: the Tokio semaphore already exposes a non-blocking owned-permit API that cleanly answers “is capacity available right now?” and produces the exact `OwnedSemaphorePermit` that must be moved into the spawned task. This avoids writing a permit-count probe that then has to be reconciled with the actual permit acquisition.
  Date/Author: 2026-03-19 / Codex

- Decision: keep daemon-only validation behavior distinct from agent-backed runs even after extraction.
  Rationale: the current daemon-only path does not emit prompt/response artifacts or periodic job heartbeats. The goal of this refactor is concurrent supervision, not a side-effect rewrite, so the extracted helper should preserve the current harness-validation semantics while only changing how it is scheduled.
  Date/Author: 2026-03-19 / Codex

- Decision: make the new concurrency tests use separate item revisions for each authoring job.
  Rationale: reusing one revision would trip the existing authoring-workspace `Busy` guard in `ensure_authoring_workspace_state()`, which is unrelated to the `JoinSet` refactor and would make the tests assert the wrong behavior.
  Date/Author: 2026-03-19 / Codex

- Decision: keep the runtime’s current Codex-only agent selection semantics unchanged in this plan.
  Rationale: although `CliAgentRunner` knows how to launch both Codex and ClaudeCode adapters, `select_agent()` currently hard-filters to Codex agents. Broadening adapter selection would be a separate behavior change unrelated to `JoinSet`-backed supervision.
  Date/Author: 2026-03-19 / Codex

- Decision: in the concurrent `run_forever()` launch path, treat `RuntimeError::Workspace(WorkspaceError::Busy)` as “not launchable yet” and continue scanning later queued jobs, while leaving `tick()` behavior unchanged.
  Rationale: `Database::create_job()` does not prevent multiple queued rows from targeting the same revision, and the current workspace helpers can reject that with `WorkspaceError::Busy`. If the supervisor lets that error abort the scan, one head-of-queue conflict still blocks unrelated work and defeats the point of bounded concurrency.
  Date/Author: 2026-03-19 / Codex

## Outcomes & Retrospective

At the time this document was created, no implementation had been started. The useful outcome so far is design clarity: the daemon needs a supervisor-owned `JoinSet` for completion wakeups, semaphore-held permits for bounded concurrency, and a queue scan that can skip blocked candidates while preserving queue order among launchable jobs. This revision also makes explicit that the refactor must preserve the maintenance, convergence, and projected-review work that the daemon currently performs only because `run_forever()` delegates to `tick()`. Success for the implementation phase will mean the daemon overlaps job execution without weakening the existing workspace, lease, recovery, or control-plane invariants.

## Context and Orientation

The relevant daemon wiring lives in `apps/ingot-daemon/src/main.rs`. That file constructs `JobDispatcher`, calls `reconcile_startup()`, and then spawns one long-lived async task that runs `dispatcher.run_forever().await`. There is no other dispatcher process in this repository, so the concurrency supervisor can remain local to that single background loop.

The main runtime implementation lives in `crates/ingot-agent-runtime/src/lib.rs`. `DispatcherConfig` currently contains `state_root`, `poll_interval`, `heartbeat_interval`, and `job_timeout`. `JobDispatcher::run_forever()` currently calls `tick()` in a loop and sleeps on either `DispatchNotify::notified()` or `poll_interval` when no progress was made. `tick()` performs maintenance and system actions, selects at most one queued job with `next_runnable_job()`, and then either runs a daemon-only validation inline with `execute_harness_validation()` or prepares an agent-backed run with `prepare_run()` and awaits `run_with_heartbeats()` inline.

Because `run_forever()` currently does nothing except call `tick()`, every maintenance effect the daemon has today comes through `tick()` as well. That includes `ReconciliationService::tick_maintenance()`, `ConvergenceService::tick_system_actions()`, and the trailing `recover_projected_review_jobs()` call. Any supervisor refactor must keep those three behaviors in the background loop even when queued-job launching stops going through `tick()`.

An “agent-backed job” in this repository means a job that launches a Codex CLI process through the `AgentRunner` trait. A “daemon-only validation job” means a validation step with `execution_permission == daemon_only`; those jobs are executed entirely inside the daemon by `execute_harness_validation()` and do not launch an external agent process. Both categories must participate in the same bounded-concurrency policy because both consume dispatcher attention and workspace resources, but they do not currently have identical side effects. The agent-backed path writes prompt and response artifacts and sends periodic heartbeats through `run_with_heartbeats()`. The daemon-only validation path starts the job, runs harness commands, and completes through `CompleteJobService`, but it does not currently write prompt or response artifacts and it does not issue runtime heartbeats.

The existing queue and execution store surfaces already cover most of this refactor. `crates/ingot-store-sqlite/src/store/job.rs` already provides `Database::list_queued_jobs(limit)`, `Database::start_job_execution(...)`, `Database::heartbeat_job_execution(...)`, and `Database::finish_job_non_success(...)`. `StartJobExecutionParams` and `FinishJobNonSuccessParams` are defined in `crates/ingot-domain/src/ports.rs`, and the `JobRepository` trait in that same file already exposes the matching `list_queued`, `start_execution`, and `heartbeat_execution` shape. The concurrency refactor should reuse those methods and types rather than introducing new store APIs or new domain-port requirements.

The current preparation and completion flow is already split across helpers. `prepare_run()` acquires the per-project mutation lock, validates that the job is still `queued` on the item’s current revision, loads the harness profile, chooses an available Codex-capable agent with `select_agent()`, provisions the workspace, writes the prompt snapshot into the job row, and returns `PrepareRunOutcome`. `run_with_heartbeats()` calls `start_job_execution(...)`, spawns the adapter future, updates heartbeats until completion or timeout, and returns `Result<AgentResponse, AgentError>`. Downstream completion logic is already handled by `finish_run()`, `fail_run()`, `fail_job_preparation()`, and the workspace finalization helpers. The concurrent supervisor should reuse that exact sequence instead of inventing a second completion pipeline.

The focused runtime tests live in `crates/ingot-agent-runtime/tests/dispatch.rs`, with reusable harness code in `crates/ingot-agent-runtime/tests/common/mod.rs`. `crates/ingot-http-api/tests/job_routes.rs` also matters because `complete_route_recovers_projected_review_after_warning_only_dispatch_failure_on_system_action_tick()` instantiates a dispatcher and calls `tick()` directly. That existing test is a concrete reason to keep `tick()` public and behaviorally stable while moving concurrency into `run_forever()`.

The notifier wiring lives in `crates/ingot-usecases/src/notify.rs` and `crates/ingot-http-api/src/router/mod.rs`. `DispatchNotify` is a level-triggered hint backed by Tokio `Notify`: the HTTP middleware wakes the daemon after successful write requests, while direct database inserts in runtime tests do nothing unless the test calls `notify()` explicitly. The middleware comment in the router is still accurate today because it only says the dispatcher “drains until idle.” The `DispatchNotify` comment is the one that is stale because it names the old “loop while `tick()` returns progress” implementation detail.

Four more current constraints shape the implementation. First, the current agent model in `crates/ingot-domain/src/agent.rs` only distinguishes `Available`, `Unavailable`, and `Probing`, so there is no in-memory or persisted busy-agent reservation to prevent the same agent from being selected for multiple concurrent jobs. Second, although `CliAgentRunner` can launch both Codex and ClaudeCode adapters, `select_agent()` currently hard-filters to `AdapterKind::Codex`, so this plan keeps that selection behavior unchanged. Third, `Database::create_job()` does not deduplicate queued work, so two queued rows can target the same revision at the same time. Fourth, authoring workspace reuse already rejects concurrent attachment to the same workspace: `ensure_authoring_workspace_state()` in `crates/ingot-workspace/src/lib.rs` returns `WorkspaceError::Busy` when an existing authoring workspace is already `Busy`, and both `prepare_run()` and the daemon-only validation path hit that helper only when they are using `WorkspaceKind::Authoring`. This plan therefore keeps agent selection unchanged, makes its main concurrency tests use separate item revisions so they exercise dispatcher concurrency rather than workspace exclusion, and requires the supervisor launch scan to treat authoring-workspace conflicts as “not launchable right now” instead of fatal errors.

## Plan of Work

Begin by extracting two shared helpers inside `crates/ingot-agent-runtime/src/lib.rs`. The first should own the non-job work that `tick()` performs today: maintenance, convergence system actions, and projected-review recovery. `tick()` should keep using that helper before it considers a queued job, and the new `run_forever()` supervisor should call the same helper on every wake before it tries to fill permits. The second should own “run one already-prepared job to completion” for agent-backed jobs: write the prompt artifact, build the `AgentRequest`, call `run_with_heartbeats()`, write response artifacts on success, and then route into `finish_run()` or `fail_run()` using the same error mapping that `tick()` uses now. Do not change `prepare_run()` itself in this milestone; it already performs the lock-scoped setup that makes the queued job safe to launch. In the same file, keep the daemon-only validation body distinct, but extract it enough that `run_forever()` can decide whether a queued validation job is launchable before it spends a supervisor slot copying the current `execute_harness_validation()` logic into multiple places. Because this extraction touches the harness path, the agent-backed success and failure paths, and the control-plane work that `run_forever()` currently gets only through `tick()`, rerun the existing exact tests in `crates/ingot-agent-runtime/tests/auto_dispatch.rs`, `crates/ingot-agent-runtime/tests/escalation.rs`, and `crates/ingot-agent-runtime/tests/convergence.rs` that already cover those behaviors before moving on.

Next, extend `DispatcherConfig` in `crates/ingot-agent-runtime/src/lib.rs` with `max_concurrent_jobs: usize`, set `DispatcherConfig::new(...)` to default it to `2`, and refactor `run_forever()` into a true supervisor. The supervisor should own three local pieces of state: `Arc<Semaphore>` for the permit cap, `JoinSet<RunningJobResult>` for spawned tasks, and a sidecar `HashMap<tokio::task::Id, RunningJobMeta>` for cleanup metadata. Use `try_join_next_with_id()` to reap everything that has already finished before launching more work, then call the shared non-job helper before you decide whether the loop is idle or whether there is more queued work to launch. When the loop becomes idle, only await `join_next_with_id()` inside `tokio::select!` when `!running.is_empty()`; otherwise, wait only on `DispatchNotify::notified()` and `sleep(self.config.poll_interval)`. This avoids the empty-`JoinSet` case where `join_next*()` resolves immediately and would defeat the sleep-based fallback. It also keeps the control-plane helper running on join completions, notifications, and poll wakeups instead of only when a queued job happens to launch. When spawning each supervised task, keep the returned `AbortHandle`, call `handle.id()`, and insert the metadata under that task ID immediately so later `JoinError::id()` lookups have something concrete to find.

Then replace the single-candidate launcher only in `run_forever()`. Keep `tick()` calling `next_runnable_job()` so the existing tests and HTTP call sites stay deterministic. Inside `run_forever()`, reuse `Database::list_queued_jobs(32)` and scan that ordered window from oldest to newest while permits remain. Acquire launch capacity with `semaphore.clone().try_acquire_owned()`: if it returns `TryAcquireError::NoPermits`, stop scanning; if it returns a permit, either move that permit into a spawned task or drop it immediately when preparation decides the job is not launchable after all. For each queued row, first filter unsupported jobs exactly the way `next_runnable_job()` does today. For daemon-only validation jobs, launch the extracted validation helper in a supervised task, but if the setup path hits `RuntimeError::Workspace(WorkspaceError::Busy)` while preparing an authoring workspace, drop the permit and continue scanning later jobs instead of aborting the whole supervisor loop. For agent-backed jobs, call `prepare_run(job).await` and handle the outcomes carefully: `Ok(NotPrepared)` means drop the just-acquired permit, skip this candidate, and continue scanning; `Ok(FailedBeforeLaunch)` means drop the permit, record progress, and continue scanning; `Ok(Prepared(prepared))` means spawn the shared execution helper, move the `OwnedSemaphorePermit` into the task so the slot is released only when the task exits, and record cleanup metadata keyed by the spawned task’s `AbortHandle::id()`; `Err(RuntimeError::Workspace(WorkspaceError::Busy))` means drop the permit and treat this candidate as “not launchable yet” in the concurrent supervisor only when that job is also on the authoring path. Preserve queue order among launchable jobs by scanning the rows in the order `list_queued_jobs(32)` already returns. Do not add a new store API and do not reorder jobs by agent, project, or step.

After that, make completion handling concrete. `RunningJobResult` should report the spawned task’s logical outcome, but join failures need extra state because a panic prevents the task from returning its output. Use `join_next_with_id()` so the supervisor always knows which task finished. On a normal completion, remove the sidecar metadata entry and log any returned `RuntimeError`. On a `JoinError`, look up the stored metadata by `JoinError::id()`, then inspect the current job row in SQLite before mutating anything. If the job is still `assigned` or `running`, fail it immediately using the existing runtime cleanup path or the equivalent direct database updates so it does not remain stranded behind a live lease. If the job is already terminal, do nothing. This makes panic recovery immediate; relying on `reconcile_active_jobs()` alone would not be sufficient because running jobs owned by the current dispatcher keep a fresh lease for up to thirty minutes.

Finally, update the runtime test support and add the new `run_forever()` tests. In `crates/ingot-agent-runtime/tests/common/mod.rs`, store the actual `DispatchNotify` used to build `JobDispatcher`, and when `with_config()` receives a custom config, keep `state_root` aligned to `config.state_root.clone()` rather than the throwaway local temp path. Add one bounded async helper that repeatedly reads from SQLite under `tokio::time::timeout(...)`, because that is the pattern already used in `crates/ingot-agent-runtime/tests/convergence.rs`. In `crates/ingot-agent-runtime/tests/dispatch.rs`, add a blocking fake runner that waits on `Notify` or `Barrier` so tests can hold jobs open. Register the test agent before calling `reconcile_startup()`, start `run_forever()` in a background task, enqueue work through `db.create_job(...)`, call the retained `dispatch_notify.notify()`, and wait until the database reflects the expected statuses. Each test must abort the background `run_forever()` task before exit so later tests do not inherit stray dispatcher activity.

## Concrete Steps

Work from `/Users/aa/Documents/ingot`.

Implement the refactor in this order so each milestone is independently provable.

First, extract the shared execution helper and keep `tick()` green:

    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test dispatch tick_executes_a_queued_authoring_job_and_creates_a_commit -- --exact
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test dispatch tick_executes_a_review_job_and_persists_structured_report -- --exact
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test dispatch tick_times_out_long_running_job_and_marks_it_failed -- --exact
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test auto_dispatch daemon_only_validation_job_executes_on_tick -- --exact
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test auto_dispatch harness_validation_with_commands_produces_findings_on_failure -- --exact
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test auto_dispatch daemon_only_validation_fails_on_invalid_harness_profile -- --exact
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test auto_dispatch daemon_validation_resyncs_authoring_workspace_before_running_harness -- --exact
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test auto_dispatch daemon_validation_resyncs_integration_workspace_before_running_harness -- --exact
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test escalation runtime_terminal_failure_escalates_closure_relevant_item -- --exact
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test escalation successful_authoring_retry_clears_escalation_and_reopens_review_dispatch -- --exact
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test convergence tick_reports_no_progress_when_auto_finalize_is_blocked -- --exact
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test convergence tick_auto_finalizes_not_required_prepared_convergence_even_when_commit_exists_only_in_mirror -- --exact

Expected evidence is twelve `... ok` lines for those existing focused tests. If any of them fail, the extraction changed behavior instead of just making it reusable.

Second, add `max_concurrent_jobs`, the supervisor state, and the harness updates, then add the new focused `run_forever()` tests:

    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test dispatch run_forever_launches_up_to_max_concurrent_jobs -- --exact
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test dispatch run_forever_starts_next_job_on_joinset_completion -- --exact
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test dispatch run_forever_skips_unlaunchable_head_job_when_filling_capacity -- --exact
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test dispatch run_forever_skips_workspace_busy_head_job_when_filling_capacity -- --exact

Expected evidence is:

    test run_forever_launches_up_to_max_concurrent_jobs ... ok
    test run_forever_starts_next_job_on_joinset_completion ... ok
    test run_forever_skips_unlaunchable_head_job_when_filling_capacity ... ok
    test run_forever_skips_workspace_busy_head_job_when_filling_capacity ... ok

Third, run the adjacent regression tests that depend on `tick()` semantics and projected-review recovery:

    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test dispatch
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test escalation
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test convergence
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test auto_dispatch
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test reconciliation
    cd /Users/aa/Documents/ingot && cargo test -p ingot-http-api --test job_routes complete_route_recovers_projected_review_after_warning_only_dispatch_failure_on_system_action_tick -- --exact

Expected evidence is that the full `dispatch`, `escalation`, `convergence`, `auto_dispatch`, and `reconciliation` binaries finish successfully and the HTTP test prints one `... ok` line for the named route test.

Finally, run the repository gate commands that this repo uses for backend and UI CI:

    cd /Users/aa/Documents/ingot && make test
    cd /Users/aa/Documents/ingot && make lint
    cd /Users/aa/Documents/ingot && make ci

If `make ci` fails only because of unrelated pre-existing UI issues, record that exact failure in `Surprises & Discoveries` and in the final implementation note instead of silently skipping it.

## Validation and Acceptance

Acceptance is reached when the daemon demonstrates all of the following behaviors.

First, with `max_concurrent_jobs = 2`, three queued authoring jobs on three distinct item revisions and a blocking fake runner, exactly two jobs enter `running` before either one completes. The third job must remain `queued` until one running job releases its permit.

Second, when one running job finishes and another queued job is waiting, `run_forever()` starts the next job within a timeout that is materially shorter than `poll_interval`. For example, if `poll_interval` is set to several seconds, the test should observe the next job reach `running` within a few hundred milliseconds after the first job is released. That is the proof that completion wakeups come from the `JoinSet` completion path rather than from fallback polling.

Third, the concurrency cap is strict. At no point should more than `max_concurrent_jobs` jobs be in `running` for the single dispatcher instance, as observed through SQLite reads during the test.

Fourth, agent-backed jobs still produce the same durable side effects they do today: prompt and response artifacts under `state_root/logs/<job-id>/`, periodic heartbeat updates while they are running, timeout handling through `AgentError::Timeout`, workspace cleanup, result persistence, and projected-review recovery. Daemon-only validation jobs must also keep their current behavior: they should still transition through `start_job_execution(...)`, produce a `validation_report:v1` payload through `CompleteJobService`, and auto-dispatch follow-up review or validation work exactly as they do today, but they must not gain new prompt/response artifact or heartbeat requirements in this refactor.

The existing exact tests `daemon_only_validation_job_executes_on_tick`, `harness_validation_with_commands_produces_findings_on_failure`, `daemon_only_validation_fails_on_invalid_harness_profile`, `daemon_validation_resyncs_authoring_workspace_before_running_harness`, and `daemon_validation_resyncs_integration_workspace_before_running_harness` should still pass after the helper extraction, proving that the daemon-only path still handles the clean, findings, invalid-profile, and workspace-resync branches it already supports today.

Fifth, an unlaunchable queued head job does not consume all progress. When capacity is available and a later queued job is launchable, that later job starts without manual intervention. The focused test should make the first queued job stale by changing the item’s current revision after the job row is created, because `prepare_run()` already turns that condition into `PrepareRunOutcome::NotPrepared`.

Sixth, an authoring-workspace-conflicting queued head job also does not consume all progress. When the oldest queued job wants an authoring workspace that is already `Busy`, `run_forever()` should treat that candidate as “not launchable yet” and continue scanning later queued jobs that do not share that workspace. The new focused test should construct that conflict explicitly, because `Database::create_job()` does not prevent it.

Seventh, the refactor does not strand the daemon’s non-job work behind the new supervisor. The code should show that both `tick()` and `run_forever()` invoke the same helper that wraps maintenance, convergence system actions, and projected-review recovery, and the full `dispatch`, `escalation`, `convergence`, `auto_dispatch`, `reconciliation`, and adjacent HTTP recovery tests should continue to pass.

Eighth, the updated comment in `crates/ingot-usecases/src/notify.rs` describes the implementation truthfully after the refactor, and `crates/ingot-http-api/src/router/mod.rs` has been re-checked and only changed if needed.

## Idempotence and Recovery

This refactor should be safe to apply incrementally because it does not require a schema migration or a public API change. If implementation breaks the new supervisor loop mid-flight, the existing startup reconciliation path in `reconcile_startup()` remains the recovery backstop for jobs that are left `assigned`, for convergences that are left active, and for stale workspaces. Keep the fallback `poll_interval` path in `run_forever()` even after adding `JoinSet` wakeups; that poll remains the safety net for any work that does not arrive through `DispatchNotify` and is not represented by a running supervised task.

During implementation, retry focused test commands freely because the runtime tests already create temporary SQLite databases and temporary Git repositories. When a `run_forever()` test starts a background dispatcher task, always abort that task before the test returns and then await the `JoinHandle` long enough to observe the cancellation, so later tests do not inherit stray runtime activity. When a test overrides `DispatcherConfig`, use the dispatcher’s actual `state_root` for artifact assertions; after the helper fix, that should once again be `h.state_root`.

Tokio 1.48.0’s `JoinSet` aborts tracked tasks when the set is dropped. That is useful for isolated tests, but it is not the production cleanup path to rely on. In `run_forever()` tests that intentionally block jobs open, prefer releasing those blockers and waiting for terminal job states before aborting the outer dispatcher loop. If a test does abort the outer loop while supervised jobs are still running, treat the resulting task cancellation as test teardown on a throwaway database, not as proof that the supervisor’s normal cleanup path is correct.

For panic recovery inside `run_forever()`, never assume `reconcile_active_jobs()` will clean up promptly enough to be the primary mechanism. Agent-backed supervised tasks write `lease_owner_id = self.lease_owner_id`, so a fresh lease on a panicked agent-backed job will cause reconciliation to leave it alone. Daemon-only validation tasks currently write `lease_owner_id = "daemon"`, so reconciliation would treat them as `foreign_owner`, but the supervisor should still clean them up directly for consistency and speed. In both cases, the supervisor must use its sidecar task metadata and the current job state to fail or release stranded work immediately on join error.

## Artifacts and Notes

Useful evidence to preserve while implementing includes a short debug log excerpt showing two jobs entering `running` before the blocking runner is released, and a focused test transcript showing that the completion-driven wakeup test passes even with a deliberately large `poll_interval`.

One concise transcript worth preserving after the test phase is:

    test run_forever_launches_up_to_max_concurrent_jobs ... ok
    test run_forever_starts_next_job_on_joinset_completion ... ok
    test run_forever_skips_unlaunchable_head_job_when_filling_capacity ... ok

If the implementation chooses a specific error code for “supervised task panicked or was aborted unexpectedly,” record that exact code here after implementation, because the current codebase does not already define one for outer supervisor-task failure.

## Interfaces and Dependencies

This change should stay inside `crates/ingot-agent-runtime/src/lib.rs`, `crates/ingot-store-sqlite/src/store/job.rs` only as a read-only reference for existing queue APIs, `crates/ingot-usecases/src/notify.rs`, `crates/ingot-http-api/src/router/mod.rs` only if its adjacent comment needs follow-up, `apps/ingot-daemon/src/main.rs` only if wiring needs adjustment, and the runtime test files including `crates/ingot-agent-runtime/tests/common/mod.rs` and `crates/ingot-agent-runtime/tests/dispatch.rs`. No SQLite migration, domain-entity change, or HTTP route change is required.

The runtime should gain one internal helper for the non-job work that `run_forever()` currently receives only by calling `tick()`. The exact name is flexible, but the shape should be equivalent to:

    async fn drive_non_job_work(&self) -> Result<bool, RuntimeError>;

That helper must wrap `ReconciliationService::tick_maintenance()`, `ConvergenceService::tick_system_actions()`, and `recover_projected_review_jobs()`. `tick()` should call it before attempting `next_runnable_job()`, and `run_forever()` should call it after reaping finished tasks and before sleeping or filling permits.

At the end of the implementation, `DispatcherConfig` in `crates/ingot-agent-runtime/src/lib.rs` should contain:

    pub struct DispatcherConfig {
        pub state_root: PathBuf,
        pub poll_interval: Duration,
        pub heartbeat_interval: Duration,
        pub job_timeout: Duration,
        pub max_concurrent_jobs: usize,
    }

`DispatcherConfig::new(...)` must populate that new field so existing call sites in `apps/ingot-daemon/src/main.rs`, `crates/ingot-agent-runtime/tests/common/mod.rs`, `crates/ingot-http-api/tests/job_routes.rs`, and the rest of the runtime tests continue to compile without hand-editing every constructor.

The runtime should also have an internal launched-task result type. Keep it small and focused on the completed job, for example:

    struct RunningJobResult {
        job_id: ingot_domain::ids::JobId,
        result: Result<(), RuntimeError>,
    }

Because `JoinSet` panics do not return `RunningJobResult`, `run_forever()` should also own a sidecar metadata map keyed by Tokio task ID. The exact struct name is flexible, but the stored data must be enough to clean up a stranded `assigned` or `running` job immediately if `join_next_with_id()` returns `Err(JoinError)`. For agent-backed tasks, the most direct value is a clone of `PreparedRun`; for daemon-only validation tasks, store the IDs and workspace information already extracted in `execute_harness_validation()`.

At spawn time, populate that map from the `AbortHandle` returned by `JoinSet::spawn(...)`, for example:

    let handle = running.spawn(task);
    let task_id = handle.id();
    running_meta.insert(task_id, meta);

`run_forever()` should own supervisor-local state with this shape:

    let semaphore = Arc::new(tokio::sync::Semaphore::new(self.config.max_concurrent_jobs));
    let mut running = tokio::task::JoinSet::<RunningJobResult>::new();
    let mut running_meta = std::collections::HashMap::<tokio::task::Id, RunningJobMeta>::new();

Use `tokio::sync::OwnedSemaphorePermit` so a concurrency slot is released exactly when a spawned job task exits. Keep `tick()` public and returning `Result<bool, RuntimeError>`. The shared execution helpers should preserve the existing behavior of `prepare_run()`, `run_with_heartbeats()`, `finish_run()`, `fail_run()`, `fail_job_preparation()`, `execute_harness_validation()`, and the workspace finalization methods. Reuse `Database::list_queued_jobs(32)` for the launch scan, and do not add agent-reservation state in this patch because the current agent model only distinguishes `Available`, `Unavailable`, and `Probing`.

Inside the concurrent launch scan specifically, match `RuntimeError::Workspace(WorkspaceError::Busy)` from the authoring workspace path in agent-backed preparation and in the extracted daemon-validation setup path, downgrade it to “not launchable yet,” and continue scanning later queued jobs. Do not broaden that downgrade to unrelated workspace errors such as ref mismatches or head mismatches, and do not imply that integration workspace preparation has the same `Busy` guard; those cases should still surface as real runtime failures.

Revision note: created on 2026-03-19 to address the dispatcher’s lack of `JoinSet`-based concurrent job supervision and to capture the related queue-head starvation fix that is necessary for bounded concurrency to be effective.

Revision note: revised on 2026-03-19 after deep-reading the referenced and adjacent code. This pass removed an implied new queue-reader abstraction in favor of the existing `Database::list_queued_jobs(32)` API, added the currently affected doc-comment files and HTTP `tick()` test call site, called out the missing `DispatchNotify`/wait support in the runtime test harness, and made the validation commands and scope constraints match the code that exists today.

Revision note: revised again on 2026-03-19 after re-reading the referenced files for drift. This pass corrected the plan’s own overreach by limiting the required documentation update to `crates/ingot-usecases/src/notify.rs`, added the concrete `TestHarness::with_config()` `state_root` mismatch that will matter for custom-config concurrency tests, and anchored the new async-test guidance to the existing `tokio::time::timeout(...)` pattern already used in `crates/ingot-agent-runtime/tests/convergence.rs`.

Revision note: revised once more on 2026-03-19 after re-reading startup bootstrap behavior. This pass added the requirement that daemon-style `run_forever()` tests register their intended fake agents before calling `reconcile_startup()`, because startup bootstraps a default agent whenever the registry is empty.

Revision note: revised again on 2026-03-19 after re-checking dispatcher wakeup paths and test helpers. This pass made explicit that background `run_forever()` tests must call the retained `DispatchNotify::notify()` after direct SQLite inserts, because only the HTTP write middleware notifies automatically, and it pointed implementers at the existing `TestHarness` agent-registration helpers instead of implying custom agent setup by default.

Revision note: re-audited on 2026-03-19 at 13:27Z after another deep read of the referenced files and adjacent test helpers. This pass did not uncover additional substantive code-grounded plan changes beyond the existing notes about startup bootstrap, explicit dispatch notifications, and the `TestHarness::with_config()` helper gap, so the body was left intentionally unchanged.

Revision note: re-audited again on 2026-03-19 at 13:28Z after re-reading queue selection, `PrepareRunOutcome`, `select_agent()`, and the existing test-support builders. This pass did not find additional substantive plan changes beyond the already documented requirements to reuse the existing queue API, register Codex-capable test agents through the existing harness helpers, and notify the dispatcher explicitly in background tests after direct SQLite inserts.

Revision note: revised on 2026-03-19 after a deeper code audit of `run_forever()`, `execute_harness_validation()`, `ensure_authoring_workspace_state()`, the `job_routes` `tick()` call site, the Makefile targets, and the bundled Tokio 1.48.0 source. This pass added the missing `Plan of Work` section required by `.agent/PLANS.md`, corrected the inaccurate claim that daemon-only validation jobs currently share agent-backed artifacts and heartbeats, made the supervisor design concrete about `try_join_next_with_id()` and `join_next_with_id()`, added the sidecar metadata requirement needed for immediate panic cleanup, specified distinct item revisions for concurrency tests to avoid unrelated workspace-busy failures, and replaced vague validation steps with the exact commands and test names that exist in this repository.

Revision note: revised again on 2026-03-19 after re-auditing Tokio spawn/semaphore details, the existing daemon-only validation tests, and the current adapter-selection path. This pass corrected the remaining ambiguity around spawn-time task metadata by anchoring it to `AbortHandle::id()`, tightened the launch loop to use `Semaphore::try_acquire_owned()` instead of a vague permit-availability check, added the current exact daemon-only validation tests to the extraction milestone, and documented that the runtime still intentionally filters runnable agents to `AdapterKind::Codex` even though `CliAgentRunner` knows about both Codex and ClaudeCode.

Revision note: revised again on 2026-03-19 after re-auditing queued-job insertion, workspace-busy behavior, daemon-only lease ownership, and the existing daemon-validation resync tests. This pass added the concrete requirement to treat `RuntimeError::Workspace(WorkspaceError::Busy)` as “not launchable yet” inside the concurrent supervisor so one conflicting head job does not still block later work, added the current authoring/integration daemon-validation resync tests to the extraction milestone, introduced a focused `run_forever_skips_workspace_busy_head_job_when_filling_capacity` test, and corrected the recovery notes to distinguish agent-backed current-daemon leases from daemon-only `"daemon"` leases.

Revision note: revised again on 2026-03-19 after re-checking the exact `WorkspaceError::Busy` call sites in `prepare_workspace()` and `execute_harness_validation()`. This pass narrowed the workspace-busy guidance from “authoring or integration” to the actual authoring path only, because the integration workspace branch does not use `ensure_authoring_workspace_state()` and therefore does not surface the same `Busy` error.

Revision note: revised again on 2026-03-19 after deep-reading the remaining referenced files and adjacent tick callers. This pass added the missing requirement that `run_forever()` preserve the maintenance, convergence-system-action, and projected-review-recovery work it currently inherits from `tick()`, widened the concrete regression commands to the existing `dispatch`, `escalation`, and `convergence` test binaries, documented that `crates/ingot-domain/src/ports.rs` and existing `DispatcherConfig::new(...)` call sites do not require broader interface churn, and added the `JoinSet` drop-aborts-tasks safety note for background supervisor tests.
