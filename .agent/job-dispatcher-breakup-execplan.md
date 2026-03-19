# Break up JobDispatcher into a runtime supervisor plus usecase-owned execution services

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows `.agent/PLANS.md` and must be maintained in accordance with that file.

## Purpose / Big Picture

After this change, the daemon will still launch, supervise, and recover jobs exactly as it does today, but the work will no longer be trapped inside one 5,000-line `JobDispatcher` implementation in `crates/ingot-agent-runtime/src/lib.rs`. A contributor will be able to change job preparation without re-reading harness execution, change projected follow-up dispatch without touching heartbeat code, and change Git-operation recovery without understanding prompt rendering.

The user-visible behavior should remain stable. The daemon must still:

- run queued mutating jobs through `tick()` and `run_forever()`
- refresh heartbeats for both agent-backed jobs and daemon-only validation jobs
- persist prompt and response artifacts for agent-backed jobs
- run harness validation, including cancellation and timeout cleanup
- auto-dispatch projected review and validation work
- recover interrupted jobs, convergences, Git operations, and abandoned workspaces on startup

The observable improvement is internal resilience: smaller modules, narrower ports, and a refactor shape that lets multiple agents work on separate files without re-reading or rewriting unrelated state-machine branches.

## Progress

- [x] (2026-03-19 19:05Z) Re-read `.agent/PLANS.md`, audited `crates/ingot-agent-runtime/src/lib.rs`, `crates/ingot-usecases`, `SPEC.md`, `ARCHITECTURE.md`, and the adjacent ExecPlans, and confirmed that `JobDispatcher` currently mixes runtime infrastructure with application policy.
- [x] (2026-03-19 19:05Z) Authored this ExecPlan in `.agent/job-dispatcher-breakup-execplan.md`.
- [ ] Split `crates/ingot-agent-runtime/src/lib.rs` into internal modules without changing behavior.
- [ ] Extract projected follow-up dispatch and validation-dispatch policy into `crates/ingot-usecases`.
- [ ] Extract execution-preparation policy into `crates/ingot-usecases`.
- [ ] Extract Git-operation reconciliation and maintenance policy into `crates/ingot-usecases`.
- [ ] Reduce `JobDispatcher` to a thin facade over a supervisor plus explicit services, then run the full Rust test and lint gates.

## Surprises & Discoveries

- Observation: the repository already extracted some top-level convergence and reconciliation sequencing into `ingot-usecases`, but `JobDispatcher` still owns most of the detailed policy branches that make those services hard to evolve.
  Evidence: `crates/ingot-usecases/src/convergence.rs` and `crates/ingot-usecases/src/reconciliation.rs` exist and are wired from the runtime, but `crates/ingot-agent-runtime/src/lib.rs` still contains `prepare_run`, `prepare_harness_validation`, `reconcile_git_operations`, `auto_dispatch_projected_review_locked`, and the `adopt_*` handlers.

- Observation: the same “what should happen after a job finishes” policy is implemented in more than one shape.
  Evidence: `finish_report_run`, `complete_commit_run`, and `run_prepared_harness_validation` each append activities, refresh revision context, optionally request approval, and trigger projected follow-up dispatch with slightly different local wiring.

- Observation: the current runtime crate crosses the architectural boundary described in `ARCHITECTURE.md`.
  Evidence: `crates/ingot-agent-runtime/src/lib.rs` imports `ingot_workflow::Evaluator`, dispatch helpers from `ingot_usecases::job`, convergence services, and directly computes projected validation dispatch instead of acting as a thin subprocess/runtime adapter.

- Observation: the hardest part of the split is not the Tokio supervisor loop. The hard part is untangling preparation, completion, and recovery policy from process control.
  Evidence: `run_forever` and the `JoinSet` supervisor are compact compared with the much larger policy-heavy clusters around `prepare_run`, `finish_*`, `reconcile_git_operations`, and `auto_dispatch_projected_*`.

- Observation: the public runtime surface is broader than `tick()` and `run_forever()`, and tests call several of those extra methods directly.
  Evidence: `crates/ingot-agent-runtime/src/lib.rs` exposes `refresh_project_mirror`, `reconcile_active_jobs`, `fail_prepare_convergence_attempt`, and `auto_dispatch_projected_review_locked`; direct calls appear in `crates/ingot-agent-runtime/tests/reconciliation.rs`, `crates/ingot-agent-runtime/tests/convergence.rs`, and `crates/ingot-agent-runtime/tests/auto_dispatch.rs`.

- Observation: the most important stale-state guards already live in the store layer and must survive any extraction unchanged.
  Evidence: `crates/ingot-store-sqlite/src/store/job.rs` guards `start_job_execution`, `heartbeat_job_execution`, and `finish_job_non_success` with `expected_item_revision_id`; `crates/ingot-store-sqlite/src/store/job_completion.rs` guards `apply_job_completion` with `expected_item_revision_id` plus `PreparedConvergenceGuard`.

