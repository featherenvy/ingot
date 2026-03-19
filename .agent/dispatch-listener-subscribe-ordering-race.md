# Close dispatch-listener subscribe-after-start race

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows `.agent/PLANS.md` and must be maintained in accordance with that file.

## Purpose / Big Picture

After this change, cancellation wakeups will cover the full pre-launch lifetime of runtime work instead of starting after the runtime has already crossed into “running but not yet listening.” Today, a `/cancel` that lands after `start_job_execution(...)` but before `run_with_heartbeats()` subscribes, or after `run_prepared_harness_validation()` finishes its caller-side cancellation check but before `run_harness_command_with_heartbeats()` spawns the shell, still waits for the heartbeat loop. After this fix, those windows will close: an agent-backed job will either fail before `tokio::spawn(...)` launches the runner, or a daemon-only validation command will fail before `command.spawn()` launches the shell. You will be able to see the fix by running focused runtime tests that cancel exactly in those windows and observing that the agent runner launch count stays at zero and the harness command never creates its marker file.

## Progress

- [x] (2026-03-19 19:37Z) Re-read `.agent/PLANS.md`, re-read the final notifier implementation in `crates/ingot-usecases/src/notify.rs`, and confirmed that the wait-side API is now `subscribe()` only.
- [x] (2026-03-19 19:37Z) Deep-read the current subscribe sites in `crates/ingot-agent-runtime/src/lib.rs` and confirmed the remaining race: `run_with_heartbeats()` subscribes after `start_job_execution(...)` and after `tokio::spawn(...)`, and `run_harness_command_with_heartbeats()` subscribes after `command.spawn()`.
- [x] (2026-03-19 19:37Z) Re-read the current runtime regressions in `crates/ingot-agent-runtime/tests/dispatch.rs` and `crates/ingot-agent-runtime/tests/auto_dispatch.rs`, and confirmed they only prove prompt cancellation after the runner or shell has already started.
- [x] (2026-03-19 19:44Z) Deep-read `prepare_run()`, `prepare_harness_validation()`, and `JobDispatcher::with_runner(...)`, and confirmed two planning constraints: daemon-only validation flips the job to `Running` inside `prepare_harness_validation()` before `run_harness_command_with_heartbeats()` is entered, and any deterministic test seam must not change `JobDispatcher::new(...)` or `JobDispatcher::with_runner(...)` because those constructors are used across the runtime integration tests and the daemon binary.
- [x] (2026-03-19 19:51Z) Deep-read the adjacent state-mutating paths in `crates/ingot-store-sqlite/src/store/job.rs`, `crates/ingot-usecases/src/job_lifecycle.rs`, `crates/ingot-http-api/src/router/jobs.rs`, `crates/ingot-http-api/src/router/mod.rs`, and `crates/ingot-agent-runtime/tests/reconciliation.rs`, and updated this plan to name the actual invariant-bearing fields, both `tick()` and `run_forever()` caller paths, and the nearby lease-recovery regression that must stay green.
- [x] (2026-03-19 20:12Z) Re-audited auxiliary terminal-state writers and adjacent invariant tests. This pass confirmed that `crates/ingot-usecases/src/teardown.rs::teardown_revision_lane(...)` is another cancellation path reached from `crates/ingot-http-api/src/router/items.rs` and `crates/ingot-http-api/src/router/convergence.rs`, and that the adjacent store/usecase regression surface already includes `finish_job_non_success_rolls_back_when_item_revision_changes_before_commit`, `start_job_execution_rejects_jobs_without_workspace_binding`, and `teardown_cancels_active_jobs_for_revision`.
- [x] (2026-03-19 20:24Z) Re-read the execution-parameter definitions and the harness test setup. This pass confirmed that `StartJobExecutionParams` still includes `process_pid` in `crates/ingot-domain/src/ports.rs`, and that any new `src/lib.rs` daemon-only unit test must create `project_path/.ingot/harness.toml` because `load_harness_profile(...)` reads that exact path.
- [x] (2026-03-19 20:21Z) Moved listener creation earlier in `crates/ingot-agent-runtime/src/lib.rs::run_with_heartbeats()` so the listener now exists before `start_job_execution(...)` and before the agent launch window opens.
- [x] (2026-03-19 20:21Z) Added an explicit post-start, pre-spawn cancellation check in `crates/ingot-agent-runtime/src/lib.rs::run_with_heartbeats()` so a cancelled job returns the existing `"job cancelled"` error before `tokio::spawn(...)` launches the runner.
- [x] (2026-03-19 20:21Z) Updated `crates/ingot-agent-runtime/src/lib.rs::run_harness_command_with_heartbeats()` to subscribe before any spawn attempt and to return a cancelled `HarnessCommandResult` when the new direct pre-spawn reload sees a cancelled job, while leaving `prepare_harness_validation()` and the caller-side `harness_validation_cancelled(...)` guard unchanged.
- [x] (2026-03-19 20:21Z) Added deterministic pre-launch regressions in `crates/ingot-agent-runtime/src/lib.rs` using a private clone-safe `#[cfg(test)]` pause hook stored on `JobDispatcher`, plus local unit-test setup that mirrors the runtime integration-test launch-counting and harness-file patterns without changing public constructors.
- [x] (2026-03-19 20:21Z) Ran the focused lib, integration, store/usecase, reconciliation, HTTP, and daemon validation commands; all targeted checks passed, and this ExecPlan now records the actual module-qualified cargo commands needed for exact unit-test execution.

## Surprises & Discoveries

- Observation: `DispatchNotify` itself is no longer the problem. The remaining hole is entirely about where the listener is created.
  Evidence: `crates/ingot-usecases/src/notify.rs` now exposes only `notify()` plus `subscribe()`, and each listener waits with `watch::Receiver::changed()`, which only observes future notifications.

- Observation: the agent-backed path currently marks the job running before it creates the listener, and it also spawns the runner before subscribing.
  Evidence: in `crates/ingot-agent-runtime/src/lib.rs`, `run_with_heartbeats()` calls `start_job_execution(...)`, then `tokio::spawn(...)`, and only then assigns `let mut dispatch_listener = self.dispatch_notify.subscribe();`.

- Observation: the daemon-only validation path is split across two functions, and the job is already `Running` before `run_harness_command_with_heartbeats()` is entered.
  Evidence: `prepare_harness_validation()` calls `start_job_execution(...)` before returning `PreparedHarnessValidation`, while `run_prepared_harness_validation()` only later loops over commands and calls `run_harness_command_with_heartbeats(...)`.

