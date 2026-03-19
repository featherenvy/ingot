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
- [x] (2026-03-19 14:01Z) Deep-read the remaining referenced and adjacent files, including `crates/ingot-agent-runtime/src/bootstrap.rs`, `crates/ingot-domain/src/ports.rs`, `crates/ingot-agent-runtime/tests/escalation.rs`, and `crates/ingot-agent-runtime/tests/convergence.rs`; tightened the plan so `run_forever()` preserves the maintenance/system-action/projected-review work it currently inherits from `tick()`, and broadened regression coverage to the existing `dispatch`, `escalation`, and `convergence` test binaries.
- [x] (2026-03-19 14:35Z) Re-audited `tick()` control flow, `reconcile_startup()` draining, and the existing `dispatch`/`auto_dispatch` exact tests; tightened the plan so the shared non-job helper preserves `tick()`’s current early return when system actions make progress, keeps `tick_system_action()` available for startup draining, and adds the already-existing timeout/recovery regression tests that directly exercise the helpers this refactor will touch.
- [x] (2026-03-19 14:23Z) Re-audited the queued-job window and daemon-only validation lock boundary; tightened the plan so the starvation fix is scoped explicitly to the existing `Database::list_queued_jobs(32)` window, the validation extraction preserves today’s “lock for setup, then drop before running commands” behavior, and the startup exact test is called out alongside the helper-extraction regressions.
- [x] (2026-03-19 14:31Z) Re-audited the daemon-only validation preflight branches and the job-completion usecase boundary; tightened the plan so validation preparation reports `not prepared` versus `failed before launch` distinctly, matching the existing `PrepareRunOutcome` pattern instead of collapsing both states into one `Ok(())`.
- [x] (2026-03-19 14:39Z) Re-audited `reconcile_running_job()` and the runtime cleanup fallback; tightened the plan so panic cleanup for daemon-only tasks is explicit about avoiding the misleading `heartbeat_expired` fallback that current foreign-owner reconciliation would otherwise apply.
- [x] (2026-03-19 15:06Z) Re-audited the current tree again before rewriting this plan in place; corrected the stale Tokio version references to the actual `Cargo.lock` pin, aligned the new reusable test-support work with the existing fake-runner pattern in `crates/ingot-agent-runtime/tests/common/mod.rs`, and added the missing exact `auto_dispatch` regressions that already guard `prepare_run()` harness loading and repo-local skill resolution.
- [x] (2026-03-19 15:16Z) Re-audited the current `run_forever()` error boundary and the existing runtime-test coverage; tightened the plan so the supervisor keeps today’s “log and continue” liveness contract, and added a concrete heartbeat-refresh test because the current tree checks timeout and expiry behavior but does not directly prove heartbeat advancement on a still-running supervised job.
- [x] (2026-03-19 15:18Z) Re-audited the agent-backed runtime path in `run_with_heartbeats()` plus the artifact-writing helpers; tightened the plan so it now calls out the nested Tokio task inside agent execution, the exact response artifact filenames (`stdout.log`, `stderr.log`, `result.json`), and the fact that supervisor teardown is not proof of agent-process cleanup.
- [x] (2026-03-19 15:27Z) Re-audited the existing runtime artifact assertions and found that only `prompt.txt` is currently checked in-tree; tightened the plan so the existing authoring success test extends to verify `stdout.log`, `stderr.log`, and `result.json` too, using the current `FakeRunner` path instead of inventing a new artifact test harness.
- [x] (2026-03-19 14:50Z) Re-audited workspace-finalization helpers, `reconcile_assigned_job()`, and `reconcile_running_job()`; tightened the plan so join-error cleanup now states how active job rows and their workspaces must be repaired, and made the completion-wakeup test explicitly prove JoinSet-driven wakeups by forbidding a second manual notify after releasing the blocker.
- [x] (2026-03-19 15:01Z) Re-audited the pinned Tokio `JoinSet::spawn(...)` signature and the current runtime helper shapes; tightened the plan so spawned helpers now explicitly take owned inputs plus a cloned `JobDispatcher`, and normal task-returned `RuntimeError`s now require the same active-row cleanup check as join failures instead of being treated as log-only noise.
- [x] (2026-03-19 15:07Z) Re-audited launch-time ownership against the current `PreparedRun` definition; tightened the plan so sidecar metadata storage now explicitly reuses `PreparedRun`’s existing `Clone` support and requires the new daemon-validation prepared struct to be cloneable or converted into `RunningJobMeta` at spawn time.
- [x] (2026-03-19 15:45Z) Re-audited the queue-order and authoring-workspace helpers; tightened the plan so queue-head tests set explicit `JobBuilder::created_at(...)` values instead of relying on the shared default timestamp, and so the workspace-busy regression uses a DB-only Busy authoring workspace row because `ensure_authoring_workspace_state()` returns `WorkspaceError::Busy` before any Git provisioning.
- [x] (2026-03-19 16:02Z) Re-audited the item/revision store APIs and builder shapes; tightened the plan so the stale-head regression now uses the real `db.create_revision(...)` plus `db.update_item(...)` recipe to move `item.current_revision_id`, instead of hand-waving over how revision drift is created in tests.
- [x] (2026-03-19 15:41Z) Re-audited the supervisor-only branches against adjacent validation and reconciliation tests; tightened the plan so the new background-loop coverage now includes the daemon-only validation path in `crates/ingot-agent-runtime/tests/auto_dispatch.rs`, reuses that file’s existing harness/workspace helpers instead of duplicating them in `crates/ingot-agent-runtime/tests/dispatch.rs`, and calls out the exact reconciliation tests that define today’s assigned/running job cleanup semantics.
- [x] (2026-03-19 15:47Z) Re-audited remaining helper-level tests and daemon-only runtime side effects; tightened the plan so the first milestone re-runs the existing `drain_until_idle_*` unit tests in `crates/ingot-agent-runtime/src/lib.rs`, clarified the Unix-only exact-test output expectation, and made the new background-loop daemon-only validation regression assert that it still does not write agent-style log artifacts.
- [x] (2026-03-19 15:49Z) Re-audited startup setup across supervisor-adjacent tests; tightened the plan so agent-backed `run_forever()` tests still register their mutating agent before `reconcile_startup()`, while the new daemon-only validation background-loop test may rely on the same empty-registry startup bootstrap pattern already exercised in `crates/ingot-agent-runtime/tests/reconciliation.rs` because that path never calls `select_agent()`.
- [x] (2026-03-19 16:18Z) Re-audited the current runtime and test helpers while improving this plan in place; tightened the extraction milestone so it keeps `execute_harness_validation()` as the synchronous wrapper `tick()` already calls, grounded supervisor cleanup against the exact `reconcile_assigned_job()` and `reconcile_running_job()` behaviors in `crates/ingot-agent-runtime/src/lib.rs`, added reuse of the existing `test_authoring_job(...)` plus `register_mutating_agent()` helpers for the new dispatch tests, and explicitly flagged that non-`Busy` preparation failures remain on the daemon’s existing log-and-continue path instead of becoming new scan-and-skip cases in this refactor.
- [x] (2026-03-19 16:48Z) Re-audited the test-support builders, runtime test binaries, and store helpers again before this rewrite; tightened the plan so the new concurrent tests reuse one explicit `BlockingRunner` in `crates/ingot-agent-runtime/tests/common/mod.rs`, standardized the new timestamped dispatch tests on a `parse_timestamp` re-export from that same shared module, and added the missing read-only references to `crates/ingot-workspace/src/lib.rs` plus the domain test-support builder files whose exact methods (`created_at`, `revision_no`, `current_job_id`) the plan already relies on.
- [x] (2026-03-19 17:09Z) Added `max_concurrent_jobs` to `DispatcherConfig` with default `2`, refactored `run_forever()` into a `JoinSet` + `Semaphore` supervisor with task metadata cleanup, extracted shared agent/daemon execution helpers, updated `DispatchNotify` docs, and extended the runtime harness with retained `DispatchNotify`, bounded DB waiters, `parse_timestamp`, and `BlockingRunner`.
- [x] (2026-03-19 17:18Z) Added the new background-loop regressions in `crates/ingot-agent-runtime/tests/dispatch.rs` and `crates/ingot-agent-runtime/tests/auto_dispatch.rs`, extended the existing authoring success assertion to cover `stdout.log`, `stderr.log`, and `result.json`, and stabilized the queue-skip tests so they stay green in the full `dispatch` binary.
- [x] (2026-03-19 18:05Z) Ran the focused runtime and HTTP regressions, then the full runtime binaries plus `make test`; all of those passed after one daemon-validation follow-up fix that made supervised validation jobs use the dispatcher lease owner instead of the old foreign-owner `"daemon"` lease.
- [x] (2026-03-19 18:05Z) Ran `make lint` and `make ci`; both still fail on pre-existing Clippy diagnostics in `crates/ingot-usecases/src/convergence.rs`, so repository-wide lint is blocked outside this refactor and requires follow-up tracking.

## Surprises & Discoveries

- Observation: the current dispatcher is not merely missing a `JoinSet`; it also awaits job completion inline, so the background loop cannot observe any second queued job until the first job fully returns.
  Evidence: `crates/ingot-agent-runtime/src/lib.rs` calls `self.run_with_heartbeats(&prepared, request).await` inside `tick()`, and `apps/ingot-daemon/src/main.rs` runs exactly one `dispatcher.run_forever()` task.

- Observation: the daemon’s top-level loop is deliberately failure-tolerant today; `run_forever()` logs a `tick()` error and stays alive instead of returning.
  Evidence: `JobDispatcher::run_forever()` in `crates/ingot-agent-runtime/src/lib.rs` matches on `self.tick().await`, logs `authoring job dispatcher tick failed`, and then continues into the notify-or-sleep branch with `made_progress = false`.

- Observation: the current queue launcher can starve runnable work behind one stale or temporarily unlaunchable queued job.
  Evidence: `next_runnable_job()` reads up to 32 queued rows and returns only the first supported candidate, and `tick()` stops after `PrepareRunOutcome::NotPrepared` instead of scanning the rest of that queued window.

- Observation: many existing runtime tests call `dispatcher.tick()` directly and expect one deterministic unit of work.
  Evidence: `crates/ingot-agent-runtime/tests/dispatch.rs`, `crates/ingot-agent-runtime/tests/auto_dispatch.rs`, `crates/ingot-agent-runtime/tests/convergence.rs`, and `crates/ingot-agent-runtime/tests/reconciliation.rs` all exercise `tick()` as a synchronous helper.

- Observation: the existing runtime test harness does not expose the `DispatchNotify` instance used to build the dispatcher, and it has no helper for waiting until background state changes are visible in SQLite.
  Evidence: `crates/ingot-agent-runtime/tests/common/mod.rs` stores only `db`, `dispatcher`, `project`, `state_root`, and `repo_path`; the `DispatchNotify::default()` passed to `JobDispatcher::with_runner` is not retained anywhere.

- Observation: `TestHarness::with_config()` can report the wrong `state_root` when the caller passes a custom `DispatcherConfig`.
  Evidence: `crates/ingot-agent-runtime/tests/common/mod.rs` creates a local `state_root = unique_temp_path("ingot-runtime-state")`, then replaces the dispatcher config with the caller-provided value, but still stores the local `state_root` field in the returned harness.

- Observation: the live-heartbeat regression must compare against a post-start baseline, not just against `Some(...)`, because entering `running` already writes one heartbeat timestamp before the periodic ticker fires.
  Evidence: `Database::start_job_execution(...)` in `crates/ingot-store-sqlite/src/store/job.rs` sets `heartbeat_at = Utc::now()` in the same SQL update that transitions the row to `status = 'running'`, and `run_with_heartbeats()` calls that method before its `tokio::time::interval(...)` loop begins.

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
  Evidence: `Cargo.lock` pins `tokio` 1.50.0, which exposes `try_join_next_with_id()`, `join_next_with_id()`, and `AbortHandle::id()` on the `JoinSet` path this supervisor design needs.

- Observation: `JoinSet::spawn()` does not hide the task ID problem; the returned `AbortHandle` exposes `id()`, which is the concrete way to key supervisor metadata at launch time.
  Evidence: the same Tokio 1.50.0 API surface used by this repository returns an `AbortHandle` from `JoinSet::spawn()`, and that handle exposes `id() -> tokio::task::Id`.

- Observation: the runtime runner knows about both Codex and ClaudeCode adapters, but runtime job selection still hard-filters to Codex agents today.
  Evidence: `CliAgentRunner` in `crates/ingot-agent-runtime/src/lib.rs` dispatches on `AdapterKind::{Codex, ClaudeCode}`, while `select_agent()` in the same file filters `agent.adapter_kind == AdapterKind::Codex` before `supports_job(...)`.

- Observation: the store allows multiple queued job rows for the same revision, and a conflicting queued job that goes through the authoring workspace path can currently surface as `WorkspaceError::Busy` during workspace preparation.
  Evidence: `Database::create_job()` in `crates/ingot-store-sqlite/src/store/job.rs` inserts rows without checking for duplicate queued work, while `ensure_authoring_workspace_state()` in `crates/ingot-workspace/src/lib.rs` returns `WorkspaceError::Busy`; both `prepare_run()` and `execute_harness_validation()` reach that helper when `job.workspace_kind == WorkspaceKind::Authoring`.

- Observation: daemon-only running jobs use a different lease owner string than agent-backed running jobs.
  Evidence: `run_with_heartbeats()` in `crates/ingot-agent-runtime/src/lib.rs` writes `lease_owner_id: self.lease_owner_id.clone()`, while `execute_harness_validation()` writes `lease_owner_id: "daemon".into()`, and `reconcile_running_job()` treats any lease owner different from `self.lease_owner_id` as `foreign_owner`.

- Observation: the daemon currently gets all of its non-job background work only by repeatedly calling `tick()`, so a `run_forever()` refactor that only supervises job launches would silently stop maintenance, convergence system actions, and projected-review recovery.
  Evidence: `JobDispatcher::run_forever()` in `crates/ingot-agent-runtime/src/lib.rs` does nothing except call `self.tick().await` in a loop, while `tick()` itself calls `ReconciliationService::tick_maintenance()`, `ConvergenceService::tick_system_actions()`, and `recover_projected_review_jobs()`.