- Observation: existing usecase helpers already cover part of the job termination surface, but they do not match runtime recovery semantics exactly.
  Evidence: `crates/ingot-usecases/src/job_lifecycle.rs::expire_job()` writes `error_code = "job_expired"` and releases the workspace, while `crates/ingot-agent-runtime/src/lib.rs::reconcile_running_job()` writes `error_code = "heartbeat_expired"` and marks the workspace `Stale`. The current runtime tests assert the latter behavior.

- Observation: the runtime already has three internal adapter structs that bridge into `ingot-usecases`, so the cleanest extraction path is to move policy behind more ports, not to bypass those adapters.
  Evidence: `RuntimeConvergencePort`, `RuntimeFinalizePort`, and `RuntimeReconciliationPort` in `crates/ingot-agent-runtime/src/lib.rs` already implement `ConvergenceSystemActionPort`, `PreparedConvergenceFinalizePort`, and `ReconciliationPort`.

- Observation: `refresh_project_mirror()` logic is duplicated today between the runtime and the HTTP API support layer.
  Evidence: `crates/ingot-agent-runtime/src/lib.rs::refresh_project_mirror()` and `crates/ingot-http-api/src/router/support.rs::refresh_project_mirror()` both re-check unresolved finalize operations before calling `ensure_mirror`.

## Decision Log

- Decision: land this refactor in two layers, not one. First split `lib.rs` into internal runtime modules with no behavior change, then move policy out of the runtime crate into `ingot-usecases`.
  Rationale: doing both at once would combine mechanical file moves with semantic extraction, making review and regression isolation unnecessarily difficult.
  Date/Author: 2026-03-19 / Codex

- Decision: keep agent-backed execution and daemon-only harness validation as separate executors.
  Rationale: both paths need cancellation and heartbeat behavior, but their side effects are materially different. Agent-backed jobs write prompt and response artifacts and may create commits; daemon-only validation runs shell commands directly and emits validation reports. A forced generic executor would become another large abstraction that hides those differences instead of simplifying them.
  Date/Author: 2026-03-19 / Codex

- Decision: move application policy into `ingot-usecases`, not into more runtime-local helper modules.
  Rationale: the architecture document already states that `ingot-agent-runtime` is infrastructure and `ingot-usecases` owns orchestration. This change should complete that direction instead of merely scattering policy across smaller runtime files.
  Date/Author: 2026-03-19 / Codex

- Decision: treat projected follow-up dispatch as first-class application behavior and extract both review and validation auto-dispatch together.
  Rationale: review auto-dispatch already has a usecase helper, but validation auto-dispatch is still embedded in the runtime. Splitting only one side would preserve an arbitrary boundary and keep “what happens next after job completion” spread across crates.
  Date/Author: 2026-03-19 / Codex

- Decision: define parallel work by file ownership after the initial module split.
  Rationale: multiple agents can safely work in parallel only after the large single-file runtime implementation is broken into stable internal modules. Before that, the write surface overlaps too heavily.
  Date/Author: 2026-03-19 / Codex

- Decision: preserve the current public `JobDispatcher` methods during the refactor, even if they become delegating facades.
  Rationale: `refresh_project_mirror`, `reconcile_active_jobs`, `fail_prepare_convergence_attempt`, and `auto_dispatch_projected_review_locked` are part of the current runtime surface and are already exercised by tests. Breaking them during the split would create unnecessary churn unrelated to the architectural goal.
  Date/Author: 2026-03-19 / Codex

- Decision: preserve current stale-state and lease semantics in the no-behavior-change milestones, even when a nearby usecase helper looks similar.
  Rationale: the store and runtime currently distinguish between active-job expiry, operator cancellation, prepare-failure escalation, and clean integrated validation completion with different guards and different resulting workspace states. Replacing those paths with similar but non-identical helpers would silently change behavior.
  Date/Author: 2026-03-19 / Codex

## Outcomes & Retrospective

At the time this plan was written, no runtime code had been changed yet. The design outcome of the investigation is a concrete extraction map rather than a vague “split the god type” recommendation.

The intended end state is:

`crates/ingot-agent-runtime` owns supervisor orchestration, subprocess or shell execution, heartbeats, cancellation polling, artifact I/O, workspace and Git side effects, and bootstrap wiring.

`crates/ingot-usecases` owns preparation decisions, post-execution completion policy, projected follow-up dispatch, and recovery-state decisions that currently leak into the runtime crate.

`crates/ingot-git` and `crates/ingot-workspace` continue to own low-level Git and worktree side effects only.

The most important implementation constraint discovered during this review is that the plan cannot treat “execution,” “completion,” and “recovery” as single-path concerns. The extracted code must preserve the different guarded update paths for:

- start execution and heartbeat updates
- non-success termination
- structured report completion with prepared-convergence protection
- Git-operation adoption
- queue-head convergence preparation failure
- operator cancellation and teardown