- Observation: the remaining daemon-only blind spot is narrower than the agent blind spot because one useful guard already exists before each command launch.
  Evidence: `run_prepared_harness_validation()` calls `harness_validation_cancelled(...)` immediately before each `run_harness_command_with_heartbeats(...)` call, so the missing window is specifically between that caller-side check and `command.spawn()`.

- Observation: the runtime already has two entry modes, but both of them converge on the same private launch helpers.
  Evidence: `tick()` calls `run_with_heartbeats()` directly for agent jobs and `execute_harness_validation()` for daemon-only validation, while `run_forever()` reaches the same helpers through `launch_supervised_jobs()`, `run_prepared_agent_job(...)`, and `run_prepared_harness_validation_job(...)`.

- Observation: the state guard that prevents stale work is not a new field invented for this fix; it is the existing `job.item_revision_id`, threaded as `expected_item_revision_id` through every durable state mutation.
  Evidence: `start_job_execution(...)`, `heartbeat_job_execution(...)`, `job_lifecycle::cancel_job(...)`, `job_lifecycle::expire_job(...)`, `reconcile_running_job(...)`, and the harness cleanup path all pass `expected_item_revision_id` into guarded store writes.

- Observation: lease recovery is adjacent to this bug even though it is not the bug itself.
  Evidence: `start_job_execution(...)` writes `lease_owner_id` and `lease_expires_at`, `heartbeat_job_execution(...)` refreshes them, and `reconcile_running_job(...)` expires running jobs whose lease is stale or owned by another daemon. `crates/ingot-agent-runtime/tests/reconciliation.rs::reconcile_startup_expires_stale_running_jobs_and_marks_workspace_stale` is the nearby proof.

- Observation: `/cancel` is not the only way an active job can become terminal before launch. Revision-lane teardown also cancels active jobs on item-mutation paths, and `/fail` plus `/expire` can also terminalize the same row.
  Evidence: `crates/ingot-http-api/src/router/jobs.rs` exposes `cancel_item_job()`, `fail_job()`, and `expire_job()`, all of which end in guarded `finish_non_success(...)` writes; `crates/ingot-http-api/src/router/mod.rs::teardown_revision_lane_state()` calls `crates/ingot-usecases/src/teardown.rs::teardown_revision_lane(...)`, and `crates/ingot-http-api/src/router/items.rs::{revise_item,defer_item,finish_item_manually}` plus `crates/ingot-http-api/src/router/convergence.rs::teardown_reject_approval()` all route through that teardown helper.

- Observation: the existing integration helpers cannot deterministically stop in the pre-launch window.
  Evidence: `crates/ingot-agent-runtime/tests/common/mod.rs::BlockingRunner` increments its `launches` counter inside `AgentRunner::launch(...)`, which is already after `tokio::spawn(...)`; there is no helper that pauses between `start_job_execution(...)` and the new direct reload, or between the caller-side harness cancellation check and the callee-side spawn boundary.

- Observation: constructor churn would create unnecessary collateral changes for this test-only fix.
  Evidence: `JobDispatcher::with_runner(...)` is called from `crates/ingot-agent-runtime/tests/common/mod.rs`, multiple runtime integration tests, `crates/ingot-agent-runtime/tests/reconciliation.rs`, and `apps/ingot-daemon/src/main.rs`, so a deterministic test seam should default internally instead of changing constructor signatures.

- Observation: the harness caller currently logs every cancelled command as “while harness command was running,” even though the new pre-spawn path can cancel before launch.
  Evidence: `run_prepared_harness_validation()` logs that exact message whenever `HarnessCommandResult.cancelled` is `true`, and `HarnessCommandResult` currently has only a single `cancelled` flag with no “spawned” discriminator.

- Observation: the unchanged running-state parameter contract is slightly wider than the earlier draft of this plan said.
  Evidence: `crates/ingot-domain/src/ports.rs::StartJobExecutionParams` includes `process_pid` in addition to `job_id`, `item_id`, `expected_item_revision_id`, `workspace_id`, `agent_id`, `lease_owner_id`, and `lease_expires_at`, and both current runtime launch paths pass `process_pid: None`.

- Observation: if the new daemon-only unit test reuses `prepare_harness_validation(...)`, it must create a real harness profile file with at least one command, because the runtime defaults a missing file to an empty profile.
  Evidence: `crates/ingot-agent-runtime/src/lib.rs::load_harness_profile(...)` wraps `read_harness_profile_if_present(...)`, and `crates/ingot-domain/src/harness.rs::HarnessProfile::default()` contains no commands. The existing integration helpers in `crates/ingot-agent-runtime/tests/auto_dispatch.rs` therefore create `project_path/.ingot/harness.toml` before daemon-only validation tests call into the runtime.

- Observation: the repo already has concrete test patterns for both launch-counting and harness-profile setup, even though those helpers live in integration-test modules that unit tests cannot import.
  Evidence: `crates/ingot-agent-runtime/tests/common/mod.rs::BlockingRunner` uses `Arc<Mutex<_>>` plus paired `tokio::sync::Notify` handles to count launches and coordinate a pause/release handshake, and `crates/ingot-agent-runtime/tests/auto_dispatch.rs::write_harness_toml(...)` shows the exact `.ingot/harness.toml` creation pattern the daemon-only tests already rely on.

- Observation: cargo's bare test-name filter was not sufficient for the crate-internal unit tests when `--exact` was enabled.
  Evidence: the first bare-name runs of `cargo test -p ingot-agent-runtime --lib run_with_heartbeats_does_not_launch_runner_when_job_is_cancelled_before_spawn -- --exact`, `cargo test -p ingot-store-sqlite finish_job_non_success_rolls_back_when_item_revision_changes_before_commit -- --exact`, and `cargo test -p ingot-usecases teardown_cancels_active_jobs_for_revision -- --exact` each built successfully but reported `running 0 tests`; `cargo test -- --list` showed the real paths as `tests::...`, `store::job::tests::...`, and `teardown::tests::...`.

## Decision Log

- Decision: fix this as an ordering bug inside the runtime helpers, not by altering heartbeat timing or notifier semantics again.
  Rationale: the notifier is already broadcast-safe. The remaining missed-cancel case is caused by listener creation occurring after the job becomes externally running work.
  Date/Author: 2026-03-19 / Codex

- Decision: require an explicit direct cancellation reload in both execution paths after the vulnerable transition and before the actual spawn.
  Rationale: subscribing earlier closes the missed-notification hole, but a cancellation that lands after the running-state transition and before the actual runner or child process launch still needs a direct state check to avoid launching work unnecessarily.
  Date/Author: 2026-03-19 / Codex