- Observation: daemon-only validation still has one synchronous entrypoint today, and `tick()` depends on that wrapper shape.
  Evidence: `tick()` calls `self.execute_harness_validation(job).await?` in `crates/ingot-agent-runtime/src/lib.rs`, and there are no other direct callers of `execute_harness_validation()` in the tree.

- Observation: `tick()` does not treat all non-job work as one undifferentiated boolean today; it returns immediately after a successful `ConvergenceService::tick_system_actions()` pass, and `reconcile_startup()` separately drains only system actions through `tick_system_action()`.
  Evidence: `crates/ingot-agent-runtime/src/lib.rs` returns `Ok(true)` immediately after `tick_system_actions()` plus one `recover_projected_review_jobs()` call, and `reconcile_startup()` still uses `drain_until_idle(|| self.tick_system_action())`.

- Observation: the repository already has exact tests for the helper paths this refactor will touch beyond the ones currently listed in the plan, including harness timeout cleanup, idle projected-review recovery during `tick()`, and maintenance tolerance of a broken sibling project.
  Evidence: `crates/ingot-agent-runtime/tests/dispatch.rs` includes `tick_runs_healthy_queued_job_even_when_another_project_is_broken`, and `crates/ingot-agent-runtime/tests/auto_dispatch.rs` includes `harness_validation_timeout_kills_background_processes` plus `tick_recovers_idle_review_work_even_when_processing_other_queued_jobs`.

- Observation: the queue-starvation fix can only operate inside the existing 32-row queued-job window unless the store API changes.
  Evidence: `Database::list_queued_jobs(limit)` in `crates/ingot-store-sqlite/src/store/job.rs` runs one `SELECT ... WHERE status = 'queued' ORDER BY created_at ASC LIMIT ?`, and both `next_runnable_job()` and this plan intentionally reuse `list_queued_jobs(32)` instead of adding pagination or a new repository method.

- Observation: new queue-order tests cannot rely on default builder timestamps to make one job the “head” of the queued window.
  Evidence: `JobBuilder::new(...)` in `crates/ingot-domain/src/test_support/job.rs` sets `created_at` to the fixed `default_timestamp()` (`2026-03-12T00:00:00Z`), while `Database::list_queued_jobs(limit)` orders only by `created_at ASC`, so separately inserted test jobs tie unless the test overrides `created_at`.

- Observation: the queue-order examples need a real `DateTime<Utc>` source, not raw string literals, because the builders do not accept timestamp strings directly.
  Evidence: `JobBuilder::created_at(...)`, `WorkspaceBuilder::created_at(...)`, and `RevisionBuilder::created_at(...)` all take `chrono::DateTime<Utc>`, while `ingot_domain::test_support` re-exports `parse_timestamp(...)` through `ingot_test_support::fixtures`.

- Observation: any waiter built on `Database::list_jobs_by_project(...)` must not assume queue order from the returned vector.
  Evidence: `Database::list_jobs_by_project(...)` in `crates/ingot-store-sqlite/src/store/job.rs` orders rows by `created_at DESC`, which is the opposite direction from `list_queued_jobs(32)`, so the waiter is fine for counting/filtering running jobs but not for asserting “oldest first”.

- Observation: the stale-head regression needs a real persisted next revision plus an item update; there is no helper in `crates/ingot-agent-runtime/tests/common/mod.rs` that flips `current_revision_id` for you.
  Evidence: `prepare_run()` in `crates/ingot-agent-runtime/src/lib.rs` returns `PrepareRunOutcome::NotPrepared` when `item.current_revision_id != job.item_revision_id`, while the concrete store APIs that create that drift are `Database::create_revision(...)` in `crates/ingot-store-sqlite/src/store/revision.rs` and `Database::update_item(...)` in `crates/ingot-store-sqlite/src/store/item.rs`.

- Observation: daemon-only validation already avoids holding the per-project mutation lock while harness commands run.
  Evidence: `execute_harness_validation()` in `crates/ingot-agent-runtime/src/lib.rs` performs queue checks, workspace preparation, assignment, and `start_job_execution(...)` inside a scoped block guarded by `project_locks.acquire_project_mutation(...)`, then drops that guard before iterating over `harness.commands`.

- Observation: the workspace-busy launch-skip test does not need to provision a real authoring worktree just to hit the Busy branch.
  Evidence: `ensure_authoring_workspace_state()` in `crates/ingot-workspace/src/lib.rs` returns `WorkspaceError::Busy` immediately when the existing workspace row is already `WorkspaceStatus::Busy`, before it calls `provision_authoring_workspace(...)` or touches Git state on disk.

- Observation: daemon-only validation currently collapses “not launchable” and “failed before launch” into the same `Ok(())` return, so an extracted preflight helper needs a richer outcome than `Option`.
  Evidence: in `execute_harness_validation()` in `crates/ingot-agent-runtime/src/lib.rs`, both the `job.state.status() != JobStatus::Queued || !is_daemon_only_validation(&job)` / revision-mismatch branches and the invalid-harness-profile branch end by returning `Ok(())`, even though the invalid-profile branch first calls `fail_job_preparation(...)` and therefore should count as progress.

- Observation: if daemon-only running jobs fall through to normal reconciliation, they are marked `Expired` with `error_code = "heartbeat_expired"` immediately because their lease owner is always `"daemon"`, which this dispatcher treats as foreign.
  Evidence: `execute_harness_validation()` starts daemon-only jobs with `lease_owner_id: "daemon".into()`, and `reconcile_running_job()` in `crates/ingot-agent-runtime/src/lib.rs` expires any running job where `job.state.lease_owner_id() != Some(self.lease_owner_id.as_str())`, then writes `FinishJobNonSuccessParams { status: JobStatus::Expired, error_code: Some("heartbeat_expired".into()), ... }` even when the lease is still fresh.

- Observation: the helper-extraction milestone touches more existing harness-loading surface than the original regression list covered, including authoring prep failures before prompt artifacts are written and repo-local skill expansion inside `prepare_run()`.
  Evidence: `prepare_run()` calls `resolve_harness_prompt_context(...)`, and `crates/ingot-agent-runtime/tests/auto_dispatch.rs` already contains `queued_authoring_job_fails_on_invalid_harness_profile`, `authoring_prompt_includes_resolved_repo_local_skill_files`, `queued_authoring_job_fails_when_harness_skill_glob_escapes_repo`, and the Unix-only escaping-symlink test.

- Observation: shared fake runners already live in `crates/ingot-agent-runtime/tests/common/mod.rs`, not in each test binary.
  Evidence: `crates/ingot-agent-runtime/tests/common/mod.rs` defines `FakeRunner`, `StaticReviewRunner`, and `ScriptedLoopRunner`, and `crates/ingot-agent-runtime/tests/dispatch.rs` imports them with `use common::*;`.

- Observation: the runtime tests still do not have any reusable async coordination helper for “launch N jobs, then release one or all,” even though the planned `run_forever()` regressions need exactly that shape.
  Evidence: `crates/ingot-agent-runtime/tests/common/mod.rs` currently defines only `FakeRunner`, `StaticReviewRunner`, and `ScriptedLoopRunner`, and a repository-wide search under `crates/ingot-agent-runtime/tests/` finds no existing `Notify`, `Barrier`, `Semaphore`, `oneshot`, `watch`, or similar helper beyond one `tokio::time::timeout(...)` call in `crates/ingot-agent-runtime/tests/convergence.rs`.

- Observation: supervised daemon-only validation jobs still get expired immediately under the background loop if they keep the old `"daemon"` lease owner string.
  Evidence: the first `run_forever_executes_daemon_only_validation_job` implementation repeatedly timed out with the job row in `Expired`, because `tick_maintenance()` calls `reconcile_running_job()` on every loop and that method treats any running job whose `lease_owner_id()` differs from `self.lease_owner_id` as `foreign_owner`.

- Observation: `make lint` and therefore `make ci` are currently blocked by unrelated pre-existing Clippy diagnostics in `crates/ingot-usecases/src/convergence.rs`.
  Evidence: both commands fail on the same three warnings promoted to errors there: `large_enum_variant` for `ApprovalFinalizeReadiness`, `too_many_arguments` on `apply_successful_finalization`, and `collapsible_if` around the auto-finalize branch.

- Observation: the current daemon only skips a queued head job when preparation reports a modeled non-terminal outcome; most other preparation failures still surface as iteration errors.
  Evidence: `prepare_run()` returns `PrepareRunOutcome::{NotPrepared, FailedBeforeLaunch}` only for handled cases such as stale revisions, missing agents, and harness-profile failures, while `JobDispatcher::run_forever()` currently logs any other `tick()` error and loops instead of marking the head job failed or scanning past it.

- Observation: agent-backed execution already writes response artifacts with concrete filenames that the plan should name explicitly if it expects side effects to remain observable.
  Evidence: `write_prompt_artifact()` writes `prompt.txt`, and `write_response_artifacts()` writes `stdout.log`, `stderr.log`, and `result.json` under `state_root/logs/<job-id>/`.

- Observation: the agent-backed runtime path contains a nested Tokio task inside `run_with_heartbeats()`, so aborting the outer supervised task is not the same thing as proving the inner runner task was cleaned up.
  Evidence: `run_with_heartbeats()` calls `tokio::spawn(async move { runner.launch(...).await })` and only calls `handle.abort()` in the explicit timeout and operator-cancel branches before returning.

- Observation: current runtime tests assert only the prompt artifact, not the full agent-backed response artifact set.
  Evidence: `crates/ingot-agent-runtime/tests/dispatch.rs` checks `state_root/logs/<job-id>/prompt.txt` in `tick_executes_a_queued_authoring_job_and_creates_a_commit()`, and a repository-wide search of runtime and HTTP tests finds no assertions for `stdout.log`, `stderr.log`, or `result.json`.

- Observation: current runtime tests cover timeout and stale-lease expiry, but they do not directly prove that `heartbeat_at` advances while an agent-backed job is still running.
  Evidence: `crates/ingot-agent-runtime/tests/dispatch.rs` sets a short `heartbeat_interval` in `tick_times_out_long_running_job_and_marks_it_failed()` but only asserts the final `job_timeout` failure, while `crates/ingot-agent-runtime/tests/reconciliation.rs` checks expiry behavior after stale heartbeats rather than a live heartbeat refresh.

- Observation: the repository already has two different workspace-repair behaviors, and the supervisor’s join-error cleanup needs to pick the one that matches the kind of task that died.
  Evidence: agent-backed failures go through `finalize_workspace_after_failure()` in `crates/ingot-agent-runtime/src/lib.rs`, which resets the worktree and releases persistent workspaces back to `Ready`, while `reconcile_running_job()` marks the workspace `Stale` when a running job dies unexpectedly and `reconcile_assigned_job()` simply releases an `Assigned` workspace without pretending any in-flight work finished cleanly.

- Observation: the current inline helper shapes are not directly spawnable under `JoinSet`.
  Evidence: Tokio 1.50.0 defines `JoinSet::spawn<F>(&mut self, task: F)` with `F: Future + Send + 'static`, while the current runtime path in `crates/ingot-agent-runtime/src/lib.rs` drives execution through borrowed methods like `run_with_heartbeats(&self, prepared: &PreparedRun, request: AgentRequest)` and `finish_run(&self, prepared: PreparedRun, response: AgentResponse)`.

- Observation: the existing agent-backed prepared state is already cloneable, which matters because the spawned task and the supervisor metadata map both need owned launch data at the same time.
  Evidence: `PreparedRun` in `crates/ingot-agent-runtime/src/lib.rs` is declared `#[derive(Debug, Clone)]`, and the supervisor design in this plan stores agent-backed launch metadata separately from the future moved into `JoinSet::spawn(...)`.

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

- Decision: make the shared non-job helper return enough information to preserve `tick()`’s current early-return behavior when system actions make progress, instead of collapsing everything to one anonymous `bool`.
  Rationale: `tick()` is currently deterministic in a stronger sense than “returns whether anything happened”: once `ConvergenceService::tick_system_actions()` reports progress, `tick()` performs projected-review recovery and returns without also launching a queued job. The existing `tick()`-oriented tests depend on that shape.
  Date/Author: 2026-03-19 / Codex

- Decision: keep `tick_system_action()` and `drain_until_idle(|| self.tick_system_action())` intact for `reconcile_startup()` instead of folding startup draining into the new supervisor helper.
  Rationale: startup already has a distinct control flow in `crates/ingot-agent-runtime/src/lib.rs`, and changing that flow is outside the goal of concurrent `run_forever()` supervision. Preserving the dedicated startup drain reduces scope and keeps the current `reconcile_startup()` tests meaningful.
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
  Rationale: Tokio 1.50.0 already exposes task IDs for completed and failed `JoinSet` entries. Using the `*_with_id` APIs lets the supervisor correlate panics with sidecar metadata, and guarding the blocking wait avoids the empty-set case where `join_next*()` returns `None` immediately and would otherwise starve the sleep branch.
  Date/Author: 2026-03-19 / Codex

- Decision: key the supervisor’s sidecar metadata from `AbortHandle::id()` immediately after each `JoinSet::spawn(...)`.
  Rationale: `JoinSet::spawn(...)` returns `AbortHandle`, and `AbortHandle::id()` is available in the bundled Tokio 1.50.0 API. Capturing that ID at spawn time is the concrete way to correlate later `join_next_with_id()` panics with the prepared job metadata needed for cleanup.
  Date/Author: 2026-03-19 / Codex

- Decision: use `Arc<Semaphore>::try_acquire_owned()` in the launch scan instead of manually checking `available_permits()`.
  Rationale: the Tokio semaphore already exposes a non-blocking owned-permit API that cleanly answers “is capacity available right now?” and produces the exact `OwnedSemaphorePermit` that must be moved into the spawned task. This avoids writing a permit-count probe that then has to be reconciled with the actual permit acquisition.
  Date/Author: 2026-03-19 / Codex