If implementation follows this plan, the runtime should become much easier to test and modify because behavioral changes will land in smaller services and modules with explicit invariants instead of inside one monolithic dispatcher impl.

## Context and Orientation

`crates/ingot-agent-runtime/src/lib.rs` is currently 5,274 lines long and defines `JobDispatcher`. A “dispatcher” in this repository is the long-running daemon component that wakes up, finds queued jobs, prepares their workspaces, launches work, refreshes heartbeats, and cleans up after interrupted or completed jobs.

The current public runtime surface is:

- `DispatcherConfig::new`
- `JobDispatcher::new`
- `JobDispatcher::with_runner`
- `JobDispatcher::refresh_project_mirror`
- `JobDispatcher::run_forever`
- `JobDispatcher::reconcile_startup`
- `JobDispatcher::tick`
- `JobDispatcher::reconcile_active_jobs`
- `JobDispatcher::fail_prepare_convergence_attempt`
- `JobDispatcher::auto_dispatch_projected_review_locked`

The current file also contains three runtime-to-usecase adapter types:

- `RuntimeConvergencePort`
- `RuntimeFinalizePort`
- `RuntimeReconciliationPort`

Those adapters are important context. The runtime is not purely monolithic today; it already exposes some behavior through usecase ports. The breakup should extend that direction instead of introducing a parallel architecture.

The major concern clusters currently present in `crates/ingot-agent-runtime/src/lib.rs` are:

Supervisor and wakeup control:

- `run_forever`
- `drive_non_job_work`
- `run_supervisor_iteration`
- `reap_completed_tasks`
- `handle_supervised_join_result`
- `cleanup_supervised_task`
- `launch_supervised_jobs`

Preparation and prompt assembly:

- `prepare_run`
- `prepare_harness_validation`
- `select_agent`
- `prepare_workspace`
- `integration_workspace_id_for_job`
- `assemble_prompt`
- `hydrate_convergences`
- `compute_target_head_valid`
- harness profile and skill resolution helpers near the bottom of the file

Agent-backed execution:

- `run_with_heartbeats`
- `execute_prepared_agent_job`
- `finish_run`
- `finish_commit_run`
- `finish_report_run`
- `create_commit`
- `complete_commit_run`

Daemon-only harness execution:

- `execute_harness_validation`
- `run_prepared_harness_validation`
- `run_harness_command_with_heartbeats`
- `refresh_daemon_validation_heartbeat`
- `harness_validation_cancelled`

Completion and workspace cleanup helpers:

- `fail_run`
- `fail_job_preparation`
- `append_escalation_cleared_activity_if_needed`
- `finalize_workspace_after_success`
- `finalize_workspace_after_failure`
- `finalize_integration_workspace_after_close`
- `reset_workspace`
- `refresh_revision_context`
- `refresh_revision_context_for_ids`

Git-operation recovery and adoption:

- `reconcile_git_operations`
- `complete_finalize_target_ref_operation`
- `reconcile_finalize_target_ref_operation`
- `adopt_reconciled_git_operation`
- `adopt_create_job_commit`
- `adopt_finalized_target_ref`
- `adopt_prepared_convergence`
- `adopt_reset_workspace`
- `adopt_removed_workspace_ref`

Other reconciliation and follow-up dispatch:

- `reconcile_assigned_job`
- `reconcile_running_job`
- `reconcile_active_convergences`
- `reconcile_workspace_retention`
- `workspace_can_be_removed`
- `remove_abandoned_workspace`
- `recover_projected_review_jobs`
- `auto_dispatch_projected_review`
- `auto_dispatch_projected_review_locked`
- `auto_dispatch_projected_validation_job`

Convergence system-action helpers that still live in the runtime:

- `auto_finalize_prepared_convergence`
- `invalidate_prepared_convergence`
- `fail_prepare_convergence_attempt`
- `prepare_queue_head_convergence`
- checkout sync helpers used by finalization

The file also contains a large amount of prompt, schema, artifact, and harness text utility code. Those helpers are not themselves business policy, but they currently live next to it and contribute to the “god type” problem.

## Lifecycle and Invariants

The core rule for this refactor is that every extracted mutating path must preserve the guards that the current code already enforces. Those guards are not optional cleanup. They are the stale-work protection for this daemon.

### Job revision guard

The durable job guard field is `expected_item_revision_id`, backed by the job’s `item_revision_id` and the item row’s `current_revision_id`.

Creation and dispatch paths:

- `ingot_usecases::job::dispatch_job()` creates jobs against `item.current_revision_id`.
- `retry_job()` rejects retries against superseded revisions.
- `auto_dispatch_projected_review_locked()` and `auto_dispatch_projected_validation_job()` both load `item.current_revision_id` before creating follow-up jobs.

Preparation paths:

- `prepare_run()` returns `NotPrepared` if `item.current_revision_id != job.item_revision_id`.
- `prepare_harness_validation()` does the same.
- `prepare_queue_head_convergence()` returns early if the current item revision no longer matches the revision it was asked to prepare.