- Decision: keep `start_job_execution(...)` exactly where it already defines the runtime lease for each flow.
  Rationale: `start_job_execution(...)` is the write that sets `status = running`, `lease_owner_id`, `heartbeat_at`, and `lease_expires_at`. Moving it would change how `reconcile_running_job(...)` reasons about stale running work. This plan changes listener and reload ordering around that write, not the lease lifecycle itself.
  Date/Author: 2026-03-19 / Codex

- Decision: keep `prepare_harness_validation()` as the place that calls `start_job_execution(...)` for daemon-only validation.
  Rationale: the code already has a caller-side `harness_validation_cancelled(...)` guard in `run_prepared_harness_validation()`, so the concrete remaining bug is the subscribe-and-spawn ordering inside `run_harness_command_with_heartbeats()`. Moving the running-state transition would be a broader lifecycle change that is not required by the failure mode found in the code.
  Date/Author: 2026-03-19 / Codex

- Decision: place the new deterministic proofs in `crates/ingot-agent-runtime/src/lib.rs` unit tests behind a private `#[cfg(test)]` pause hook rather than trying to force them into the existing integration-test harness.
  Rationale: the runtime integration tests do not have a stable pause point before launch, while `src/lib.rs` already has a `#[cfg(test)] mod tests` section that can access private functions and private fields. A private, default-off hook keeps constructor signatures stable for the rest of the codebase.
  Date/Author: 2026-03-19 / Codex

- Decision: pause before the new direct reloads, not after them.
  Rationale: the feature being proved is “a cancellation that lands inside the vulnerable window is seen by the new direct reload and prevents spawn.” A hook placed after the reload would miss the very race this plan is supposed to close.
  Date/Author: 2026-03-19 / Codex

- Decision: do not enlarge `HarnessCommandResult` just to improve one log line unless implementation proves that distinction is needed for correctness.
  Rationale: the existing behavioral contract is `cancelled = true` plus “no child keeps running.” The observed wording mismatch is worth noting, but it is secondary to closing the launch race and is not, by itself, a reason to expand the runtime result shape.
  Date/Author: 2026-03-19 / Codex

- Decision: have the new unit tests obtain `PreparedRun` and `PreparedHarnessValidation` through the real `prepare_run()` and `prepare_harness_validation()` helpers instead of fabricating those structs by hand.
  Rationale: both prepared structs carry state that is easy to drift from reality, especially the daemon-only path where `prepare_harness_validation()` already performs the running-state transition and sets the lease. Reusing the real prepare helpers keeps the proof aligned with the lifecycle this fix is changing.
  Date/Author: 2026-03-19 / Codex

- Decision: if the deterministic pause seam is stored on `JobDispatcher`, keep it clone-safe and default-off.
  Rationale: `JobDispatcher` derives `Clone`, and both supervised execution helpers run on dispatcher clones. A test-only seam that breaks `Clone` or that is not shared across clones would create implementation churn unrelated to the race being fixed.
  Date/Author: 2026-03-19 / Codex

- Decision: have the new deterministic tests cancel jobs through `ingot_usecases::job_lifecycle::cancel_job(...)`, not by directly editing the row.
  Rationale: this fix is guarding the real cancellation writer that the HTTP route and runtime integration tests already use. Reusing that usecase keeps the proof aligned with `expected_item_revision_id`, workspace release, and activity side effects instead of creating a shortcut that bypasses the lifecycle contract.
  Date/Author: 2026-03-19 / Codex

- Decision: record the exact validation commands with module-qualified unit-test paths rather than preserving the earlier bare-name forms.
  Rationale: the implementation landed in crate-internal unit-test modules, and cargo required fully qualified test paths for `--exact` to execute those tests instead of filtering everything out. The ExecPlan should prefer reproducible commands over aspirational ones.
  Date/Author: 2026-03-19 / Codex

## Outcomes & Retrospective

The plan is now implemented. `run_with_heartbeats()` subscribes before `start_job_execution(...)`, performs a direct cancellation reload before `tokio::spawn(...)`, and returns the existing `"job cancelled"` process error without launching the runner when the job is cancelled in that window. `run_harness_command_with_heartbeats()` now does the equivalent work for daemon-only validation commands: it subscribes before the spawn boundary, re-reads job state immediately before `command.spawn()`, and returns a cancelled `HarnessCommandResult` without creating the child process when cancellation lands in the caller-check-to-spawn gap.

The private `#[cfg(test)]` pause hook on `JobDispatcher` turned out to be enough to prove both boundaries without touching public constructors or the supervised runtime call sites. The new unit tests use the real `prepare_run(...)`, `prepare_harness_validation(...)`, and `job_lifecycle::cancel_job(...)` paths, so they observe the persisted cancelled job plus released workspace before the hook is released. The broader runtime, reconciliation, HTTP, store/usecase, and daemon checks all stayed green, which means the fix remained an observation-ordering change instead of turning into a lease or lifecycle rewrite.

## Context and Orientation

`crates/ingot-usecases/src/notify.rs` defines `DispatchNotify`, a shared wakeup primitive built on `tokio::sync::watch`. In plain language, each waiter must call `DispatchNotify::subscribe()` to obtain its own future notifications; a listener does not replay past notifications. That is why listener creation order matters.

`apps/ingot-daemon/src/main.rs` creates one `DispatchNotify` and passes the same clone to the runtime and to the HTTP router. `crates/ingot-http-api/src/router/mod.rs::dispatch_notify_layer()` calls `dispatch_notify.notify()` after every successful write request, and `crates/ingot-http-api/src/router/jobs.rs::cancel_item_job()` performs cancellation through `ingot_usecases::job_lifecycle::cancel_job(...)`. The HTTP path is already correct for this feature: the bug is that the runtime sometimes subscribes too late to hear the notification.

The invariant-bearing fields for this feature already exist in the job lifecycle and must remain unchanged. `job.item_revision_id` is the stale-work guard. The store receives it as `expected_item_revision_id` in `crates/ingot-store-sqlite/src/store/job.rs::start_job_execution(...)` and `heartbeat_job_execution(...)`, and the termination paths reuse the same guard through `job_lifecycle::cancel_job(...)`, `job_lifecycle::expire_job(...)`, `JobDispatcher::reconcile_running_job(...)`, and the harness branch of `cleanup_supervised_task(...)`. `lease_owner_id` and `lease_expires_at` are the runtime-recovery guard: `start_job_execution(...)` creates the lease, the heartbeat helpers refresh it, and `reconcile_running_job(...)` expires stale or foreign-owned running jobs. `workspace_id` is also part of the invariant surface because `start_job_execution(...)` rejects execution without a workspace binding, while cancellation and cleanup paths release or mark that workspace. This plan must not introduce any new write path that bypasses those guards.