- Decision: keep daemon-only validation behavior distinct from agent-backed runs even after extraction.
  Rationale: the current daemon-only path does not emit prompt/response artifacts or periodic job heartbeats. The goal of this refactor is concurrent supervision, not a side-effect rewrite, so the extracted helper should preserve the current harness-validation semantics while only changing how it is scheduled.
  Date/Author: 2026-03-19 / Codex

- Decision: split daemon-only validation extraction at the current lock boundary instead of wrapping the whole harness command loop in one lock-scoped helper.
  Rationale: `execute_harness_validation()` today holds the project mutation lock only long enough to validate queue state, prepare the workspace, assign the job, and transition it to `running`. Preserving that boundary avoids serializing long-running harness commands behind the lock and keeps concurrent supervision aligned with the current runtime behavior.
  Date/Author: 2026-03-19 / Codex

- Decision: model extracted daemon-only validation preflight with an explicit outcome enum that mirrors `PrepareRunOutcome`.
  Rationale: the current inline validation path has three real outcomes during setup: “not launchable anymore,” “failed before launch and already marked failed,” and “prepared and moved to running.” A plain `Option<PreparedHarnessValidation>` would lose the second case and make the supervisor scan undercount progress when invalid harness configuration or another preflight failure marks a job terminal.
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

- Decision: put any reusable blocking runner and timeout-based state waiter in `crates/ingot-agent-runtime/tests/common/mod.rs`, and keep `crates/ingot-agent-runtime/tests/dispatch.rs` focused on the new supervisor assertions.
  Rationale: `crates/ingot-agent-runtime/tests/common/mod.rs` already centralizes shared runtime-test harness state and fake runners. Extending that file preserves the test structure the repository already uses instead of creating a second, inconsistent location for reusable runtime-test scaffolding.
  Date/Author: 2026-03-19 / Codex

- Decision: make the plan’s cleanup guidance explicit that new supervisor tests must let agent-backed tasks reach a normal terminal path before aborting the outer dispatcher loop.
  Rationale: `run_with_heartbeats()` already contains its own nested `tokio::spawn` around `runner.launch(...)`. Aborting the outer supervised task or dropping the `JoinSet` is therefore not a trustworthy proof that the inner runner future or any subprocess it launched was cleaned up.
  Date/Author: 2026-03-19 / Codex

- Decision: extend the existing `tick_executes_a_queued_authoring_job_and_creates_a_commit` runtime test to cover `stdout.log`, `stderr.log`, and `result.json` instead of adding a separate artifact-only test.
  Rationale: that exact test already uses `TestHarness` plus `FakeRunner` on the main agent-backed success path and already asserts `prompt.txt`. Extending it follows the repository’s current test layout and proves the preserved side effects on a path that already exists.
  Date/Author: 2026-03-19 / Codex

- Decision: preserve the current `run_forever()` liveness contract by keeping one outer “log and continue” error boundary around each supervisor iteration.
  Rationale: the current daemon loop does not die when `tick()` returns `Err`; it logs and keeps running. A `JoinSet` refactor that lets one preparation, reaping, or control-plane error unwind `run_forever()` would be a behavioral regression from the code that exists today.
  Date/Author: 2026-03-19 / Codex

- Decision: add one focused `run_forever()` heartbeat-refresh test using the new blocking runner and timeout-based SQLite waiter.
  Rationale: this refactor moves agent-backed execution behind a supervised spawned-task boundary, but the current tree has no test that directly proves `heartbeat_at` advances on a still-running job. A supervisor-era heartbeat assertion is the concrete proof that the extracted agent path still flows through `run_with_heartbeats()`.
  Date/Author: 2026-03-19 / Codex

- Decision: make the heartbeat-refresh regression capture the first `running` row and then wait for a strictly later `heartbeat_at` value before releasing the blocker.
  Rationale: `start_job_execution(...)` already writes the initial heartbeat when the job first enters `running`, so merely observing a non-null heartbeat would not prove that the supervised task continued refreshing it.
  Date/Author: 2026-03-19 / Codex

- Decision: on supervised join error, route agent-backed cleanup back through the existing failure helper and use conservative direct cleanup for daemon-only validation tasks.
  Rationale: `PreparedRun` already carries the state `fail_run(...)` and `finalize_workspace_after_failure()` need to reset and release agent-backed workspaces correctly. Daemon-only validation has no equivalent helper today, and when an unexpected failure happens after `start_job_execution(...)` the current conservative precedent is `reconcile_running_job()`, which marks the workspace stale rather than claiming it is safely reusable.
  Date/Author: 2026-03-19 / Codex

- Decision: make the spawned supervisor helpers own their prepared state and a cloned `JobDispatcher` instead of borrowing `&self` or stack-local values from `run_forever()`.
  Rationale: Tokio’s pinned `JoinSet::spawn(...)` API requires a `Send + 'static` future. The current inline helper signatures in `crates/ingot-agent-runtime/src/lib.rs` are fine for `tick()`, but they cannot be moved into a `JoinSet` task unchanged.
  Date/Author: 2026-03-19 / Codex

- Decision: make the new daemon-validation prepared struct cloneable, or derive the sidecar metadata from it immediately at spawn time, instead of trying to share one owned value between the task and the metadata map.
  Rationale: `PreparedRun` already solves this for agent-backed tasks by deriving `Clone`. The daemon-validation path will need the same ownership answer because both the spawned future and `running_meta` need launch-time data simultaneously.
  Date/Author: 2026-03-19 / Codex

- Decision: add the new concurrent-test coordination primitives to `crates/ingot-agent-runtime/tests/common/mod.rs` instead of open-coding channels or notifiers inside `dispatch.rs`.
  Rationale: that shared module already owns the repository’s reusable fake runners and `TestHarness`, and the current tree has no other runtime-test coordination helper to reuse. Centralizing the blocking runner, waiter helpers, and `parse_timestamp` re-export there keeps the six new `run_forever()` tests consistent with existing test-support layout.
  Date/Author: 2026-03-19 / Codex

## Outcomes & Retrospective

This refactor is now implemented in the working tree. `JobDispatcher::run_forever()` owns a supervisor-local `JoinSet`, bounded `Semaphore`, and sidecar metadata map; `tick()` remains the synchronous one-unit helper used by existing tests and HTTP call sites; queue scanning in the background loop now fills capacity across the existing `list_queued_jobs(32)` window instead of stopping at the first stale or authoring-Busy head; and both the agent-backed path and the daemon-only validation path can run under the same supervisor without weakening their current durable side effects.

The focused runtime outcome is observable and covered by tests: `dispatch.rs` now proves concurrent launch, completion-driven wakeups, queue-head skipping, and heartbeat refresh, while `auto_dispatch.rs` proves daemon-only validation also runs under `run_forever()` and still stays artifact-free. The implementation also extended the existing authoring success assertion so agent-backed artifact coverage now includes `prompt.txt`, `stdout.log`, `stderr.log`, and `result.json`.

Validation status is mixed but clear. The runtime-focused regressions, the adjacent HTTP recovery regression, the full runtime test binaries, and `make test` all pass. `make lint` and `make ci` still fail, but the blocker is pre-existing Clippy debt in `crates/ingot-usecases/src/convergence.rs`, not this dispatcher refactor. A follow-up issue is required before the repository-wide lint gate can be considered green.

## Context and Orientation

The relevant daemon wiring lives in `apps/ingot-daemon/src/main.rs`. That file constructs `JobDispatcher`, calls `reconcile_startup()`, and then spawns one long-lived async task that runs `dispatcher.run_forever().await`. There is no other dispatcher process in this repository, so the concurrency supervisor can remain local to that single background loop.

In this plan, a “supervisor” means the `run_forever()` loop after it stops awaiting jobs inline. It is the one background loop that owns the `JoinSet`, waits for notifications or poll wakeups, launches queued work, and reaps completed tasks. A “preflight” helper means the lock-scoped setup phase that decides whether a queued job is still launchable and, if it is, moves it into the repository state needed for execution. A “sidecar metadata map” means a separate in-memory `HashMap` keyed by Tokio task ID; it exists because `JoinSet` can report a task ID on panic via `join_next_with_id()`, but the panicked task itself cannot return cleanup data.

The main runtime implementation lives in `crates/ingot-agent-runtime/src/lib.rs`. `DispatcherConfig` currently contains `state_root`, `poll_interval`, `heartbeat_interval`, and `job_timeout`. `JobDispatcher::run_forever()` currently calls `tick()` in a loop and sleeps on either `DispatchNotify::notified()` or `poll_interval` when no progress was made. `tick()` performs maintenance and system actions, selects at most one queued job with `next_runnable_job()`, and then either runs a daemon-only validation inline with `execute_harness_validation()` or prepares an agent-backed run with `prepare_run()` and awaits `run_with_heartbeats()` inline.

One current behavior matters for safety as much as for concurrency: the daemon loop is resilient to per-iteration failures. Today `run_forever()` does not return when `tick()` fails. It logs the error and keeps waiting for the next notify-or-poll wakeup. The supervisor rewrite must preserve that outer error boundary so one preparation failure, one cleanup failure, or one control-plane failure does not kill the long-lived dispatcher task that `apps/ingot-daemon/src/main.rs` spawns.

Because `run_forever()` currently does nothing except call `tick()`, every maintenance effect the daemon has today comes through `tick()` as well. That includes `ReconciliationService::tick_maintenance()`, `ConvergenceService::tick_system_actions()`, and the trailing `recover_projected_review_jobs()` call. Any supervisor refactor must keep those three behaviors in the background loop even when queued-job launching stops going through `tick()`. It must also preserve `tick()`’s current control flow: when `tick_system_actions()` reports progress, `tick()` performs projected-review recovery and returns `Ok(true)` without also launching a queued job in the same call.

An “agent-backed job” in this repository means a job that launches a Codex CLI process through the `AgentRunner` trait. A “daemon-only validation job” means a validation step with `execution_permission == daemon_only`; those jobs are executed entirely inside the daemon by `execute_harness_validation()` and do not launch an external agent process. Both categories must participate in the same bounded-concurrency policy because both consume dispatcher attention and workspace resources, but they do not currently have identical side effects. The agent-backed path writes prompt and response artifacts and sends periodic heartbeats through `run_with_heartbeats()`. The daemon-only validation path starts the job, runs harness commands, and completes through `CompleteJobService`, but it does not currently write prompt or response artifacts and it does not issue runtime heartbeats.

The existing queue and execution store surfaces already cover most of this refactor. `crates/ingot-store-sqlite/src/store/job.rs` already provides `Database::list_queued_jobs(limit)`, `Database::start_job_execution(...)`, `Database::heartbeat_job_execution(...)`, and `Database::finish_job_non_success(...)`. `StartJobExecutionParams` and `FinishJobNonSuccessParams` are defined in `crates/ingot-domain/src/ports.rs`, and the `JobRepository` trait in that same file already exposes the matching `list_queued`, `start_execution`, and `heartbeat_execution` shape. The concurrency refactor should reuse those methods and types rather than introducing new store APIs or new domain-port requirements.

Two adjacent SQLite store files matter for the new queue-head tests even though they are not part of the runtime implementation. `crates/ingot-store-sqlite/src/store/revision.rs` provides `Database::create_revision(...)`, and `crates/ingot-store-sqlite/src/store/item.rs` provides `Database::update_item(...)`; together they are the real persisted path for making one queued job stale by moving an item to a newer revision. `crates/ingot-store-sqlite/src/store/workspace.rs` provides `Database::find_authoring_workspace_for_revision(...)`, which is the existing lookup that feeds the authoring `WorkspaceError::Busy` path during preparation.

The current preparation and completion flow is already split across helpers. `prepare_run()` acquires the per-project mutation lock, validates that the job is still `queued` on the item’s current revision, loads the harness profile, chooses an available Codex-capable agent with `select_agent()`, provisions the workspace, writes the prompt snapshot into the job row, and returns `PrepareRunOutcome`. `run_with_heartbeats()` calls `start_job_execution(...)`, spawns the adapter future in a nested Tokio task, updates heartbeats until completion or timeout, and returns `Result<AgentResponse, AgentError>`. Downstream completion logic is already handled by `finish_run()`, `fail_run()`, `fail_job_preparation()`, `write_prompt_artifact()`, `write_response_artifacts()`, and the workspace finalization helpers. `write_prompt_artifact()` and `write_response_artifacts()` currently make the agent-backed side effects observable through `prompt.txt`, `stdout.log`, `stderr.log`, and `result.json` under `state_root/logs/<job-id>/`. `execute_harness_validation()` already has a similar split point internally: it performs queue/workspace setup and `start_job_execution(...)` under the project lock, then drops that lock before running harness commands and completing the job. The concurrent supervisor should reuse those existing boundaries instead of inventing a second completion pipeline or broadening lock scope.

Keep the existing synchronous wrapper shape in mind while refactoring that daemon-only path. Today `tick()` knows only about `execute_harness_validation()`; it does not know about the internal lock-scoped locals inside that function. The lowest-risk extraction is therefore to keep `execute_harness_validation()` as a thin wrapper that composes the new prepare/run helpers for the single-job `tick()` path, while `run_forever()` calls those same helpers directly when it needs supervised launch-time outcomes.

`crates/ingot-agent-runtime/src/lib.rs` also contains three existing `#[tokio::test]` unit tests for `drain_until_idle(...)`: `drain_until_idle_stops_after_first_idle_result`, `drain_until_idle_retries_until_idle_result`, and `drain_until_idle_returns_first_error`. Because this plan keeps startup draining on `drain_until_idle(|| self.tick_system_action())` while extracting additional shared non-job helpers nearby, those unit tests are part of the concrete regression surface and should be re-run in the first milestone rather than left to the final repository-wide gate.