Execution and termination store guards:

- `Database::start_job_execution()` updates only when the job is still queued or assigned and the item still points at `expected_item_revision_id`.
- `Database::heartbeat_job_execution()` updates only when the job is still running, the lease owner matches, and the item still points at `expected_item_revision_id`.
- `Database::finish_job_non_success()` updates only when the job is still queued, assigned, or running and the item still points at `expected_item_revision_id`.
- `Database::apply_job_completion()` updates only when the job is still queued, assigned, or running and the item still points at `expected_item_revision_id`.

Conflict mapping:

- `classify_running_job_conflict()` returns `job_revision_stale`, `job_not_active`, `job_missing_workspace`, or `job_update_conflict`.
- `classify_terminal_job_conflict()` returns `job_revision_stale`, `job_not_active`, or `job_update_conflict`.
- `classify_job_completion_conflict()` returns `job_revision_stale`, `prepared_convergence_missing`, `prepared_convergence_stale`, or `job_not_active`.

The extracted preparation, completion, and recovery services must continue to pass and interpret these exact guards. Do not replace them with unguarded `update_job()` calls.

### Lease guard

The durable lease fields are `lease_owner_id`, `heartbeat_at`, and `lease_expires_at`.

- `run_with_heartbeats()` starts execution with `lease_owner_id = self.lease_owner_id`, then refreshes heartbeats through `heartbeat_job_execution()`.
- `prepare_harness_validation()` starts daemon-only validation with `agent_id = None` but the same `lease_owner_id = self.lease_owner_id`.
- `refresh_daemon_validation_heartbeat()` also uses `self.lease_owner_id`.
- `reconcile_running_job()` expires a running job when either `lease_expires_at` is stale or `lease_owner_id` does not match the current dispatcher.

This is important because it means a recovery extraction must preserve the current “foreign owner means expired” rule. It also means a usecase extraction cannot drop `lease_owner_id` on the floor and treat a heartbeat update as a generic running-job write.

### Prepared convergence guard

The durable integrated-validation guard is `PreparedConvergenceGuard`, which contains:

- `convergence_id`
- `item_revision_id`
- `target_ref`
- `expected_target_head_oid`
- `next_approval_state`

This guard is created in `crates/ingot-usecases/src/job.rs::prepared_convergence_guard()` for clean `validate_integrated` completion and enforced in `Database::apply_job_completion()`.

Any extraction that touches report completion must preserve the existing flow:

- `finish_report_run()` uses `CompleteJobService`.
- `run_prepared_harness_validation()` also uses `CompleteJobService`.
- `CompleteJobService` computes the prepared-convergence guard and relies on store enforcement to reject stale integrated validation.

Do not replace those paths with a direct `update_job()` or an unguarded custom completion mutation.

### Queue-head and convergence-preparation guard

The convergence prepare flow is guarded by:

- current item revision
- active queue entry identity and `Head` status
- absence of another active convergence
- current target-ref state in the mirror

`prepare_queue_head_convergence()` explicitly re-checks all of those before it mutates workspace, convergence, queue entry, and git-operation rows. `fail_prepare_convergence_attempt()` then updates:

- the integration workspace status
- the convergence state
- the item escalation and approval state
- the queue entry release state
- the git operation status and replay metadata
- activities

If that flow is moved, those mutations must stay together and preserve the current queue-entry and replay-metadata handling.

### Git-operation adoption guards

The current adoption helpers already contain stale-state protection:

- `adopt_create_job_commit()` returns early if the job is no longer active.
- `adopt_finalized_target_ref()` only closes the item when `item.current_revision_id == convergence.item_revision_id`.
- `adopt_prepared_convergence()` returns early for cancelled, failed, or finalized convergences.
- `find_or_create_finalize_operation()` relies on the unresolved-finalize uniqueness constraint and fetches the existing row on conflict.

Do not simplify those into unconditional “mark completed” helpers during extraction.

### Workspace-state guard asymmetry

Different paths deliberately leave workspaces in different states:

- `reconcile_assigned_job()` re-queues the job and releases the workspace to `Ready`.
- `reconcile_running_job()` expires the job and marks the workspace `Stale`.
- `fail_run()` resets the workspace filesystem and then releases or abandons the workspace based on `WorkspaceLifecycle`.
- `cleanup_supervised_task()` uses `fail_run()` for agent-backed jobs but writes a non-success row and marks the workspace `Stale` for daemon-only validation jobs.
- `job_lifecycle::cancel_job()`, `fail_job()`, `expire_job()`, and `teardown_revision_lane()` release workspaces through generic repository updates.

This asymmetry is real and tested. The extraction must not collapse all termination paths onto one helper unless that helper can preserve each distinct resulting state.

## Plan of Work

Begin with a behavior-preserving runtime-only split. Create internal runtime modules and move code out of `crates/ingot-agent-runtime/src/lib.rs` while keeping the current public API stable. This first step is mechanical on purpose. It reduces merge conflict pressure and makes later semantic extraction reviewable.