The parameter structs that carry those guards are defined in `crates/ingot-domain/src/ports.rs`, even though the guarded SQL updates live in `crates/ingot-store-sqlite/src/store/job.rs`. That matters here because the launch helpers should keep the entire `StartJobExecutionParams` contract unchanged, including `process_pid`. Both current launch helpers pass `process_pid: None`, and this fix should leave that behavior alone.

`crates/ingot-domain/src/harness.rs` defines the daemon-only harness types that `prepare_harness_validation()` carries forward. `HarnessProfile` is the parsed contents of `<project>/.ingot/harness.toml`, `HarnessCommand` is one named shell command plus timeout, and `HarnessProfile::default()` is an empty profile. In this repository that means a daemon-only unit test only needs to write `.ingot/harness.toml` if it wants `prepare_harness_validation()` to yield a real command to pass into `run_harness_command_with_heartbeats()`. If the file is absent, runtime loading succeeds but produces zero commands.

Active jobs become terminal through more than one writer family, and this plan needs to acknowledge all of them even though the fix itself stays local to the launch helpers. The HTTP job routes in `crates/ingot-http-api/src/router/jobs.rs` can cancel, fail, or expire an active job through `job_lifecycle::{cancel_job,fail_job,expire_job}`. Runtime-owned cleanup paths in `crates/ingot-agent-runtime/src/lib.rs` can also terminalize the same row through `fail_run(...)`, `fail_job_preparation(...)`, `cleanup_supervised_task(...)`, and `reconcile_running_job(...)`. Separate item-mutation paths also cancel active jobs through `crates/ingot-usecases/src/teardown.rs::teardown_revision_lane(...)`, which is reached from `crates/ingot-http-api/src/router/items.rs::{revise_item,defer_item,finish_item_manually}` and `crates/ingot-http-api/src/router/convergence.rs::teardown_reject_approval()`. This fix must remain an observation-side change in `run_with_heartbeats()` and `run_harness_command_with_heartbeats()`, not a rewrite of those existing guarded writers.

`crates/ingot-agent-runtime/src/lib.rs` has two launch families and two caller modes. The caller modes are `tick()`, which runs work inline, and `run_forever()`, which supervises spawned tasks. Those two caller modes both converge on the same private helpers, so the bug fix belongs in the helpers, not in the loop wrappers. Agent-backed jobs flow through `prepare_run()` and then `run_with_heartbeats()`. Daemon-only validation jobs flow through `prepare_harness_validation()`, then `run_prepared_harness_validation()`, then `run_harness_command_with_heartbeats()`. `prepare_harness_validation()` already moves the job into `Running`; `run_prepared_harness_validation()` already performs a caller-side `harness_validation_cancelled(...)` check before each command; and `run_harness_command_with_heartbeats()` is still responsible for the final spawn boundary.

The existing integration tests are still valuable, but they cover only post-launch behavior. `crates/ingot-agent-runtime/tests/dispatch.rs::run_forever_starts_next_job_after_running_job_cancellation` proves that the dispatcher wakes promptly once an agent job has already launched and occupied the only permit. `crates/ingot-agent-runtime/tests/auto_dispatch.rs::run_forever_cancels_daemon_only_validation_command` proves prompt cancellation after a daemon-only shell command has already started writing its marker file. `crates/ingot-agent-runtime/tests/reconciliation.rs::reconcile_startup_expires_stale_running_jobs_and_marks_workspace_stale` proves that startup recovery still trusts the lease fields written by `start_job_execution(...)`. `crates/ingot-http-api/tests/job_routes.rs::cancel_route_marks_active_job_cancelled_and_clears_workspace_attachment` proves the HTTP cancellation path still mutates state correctly. None of those tests close the pre-launch race on their own, but all of them must stay green.

## Milestones

### Milestone 1: Close the agent-backed pre-launch window in `run_with_heartbeats()`

At the end of this milestone, `crates/ingot-agent-runtime/src/lib.rs::run_with_heartbeats()` will subscribe before `start_job_execution(...)`. After `start_job_execution(...)` succeeds, it will pause only in tests, then reload the job directly from the database, and only then decide whether to call `tokio::spawn(...)`. If the job was cancelled in that gap, the function will return the same `AgentError::ProcessError("job cancelled".into())` shape that the existing notification and heartbeat branches already use, and the runner will not launch. The proof is a deterministic unit test in `crates/ingot-agent-runtime/src/lib.rs` whose launch counter stays at zero.

### Milestone 2: Close the daemon-only command-launch window without changing harness lifecycle ownership

At the end of this milestone, `prepare_harness_validation()` will still be the place that flips daemon-only validation jobs into `Running`, and `run_prepared_harness_validation()` will still perform its direct `harness_validation_cancelled(...)` check before each command. The change will be local to `run_harness_command_with_heartbeats()`: subscribe before any spawn attempt, pause only in tests after the function has entered the vulnerable boundary but before its new local job reload, then re-read job state immediately before `command.spawn()`. If the job is already cancelled in that narrow window, return a cancelled `HarnessCommandResult` without spawning the child. The proof is a deterministic unit test in `crates/ingot-agent-runtime/src/lib.rs` whose marker file never appears.

### Milestone 3: Keep the surrounding lifecycle and recovery behavior intact

At the end of this milestone, the new pre-launch unit tests will pass, the existing post-launch integration tests will still pass, the nearby running-job reconciliation regression will still pass, the HTTP cancel-route regression will still pass, and `cargo check -p ingot-daemon` will still compile the shared wiring. The proof is the exact command list in the validation section.

## Plan of Work

Start in `crates/ingot-agent-runtime/src/lib.rs::run_with_heartbeats()`. Move `let mut dispatch_listener = self.dispatch_notify.subscribe();` above `start_job_execution(...)`. Leave the `StartJobExecutionParams` payload unchanged so that `job_id`, `item_id`, `expected_item_revision_id`, `workspace_id`, `agent_id`, `lease_owner_id`, `process_pid`, and `lease_expires_at` still define the running-state transition contract. Immediately after `start_job_execution(...)` succeeds, add a new direct reload of `self.db.get_job(prepared.job.id)`. If that row is already `Cancelled`, return `Err(AgentError::ProcessError("job cancelled".into()))` before `tokio::spawn(...)`. Reuse the existing cancellation error shape instead of inventing a new one so that both `tick()` and `execute_prepared_agent_job()` continue to recognize cancellation through the same downstream status reload they already use.