The focused runtime tests live in several files, and the plan should follow the role each one already has. `crates/ingot-agent-runtime/tests/dispatch.rs` covers single-job agent-backed execution, `crates/ingot-agent-runtime/tests/auto_dispatch.rs` covers daemon-only validation plus projected-review recovery, `crates/ingot-agent-runtime/tests/escalation.rs` covers escalation side effects, `crates/ingot-agent-runtime/tests/convergence.rs` covers startup/system-action draining, and `crates/ingot-agent-runtime/tests/reconciliation.rs` covers startup cleanup and workspace repair. Reusable harness code and fake runners live in `crates/ingot-agent-runtime/tests/common/mod.rs`. That shared file already defines `FakeRunner`, `StaticReviewRunner`, and `ScriptedLoopRunner`, and `TestHarness` already exposes `register_mutating_agent()`, `register_review_agent()`, and `register_full_agent()`. Reuse those existing helpers in the new `run_forever()` tests instead of open-coding agent rows, and add any reusable blocking runner or timeout-based waiter to `crates/ingot-agent-runtime/tests/common/mod.rs` rather than inventing a parallel test-support pattern. `crates/ingot-http-api/tests/job_routes.rs` also matters because `complete_route_recovers_projected_review_after_warning_only_dispatch_failure_on_system_action_tick()` instantiates a dispatcher and calls `tick()` directly. That existing test is a concrete reason to keep `tick()` public and behaviorally stable while moving concurrency into `run_forever()`.

Three adjacent test-support files explain why the queue-head regressions must set explicit timestamps and why this plan now standardizes the timestamp import path. `crates/ingot-domain/src/test_support/job.rs` gives `JobBuilder::new(...)` the shared `default_timestamp()` unless the test overrides `created_at(...)`, so several queued jobs created back-to-back are otherwise tied on the only field `Database::list_queued_jobs(32)` orders by. `crates/ingot-domain/src/test_support/revision.rs` is the existing builder that provides `RevisionBuilder::revision_no(...)` for the stale-head setup, and `crates/ingot-domain/src/test_support/workspace.rs` is the existing builder that provides `WorkspaceBuilder::current_job_id(...)` and `WorkspaceBuilder::status(...)` for the Busy-workspace setup. The new authoring-path supervisor tests should still build queued rows with the existing `test_authoring_job(...)` helper in `crates/ingot-agent-runtime/tests/common/mod.rs`, then override only the fields that need to differ per case, such as `created_at(...)` and `id(...)`.

Those new timestamped tests also need one small import detail called out explicitly. `crates/ingot-agent-runtime/tests/common/mod.rs` currently re-exports `default_timestamp()` through `use common::*;`, but it does not re-export `parse_timestamp(...)` even though `ingot_domain::test_support::mod.rs` already exposes that helper. Because multiple new `crates/ingot-agent-runtime/tests/dispatch.rs` tests in this plan use timestamp strings, standardize the shared test-support surface by extending `crates/ingot-agent-runtime/tests/common/mod.rs` to re-export `parse_timestamp(...)` alongside the existing fixture builders instead of sprinkling new direct imports across each test.

Daemon-only validation already has adjacent test-only helpers in `crates/ingot-agent-runtime/tests/auto_dispatch.rs`: `write_harness_toml(...)`, `make_runtime_workspace(...)`, and `create_authoring_validation_workspace(...)`. Those functions already encode the repository’s current harness-profile and validation-workspace setup patterns. If the new supervisor coverage needs a daemon-only validation background-loop test, keep that test in `crates/ingot-agent-runtime/tests/auto_dispatch.rs` so it can reuse those helpers directly, unless there is a compelling reason to promote one of them into `crates/ingot-agent-runtime/tests/common/mod.rs`.

Startup has one more dependency that the supervisor refactor must not trample. `reconcile_startup()` in `crates/ingot-agent-runtime/src/lib.rs` still calls `bootstrap::ensure_default_agent(&self.db)`, then `ReconciliationService::reconcile_startup()`, and only after that drains convergence system actions with `drain_until_idle(|| self.tick_system_action())`. The new shared helper is for `tick()` and `run_forever()`, not a replacement for that startup-only drain.

That bootstrap behavior lives in `crates/ingot-agent-runtime/src/bootstrap.rs`. It only creates a default Codex agent when `db.list_agents()` is empty, so the new supervisor tests need two different startup setups: the agent-backed `crates/ingot-agent-runtime/tests/dispatch.rs` cases should seed their own mutating agent before `reconcile_startup()`, while the daemon-only validation background-loop case may rely on the existing empty-registry bootstrap path because it never calls `select_agent()`.

The notifier wiring lives in `crates/ingot-usecases/src/notify.rs` and `crates/ingot-http-api/src/router/mod.rs`. `DispatchNotify` is a dispatcher wakeup hint backed by Tokio `Notify`: the HTTP middleware wakes the daemon after successful write requests, while direct database inserts in runtime tests do nothing unless the test calls `notify()` explicitly. The middleware comment in the router is still accurate today because it only says the dispatcher “drains until idle.” The `DispatchNotify` comment is the one that is stale because it names the old “loop while `tick()` returns progress” implementation detail.

One final adjacent runtime file matters for the queue-skip semantics this plan relies on. `crates/ingot-workspace/src/lib.rs` contains `ensure_authoring_workspace_state(...)`, which is the exact helper that returns `WorkspaceError::Busy` before any Git provisioning when an authoring workspace row is already `Busy`. That file remains read-only for this refactor, but its current behavior is the reason the concurrent queue scan may downgrade only the authoring-path Busy case to “not launchable yet” and why the new busy-head regression can use a DB-only workspace row instead of provisioning a real worktree.

Four more current constraints shape the implementation. First, the current agent model in `crates/ingot-domain/src/agent.rs` only distinguishes `Available`, `Unavailable`, and `Probing`, so there is no in-memory or persisted busy-agent reservation to prevent the same agent from being selected for multiple concurrent jobs. Second, although `CliAgentRunner` can launch both Codex and ClaudeCode adapters, `select_agent()` currently hard-filters to `AdapterKind::Codex`, so this plan keeps that selection behavior unchanged. Third, `Database::create_job()` does not deduplicate queued work, so two queued rows can target the same revision at the same time. Fourth, authoring workspace reuse already rejects concurrent attachment to the same workspace: `ensure_authoring_workspace_state()` in `crates/ingot-workspace/src/lib.rs` returns `WorkspaceError::Busy` when an existing authoring workspace is already `Busy`, and both `prepare_run()` and the daemon-only validation path hit that helper only when they are using `WorkspaceKind::Authoring`. This plan therefore keeps agent selection unchanged, makes its main concurrency tests use separate item revisions so they exercise dispatcher concurrency rather than workspace exclusion, and requires the supervisor launch scan to treat authoring-workspace conflicts as “not launchable right now” instead of fatal errors.

## Plan of Work

Begin by extracting two shared helpers inside `crates/ingot-agent-runtime/src/lib.rs`. The first should own the non-job work that `tick()` performs today: maintenance, convergence system actions, and projected-review recovery. Its return value must preserve the distinction that exists today between “maintenance made progress” and “system actions made progress,” because `tick()` currently returns immediately after a successful `tick_system_actions()` pass instead of also launching a queued job in the same call. `tick()` should keep that behavior while using the helper, and the new `run_forever()` supervisor should call the same helper on every wake before it tries to fill permits. Do not delete or repurpose `tick_system_action()`: `reconcile_startup()` still needs it for `drain_until_idle(...)`.

The second extracted helper should own “run one already-prepared job to completion” for agent-backed jobs: write the prompt artifact, build the `AgentRequest`, call `run_with_heartbeats()`, write response artifacts on success, and then route into `finish_run()` or `fail_run()` using the same error mapping that `tick()` uses now. Preserve the current agent-backed artifact contract exactly: `prompt.txt` before launch, then `stdout.log`, `stderr.log`, and `result.json` on successful agent completion. Because this helper will be moved into `JoinSet::spawn(...)`, give it owned inputs rather than borrowed ones: the spawned future must own a cloned `JobDispatcher`, the `PreparedRun`, and the `OwnedSemaphorePermit` it should hold until exit. Do not change `prepare_run()` itself in this milestone; it already performs the lock-scoped setup that makes the queued job safe to launch. In the same file, split daemon-only validation into the same two phases it already has today: one helper that performs the current lock-scoped preflight and returns an explicit outcome for `not prepared`, `failed before launch`, or `prepared`, carrying the values now stored in the local `harness`, `job_id`, `item_id`, `project_id`, `workspace_path`, `step_id`, and `revision_id` variables only in the prepared case; and a second helper that runs commands plus completion after the lock has been released. That second helper must also take owned inputs plus a cloned `JobDispatcher`, for the same `'static` reason. Keep `execute_harness_validation()` itself as the synchronous wrapper for `tick()`: after the extraction it should simply compose the new validation preflight plus post-lock runner so the existing single-job path continues to flow through the helper `tick()` already calls today. This lets `run_forever()` test whether a validation job is launchable without duplicating `execute_harness_validation()` and without accidentally holding the project lock across long-running harness commands, while still recording progress when invalid harness configuration marks a queued job failed during setup. Because this extraction touches the harness path, the agent-backed success and failure paths, prompt assembly, repo-local skill loading, response artifact writing, and the control-plane work that `run_forever()` currently gets only through `tick()`, rerun the existing exact tests in `crates/ingot-agent-runtime/tests/dispatch.rs`, `crates/ingot-agent-runtime/tests/auto_dispatch.rs`, `crates/ingot-agent-runtime/tests/escalation.rs`, and `crates/ingot-agent-runtime/tests/convergence.rs` that already cover those behaviors before moving on. As part of that extraction milestone, extend `tick_executes_a_queued_authoring_job_and_creates_a_commit()` to assert that `state_root/logs/<job-id>/stdout.log`, `state_root/logs/<job-id>/stderr.log`, and `state_root/logs/<job-id>/result.json` exist alongside `prompt.txt`.

Next, extend `DispatcherConfig` in `crates/ingot-agent-runtime/src/lib.rs` with `max_concurrent_jobs: usize`, set `DispatcherConfig::new(...)` to default it to `2`, and refactor `run_forever()` into a true supervisor. The supervisor should own three local pieces of state: `Arc<Semaphore>` for the permit cap, `JoinSet<RunningJobResult>` for spawned tasks, and a sidecar `HashMap<tokio::task::Id, RunningJobMeta>` for cleanup metadata. Use `try_join_next_with_id()` to reap everything that has already finished before launching more work, then call the shared non-job helper before you decide whether the loop is idle or whether there is more queued work to launch. When the loop becomes idle, only await `join_next_with_id()` inside `tokio::select!` when `!running.is_empty()`; otherwise, wait only on `DispatchNotify::notified()` and `sleep(self.config.poll_interval)`. This avoids the empty-`JoinSet` case where `join_next*()` resolves immediately and would defeat the sleep-based fallback. It also keeps the control-plane helper running on join completions, notifications, and poll wakeups instead of only when a queued job happens to launch. When spawning each supervised task, keep the returned `AbortHandle`, call `handle.id()`, and insert the metadata under that task ID immediately so later `JoinError::id()` lookups have something concrete to find. Before writing the six new background-loop tests, extend `crates/ingot-agent-runtime/tests/common/mod.rs` with the shared support those tests are currently missing: retain the real `DispatchNotify`, re-export `parse_timestamp(...)`, add bounded DB waiter helpers, and add one reusable blocking runner with per-launch coordination so the tests can hold jobs open and then release one or all without arbitrary sleeps. The new background-loop tests should then drive the code the same way the daemon does today: clone `h.dispatcher`, spawn `run_forever()` on a Tokio task, notify it after direct SQLite inserts, and abort that outer task only after the test has either observed the terminal state it cares about or intentionally finished teardown.

Keep the current top-level daemon error posture while doing this. In the existing code, `run_forever()` treats one failed `tick()` as a logged iteration failure, not as a fatal daemon exit. Preserve that shape by keeping one outer match around each supervisor pass: if reaping, control-plane work, queue scanning, or candidate preparation returns `RuntimeError`, log it, drop any just-acquired permit, and continue the loop rather than returning from `run_forever()`. The goal of this plan is concurrent supervision, not a more fragile daemon.

Then replace the single-candidate launcher only in `run_forever()`. Keep `tick()` calling `next_runnable_job()` so the existing tests and HTTP call sites stay deterministic. Inside `run_forever()`, reuse `Database::list_queued_jobs(32)` and scan that ordered window from oldest to newest while permits remain. Acquire launch capacity with `semaphore.clone().try_acquire_owned()`: if it returns `TryAcquireError::NoPermits`, stop scanning; if it returns a permit, either move that permit into a spawned task or drop it immediately when preparation decides the job is not launchable after all. For each queued row, first filter unsupported jobs exactly the way `next_runnable_job()` does today. For daemon-only validation jobs, handle the extracted preflight the same way `prepare_run(...)` is handled today for agent-backed jobs: `NotPrepared` means drop the permit and continue scanning, `FailedBeforeLaunch` means drop the permit, record progress, and continue scanning, and `Prepared(prepared)` means spawn the post-lock validation runner in a supervised task. If the validation preflight hits `RuntimeError::Workspace(WorkspaceError::Busy)` while preparing an authoring workspace, drop the permit and continue scanning later jobs instead of aborting the whole supervisor loop. For agent-backed jobs, call `prepare_run(job).await` and handle the outcomes carefully: `Ok(NotPrepared)` means drop the just-acquired permit, skip this candidate, and continue scanning; `Ok(FailedBeforeLaunch)` means drop the permit, record progress, and continue scanning; `Ok(Prepared(prepared))` means spawn the shared execution helper, move the `OwnedSemaphorePermit` into the task so the slot is released only when the task exits, and record cleanup metadata keyed by the spawned task’s `AbortHandle::id()`; `Err(RuntimeError::Workspace(WorkspaceError::Busy))` means drop the permit and treat this candidate as “not launchable yet” in the concurrent supervisor only when that job is also on the authoring path. Preserve queue order among launchable jobs by scanning the rows in the order `list_queued_jobs(32)` already returns. Do not add a new store API and do not reorder jobs by agent, project, or step. Because the plan intentionally reuses the current store API, this fix only skips blocked jobs inside that first 32-row queued window; if all 32 rows are unlaunchable, later queued rows remain out of scope for this change.