The runtime split should reflect the clusters that already exist in the code today:

`crates/ingot-agent-runtime/src/lib.rs`

- keep the public surface and re-exports only
- keep `mod bootstrap;`
- add the internal module declarations for the new split

`crates/ingot-agent-runtime/src/dispatcher/ports.rs`

- move `RuntimeConvergencePort`
- move `RuntimeFinalizePort`
- move `RuntimeReconciliationPort`
- keep their current trait impls and preserve their current mapping through `usecase_to_runtime_error()` and `usecase_from_runtime_error()`

`crates/ingot-agent-runtime/src/dispatcher/supervisor.rs`

- move `run_forever`
- move `drive_non_job_work`
- move `run_supervisor_iteration`
- move `reap_completed_tasks`
- move `handle_supervised_join_result`
- move `cleanup_supervised_task`
- move `launch_supervised_jobs`
- move `next_runnable_job`
- preserve the current `tick()` behavior where `system_actions_progressed` causes an early return before launching a job

`crates/ingot-agent-runtime/src/dispatcher/prepare.rs`

- move `PreparedRun`, `PrepareRunOutcome`, `PreparedHarnessValidation`, `PrepareHarnessValidationOutcome`, `WorkspaceLifecycle`, and the runtime support structs tied to preparation
- move `prepare_run`
- move `prepare_harness_validation`
- move `select_agent`
- move `prepare_workspace`
- move `integration_workspace_id_for_job`
- move `hydrate_convergences`
- move `compute_target_head_valid`
- preserve current agent-selection semantics, including the current `AdapterKind::Codex` filter and `supports_job()` checks

`crates/ingot-agent-runtime/src/dispatcher/prompt.rs`

- move `assemble_prompt`
- move `HarnessPromptContext`, `ResolvedHarnessSkill`, `HarnessLoadError`
- move `read_harness_profile_if_present`, `load_harness_profile`, `resolve_harness_prompt_context`, `resolve_harness_skills`
- move the built-in template and schema helpers near the bottom of the file
- keep prompt text and schema contracts byte-for-byte stable in Milestone 1

`crates/ingot-agent-runtime/src/dispatcher/agent_execution.rs`

- move `run_with_heartbeats`
- move `execute_prepared_agent_job`
- move `run_prepared_agent_job`
- move `finish_run`
- move `finish_commit_run`
- move `finish_report_run`
- move `verify_mutating_workspace_protocol`
- move `verify_read_only_workspace_protocol`
- move `create_commit`
- move `complete_commit_run`

`crates/ingot-agent-runtime/src/dispatcher/harness_execution.rs`

- move `execute_harness_validation`
- move `run_prepared_harness_validation`
- move `run_prepared_harness_validation_job`
- move `run_harness_command_with_heartbeats`
- move `refresh_daemon_validation_heartbeat`
- move `harness_validation_cancelled`
- keep daemon-only validation artifact behavior unchanged; current tests expect no agent-style prompt or response artifact writes for that path

`crates/ingot-agent-runtime/src/dispatcher/workspace.rs`

- move `finalize_workspace_after_success`
- move `finalize_workspace_after_failure`
- move `finalize_integration_workspace_after_close`
- move `reset_workspace`
- move `workspace_can_be_removed`
- move `remove_abandoned_workspace`

`crates/ingot-agent-runtime/src/dispatcher/completion.rs`

- move `fail_run`
- move `fail_job_preparation`
- move `append_escalation_cleared_activity_if_needed`
- move `refresh_revision_context`
- move `refresh_revision_context_for_ids`
- move `current_authoring_head_for_revision_with_workspace`
- move `effective_authoring_base_commit_oid`
- move `complete_job_service`
- move `append_activity`
- preserve the current split where commit jobs use `apply_job_completion()` directly but report jobs and harness validation use `CompleteJobService`

`crates/ingot-agent-runtime/src/dispatcher/git_ops.rs`

- move `reconcile_git_operations`
- move `complete_finalize_target_ref_operation`
- move `reconcile_finalize_target_ref_operation`
- move `adopt_reconciled_git_operation`
- move `adopt_create_job_commit`
- move `adopt_finalized_target_ref`
- move `adopt_prepared_convergence`
- move `adopt_reset_workspace`
- move `adopt_removed_workspace_ref`

`crates/ingot-agent-runtime/src/dispatcher/system_actions.rs`

- move `tick_system_action`
- move `promote_queue_heads`
- move `auto_finalize_prepared_convergence`
- move `invalidate_prepared_convergence`
- move `fail_prepare_convergence_attempt`
- move `prepare_queue_head_convergence`
- move checkout-sync and finalization-readiness helpers tied to convergence system actions

`crates/ingot-agent-runtime/src/dispatcher/projected_dispatch.rs`

- move `recover_projected_review_jobs`
- move `auto_dispatch_projected_review`
- move `auto_dispatch_projected_review_locked`
- move `auto_dispatch_projected_validation_job`