Then update `crates/ingot-agent-runtime/src/lib.rs::run_harness_command_with_heartbeats()`. Create the dispatch listener before any spawn attempt. Add a new direct reload of `self.db.get_job(prepared.job_id)` immediately before `command.spawn()`. If that row is already `Cancelled`, return a cancelled `HarnessCommandResult` without ever spawning the child. Keep the assertion surface behavioral rather than stringly: `cancelled = true`, `timed_out = false`, no marker file is created, and no child process launches. Do not require `stderr_tail` to be empty, because `build_harness_command_result(...)` legitimately synthesizes note text such as `"command cancelled"` even when no command output exists. Do not move `start_job_execution(...)` out of `prepare_harness_validation()`, and do not remove the existing `harness_validation_cancelled(...)` check in `run_prepared_harness_validation()`. Those are already the current lifecycle boundaries for daemon-only validation and are not the source of the remaining missed notification.

After the runtime ordering changes, add a narrow deterministic pause hook in `crates/ingot-agent-runtime/src/lib.rs` under `#[cfg(test)]`. Keep it private to this crate, clone-safe if it is stored on `JobDispatcher`, and default it to “disabled” so production builds and existing constructor call sites are unaffected. The agent hook must pause after `start_job_execution(...)` has succeeded and after the listener already exists, but before the new direct `self.db.get_job(prepared.job.id)` reload and before `tokio::spawn(...)`. The harness hook must pause after `run_prepared_harness_validation()` has already decided to enter `run_harness_command_with_heartbeats()` and after the callee listener already exists, but before the new direct `self.db.get_job(prepared.job_id)` reload and before `command.spawn()`. Because the unit tests live in the same module as `JobDispatcher`, they can set a private `#[cfg(test)]` field or helper after constructing the dispatcher; there is no need to widen `JobDispatcher::new(...)` or `JobDispatcher::with_runner(...)`.

Use the existing `#[cfg(test)] mod tests` at the bottom of `crates/ingot-agent-runtime/src/lib.rs` for the new deterministic regressions. Integration-test helpers under `crates/ingot-agent-runtime/tests/common/mod.rs` are not importable from unit tests, so recreate only the minimal setup needed there by using the same dev-dependency fixtures already in this crate: `ingot_test_support::sqlite::migrated_test_db`, the fixture builders from `ingot_test_support::fixtures`, and the temp git helpers from `ingot_test_support::git::{temp_git_repo, unique_temp_path}`. Mirror the existing integration-test patterns instead of inventing new ones: copy the `.ingot/harness.toml` creation approach from `crates/ingot-agent-runtime/tests/auto_dispatch.rs::write_harness_toml(...)`, and mirror `crates/ingot-agent-runtime/tests/common/mod.rs::BlockingRunner`'s `Arc<Mutex<_>>` plus `tokio::sync::Notify` handshake locally for the agent launch counter and pause coordination. Because `load_harness_profile(...)` defaults a missing file to `HarnessProfile::default()`, the daemon-only unit test must write `project_path/.ingot/harness.toml` before calling `prepare_harness_validation(...)` so that the prepared value contains a real command. Use the real `prepare_run(...)` and `prepare_harness_validation(...)` helpers in those tests to obtain the prepared values rather than fabricating `PreparedRun` or `PreparedHarnessValidation` directly. Add these exact tests:

- `run_with_heartbeats_does_not_launch_runner_when_job_is_cancelled_before_spawn`
- `run_harness_command_with_heartbeats_does_not_spawn_command_when_job_is_cancelled_before_spawn`

The first test should install the agent prelaunch hook, use `prepare_run(...)` to obtain a real `PreparedRun`, start `run_with_heartbeats()` on that prepared job, wait until the hook proves that `start_job_execution(...)` has completed and the function is paused before its new direct reload, then cancel the job through `ingot_usecases::job_lifecycle::cancel_job(...)` using the real `Job`, `Item`, and `WorkspaceStatus::Ready` path. After that cancellation call, reload the persisted job and workspace and assert that the real lifecycle side effects already happened: the job row is `Cancelled` and the workspace attachment is cleared. Optionally call `dispatch_notify.notify()`, release the hook, and assert that the runner launch counter is still zero and the returned error is the existing `"job cancelled"` process error. The second test should install the harness pre-spawn hook, use `prepare_harness_validation(...)` to obtain a real `PreparedHarnessValidation`, read the command from `prepared.harness.commands`, enter `run_harness_command_with_heartbeats()` for a prepared validation command that would create a marker file if spawned, wait until the hook proves the function is paused before its new direct reload, then cancel the job through `job_lifecycle::cancel_job(...)` while the hook is holding that boundary. Reload the persisted job and workspace there as well so the test proves it used the real guarded writer, not an in-memory shortcut. Release the hook and assert that the returned `HarnessCommandResult` has `cancelled = true` and that the marker file does not exist. Calling `dispatch_notify.notify()` in either test is acceptable but not required for correctness because the direct pre-spawn reload, not a later wait loop, is what must stop launch there.

Leave the existing integration tests in `crates/ingot-agent-runtime/tests/dispatch.rs` and `crates/ingot-agent-runtime/tests/auto_dispatch.rs` in place as the post-launch regression net. They already prove the dispatcher wakes promptly once work has started. Also rerun the nearby reconciliation regression in `crates/ingot-agent-runtime/tests/reconciliation.rs` because this change still depends on `start_job_execution(...)` preserving the current lease semantics. If the harness caller log message remains slightly imprecise for the new pre-spawn cancellation case, record that fact in the plan’s outcomes or artifacts rather than broadening the result type without a correctness reason.

## Concrete Steps

From `/Users/aa/Documents/ingot`, implement in this order:

1. Edit `crates/ingot-agent-runtime/src/lib.rs::run_with_heartbeats()`:
   - move `self.dispatch_notify.subscribe()` above `start_job_execution(...)`;
   - leave the existing `StartJobExecutionParams` fields unchanged, including `process_pid`;
   - add a direct job reload after `start_job_execution(...)` and before `tokio::spawn(...)`;
   - if the reloaded job is `Cancelled`, return `Err(AgentError::ProcessError("job cancelled".into()))`.
2. Edit `crates/ingot-agent-runtime/src/lib.rs::run_harness_command_with_heartbeats()`:
   - move `self.dispatch_notify.subscribe()` above any spawn attempt;
   - add a direct job reload immediately before `command.spawn()`;
   - if the reloaded job is `Cancelled`, return a cancelled `HarnessCommandResult` without spawning the child.
3. Add a private `#[cfg(test)]` pause hook in `crates/ingot-agent-runtime/src/lib.rs`:
   - keep it default-off;
   - if it lives on `JobDispatcher`, keep it clone-safe so `#[derive(Clone)]` and supervised dispatcher clones still work;
   - do not change `JobDispatcher::new(...)` or `JobDispatcher::with_runner(...)`;
   - invoke the agent hook after `start_job_execution(...)` and before the new direct reload;
   - invoke the harness hook after the callee listener exists and before the new direct reload.