After that, make completion handling concrete. `RunningJobResult` should report the spawned task’s logical outcome, but join failures need extra state because a panic prevents the task from returning its output. Use `join_next_with_id()` so the supervisor always knows which task finished. Treat any spawn, any successful reap, and any preflight branch that already changed durable state (`FailedBeforeLaunch`) as supervisor progress, and immediately continue the loop after those events instead of falling through to the sleep path. On a normal completion, remove the sidecar metadata entry. If the returned `RunningJobResult.result` is `Ok(())`, continue normally. If it is `Err(RuntimeError)`, do not stop at logging alone: log the error, then inspect the current job row just like the join-error path does. If that current row is still active, apply the same metadata-driven cleanup path that a `JoinError` would use so a task that returned an error before durable completion does not leave a live lease or busy workspace behind. On a `JoinError`, look up the stored metadata by `JoinError::id()`, then inspect the current job row in SQLite before mutating anything. If the metadata is agent-backed and the job is still active, route cleanup back through the existing failure path so `fail_run(...)` and `finalize_workspace_after_failure()` keep resetting and releasing the workspace exactly the way the inline `tick()` path already does. If the metadata is daemon-only validation and the job is still `running`, fail it directly and mark the current workspace stale, matching the existing conservative `reconcile_running_job()` behavior for unexpected runtime death. If the current row is still only `assigned`, mirror `reconcile_assigned_job()` exactly: move the job back to `queued`, then release the attached workspace to `WorkspaceStatus::Ready`. If the job is already terminal, do nothing. This makes panic recovery immediate; relying on `reconcile_active_jobs()` alone would not be sufficient because running jobs owned by the current dispatcher keep a fresh lease for up to thirty minutes.

Finally, update the runtime test support and add the new `run_forever()` tests. In `crates/ingot-agent-runtime/tests/common/mod.rs`, store the actual `DispatchNotify` used to build `JobDispatcher`, and when `with_config()` receives a custom config, keep `state_root` aligned to `config.state_root.clone()` rather than the throwaway local temp path. Add one bounded async helper there that repeatedly reads from SQLite under `tokio::time::timeout(...)` instead of relying on fixed sleeps; there is no reusable helper for that in tree today. Because `crates/ingot-agent-runtime/tests/common/mod.rs` already houses the shared fake runners and agent-registration helpers, add any reusable blocking runner there as well. That blocking runner should expose both “launch started” and “release one blocked run now” signals so the new concurrency and completion-wakeup tests can prove exactly when the first two jobs enter `running` and exactly when one slot becomes free. Keep the agent-backed concurrency, wakeup, and heartbeat assertions in `crates/ingot-agent-runtime/tests/dispatch.rs`, but keep the new daemon-only validation background-loop regression in `crates/ingot-agent-runtime/tests/auto_dispatch.rs` so it can reuse that file’s existing `write_harness_toml(...)`, `make_runtime_workspace(...)`, and `create_authoring_validation_workspace(...)` helpers instead of duplicating validation setup in a second test file. Follow the actual daemon startup sequence from `apps/ingot-daemon/src/main.rs`: for the agent-backed `crates/ingot-agent-runtime/tests/dispatch.rs` supervisor tests, register the mutating test agent first with `TestHarness::register_mutating_agent()`, then call `reconcile_startup()`. Build those queued authoring jobs with the existing `test_authoring_job(...)` helper so the new cases stay aligned with the already-passing single-job authoring path. For the daemon-only validation supervisor test in `crates/ingot-agent-runtime/tests/auto_dispatch.rs`, it is acceptable to call `reconcile_startup()` on an empty registry and let `bootstrap::ensure_default_agent(...)` populate the default agent row, because that validation path never calls `select_agent()` and the current tree already exercises that empty-registry startup pattern in `crates/ingot-agent-runtime/tests/reconciliation.rs`. After startup, clone the dispatcher into a background task with `let dispatcher = h.dispatcher.clone(); let handle = tokio::spawn(async move { dispatcher.run_forever().await; });`, enqueue work through `db.create_job(...)`, call the retained `dispatch_notify.notify()`, and wait until the database reflects the expected statuses. For the completion-driven wakeup test specifically, set `poll_interval` much larger than the assertion window and do not call `dispatch_notify.notify()` again after releasing the first blocked job; the next queued job must start from the `JoinSet` completion wakeup alone. Each test must abort the background `run_forever()` task before exit, then await the cancelled `JoinHandle`, so later tests do not inherit stray dispatcher activity.

Because the store orders queued jobs only by `created_at`, every new `run_forever()` test that cares about “oldest head job” versus “later runnable job” must set explicit ascending timestamps with `JobBuilder::created_at(...)` instead of relying on insertion order or on the builders’ shared default timestamp. Use that in both the stale-head and workspace-busy focused tests so the first row in the `list_queued_jobs(32)` window is deterministic. For the stale-head case, create the original queued job on revision 1, then create a real second revision with `RevisionBuilder::new(item_id).revision_no(2)...build()`, persist it with `db.create_revision(&next_revision)`, load the item back from SQLite, set `item.current_revision_id = next_revision.id`, and write it with `db.update_item(&item)` before notifying the dispatcher. That is the concrete way this repository creates the revision-drift condition that `prepare_run()` already treats as `NotPrepared`. For the workspace-busy case, prefer inserting a Busy authoring workspace row with `WorkspaceBuilder` directly in `crates/ingot-agent-runtime/tests/dispatch.rs`; because `ensure_authoring_workspace_state()` returns `WorkspaceError::Busy` before any Git work when the existing row is already Busy, this focused regression does not need a provisioned on-disk worktree.

One of those new `run_forever()` tests should prove that the supervised agent-backed path still refreshes heartbeats while the job is blocked in the runner. Reuse the same blocking runner and timeout-based waiter planned for the concurrency tests: configure a short `heartbeat_interval`, wait for the job to enter `running`, record its first `heartbeat_at`, then keep waiting until a later SQLite read shows a strictly newer heartbeat before releasing the blocker. That is the direct regression test for the extracted `run_with_heartbeats()` path that the current tree is missing.

## Concrete Steps

Work from `/Users/aa/Documents/ingot`.

Implement the refactor in this order so each milestone is independently provable.

First, extract the shared execution helper and keep `tick()` green:

    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --lib drain_until_idle_
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test dispatch tick_executes_a_queued_authoring_job_and_creates_a_commit -- --exact
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test dispatch tick_executes_a_review_job_and_persists_structured_report -- --exact
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test dispatch tick_times_out_long_running_job_and_marks_it_failed -- --exact
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test dispatch tick_runs_healthy_queued_job_even_when_another_project_is_broken -- --exact
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test auto_dispatch daemon_only_validation_job_executes_on_tick -- --exact
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test auto_dispatch harness_validation_with_commands_produces_findings_on_failure -- --exact
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test auto_dispatch daemon_only_validation_fails_on_invalid_harness_profile -- --exact
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test auto_dispatch queued_authoring_job_fails_on_invalid_harness_profile -- --exact
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test auto_dispatch authoring_prompt_includes_resolved_repo_local_skill_files -- --exact
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test auto_dispatch queued_authoring_job_fails_when_harness_skill_glob_escapes_repo -- --exact
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test auto_dispatch queued_authoring_job_fails_when_repo_local_skill_symlink_points_outside_repo -- --exact
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test auto_dispatch harness_validation_timeout_kills_background_processes -- --exact
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test auto_dispatch daemon_validation_resyncs_authoring_workspace_before_running_harness -- --exact
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test auto_dispatch daemon_validation_resyncs_integration_workspace_before_running_harness -- --exact
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test auto_dispatch tick_recovers_idle_review_work_even_when_processing_other_queued_jobs -- --exact
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test escalation runtime_terminal_failure_escalates_closure_relevant_item -- --exact
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test escalation successful_authoring_retry_clears_escalation_and_reopens_review_dispatch -- --exact
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test convergence reconcile_startup_does_not_spin_when_auto_finalize_is_blocked -- --exact
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test convergence tick_reports_no_progress_when_auto_finalize_is_blocked -- --exact
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test convergence tick_auto_finalizes_not_required_prepared_convergence_even_when_commit_exists_only_in_mirror -- --exact
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test reconciliation reconcile_startup_expires_stale_running_jobs_and_marks_workspace_stale -- --exact
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test reconciliation reconcile_active_jobs_reports_progress_when_it_expires_a_running_job -- --exact
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test reconciliation reconcile_startup_handles_mixed_inflight_states_conservatively -- --exact

Expected evidence is the three `drain_until_idle_* ... ok` unit-test lines, plus twenty-three exact-test `... ok` lines in this macOS checkout. On non-Unix platforms, the `queued_authoring_job_fails_when_repo_local_skill_symlink_points_outside_repo` invocation should still succeed, but it will report zero matched tests instead of an `... ok` line because that test is behind `#[cfg(unix)]`, so expect twenty-two exact-test `... ok` lines there. If any of the other named commands fail, the extraction changed behavior instead of just making it reusable.

Second, add the harness updates and `max_concurrent_jobs`, then refactor `run_forever()` into the supervisor that the new tests exercise. Each new `run_forever()` test should mirror `apps/ingot-daemon/src/main.rs` by calling `reconcile_startup()` before the background loop starts:

    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test dispatch run_forever_launches_up_to_max_concurrent_jobs -- --exact
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test dispatch run_forever_starts_next_job_on_joinset_completion -- --exact
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test dispatch run_forever_skips_unlaunchable_head_job_when_filling_capacity -- --exact
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test dispatch run_forever_skips_workspace_busy_head_job_when_filling_capacity -- --exact
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test dispatch run_forever_refreshes_heartbeat_while_job_is_running -- --exact
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test auto_dispatch run_forever_executes_daemon_only_validation_job -- --exact

Before adding those six tests, update `crates/ingot-agent-runtime/tests/common/mod.rs` in one pass: re-export `parse_timestamp(...)`, retain `dispatch_notify`, add the DB waiter helpers described below, and add a shared `BlockingRunner` that records how many launches have started, blocks each launch on an explicit release signal, and lets tests release one launch at a time or all remaining launches. Use that shared runner rather than bespoke per-test channels. Then use a custom `DispatcherConfig` in the new tests with a deliberately large `poll_interval` such as `Duration::from_secs(10)`, so an unexpected fallback poll cannot satisfy the wakeup assertions. Start each background-loop test with the same pattern the daemon uses today, for example `let dispatcher = h.dispatcher.clone(); let handle = tokio::spawn(async move { dispatcher.run_forever().await; });`, and end it with `handle.abort()` plus a bounded await once the assertion phase is over. In the heartbeat test, also set `heartbeat_interval` small enough that the test can observe at least one refresh well before `job_timeout`, then read the job once it first reaches `running`, record that initial `heartbeat_at`, and wait until a later read shows a strictly newer timestamp before releasing the blocker. In `run_forever_starts_next_job_on_joinset_completion`, use that same blocking runner to hold the initial two launches open, release exactly one launch after the test has observed two running jobs, and then wait for the third job to reach `running` without sending a second notify. Register one mutating Codex agent through `TestHarness::register_mutating_agent()` rather than multiple agents for the first five agent-backed tests, and construct those authoring jobs with the existing `test_authoring_job(...)` helper in `crates/ingot-agent-runtime/tests/common/mod.rs`; the current runtime has no busy-agent reservation, so one available agent plus the normal authoring-job helper are enough to prove the bounded dispatcher concurrency this plan is changing. Keep those first five new background-loop tests in `crates/ingot-agent-runtime/tests/dispatch.rs`, but put `run_forever_executes_daemon_only_validation_job` in `crates/ingot-agent-runtime/tests/auto_dispatch.rs` so it can reuse `write_harness_toml(...)`, `make_runtime_workspace(...)`, and `create_authoring_validation_workspace(...)` instead of cloning validation setup into `crates/ingot-agent-runtime/tests/dispatch.rs`. That daemon-only validation test does not need a pre-registered test agent, because the runtime path under test never calls `select_agent()`. In that daemon-only validation test, also assert that `h.state_root.join("logs").join(job.id.to_string())` does not gain `prompt.txt` or `result.json`; the current daemon-only path in `execute_harness_validation()` does not call `write_prompt_artifact()` or `write_response_artifacts()`, and the supervisor refactor must preserve that.

When setting up the two queue-head regressions in this milestone, override each queued job’s `created_at` explicitly with real `DateTime<Utc>` values, for example `parse_timestamp("2026-03-12T00:00:00Z")`, `parse_timestamp("2026-03-12T00:00:01Z")`, and `parse_timestamp("2026-03-12T00:00:02Z")`, so the stale or busy job is observably the oldest row returned by `Database::list_queued_jobs(32)`. For `run_forever_skips_workspace_busy_head_job_when_filling_capacity`, create the conflicting authoring workspace row directly with `WorkspaceBuilder::new(...).created_for_revision_id(...).status(WorkspaceStatus::Busy).current_job_id(...)`; do not spend test setup time provisioning a real worktree, because the Busy branch in `ensure_authoring_workspace_state()` returns before any Git verification.

For `run_forever_skips_unlaunchable_head_job_when_filling_capacity`, make the first queued job stale with the actual store sequence this repository supports: persist the initial item/revision pair, create the queued job against that first revision, build a second revision with `RevisionBuilder::new(item_id).revision_no(2)` and the same template/seed shape, `db.create_revision(&next_revision)`, then load the item, assign `item.current_revision_id = next_revision.id`, and `db.update_item(&item)` before starting the dispatcher. This mirrors the same persisted revision-drift condition exercised in the SQLite store tests.

Expected evidence is:

    test run_forever_launches_up_to_max_concurrent_jobs ... ok
    test run_forever_starts_next_job_on_joinset_completion ... ok
    test run_forever_skips_unlaunchable_head_job_when_filling_capacity ... ok
    test run_forever_skips_workspace_busy_head_job_when_filling_capacity ... ok
    test run_forever_refreshes_heartbeat_while_job_is_running ... ok
    test run_forever_executes_daemon_only_validation_job ... ok

Third, run the adjacent regression tests that depend on `tick()` semantics and projected-review recovery:

    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test dispatch
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test escalation
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test convergence
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test auto_dispatch
    cd /Users/aa/Documents/ingot && cargo test -p ingot-agent-runtime --test reconciliation
    cd /Users/aa/Documents/ingot && cargo test -p ingot-http-api --test job_routes complete_route_recovers_projected_review_after_warning_only_dispatch_failure_on_system_action_tick -- --exact