`crates/ingot-agent-runtime/src/dispatcher/artifacts.rs`

- move `write_prompt_artifact`
- move `write_response_artifacts`
- move `artifact_dir`

Milestone 1 must stop there. Do not move policy across crate boundaries yet.

After the split is green, start semantic extraction in the order that best matches the code’s current duplication and guards.

First extract projected follow-up dispatch into `ingot-usecases`, because the runtime already delegates review auto-dispatch to `ingot_usecases::dispatch::auto_dispatch_review()` but still owns the validation half and the item-wide recovery scan. Model the new code after the existing `dispatch.rs` helpers and keep `JobDispatcher::auto_dispatch_projected_review_locked()` as a public facade that delegates into the new service.

Second extract execution completion policy into `ingot-usecases`, but compose existing helpers instead of replacing them blindly:

- reuse `CompleteJobService` for report and harness-validation completion
- reuse `job_lifecycle` helpers where the resulting status and workspace semantics match the runtime behavior
- do not replace `reconcile_running_job()` with `job_lifecycle::expire_job()` without first deciding whether to preserve current `heartbeat_expired` plus `WorkspaceStatus::Stale` semantics or to intentionally change them and update tests
- do not bypass the existing `PreparedConvergenceGuard` flow for clean `validate_integrated`

Third extract execution preparation policy into `ingot-usecases`. That service should decide whether the job is launchable and should return the durable execution facts that the runtime needs, but it should not perform worktree provisioning or process launching. Reuse the existing `ingot_usecases::dispatch` helpers for candidate-subject derivation instead of re-implementing them.

Fourth extract recovery policy into `ingot-usecases`. That includes:

- Git-operation adoption decisions
- active-job recovery decisions
- active-convergence recovery decisions
- projected-review recovery sequencing
- workspace-retention eligibility decisions

This extraction should compose existing `job_lifecycle` and `teardown` helpers where they match the runtime behavior, not create a second unrelated state machine.

The HTTP API is adjacent code but is not the primary target of this refactor. Do not widen scope into `crates/ingot-http-api/src/router/dispatch.rs` or `router/convergence.rs` unless a shared helper must move to keep runtime and HTTP behavior aligned. If that becomes necessary, record it explicitly in this plan before doing it.

## Milestones

### Milestone 1: Split the runtime file without changing behavior

At the end of this milestone, the public API of `crates/ingot-agent-runtime` should behave exactly as before, but `crates/ingot-agent-runtime/src/lib.rs` should no longer contain the whole implementation. Contributors should be able to open `ports.rs`, `supervisor.rs`, `prepare.rs`, `prompt.rs`, `agent_execution.rs`, `harness_execution.rs`, `workspace.rs`, `completion.rs`, `git_ops.rs`, `system_actions.rs`, `projected_dispatch.rs`, and `artifacts.rs` and find one coherent concern per file.

This milestone must preserve:

- the current public methods on `JobDispatcher`
- the current `DispatchNotify` watch-based wakeup semantics
- the current `tick()` early return after system-action progress
- the current test-observed output artifact paths and names
- the current lease-owner and stale-revision guards

Run `cargo test -p ingot-agent-runtime` after the split. Acceptance for this milestone is behavioral parity plus a much smaller `lib.rs`.

### Milestone 2: Move projected follow-up dispatch into usecases

At the end of this milestone, the runtime should no longer compute projected validation dispatch itself, and the projected review recovery scan should no longer be defined primarily inside the runtime crate.

This milestone must preserve the behaviors tested in `crates/ingot-agent-runtime/tests/auto_dispatch.rs`, including:

- `authoring_success_auto_dispatches_incremental_review`
- `implicit_revision_auto_dispatches_incremental_review_from_bound_workspace_base`
- `auto_dispatch_projected_review_rejects_missing_candidate_subject`
- `tick_recovers_idle_review_work_even_when_processing_other_queued_jobs`
- `clean_incremental_review_auto_dispatches_candidate_review`
- `clean_candidate_review_auto_dispatches_candidate_validation`
- `idle_item_auto_dispatches_candidate_review_after_nonblocking_incremental_triage`

Run `cargo test -p ingot-agent-runtime --test auto_dispatch` and `cargo test -p ingot-usecases`. Acceptance is that the runtime public facade still passes the same tests while the policy lives in `ingot-usecases`.

### Milestone 3: Move execution-completion policy into usecases

At the end of this milestone, `fail_run`, `fail_job_preparation`, the shared post-success activity and revision-context logic, and the “what happens after this execution result” policy should no longer live primarily in the runtime crate.

This milestone must preserve the distinct paths for:

- commit completion
- report completion
- harness-validation completion
- preparation failure
- running-job timeout
- agent launch failure
- operator cancellation detected mid-run
- supervised-task cleanup after join error or task error

It must also preserve the stale-state guards around `expected_item_revision_id`, `lease_owner_id`, and `PreparedConvergenceGuard`.