4. Add the new unit tests in `crates/ingot-agent-runtime/src/lib.rs` with these exact names:
   - `run_with_heartbeats_does_not_launch_runner_when_job_is_cancelled_before_spawn`
   - `run_harness_command_with_heartbeats_does_not_spawn_command_when_job_is_cancelled_before_spawn`
   - prepare the runtime state through the real `prepare_run(...)` and `prepare_harness_validation(...)` helpers instead of hand-constructing private prepared structs;
   - mirror `tests/common/mod.rs::BlockingRunner` locally with `Arc<Mutex<_>>` launch state plus `tokio::sync::Notify` so the lib test can count launches and coordinate the pause hook without importing integration-test modules;
   - for the daemon-only test, create `.ingot/harness.toml` with the same pattern as `tests/auto_dispatch.rs::write_harness_toml(...)`, then read the command back from `prepared.harness.commands` after `prepare_harness_validation(...)`;
   - cancel through `ingot_usecases::job_lifecycle::cancel_job(...)`, not a direct row mutation;
   - reload the persisted job and workspace after cancellation and assert that `status == Cancelled` and `current_job_id() == None` before releasing the pause hook;
   - use a local unit-test runner that counts launches, because `tests/common/mod.rs::BlockingRunner` is not importable from `src/lib.rs`.
5. Re-run the focused unit tests first, then the existing exact-name integration regressions, then the adjacent reconciliation regression, then the broader runtime test binaries, then the HTTP exact test and daemon compile check.

Run these commands as you validate:

    cd /Users/aa/Documents/ingot
    cargo test -p ingot-agent-runtime --lib tests::run_with_heartbeats_does_not_launch_runner_when_job_is_cancelled_before_spawn -- --exact
    cargo test -p ingot-agent-runtime --lib tests::run_harness_command_with_heartbeats_does_not_spawn_command_when_job_is_cancelled_before_spawn -- --exact
    cargo test -p ingot-store-sqlite store::job::tests::finish_job_non_success_rolls_back_when_item_revision_changes_before_commit -- --exact
    cargo test -p ingot-store-sqlite store::job::tests::start_job_execution_rejects_jobs_without_workspace_binding -- --exact
    cargo test -p ingot-usecases teardown::tests::teardown_cancels_active_jobs_for_revision -- --exact
    cargo test -p ingot-agent-runtime --test dispatch run_forever_starts_next_job_after_running_job_cancellation -- --exact
    cargo test -p ingot-agent-runtime --test auto_dispatch run_forever_cancels_daemon_only_validation_command -- --exact
    cargo test -p ingot-agent-runtime --test reconciliation reconcile_startup_expires_stale_running_jobs_and_marks_workspace_stale -- --exact
    cargo test -p ingot-agent-runtime --test dispatch run_forever_starts_next_job_on_joinset_completion -- --exact
    cargo test -p ingot-agent-runtime --test dispatch run_forever_refreshes_heartbeat_while_job_is_running -- --exact
    cargo test -p ingot-agent-runtime --test auto_dispatch run_forever_refreshes_heartbeat_for_daemon_only_validation_job -- --exact
    cargo test -p ingot-agent-runtime --lib
    cargo test -p ingot-agent-runtime --test auto_dispatch
    cargo test -p ingot-agent-runtime --test dispatch
    cargo test -p ingot-agent-runtime --test reconciliation
    cargo test -p ingot-http-api --test job_routes cancel_route_marks_active_job_cancelled_and_clears_workspace_attachment -- --exact
    cargo check -p ingot-daemon

Expected focused-test output shape is:

    test tests::run_with_heartbeats_does_not_launch_runner_when_job_is_cancelled_before_spawn ... ok
    test tests::run_harness_command_with_heartbeats_does_not_spawn_command_when_job_is_cancelled_before_spawn ... ok
    test store::job::tests::finish_job_non_success_rolls_back_when_item_revision_changes_before_commit ... ok
    test store::job::tests::start_job_execution_rejects_jobs_without_workspace_binding ... ok
    test teardown::tests::teardown_cancels_active_jobs_for_revision ... ok
    test run_forever_starts_next_job_after_running_job_cancellation ... ok
    test run_forever_cancels_daemon_only_validation_command ... ok
    test reconcile_startup_expires_stale_running_jobs_and_marks_workspace_stale ... ok
    test cancel_route_marks_active_job_cancelled_and_clears_workspace_attachment ... ok

## Validation and Acceptance

Acceptance is reached when all of the following are true:

1. In the agent-backed path, there is no longer a notification blind spot between `start_job_execution(...)` and runner launch. `run_with_heartbeats_does_not_launch_runner_when_job_is_cancelled_before_spawn` proves that a cancellation in that window returns the existing cancellation error, keeps the runner launch count at zero, and observes the real `job_lifecycle::cancel_job(...)` side effects on the persisted job and workspace rows before release.
2. In the daemon-only path, there is no longer a blind spot between the caller-side `harness_validation_cancelled(...)` check and `command.spawn()`. `run_harness_command_with_heartbeats_does_not_spawn_command_when_job_is_cancelled_before_spawn` proves that a cancellation in that window returns a cancelled `HarnessCommandResult`, leaves the marker file absent, and sees the persisted cancelled job plus released workspace that `job_lifecycle::cancel_job(...)` wrote.
3. Both caller modes still behave correctly because they share the fixed helpers. The new direct helper unit tests are the inline `tick()` proof, because `tick()` reaches `run_with_heartbeats()` and `execute_harness_validation()` reaches `run_harness_command_with_heartbeats()` through those same private helpers. `run_forever_starts_next_job_after_running_job_cancellation` and `run_forever_cancels_daemon_only_validation_command` still pass, proving the supervised loop still wakes promptly after launch.
4. The existing heartbeat and lease-refresh behavior remains intact. `run_forever_starts_next_job_on_joinset_completion`, `run_forever_refreshes_heartbeat_while_job_is_running`, and `run_forever_refreshes_heartbeat_for_daemon_only_validation_job` still pass.
5. Startup recovery still understands the lease fields written by `start_job_execution(...)`. `reconcile_startup_expires_stale_running_jobs_and_marks_workspace_stale` still passes.
6. The adjacent store and usecase invariant proofs still pass. `finish_job_non_success_rolls_back_when_item_revision_changes_before_commit`, `start_job_execution_rejects_jobs_without_workspace_binding`, and `teardown_cancels_active_jobs_for_revision` stay green, proving the pre-launch fix did not weaken the stale-revision guard or workspace-binding requirement that the runtime still relies on.
7. The shared notifier wiring remains intact. `cancel_route_marks_active_job_cancelled_and_clears_workspace_attachment` and `cargo check -p ingot-daemon` still pass, proving `DispatchNotify` is still wired correctly across `ingot-usecases`, `ingot-http-api`, `ingot-agent-runtime`, and `apps/ingot-daemon`.