Expected evidence is that the full `dispatch`, `escalation`, `convergence`, `auto_dispatch`, and `reconciliation` binaries finish successfully and the HTTP test prints one `... ok` line for the named route test.

Finally, run the repository gate commands that this repo uses for backend and UI CI:

If `ui/node_modules` is missing, run `cd /Users/aa/Documents/ingot && make ui-install` once before the combined frontend commands below.

    cd /Users/aa/Documents/ingot && make test
    cd /Users/aa/Documents/ingot && make lint
    cd /Users/aa/Documents/ingot && make ci

If `make ci` fails only because of unrelated pre-existing UI issues, record that exact failure in `Surprises & Discoveries` and in the final implementation note instead of silently skipping it.

## Validation and Acceptance

Acceptance is reached when the daemon demonstrates all of the following behaviors.

First, with `max_concurrent_jobs = 2`, three queued authoring jobs on three distinct item revisions and a blocking fake runner, exactly two jobs enter `running` before either one completes. The third job must remain `queued` until one running job releases its permit.

Second, when one running job finishes and another queued job is waiting, `run_forever()` starts the next job within a timeout that is materially shorter than `poll_interval`. For example, if `poll_interval` is set to ten seconds, the test should observe the next job reach `running` within a few hundred milliseconds after the first job is released. Do not send a second `dispatch_notify.notify()` after releasing the blocker in this test. That is the proof that completion wakeups come from the `JoinSet` completion path rather than from fallback polling or a second manual notification.

Third, the concurrency cap is strict. At no point should more than `max_concurrent_jobs` jobs be in `running` for the single dispatcher instance, as observed through SQLite reads during the test.

Fourth, agent-backed jobs still produce the same durable side effects they do today: `prompt.txt`, `stdout.log`, `stderr.log`, and `result.json` under `state_root/logs/<job-id>/`, periodic heartbeat updates while they are running, timeout handling through `AgentError::Timeout`, workspace cleanup, result persistence, and projected-review recovery. At least one focused agent-backed success path should assert those exact filenames after the refactor so the side effects stay observable rather than implied. The new exact test `run_forever_refreshes_heartbeat_while_job_is_running` should also prove the live-heartbeat part directly by observing `job.state.heartbeat_at()` advance on a blocked supervised job before that job is released, not merely by checking that `heartbeat_at` is non-null. Daemon-only validation jobs must also keep their current behavior: they should still transition through `start_job_execution(...)`, produce a `validation_report:v1` payload through `CompleteJobService`, and auto-dispatch follow-up review or validation work exactly as they do today, but they must not gain new prompt/response artifact or heartbeat requirements in this refactor. Add one new background-loop regression in `crates/ingot-agent-runtime/tests/auto_dispatch.rs`, `run_forever_executes_daemon_only_validation_job`, that uses the existing validation setup helpers in that file, queues a daemon-only validation job after `reconcile_startup()`, calls `dispatch_notify.notify()`, and then waits for `Completed` plus `result_schema_version == Some("validation_report:v1")` without calling `tick()`. That same test should assert that the per-job artifact directory does not gain agent-style files like `prompt.txt` or `result.json`, because the current daemon-only path does not write them.

The concrete place to keep that assertion is the existing exact test `tick_executes_a_queued_authoring_job_and_creates_a_commit` in `crates/ingot-agent-runtime/tests/dispatch.rs`. That test already reaches the happy-path agent-backed execution with `FakeRunner`; after the refactor it should continue asserting `prompt.txt` and additionally assert that `stdout.log`, `stderr.log`, and `result.json` were written in the same artifact directory.

The existing exact tests `daemon_only_validation_job_executes_on_tick`, `harness_validation_with_commands_produces_findings_on_failure`, `daemon_only_validation_fails_on_invalid_harness_profile`, `daemon_validation_resyncs_authoring_workspace_before_running_harness`, and `daemon_validation_resyncs_integration_workspace_before_running_harness` should still pass after the helper extraction, proving that the daemon-only path still handles the clean, findings, invalid-profile, and workspace-resync branches it already supports today.

The existing exact tests `queued_authoring_job_fails_on_invalid_harness_profile`, `authoring_prompt_includes_resolved_repo_local_skill_files`, `queued_authoring_job_fails_when_harness_skill_glob_escapes_repo`, and, on Unix, `queued_authoring_job_fails_when_repo_local_skill_symlink_points_outside_repo` should also still pass after the extraction. Those tests are the current proof that `prepare_run()` still loads harness commands and repo-local skills correctly, still rejects escaping skill paths before launch, and still avoids writing `prompt.txt` on prep-time harness failures.

Fifth, an unlaunchable queued head job does not consume all progress inside the current `Database::list_queued_jobs(32)` window. When capacity is available and a later queued job in that returned ordered slice is launchable, that later job starts without manual intervention. The focused test should make the first queued job stale by changing the item’s current revision after the job row is created, because `prepare_run()` already turns that condition into `PrepareRunOutcome::NotPrepared`.
The concrete setup in this repository is: persist revision 1 with `db.create_item_with_revision(&item, &revision)`, create the head queued job against revision 1, create revision 2 with `db.create_revision(&next_revision)`, then load and update the item so `current_revision_id = next_revision.id` before notifying the dispatcher. Do not fake this by mutating only the in-memory `ItemBuilder`; `prepare_run()` reads the persisted item row from SQLite.

Sixth, an authoring-workspace-conflicting queued head job also does not consume all progress inside that same current queue window. When the oldest queued job wants an authoring workspace that is already `Busy`, `run_forever()` should treat that candidate as “not launchable yet” and continue scanning later queued jobs in the same `list_queued_jobs(32)` result that do not share that workspace. The new focused test should construct that conflict explicitly with deterministic `created_at` ordering and a pre-existing Busy workspace row for the head revision, because `Database::create_job()` does not prevent the conflicting queued rows and `ensure_authoring_workspace_state()` hits the Busy branch before it touches Git state.

Seventh, the refactor does not strand the daemon’s non-job work behind the new supervisor. The code should show that both `tick()` and `run_forever()` invoke the same helper that wraps maintenance, convergence system actions, and projected-review recovery, and the full `dispatch`, `escalation`, `convergence`, `auto_dispatch`, `reconciliation`, and adjacent HTTP recovery tests should continue to pass.

Eighth, `tick()` remains a one-unit helper for existing tests and call sites. In particular, the existing exact tests `tick_runs_healthy_queued_job_even_when_another_project_is_broken` and `tick_recovers_idle_review_work_even_when_processing_other_queued_jobs` should still pass, proving that extracting shared helpers did not accidentally make `tick()` launch extra work or stop recovering queued review work in the same situations it handles today.

Ninth, join-error cleanup repairs both the job row and the workspace row in a way that matches current runtime semantics. Agent-backed tasks should still use the existing failure cleanup path that resets and releases the workspace, while daemon-only validation tasks that die unexpectedly after entering `running` should conservatively mark their workspace stale instead of pretending it is cleanly reusable.
The existing exact tests `reconcile_startup_expires_stale_running_jobs_and_marks_workspace_stale`, `reconcile_active_jobs_reports_progress_when_it_expires_a_running_job`, and `reconcile_startup_handles_mixed_inflight_states_conservatively` are the concrete baseline for those semantics today; keep them green while adding the supervisor-specific cleanup path.

Tenth, task-returned `RuntimeError`s do not strand active work. A supervised task that returns `Err(RuntimeError)` instead of panicking must still trigger the same “is the current job row still active?” cleanup check that the join-error path uses.

Eleventh, the updated comment in `crates/ingot-usecases/src/notify.rs` describes the implementation truthfully after the refactor, and `crates/ingot-http-api/src/router/mod.rs` has been re-checked and only changed if needed.

Twelfth, the daemon loop still survives iteration-scoped runtime failures. The final `run_forever()` implementation should retain one outer catch-and-log boundary analogous to today’s `match self.tick().await` block, so the spawned daemon task in `apps/ingot-daemon/src/main.rs` does not exit permanently because one reap, preparation, or non-job helper call returned `Err(RuntimeError)`.

Thirteenth, the new queue scan does not over-promise behavior the current code does not implement. Only launch-time outcomes that are already modeled as `NotPrepared`, `FailedBeforeLaunch`, or authoring-path `WorkspaceError::Busy` should be downgraded to “skip this candidate and continue scanning.” Other `RuntimeError`s should still surface through the outer log-and-continue boundary, because changing those failure semantics would be a separate behavioral change from this bounded-concurrency refactor.

## Idempotence and Recovery

This refactor should be safe to apply incrementally because it does not require a schema migration or a public API change. If implementation breaks the new supervisor loop mid-flight, the existing startup reconciliation path in `reconcile_startup()` remains the recovery backstop for jobs that are left `assigned`, for convergences that are left active, and for stale workspaces. Keep the fallback `poll_interval` path in `run_forever()` even after adding `JoinSet` wakeups; that poll remains the safety net for any work that does not arrive through `DispatchNotify` and is not represented by a running supervised task.

During implementation, retry focused test commands freely because the runtime tests already create temporary SQLite databases and temporary Git repositories. When a `run_forever()` test starts a background dispatcher task, always abort that task before the test returns and then await the `JoinHandle` long enough to observe the cancellation, so later tests do not inherit stray runtime activity. When a test overrides `DispatcherConfig`, use the dispatcher’s actual `state_root` for artifact assertions; after the helper fix, that should once again be `h.state_root`.

Keep the supervisor’s own outer error handling idempotent too. If one iteration hits `RuntimeError` while reaping tasks, driving non-job work, or preparing a candidate, log it and continue the loop rather than returning from `run_forever()`. That preserves the current daemon behavior and makes retries automatic on the next notify-or-poll wakeup.

That log-and-continue recovery path remains intentionally narrow. This plan only teaches the supervisor to keep scanning past stale / otherwise `NotPrepared` jobs, `FailedBeforeLaunch` preflight failures that already wrote durable terminal state, and authoring-path `WorkspaceError::Busy`. If a queued head job keeps returning some other `RuntimeError` such as a workspace ref/head mismatch or mirror failure, the daemon will continue to log that iteration failure and revisit the same head row later, just as it does today. Capture any desire to broaden that skip behavior as a separate follow-up issue rather than silently changing it inside this refactor.

Tokio 1.50.0’s `JoinSet` aborts tracked tasks when the set is dropped. That is useful for isolated tests, but it is not the production cleanup path to rely on. In `run_forever()` tests that intentionally block jobs open, prefer releasing those blockers and waiting for terminal job states before aborting the outer dispatcher loop. That caution is especially important for agent-backed tasks because `run_with_heartbeats()` already spawns a nested Tokio task for `runner.launch(...)`; dropping the outer supervised task is not the same thing as exercising the inner task’s normal timeout, cancel, or completion cleanup path. If a test does abort the outer loop while supervised jobs are still running, treat the resulting task cancellation as test teardown on a throwaway database, not as proof that the supervisor’s normal cleanup path is correct.

For panic recovery inside `run_forever()`, never assume `reconcile_active_jobs()` will clean up promptly enough to be the primary mechanism. Agent-backed supervised tasks write `lease_owner_id = self.lease_owner_id`, so a fresh lease on a panicked agent-backed job will cause reconciliation to leave it alone. Daemon-only validation tasks currently write `lease_owner_id = "daemon"`, so reconciliation would treat them as `foreign_owner` and immediately mark them `Expired` with `error_code = "heartbeat_expired"` even if the real cause was a panic in the supervised task. The supervisor should therefore clean up both categories directly, using its sidecar task metadata and the current job state to fail or release stranded work with the right semantics on join error.

## Artifacts and Notes

Useful evidence to preserve while implementing includes a short debug log excerpt showing two jobs entering `running` before the blocking runner is released, and a focused test transcript showing that the completion-driven wakeup test passes even with a deliberately large `poll_interval`.

One concise transcript worth preserving after the test phase is:

    test run_forever_launches_up_to_max_concurrent_jobs ... ok
    test run_forever_starts_next_job_on_joinset_completion ... ok
    test run_forever_skips_unlaunchable_head_job_when_filling_capacity ... ok
    test run_forever_skips_workspace_busy_head_job_when_filling_capacity ... ok
    test run_forever_refreshes_heartbeat_while_job_is_running ... ok
    test run_forever_executes_daemon_only_validation_job ... ok

If the implementation chooses a specific error code for “supervised task panicked or was aborted unexpectedly,” record that exact code here after implementation, because the current codebase does not already define one for outer supervisor-task failure.

## Interfaces and Dependencies

This change should stay inside `crates/ingot-agent-runtime/src/lib.rs`, `crates/ingot-agent-runtime/src/bootstrap.rs` as the read-only reference for startup bootstrap semantics, `crates/ingot-store-sqlite/src/store/job.rs` as the read-only reference for existing queue APIs, `crates/ingot-store-sqlite/src/store/item.rs` plus `crates/ingot-store-sqlite/src/store/revision.rs` as the read-only references for the stale-head test setup, `crates/ingot-store-sqlite/src/store/workspace.rs` plus `crates/ingot-workspace/src/lib.rs` as the read-only references for the authoring-workspace lookup and Busy-path behavior, `crates/ingot-usecases/src/notify.rs`, `crates/ingot-http-api/src/router/mod.rs` only if its adjacent comment needs follow-up, the domain test-support builders in `crates/ingot-domain/src/test_support/mod.rs`, `crates/ingot-domain/src/test_support/job.rs`, `crates/ingot-domain/src/test_support/revision.rs`, and `crates/ingot-domain/src/test_support/workspace.rs`, and the runtime test files including `crates/ingot-agent-runtime/tests/common/mod.rs`, `crates/ingot-agent-runtime/tests/dispatch.rs`, `crates/ingot-agent-runtime/tests/auto_dispatch.rs`, `crates/ingot-agent-runtime/tests/escalation.rs`, `crates/ingot-agent-runtime/tests/convergence.rs`, `crates/ingot-agent-runtime/tests/reconciliation.rs`, plus the adjacent HTTP test file `crates/ingot-http-api/tests/job_routes.rs`. `apps/ingot-daemon/src/main.rs` already constructs `DispatcherConfig` through `DispatcherConfig::new(state_root.clone())`, so it should compile unchanged once that constructor supplies the new default; only touch the daemon wiring if the implementation introduces a compile fix there. No SQLite migration, domain-entity change, or HTTP route change is required.