Run `cargo test -p ingot-agent-runtime --test dispatch`, `cargo test -p ingot-agent-runtime --test escalation`, and `cargo test -p ingot-usecases`. Acceptance is that the runtime still produces the same terminal job states, activities, escalation behavior, and follow-up dispatch while the policy lives in `ingot-usecases`.

### Milestone 4: Move execution-preparation policy into usecases

At the end of this milestone, the runtime should still provision workspaces and launch work, but it should receive a prepared execution plan rather than deciding launchability itself.

This milestone must preserve:

- the current stale-revision checks in `prepare_run()` and `prepare_harness_validation()`
- the current `AdapterKind::Codex` selection filter
- the current `WorkspaceError::Busy` handling in the supervisor
- the current integration-workspace lookup from prepared convergence for daemon-only validation
- the current prompt contract, including repo-local skill inclusion and invalid-harness failure behavior

Run `cargo test -p ingot-agent-runtime --test dispatch`, `cargo test -p ingot-agent-runtime --test auto_dispatch`, and `cargo test -p ingot-usecases`. Acceptance is that preparation behavior is unchanged but the decision logic now lives in `ingot-usecases`.

### Milestone 5: Move recovery and convergence-system-action policy into usecases and leave JobDispatcher as a facade

At the end of this milestone, `JobDispatcher` should mainly wire services and delegate, while Git-operation recovery, active-job and active-convergence recovery, workspace-retention decisions, projected-review recovery, and convergence system-action decisions are usecase-owned.

This milestone must preserve the behaviors tested in:

- `crates/ingot-agent-runtime/tests/reconciliation.rs`
- `crates/ingot-agent-runtime/tests/convergence.rs`

That includes:

- expiring stale running jobs and marking their workspaces stale
- adopting create-job-commit, prepare-convergence, reset-workspace, and remove-workspace-ref operations
- finalizing prepared convergence only when target-ref and checkout state allow it
- leaving blocked finalize operations unresolved
- invalidating stale prepared convergence
- preserving `fail_prepare_convergence_attempt()` semantics, including queue-entry release and replay metadata
- conservative mixed-state startup recovery

Run `cargo test -p ingot-agent-runtime --test reconciliation`, `cargo test -p ingot-agent-runtime --test convergence`, and `cargo test -p ingot-usecases`. Acceptance is that those flows stay green while `JobDispatcher` becomes a thin façade.

## Concrete Steps

Work from `/Users/aa/.codex/worktrees/4c3c/ingot`.

Before editing, inspect the current tree:

    git status --short

For a fast Milestone 1 loop, run:

    cargo test -p ingot-agent-runtime

Expected success signal:

    test result: ok

For Milestone 2, after the projected-dispatch extraction, run:

    cargo test -p ingot-agent-runtime --test auto_dispatch
    cargo test -p ingot-usecases

For Milestone 3, after the completion-policy extraction, run:

    cargo test -p ingot-agent-runtime --test dispatch
    cargo test -p ingot-agent-runtime --test escalation
    cargo test -p ingot-usecases

For Milestone 4, after the preparation extraction, run:

    cargo test -p ingot-agent-runtime --test dispatch
    cargo test -p ingot-agent-runtime --test auto_dispatch
    cargo test -p ingot-usecases

For Milestone 5, after the recovery and system-action extraction, run:

    cargo test -p ingot-agent-runtime --test reconciliation
    cargo test -p ingot-agent-runtime --test convergence
    cargo test -p ingot-usecases

Before ending the overall work, run the repository-level gates from the same working directory:

    make test
    make lint
    make ci

Expected final success signal:

    test result: ok

for the test commands, and no nonzero exit status from `make lint` or `make ci`.

If `make lint` or `make ci` fail, record the exact failing command and the exact file paths or diagnostics in this document before stopping. Do not hand-wave “pre-existing lint failure” without the concrete evidence.

## Validation and Acceptance

The refactor is acceptable when all of the following are true:

`crates/ingot-agent-runtime/src/lib.rs` is reduced to a small crate root plus public API wiring, and the real implementation lives in focused internal modules.

The runtime still exposes and passes tests for `refresh_project_mirror`, `reconcile_active_jobs`, `fail_prepare_convergence_attempt`, and `auto_dispatch_projected_review_locked`.

The runtime still launches queued mutating, read-only, and daemon-only validation jobs correctly through both `tick()` and `run_forever()`, including:

- timeouts
- cancellation wakeups
- heartbeat refresh
- workspace-busy skip behavior
- stale-head queued-job skip behavior
- harness validation cancellation and timeout cleanup

The extracted code still preserves the current stale-state guards for:

- `expected_item_revision_id`
- `lease_owner_id`
- `PreparedConvergenceGuard`
- queue-entry identity and `Head` status
- item `current_revision_id` checks inside convergence preparation and item-closing adoption paths

Projected review and projected validation auto-dispatch still happen after the same job outcomes as before, but the policy lives in `ingot-usecases`.