The two new focused unit tests must fail before the code change and pass after it. That fail-before / pass-after proof is the main evidence that this plan closed the actual remaining hole rather than only reshuffling control flow.

## Idempotence and Recovery

This work is safe to repeat because it is confined to in-memory notification ordering, runtime wait-loop control flow, and test-only hooks that compile only under `#[cfg(test)]`. No schema migration, persisted payload migration, or durable state rewrite is involved.

Keep the change idempotent by leaving `DispatchNotify` unchanged, leaving `JobDispatcher::new(...)` and `JobDispatcher::with_runner(...)` unchanged, and limiting the test hook to a default-off private field or helper. Re-running the focused tests should not require cleaning any persistent state beyond the temp directories they already create.

Do not add a new write path for cancellation. The runtime must continue to rely on the existing guarded mutation paths that already carry `expected_item_revision_id`, namely `start_job_execution(...)`, `heartbeat_job_execution(...)`, `job_lifecycle::cancel_job(...)`, `finish_job_non_success(...)`, and the existing workspace release or stale-marking helpers. The new pre-spawn branches should observe cancellation and skip launch; they should not invent a parallel way to mark jobs terminal.

The deterministic tests should follow the same rule. When they need to flip the job terminal inside the pause window, they should call `job_lifecycle::cancel_job(...)` against the real repositories instead of mutating the row directly. That keeps the proof aligned with the same guarded cancellation contract the runtime sees in production.

Do not silently broaden this plan from “cancelled-before-spawn” to “any terminal status before spawn” unless you also update the caller-side handling in both the inline agent path (`tick()` plus `execute_prepared_agent_job(...)`) and the supervised path to treat already-failed or already-expired jobs coherently. The repository does expose `/fail`, `/expire`, and teardown-driven cancellation as adjacent terminal writers, but the intent of this plan is to close the confirmed cancellation wakeup hole without smuggling in a wider lifecycle rewrite.

If a partial implementation compiles but the new unit tests fail, first verify that the pause hook is placed before the new direct reload rather than after it. For the agent test, the hook must be after `start_job_execution(...)` and before the new `self.db.get_job(prepared.job.id)` check. For the harness test, the hook must be before the new `self.db.get_job(prepared.job_id)` check and before `command.spawn()`. If the integration or reconciliation tests regress, compare the changed control flow against the existing `"job cancelled"` and `HarnessCommandResult { cancelled: true, ... }` shapes before changing any broader lifecycle code. Do not use destructive git resets in a dirty worktree.

## Artifacts and Notes

When implementation is complete, record the key evidence here. At minimum include:

    cargo test -p ingot-agent-runtime --lib tests::run_with_heartbeats_does_not_launch_runner_when_job_is_cancelled_before_spawn -- --exact
    test tests::run_with_heartbeats_does_not_launch_runner_when_job_is_cancelled_before_spawn ... ok

    cargo test -p ingot-agent-runtime --lib tests::run_harness_command_with_heartbeats_does_not_spawn_command_when_job_is_cancelled_before_spawn -- --exact
    test tests::run_harness_command_with_heartbeats_does_not_spawn_command_when_job_is_cancelled_before_spawn ... ok

    cargo test -p ingot-store-sqlite store::job::tests::finish_job_non_success_rolls_back_when_item_revision_changes_before_commit -- --exact
    test store::job::tests::finish_job_non_success_rolls_back_when_item_revision_changes_before_commit ... ok

    cargo test -p ingot-store-sqlite store::job::tests::start_job_execution_rejects_jobs_without_workspace_binding -- --exact
    test store::job::tests::start_job_execution_rejects_jobs_without_workspace_binding ... ok

    cargo test -p ingot-usecases teardown::tests::teardown_cancels_active_jobs_for_revision -- --exact
    test teardown::tests::teardown_cancels_active_jobs_for_revision ... ok

    cargo test -p ingot-agent-runtime --test dispatch run_forever_starts_next_job_after_running_job_cancellation -- --exact
    test run_forever_starts_next_job_after_running_job_cancellation ... ok

    cargo test -p ingot-agent-runtime --test auto_dispatch run_forever_cancels_daemon_only_validation_command -- --exact
    test run_forever_cancels_daemon_only_validation_command ... ok

    cargo test -p ingot-agent-runtime --test reconciliation reconcile_startup_expires_stale_running_jobs_and_marks_workspace_stale -- --exact
    test reconcile_startup_expires_stale_running_jobs_and_marks_workspace_stale ... ok

    cargo test -p ingot-http-api --test job_routes cancel_route_marks_active_job_cancelled_and_clears_workspace_attachment -- --exact
    test cancel_route_marks_active_job_cancelled_and_clears_workspace_attachment ... ok

    cargo check -p ingot-daemon
    Finished `dev` profile ... target(s) in ...

If the private test hook ends up needing a slightly different shape than expected here, add a short note describing the final hook, the exact pause boundary it guards, and why the existing integration helpers could not reliably hit that boundary. If the harness caller log message stays slightly imprecise for pre-spawn cancellation, note that explicitly so a later cleanup does not mistake it for a functional bug.

The landed hook is a private `PreSpawnPauseHook` stored on `JobDispatcher` under `#[cfg(test)]`. It shares state across dispatcher clones, stays disabled in production, pauses after the listener exists but before the new direct reload, and was sufficient because the integration-test helpers still only observe work after `AgentRunner::launch(...)` or `command.spawn()` has already happened.

## Interfaces and Dependencies

At the end of this work, `crates/ingot-usecases/src/notify.rs` should remain unchanged in shape:

    #[derive(Clone)]
    pub struct DispatchNotify { ... }

    impl DispatchNotify {
        pub fn new() -> Self;
        pub fn notify(&self);
        pub fn subscribe(&self) -> DispatchNotifyListener;
    }

    pub struct DispatchNotifyListener { ... }

    impl DispatchNotifyListener {
        pub async fn notified(&mut self);
    }

The fix is not another notifier redesign. It is an ordering correction in `crates/ingot-agent-runtime/src/lib.rs`.