The runtime should gain one internal helper for the non-job work that `run_forever()` currently receives only by calling `tick()`. The exact name is flexible, but it must preserve the current distinction between generic maintenance progress and system-action progress so `tick()` can keep its existing early return when system actions fire. The shape can be a small struct or enum rather than a bare `bool`, for example:

    struct NonJobWorkProgress {
        made_progress: bool,
        system_actions_progressed: bool,
    }

    async fn drive_non_job_work(&self) -> Result<NonJobWorkProgress, RuntimeError>;

That helper must wrap `ReconciliationService::tick_maintenance()`, `ConvergenceService::tick_system_actions()`, and `recover_projected_review_jobs()`. `tick()` should call it before attempting `next_runnable_job()` and preserve its current “return after system action progress” behavior; `run_forever()` should call it after reaping finished tasks and before sleeping or filling permits. Keep `tick_system_action()` available for `reconcile_startup()` so `drain_until_idle(|| self.tick_system_action())` stays intact.

At the end of the implementation, `DispatcherConfig` in `crates/ingot-agent-runtime/src/lib.rs` should contain:

    pub struct DispatcherConfig {
        pub state_root: PathBuf,
        pub poll_interval: Duration,
        pub heartbeat_interval: Duration,
        pub job_timeout: Duration,
        pub max_concurrent_jobs: usize,
    }

`DispatcherConfig::new(...)` must populate that new field so existing call sites in `apps/ingot-daemon/src/main.rs`, `crates/ingot-agent-runtime/tests/common/mod.rs`, `crates/ingot-http-api/tests/job_routes.rs`, `crates/ingot-agent-runtime/tests/reconciliation.rs`, `crates/ingot-agent-runtime/tests/convergence.rs`, and the rest of the runtime tests continue to compile without hand-editing every constructor.

The `run_forever()` refactor must also keep the current outer error boundary visible in code. Today that boundary is the `match self.tick().await` block in `crates/ingot-agent-runtime/src/lib.rs`; after the rewrite, the equivalent supervisor iteration should still log a `RuntimeError` and continue rather than unwinding the daemon task.

In `crates/ingot-agent-runtime/tests/common/mod.rs`, `TestHarness` should end this refactor with the existing fields plus the actual notifier it constructed, for example:

    pub struct TestHarness {
        pub db: Database,
        pub dispatcher: JobDispatcher,
        pub dispatch_notify: DispatchNotify,
        pub project: Project,
        pub state_root: PathBuf,
        pub repo_path: PathBuf,
    }

`TestHarness::with_config(...)` should create one `DispatchNotify`, clone it into `JobDispatcher::with_runner(...)`, store it on the harness, and store `config.state_root.clone()` as `state_root` when a custom config is supplied. Add one small polling helper on the harness that uses `tokio::time::timeout(...)` plus repeated `db.get_job(...)` or `db.list_jobs_by_item(...)` reads; the exact helper signature is flexible, but the new `run_forever()` tests should stop depending on fixed sleeps for state convergence. Because `crates/ingot-agent-runtime/tests/common/mod.rs` already contains the reusable fake runners, place any shared blocking runner there too instead of redefining it in each test. Do not use `Database::list_active_jobs()` as a proxy for “currently running” in those assertions, because the current store implementation includes `queued` and `assigned` rows in that query as well as `running`. Leave the existing daemon-only validation helpers in `crates/ingot-agent-runtime/tests/auto_dispatch.rs` unless they are genuinely reused by more than one test file; today that file already owns `write_harness_toml(...)`, `make_runtime_workspace(...)`, and `create_authoring_validation_workspace(...)`, and the new supervised validation regression should follow that existing layout rather than scattering harness setup across multiple binaries.

Standardize the timestamp helper there too. `crates/ingot-agent-runtime/tests/common/mod.rs` should re-export `parse_timestamp(...)` alongside the existing fixture builders so the new `dispatch.rs` supervisor tests can continue to use `use common::*;` instead of adding a second import style only for queue-order setup.

Make those new waiters concrete enough that a novice does not need to invent them. The minimal useful surface is:

    pub async fn wait_for_job_status(
        &self,
        job_id: ingot_domain::ids::JobId,
        expected: JobStatus,
        timeout: Duration,
    ) -> Job;

    pub async fn wait_for_running_jobs(
        &self,
        expected: usize,
        timeout: Duration,
    ) -> Vec<Job>;

`wait_for_job_status(...)` should loop on `db.get_job(job_id)` until the row reaches the requested status or the timeout expires. `wait_for_running_jobs(...)` should read `db.list_jobs_by_project(self.project.id)`, filter to `JobStatus::Running`, and return the matching rows once the count matches `expected`. Because `list_jobs_by_project(...)` orders by `created_at DESC`, callers should treat the returned vector as an unordered set for assertions and match by job ID rather than by element position. Use those helpers, not raw sleeps, in the new concurrent-launch, join-completion, and heartbeat-refresh tests.

Add one reusable runner for the new supervisor tests in that same file. A minimal useful shape is:

    #[derive(Clone)]
    pub struct BlockingRunner { ... }

    impl BlockingRunner {
        pub fn new() -> Self;
        pub async fn wait_for_launches(&self, expected: usize, timeout: Duration);
        pub fn release_one(&self);
        pub fn release_all(&self);
    }

`BlockingRunner::launch(...)` should increment a shared launch count, notify waiters that a launch has started, then block until the test releases a permit. Using one shared runner with `wait_for_launches(...)` plus `release_one()` is the code-grounded way to make `run_forever_launches_up_to_max_concurrent_jobs` and `run_forever_starts_next_job_on_joinset_completion` deterministic without scattered per-test channels.

For the multi-job supervisor assertions, it is acceptable for that polling helper to read `db.list_jobs_by_project(h.project.id)` and filter by `job.state.status() == JobStatus::Running`, because the concurrency tests spread work across several items in one project. When those tests need one job to be observably older than another, use `JobBuilder::created_at(...)` with `parse_timestamp(...)`; the default test builders all share the same `default_timestamp()`, so leaving timestamps implicit makes the queue head ambiguous.

The daemon-only validation extraction should also become explicit enough that the launch scan can prepare or reject a validation job without reimplementing `execute_harness_validation()` inline. The exact names are flexible, but the boundary should look like:

    enum PrepareHarnessValidationOutcome {
        NotPrepared,
        FailedBeforeLaunch,
        Prepared(PreparedHarnessValidation),
    }

    async fn prepare_harness_validation(
        &self,
        queued_job: Job,
    ) -> Result<PrepareHarnessValidationOutcome, RuntimeError>;

    async fn run_prepared_harness_validation(
        &self,
        prepared: PreparedHarnessValidation,
    ) -> Result<(), RuntimeError>;

`PreparedHarnessValidation` should replace the current loose local tuple in `execute_harness_validation()`. At minimum it needs the loaded harness profile plus the IDs and paths that the post-lock command/completion phase already carries today: `job_id`, `item_id`, `project_id`, `revision_id`, `step_id`, and `workspace_path`. Add `workspace_id` while extracting this struct even though the current local tuple does not retain it, because the supervisor cleanup path already needs a stable workspace identity when it has to release or mark that workspace stale after an unexpected task failure. Preserve the existing lock boundary by ensuring `prepare_harness_validation(...)` returns `Prepared(...)` only after the job has been assigned and `start_job_execution(...)` has succeeded, returns `FailedBeforeLaunch` when it has already called `fail_job_preparation(...)`, and `run_prepared_harness_validation(...)` runs the command loop and completion path after that lock has been dropped.

Because the supervisor stores launch metadata separately from the future moved into `JoinSet::spawn(...)`, `PreparedHarnessValidation` should either derive `Clone` the way `PreparedRun` already does, or be converted into a smaller cloneable `RunningJobMeta` payload before spawn. Do not rely on moving the one prepared value into the task and then trying to borrow it back for `running_meta`.

The stale-head regression also depends on existing store interfaces outside the runtime crate. Use `Database::create_revision(...)` from `crates/ingot-store-sqlite/src/store/revision.rs` plus `Database::update_item(...)` from `crates/ingot-store-sqlite/src/store/item.rs` rather than introducing a test-only helper for “make this item stale.” Those APIs already exist, and `prepare_run()` already consumes their persisted result by loading the item row fresh from SQLite.

The runtime should also have an internal launched-task result type. Keep it small and focused on the completed job, for example:

    struct RunningJobResult {
        job_id: ingot_domain::ids::JobId,
        result: Result<(), RuntimeError>,
    }

Because `JoinSet` panics do not return `RunningJobResult`, `run_forever()` should also own a sidecar metadata map keyed by Tokio task ID. The exact struct name is flexible, but the stored data must be enough to clean up a stranded `assigned` or `running` job immediately if `join_next_with_id()` returns `Err(JoinError)`. For agent-backed tasks, the most direct value is a clone of `PreparedRun`; for daemon-only validation tasks, store the IDs and workspace information already extracted in `execute_harness_validation()`.

Make that sidecar payload explicit enough that a novice does not have to invent it. A minimal shape that matches the current code is:

    enum RunningJobMeta {
        Agent(PreparedRun),
        HarnessValidation {
            job_id: ingot_domain::ids::JobId,
            item_id: ingot_domain::ids::ItemId,
            project_id: ingot_domain::ids::ProjectId,
            revision_id: ingot_domain::ids::ItemRevisionId,
            workspace_id: ingot_domain::ids::WorkspaceId,
            step_id: String,
            workspace_path: PathBuf,
        },
    }

The exact names can differ, but the daemon-only variant must carry enough data to release an `Assigned` workspace or mark a `Running` workspace stale without guessing.

That cleanup requirement is not only about the job row. If the stranded current row is still `assigned`, the supervisor must release the attached workspace the same way `reconcile_assigned_job()` does today. If the current row is still `running`, the supervisor must fail the job and repair the workspace row immediately as well: agent-backed tasks should go back through `fail_run(...)` so `finalize_workspace_after_failure()` resets and releases the workspace, while daemon-only validation tasks should conservatively mark the workspace stale, matching the existing `reconcile_running_job()` precedent for unexpected task death. Apply that same check when a supervised task returns `RunningJobResult { result: Err(..) }`; do not reserve active-row cleanup for panics only.

At spawn time, populate that map from the `AbortHandle` returned by `JoinSet::spawn(...)`, for example:

    let handle = running.spawn(task);
    let task_id = handle.id();
    running_meta.insert(task_id, meta);

`run_forever()` should own supervisor-local state with this shape:

    let semaphore = Arc::new(tokio::sync::Semaphore::new(self.config.max_concurrent_jobs));
    let mut running = tokio::task::JoinSet::<RunningJobResult>::new();
    let mut running_meta = std::collections::HashMap::<tokio::task::Id, RunningJobMeta>::new();

Use `tokio::sync::OwnedSemaphorePermit` so a concurrency slot is released exactly when a spawned job task exits. Keep `tick()` public and returning `Result<bool, RuntimeError>`. The shared execution helpers should preserve the existing behavior of `prepare_run()`, `run_with_heartbeats()`, `finish_run()`, `fail_run()`, `fail_job_preparation()`, `execute_harness_validation()`, and the workspace finalization methods. Reuse `Database::list_queued_jobs(32)` for the launch scan, and do not add agent-reservation state in this patch because the current agent model only distinguishes `Available`, `Unavailable`, and `Probing`.

When preserving the agent-backed path, remember that `run_with_heartbeats()` is already a two-layer runtime: the supervisor task awaits `run_with_heartbeats()`, and `run_with_heartbeats()` itself spawns the adapter future on a nested Tokio task before heartbeat polling. The refactor should keep normal success, timeout, and operator-cancel cleanup flowing through that existing helper; it should not treat outer-task abortion as an equivalent control path.

Because `JoinSet::spawn(...)` requires a `Send + 'static` future, the spawned helper boundary should look like an owned helper rather than a borrowed one. The exact names are flexible, but the shape should be equivalent to:

    async fn run_prepared_agent_job(
        dispatcher: JobDispatcher,
        prepared: PreparedRun,
        permit: tokio::sync::OwnedSemaphorePermit,
    ) -> RunningJobResult;

    async fn run_prepared_harness_validation_job(
        dispatcher: JobDispatcher,
        prepared: PreparedHarnessValidation,
        permit: tokio::sync::OwnedSemaphorePermit,
    ) -> RunningJobResult;

Holding the permit as an unused owned argument is intentional: it keeps the concurrency slot occupied for the full lifetime of the spawned task without adding separate bookkeeping.

Inside the concurrent launch scan specifically, match `RuntimeError::Workspace(WorkspaceError::Busy)` from the authoring workspace path in agent-backed preparation and in the extracted daemon-validation setup path, downgrade it to “not launchable yet,” and continue scanning later queued jobs. Do not broaden that downgrade to unrelated workspace errors such as ref mismatches or head mismatches, and do not imply that integration workspace preparation has the same `Busy` guard; those cases should still surface as real runtime failures through the daemon’s outer log-and-continue boundary.

Revision note: created on 2026-03-19 to address the dispatcher’s lack of `JoinSet`-based concurrent job supervision and to capture the related queue-head starvation fix that is necessary for bounded concurrency to be effective.

Revision note: revised on 2026-03-19 after deep-reading the referenced and adjacent code. This pass removed an implied new queue-reader abstraction in favor of the existing `Database::list_queued_jobs(32)` API, added the currently affected doc-comment files and HTTP `tick()` test call site, called out the missing `DispatchNotify`/wait support in the runtime test harness, and made the validation commands and scope constraints match the code that exists today.

Revision note: revised again on 2026-03-19 after re-reading the referenced files for drift. This pass corrected the plan’s own overreach by limiting the required documentation update to `crates/ingot-usecases/src/notify.rs`, added the concrete `TestHarness::with_config()` `state_root` mismatch that will matter for custom-config concurrency tests, and anchored the new async-test guidance to the existing `tokio::time::timeout(...)` pattern already used in `crates/ingot-agent-runtime/tests/convergence.rs`.