Report completion and harness-validation completion still go through the prepared-convergence-safe completion path and do not regress integrated validation stale protection.

Git-operation reconciliation, active-job recovery, active-convergence recovery, and abandoned-workspace cleanup still pass the existing tests, but the runtime crate no longer owns those state-machine decisions directly.

The current distinct termination outcomes remain distinct unless an intentional behavior change is made and tested. In particular, runtime running-job expiry must not silently turn into `job_lifecycle::expire_job()` behavior unless that change is deliberate and reflected in the tests.

## Idempotence and Recovery

This plan intentionally starts with a behavior-preserving file split so that later extractions have smaller, safer diffs. Each milestone should be independently mergeable and should leave the repository compiling and tests passing before the next milestone begins.

If a milestone stalls midway, recover in this order:

1. Restore compilation inside the crate currently being moved.
2. Restore the targeted test binary or crate that covers the moved path.
3. Delete any duplicate old branch only after the new path is green.

Do not leave two authoritative copies of the same policy. Temporary delegation shims are acceptable. Long-lived duplicated state-machine logic is not.

No database schema changes are required by this plan. The work is code-only and safe to retry. The main risk is semantic drift during extraction, especially when a nearby helper appears reusable but does not preserve the exact current workspace-state or error-code semantics. Whenever that happens, prefer a thin delegating wrapper over a behavior-changing substitution.

## Artifacts and Notes

The most important proof points to record while implementing this plan are:

- a before-and-after line count for `crates/ingot-agent-runtime/src/lib.rs`
- a note showing which current runtime concern moved into which new file in Milestone 1
- a note showing which extracted usecase code owns projected dispatch, completion policy, preparation policy, and recovery policy after the later milestones
- the exact guards preserved for each extracted path, especially `expected_item_revision_id`, `lease_owner_id`, and `PreparedConvergenceGuard`
- any behavior that had to remain runtime-owned at the end, with a code-based reason

## Interfaces and Dependencies

The final code must preserve the existing public runtime surface listed in `Context and Orientation`.

The internal runtime split should follow existing code patterns rather than inventing a new framework. Use the current `CompleteJobService` in `crates/ingot-usecases/src/job.rs` as the model for new service extraction. Use `crates/ingot-usecases/src/job_lifecycle.rs` and `crates/ingot-usecases/src/teardown.rs` as the reference for guarded lifecycle mutations. Use `crates/ingot-usecases/src/dispatch.rs` for projected-dispatch and candidate-subject helpers instead of duplicating that logic.

The runtime crate should continue to use:

- `ingot_git` for raw Git effects
- `ingot_workspace` for worktree provisioning and removal
- `DispatchNotify` for watch-based wakeups
- `ProjectLocks` for project mutation serialization

The runtime crate should not add a third implementation of mirror refresh logic. If the refactor exposes a shared helper, use it to remove duplication between the runtime and `crates/ingot-http-api/src/router/support.rs`. If that deduplication is not needed for this refactor, leave both existing copies alone and do not create a new one.

After Milestone 1, parallel work should follow these write-ownership rules:

One agent owns:

- `crates/ingot-agent-runtime/src/dispatcher/ports.rs`
- `crates/ingot-agent-runtime/src/dispatcher/supervisor.rs`
- `crates/ingot-agent-runtime/src/dispatcher/artifacts.rs`

One agent owns:

- `crates/ingot-agent-runtime/src/dispatcher/prepare.rs`
- `crates/ingot-agent-runtime/src/dispatcher/prompt.rs`
- the new preparation code in `crates/ingot-usecases`

One agent owns:

- `crates/ingot-agent-runtime/src/dispatcher/agent_execution.rs`
- `crates/ingot-agent-runtime/src/dispatcher/harness_execution.rs`
- `crates/ingot-agent-runtime/src/dispatcher/completion.rs`
- the new completion code in `crates/ingot-usecases`

One agent owns:

- `crates/ingot-agent-runtime/src/dispatcher/git_ops.rs`
- `crates/ingot-agent-runtime/src/dispatcher/system_actions.rs`
- `crates/ingot-agent-runtime/src/dispatcher/projected_dispatch.rs`
- the new projected-dispatch and recovery code in `crates/ingot-usecases`

Those write sets are based on the current function clusters in the code, not on arbitrary naming preference.

Revision note: created this ExecPlan on 2026-03-19 after investigating the current `JobDispatcher` implementation, the existing usecase boundaries, and the adjacent ExecPlans for convergence extraction, harness hardening, and JoinSet-based concurrency.

Revision note: revised this ExecPlan after a deeper code audit of the runtime tests, store guards, HTTP-adjacent helpers, and existing usecase lifecycle helpers. The update fixes missing public API coverage, adds concrete stale-state and lease invariants, distinguishes supervisor, completion, projected-dispatch, Git-operation, and convergence-system-action paths, and replaces speculative service guidance with code-grounded extraction steps tied to the helpers and tests that already exist.