`crates/ingot-domain/src/ports.rs` should also remain unchanged in shape for the execution-parameter contract:

    pub struct StartJobExecutionParams {
        pub job_id: JobId,
        pub item_id: ItemId,
        pub expected_item_revision_id: ItemRevisionId,
        pub workspace_id: Option<WorkspaceId>,
        pub agent_id: Option<AgentId>,
        pub lease_owner_id: String,
        pub process_pid: Option<u32>,
        pub lease_expires_at: DateTime<Utc>,
    }

    pub struct FinishJobNonSuccessParams {
        pub job_id: JobId,
        pub item_id: ItemId,
        pub expected_item_revision_id: ItemRevisionId,
        pub status: JobStatus,
        pub outcome_class: Option<OutcomeClass>,
        pub error_code: Option<String>,
        pub error_message: Option<String>,
        pub escalation_reason: Option<EscalationReason>,
    }

`crates/ingot-domain/src/harness.rs` is the other unchanged contract this plan relies on:

    #[derive(Debug, Clone, Default, Serialize)]
    pub struct HarnessProfile {
        pub commands: Vec<HarnessCommand>,
        pub skills: HarnessSkills,
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct HarnessCommand {
        pub name: String,
        pub run: String,
        pub timeout: Duration,
    }

`crates/ingot-agent-runtime/src/lib.rs::load_harness_profile(project_path)` should continue to read `<project>/.ingot/harness.toml` and to return `HarnessProfile::default()` when that file is absent. The daemon-only unit test therefore needs a real file only because it reuses `prepare_harness_validation(...)` and needs a non-empty `prepared.harness.commands` list.

`JobDispatcher::new(...)` and `JobDispatcher::with_runner(...)` should keep their current signatures. If a test-only hook is required, add it as a private `#[cfg(test)]` field or helper that defaults to a no-op and is initialized internally. Unit tests in `src/lib.rs` may set that private field after constructing the dispatcher because they share module visibility with `JobDispatcher`.

If the test-only hook is stored on `JobDispatcher`, it must stay compatible with `#[derive(Clone)]`. In practice that means an `Arc`-backed hook state or an equivalent clone-safe helper, because the runtime already clones `JobDispatcher` for supervised task execution.

`crates/ingot-agent-runtime/src/lib.rs::run_with_heartbeats(&self, prepared: &PreparedRun, request: AgentRequest) -> Result<AgentResponse, AgentError>` must, at the end of this work, create its listener before `start_job_execution(...)`, preserve the existing `StartJobExecutionParams` fields unchanged, including `process_pid`, re-read the job after `start_job_execution(...)` succeeds, and return `Err(AgentError::ProcessError("job cancelled".into()))` without calling `tokio::spawn(...)` when the job is already cancelled.

`crates/ingot-agent-runtime/src/lib.rs::prepare_harness_validation(&self, queued_job: Job) -> Result<PrepareHarnessValidationOutcome, RuntimeError>` should continue to call `start_job_execution(...)`, set the job lease there, and return `PreparedHarnessValidation`. This plan does not move the running-state transition for daemon-only validation.

`crates/ingot-agent-runtime/src/lib.rs::run_prepared_harness_validation(&self, prepared: PreparedHarnessValidation) -> Result<(), RuntimeError>` should continue to call `harness_validation_cancelled(...)` before each command and to treat `HarnessCommandResult.cancelled` as the signal to stop the validation loop.

`crates/ingot-agent-runtime/src/lib.rs::run_harness_command_with_heartbeats(&self, prepared: &PreparedHarnessValidation, command_spec: &HarnessCommand) -> HarnessCommandResult` must, at the end of this work, create its listener before any spawn attempt, re-read the job immediately before spawn, and return a cancelled `HarnessCommandResult` without spawning the child when the job is already cancelled.

No new external dependency should be added. Reuse the existing Tokio primitives already in the crate and the existing dev-dependency fixture surface from `ingot-test-support`.

Revision note: created on 2026-03-19 after a fresh-eyes review found that the new listener-based cancellation path still subscribes after the job becomes running work, leaving a small missed-notification window that falls back to heartbeat timing.

Revision note: updated on 2026-03-19 by re-reading the runtime code paths and tests. This revision corrected the daemon-only boundary to include `prepare_harness_validation()`, replaced placeholder “new integration test” steps with concrete unit tests and exact cargo commands, and recorded the constructor-stability constraint for any deterministic `#[cfg(test)]` pause hook.

Revision note: updated on 2026-03-19 after a full lifecycle trace through `start_job_execution(...)`, `job_lifecycle::cancel_job(...)`, `reconcile_running_job(...)`, supervisor cleanup, HTTP cancel routing, and the nearby reconciliation tests. This revision adds the actual invariant-bearing fields (`expected_item_revision_id`, `lease_owner_id`, `lease_expires_at`, and `workspace_id`), names both `tick()` and `run_forever()` caller paths, corrects the pause-hook boundary so it fires before the new direct reloads rather than after them, and adds the adjacent running-job reconciliation regression that should remain green.

Revision note: updated again on 2026-03-19 after re-reading the auxiliary terminal-state writers and adjacent invariant tests. This revision adds revision-lane teardown as another cancellation path reached from item-mutation and approval-reject routes, relaxes the harness-result assertion so it matches the real `build_harness_command_result(...)` behavior, requires the new unit tests to use the real `prepare_run()` and `prepare_harness_validation()` helpers, records that any pause-hook field must remain clone-safe with `JobDispatcher`, and adds the adjacent SQLite and usecase exact tests that protect the stale-revision and workspace-binding invariants this runtime fix still depends on.

Revision note: updated once more on 2026-03-19 after re-reading `crates/ingot-domain/src/ports.rs`, `crates/ingot-agent-runtime/src/lib.rs::load_harness_profile(...)`, and the existing daemon-only runtime tests. This revision restores the omitted `process_pid` field to the unchanged `StartJobExecutionParams` contract, makes the deterministic-test cancellation step use the real `job_lifecycle::cancel_job(...)` path instead of vague direct row edits, and records the exact `.ingot/harness.toml` file that the new `src/lib.rs` daemon-only unit test must create.

Revision note: updated again on 2026-03-19 after re-reading `crates/ingot-domain/src/harness.rs`, `crates/ingot-agent-runtime/tests/common/mod.rs::BlockingRunner`, and `crates/ingot-agent-runtime/tests/auto_dispatch.rs::write_harness_toml(...)`. This revision clarifies that the harness file is required only because `prepare_harness_validation(...)` otherwise sees an empty default profile, points the unit-test implementation at the repo’s existing Notify-based pause and harness-file patterns, and strengthens the deterministic acceptance criteria so the new tests also prove the real cancellation writer cleared the persisted workspace attachment before launch is skipped.

Revision note: updated on 2026-03-19 after implementation and validation. This revision marks the runtime/helper/test work complete, records the final private `PreSpawnPauseHook` shape, replaces the earlier bare-name exact cargo commands with the module-qualified forms that actually execute the unit tests, and captures the passing validation evidence across runtime, store/usecase, HTTP, reconciliation, and daemon checks.