Revision note: revised once more on 2026-03-19 after re-reading startup bootstrap behavior. This pass added the requirement that daemon-style `run_forever()` tests register their intended fake agents before calling `reconcile_startup()`, because startup bootstraps a default agent whenever the registry is empty.

Revision note: revised again on 2026-03-19 after re-checking dispatcher wakeup paths and test helpers. This pass made explicit that background `run_forever()` tests must call the retained `DispatchNotify::notify()` after direct SQLite inserts, because only the HTTP write middleware notifies automatically, and it pointed implementers at the existing `TestHarness` agent-registration helpers instead of implying custom agent setup by default.

Revision note: re-audited on 2026-03-19 at 13:27Z after another deep read of the referenced files and adjacent test helpers. This pass did not uncover additional substantive code-grounded plan changes beyond the existing notes about startup bootstrap, explicit dispatch notifications, and the `TestHarness::with_config()` helper gap, so the body was left intentionally unchanged.

Revision note: re-audited again on 2026-03-19 at 13:28Z after re-reading queue selection, `PrepareRunOutcome`, `select_agent()`, and the existing test-support builders. This pass did not find additional substantive plan changes beyond the already documented requirements to reuse the existing queue API, register Codex-capable test agents through the existing harness helpers, and notify the dispatcher explicitly in background tests after direct SQLite inserts.

Revision note: revised on 2026-03-19 after a deeper code audit of `run_forever()`, `execute_harness_validation()`, `ensure_authoring_workspace_state()`, the `job_routes` `tick()` call site, the Makefile targets, and the bundled Tokio 1.50.0 source. This pass added the missing `Plan of Work` section required by `.agent/PLANS.md`, corrected the inaccurate claim that daemon-only validation jobs currently share agent-backed artifacts and heartbeats, made the supervisor design concrete about `try_join_next_with_id()` and `join_next_with_id()`, added the sidecar metadata requirement needed for immediate panic cleanup, specified distinct item revisions for concurrency tests to avoid unrelated workspace-busy failures, and replaced vague validation steps with the exact commands and test names that exist in this repository.

Revision note: revised again on 2026-03-19 after re-auditing Tokio spawn/semaphore details, the existing daemon-only validation tests, and the current adapter-selection path. This pass corrected the remaining ambiguity around spawn-time task metadata by anchoring it to `AbortHandle::id()`, tightened the launch loop to use `Semaphore::try_acquire_owned()` instead of a vague permit-availability check, added the current exact daemon-only validation tests to the extraction milestone, and documented that the runtime still intentionally filters runnable agents to `AdapterKind::Codex` even though `CliAgentRunner` knows about both Codex and ClaudeCode.

Revision note: revised again on 2026-03-19 after re-auditing queued-job insertion, workspace-busy behavior, daemon-only lease ownership, and the existing daemon-validation resync tests. This pass added the concrete requirement to treat `RuntimeError::Workspace(WorkspaceError::Busy)` as “not launchable yet” inside the concurrent supervisor so one conflicting head job does not still block later work, added the current authoring/integration daemon-validation resync tests to the extraction milestone, introduced a focused `run_forever_skips_workspace_busy_head_job_when_filling_capacity` test, and corrected the recovery notes to distinguish agent-backed current-daemon leases from daemon-only `"daemon"` leases.

Revision note: revised again on 2026-03-19 after re-checking the exact `WorkspaceError::Busy` call sites in `prepare_workspace()` and `execute_harness_validation()`. This pass narrowed the workspace-busy guidance from “authoring or integration” to the actual authoring path only, because the integration workspace branch does not use `ensure_authoring_workspace_state()` and therefore does not surface the same `Busy` error.

Revision note: revised again on 2026-03-19 after deep-reading the remaining referenced files and adjacent tick callers. This pass added the missing requirement that `run_forever()` preserve the maintenance, convergence-system-action, and projected-review-recovery work it currently inherits from `tick()`, widened the concrete regression commands to the existing `dispatch`, `escalation`, and `convergence` test binaries, documented that `crates/ingot-domain/src/ports.rs` and existing `DispatcherConfig::new(...)` call sites do not require broader interface churn, and added the `JoinSet` drop-aborts-tasks safety note for background supervisor tests.

Revision note: revised again on 2026-03-19 after auditing `tick()` control flow, `reconcile_startup()` draining, and the exact runtime tests that already cover the touched helper paths. This pass made the non-job helper contract precise enough to preserve `tick()`’s current early-return semantics, kept `tick_system_action()` in scope for startup draining, added the existing broken-project, harness-timeout, and idle-review-recovery tests to the extraction milestone, and made the harness/startup requirements for new `run_forever()` tests concrete.

Revision note: revised again on 2026-03-19 after re-reading the queued-job store query, the daemon-only validation setup block, and the convergence startup tests. This pass made the current `list_queued_jobs(32)` window limitation explicit instead of implying an unbounded starvation fix, specified that the harness-validation extraction must preserve today’s lock-release boundary before running commands, added the exact `reconcile_startup_does_not_spin_when_auto_finalize_is_blocked` regression to the extraction milestone, and warned test authors not to treat `list_active_jobs()` as “running only” because the store currently includes queued and assigned rows there too.

Revision note: revised again on 2026-03-19 after re-reading the daemon-only validation preflight branches and the current job-completion usecase shape. This pass replaced the too-weak `Option<PreparedHarnessValidation>` sketch with an explicit preflight outcome enum that mirrors the existing `PrepareRunOutcome`, so the supervisor plan now preserves the real distinction between “not launchable” and “failed before launch” when validation setup calls `fail_job_preparation(...)`.

Revision note: revised again on 2026-03-19 after re-reading `reconcile_running_job()` and the current daemon-only lease ownership. This pass made the fallback cleanup risk explicit: without supervisor-owned join-error cleanup, daemon-only tasks would currently be marked `Expired` with `heartbeat_expired` purely because `"daemon"` is a foreign lease owner. It also defined the plan’s remaining runtime terms of art (`supervisor`, `preflight`, and `sidecar metadata map`) in plain language so the implementation steps stay self-contained.

Revision note: revised again on 2026-03-19 after another full code-grounded audit before rewriting in place. This pass corrected the remaining stale Tokio version references to the actual `Cargo.lock` pin (`1.50.0`), aligned the new reusable supervisor-test scaffolding with the existing fake-runner pattern in `crates/ingot-agent-runtime/tests/common/mod.rs`, added the already-existing `auto_dispatch` regressions that cover `prepare_run()` harness loading and repo-local skill resolution, and added the one-time `make ui-install` precondition when frontend dependencies are absent.

Revision note: revised again on 2026-03-19 after re-reading `run_with_heartbeats()` and the artifact-writing helpers. This pass added the nested-runner-task safety note the supervisor must respect, named the exact agent-backed artifact filenames the runtime writes today (`prompt.txt`, `stdout.log`, `stderr.log`, `result.json`), and tightened the recovery guidance so test teardown does not get mistaken for proof of normal agent-process cleanup.

Revision note: revised again on 2026-03-19 after checking the existing runtime artifact assertions in tree. This pass added the missing code-grounded requirement to extend `tick_executes_a_queued_authoring_job_and_creates_a_commit()` so it verifies the full agent-backed artifact set (`prompt.txt`, `stdout.log`, `stderr.log`, `result.json`) rather than only `prompt.txt`, because no current test asserts the response artifact files.

Revision note: revised again on 2026-03-19 after re-reading workspace-finalization helpers, join-error cleanup paths, and the existing test harness helpers. This pass made the join-error recovery requirements concrete about repairing workspace rows as well as job rows, clarified that agent-backed panics should reuse `fail_run(...)` while daemon-only panics should conservatively mark workspaces stale, explicitly anchored the completion-wakeup test to “no second notify after release,” and noted that `apps/ingot-daemon/src/main.rs` should pick up the new concurrency default through `DispatcherConfig::new(...)` without extra wiring churn.

Revision note: revised again on 2026-03-19 after checking the pinned Tokio `JoinSet::spawn(...)` signature and the current borrowed helper shapes in `crates/ingot-agent-runtime/src/lib.rs`. This pass made the spawned-helper boundary explicit about needing owned inputs plus a cloned `JobDispatcher` to satisfy the `'static` future requirement, and it tightened completion handling so task-returned `RuntimeError`s trigger the same active-row cleanup check as join panics instead of being treated as log-only failures.

Revision note: revised again on 2026-03-19 after re-checking launch-time ownership against the current `PreparedRun` definition. This pass made the ownership requirement for `running_meta` more concrete by pointing out that `PreparedRun` already derives `Clone`, and by requiring the new `PreparedHarnessValidation` path to make the same ownership choice explicitly instead of leaving the task-versus-metadata split implicit.

Revision note: revised again on 2026-03-19 after re-reading the current `run_forever()` loop and the runtime test suite. This pass added the missing requirement to preserve the daemon’s existing “log and continue” liveness contract across supervisor iterations, and it added a concrete `run_forever_refreshes_heartbeat_while_job_is_running` regression because the current tree exercises timeout and stale-heartbeat expiry but does not directly prove live heartbeat refresh on a supervised running job.

Revision note: revised again on 2026-03-19 after re-auditing queue ordering and the authoring-workspace Busy branch against the current test builders and workspace helper. This pass made the queue-head tests deterministic by requiring explicit `JobBuilder::created_at(...)` values, because `list_queued_jobs(32)` orders only by `created_at` and the builders default every test job to the same timestamp, and it documented the cheaper busy-workspace regression setup the code already supports: insert a Busy authoring workspace row directly, because `ensure_authoring_workspace_state()` returns `WorkspaceError::Busy` before any Git provisioning.

Revision note: revised again on 2026-03-19 after re-auditing the item/revision builders and the SQLite item/revision store APIs. This pass made the stale-head regression executable for a novice by documenting the exact persisted revision-drift recipe the runtime already consumes: create a second revision with `db.create_revision(...)`, then move the item’s `current_revision_id` with `db.update_item(...)` before notifying the dispatcher, instead of vaguely saying “change the item’s current revision.”

Revision note: revised again on 2026-03-19 after re-auditing the supervisor-only branches against adjacent validation and reconciliation tests. This pass added the missing background-loop coverage for daemon-only validation jobs, kept that new regression in `crates/ingot-agent-runtime/tests/auto_dispatch.rs` so it reuses the existing harness/workspace helpers already defined there, and added the exact `reconciliation` regressions that currently define the job/workspace cleanup semantics the new supervisor cleanup paths must preserve.

Revision note: revised again on 2026-03-19 after re-auditing the remaining helper-level tests and daemon-only side effects in `crates/ingot-agent-runtime/src/lib.rs` and `crates/ingot-agent-runtime/tests/auto_dispatch.rs`. This pass added the existing `drain_until_idle_*` unit tests to the first milestone, clarified that the Unix-only exact-test command succeeds with zero matched tests off Unix rather than producing an `... ok` line, showed the concrete cloned-dispatcher spawn pattern for `run_forever()` tests, and made the new supervised daemon-only validation regression assert that agent-style artifacts are still absent.

Revision note: revised again on 2026-03-19 after re-auditing startup setup across the supervisor-adjacent tests. This pass separated the startup guidance for agent-backed versus daemon-only `run_forever()` tests: the agent-backed `crates/ingot-agent-runtime/tests/dispatch.rs` cases still register their mutating agent before `reconcile_startup()`, while the daemon-only validation case may rely on the same empty-registry bootstrap path already exercised in `crates/ingot-agent-runtime/tests/reconciliation.rs`, because the validation path under test never calls `select_agent()`.

Revision note: revised again on 2026-03-19 after re-reading the queue/store helpers, heartbeat writes, and the test harness surface the plan already relies on. This pass added the adjacent store files and test binaries that define the stale-head, busy-workspace, escalation, convergence, reconciliation, and HTTP `tick()` call-site behavior; made the new `run_forever()` test scaffolding concrete about spawning and aborting the background loop plus using explicit waiter helpers instead of sleeps; and tightened the heartbeat regression so it compares against the post-`start_job_execution(...)` baseline heartbeat rather than accidentally passing on the initial `running` transition alone.

Revision note: revised again on 2026-03-19 after re-reading the builder signatures and job-list ordering the plan’s new tests depend on. This pass corrected the queue-order examples to use real `DateTime<Utc>` inputs via the existing `parse_timestamp(...)` test helper, called out that `crates/ingot-agent-runtime/tests/common/mod.rs` does not currently re-export that helper, and warned that any waiter built on `list_jobs_by_project(...)` must treat its filtered results as unordered because the store returns project jobs newest-first rather than queue order.

Revision note: revised again on 2026-03-19 after another deep read of the current runtime and adjacent test helpers. This pass added the missing requirement to keep `execute_harness_validation()` as the synchronous wrapper that `tick()` already calls, grounded supervisor cleanup and daemon-validation prepared state against the exact `reconcile_assigned_job()` / `reconcile_running_job()` behavior in `crates/ingot-agent-runtime/src/lib.rs`, replaced the remaining shorthand file references that would mislead a novice about actual paths, made the new dispatch-side supervisor tests reuse `test_authoring_job(...)` and `register_mutating_agent()`, and explicitly flagged that non-`Busy` preparation failures remain on the daemon’s existing log-and-continue path instead of becoming new queue-scan skip cases.

Revision note: revised again on 2026-03-19 after re-auditing the current test-support builders, store helpers, and runtime test binaries. This pass tightened the plan around the missing concurrent-test scaffolding that the current tree still lacks: it now standardizes the new supervisor tests on one shared `BlockingRunner` in `crates/ingot-agent-runtime/tests/common/mod.rs`, standardizes timestamped queue-order setup on a `parse_timestamp(...)` re-export from that same module, switches the `drain_until_idle_` command to the exact `--lib` target that currently owns those unit tests, and adds the read-only references to `crates/ingot-workspace/src/lib.rs` plus the domain test-support builder files whose real methods (`created_at`, `revision_no`, `current_job_id`, `status`) the plan already depends on.
