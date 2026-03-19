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
- [x] (2026-03-19 22:05Z) Re-audited `crates/ingot-agent-runtime/src/lib.rs`, `crates/ingot-agent-runtime/tests`, `crates/ingot-http-api/src/router/jobs.rs`, `crates/ingot-usecases/src/lib.rs`, `dispatch.rs`, `job_lifecycle.rs`, `reconciliation.rs`, and `convergence.rs`; confirmed the remaining plan gaps are module-root wiring for the proposed `dispatcher/` tree, missing shared private helper types in the split map, and missing `ingot-usecases` port/export updates for the later extractions.
- [x] (2026-03-19 21:02Z) Re-checked `recover_projected_review_jobs()`, `reconcile_assigned_job()`, and the current runtime crate test inventory from `cargo test -p ingot-agent-runtime -- --list`; confirmed the remaining gaps are explicit projected-review failure-isolation coverage and explicit assigned-versus-running recovery acceptance.
- [x] (2026-03-19 21:21Z) Split `crates/ingot-agent-runtime/src/lib.rs` into `crates/ingot-agent-runtime/src/dispatcher/` modules, preserved the crate-root API, and revalidated the runtime crate with `cargo check -p ingot-agent-runtime`.
- [x] (2026-03-19 21:21Z) Extracted projected follow-up dispatch and recovery scan policy into `crates/ingot-usecases/src/dispatch.rs`, kept `JobDispatcher::auto_dispatch_projected_review_locked()` as a facade, and revalidated with `cargo test -p ingot-agent-runtime --test auto_dispatch --test dispatch` plus `cargo test -p ingot-usecases`.
- [x] (2026-03-19 21:35Z) Extracted non-success outcome bookkeeping plus completion-activity policy into `crates/ingot-usecases/src/job_lifecycle.rs`, rewired runtime report, harness, and failure paths to use those helpers, removed the daemon-validation-local revision-context rebuild in favor of `refresh_revision_context_for_ids()`, and revalidated with `cargo test -p ingot-usecases`, `cargo check -p ingot-agent-runtime`, and `cargo test -p ingot-agent-runtime --lib --test escalation --test auto_dispatch --test dispatch --test reconciliation`.
- [x] (2026-03-19 22:13Z) Extracted the first preparation-policy slice into `crates/ingot-usecases/src/job_preparation.rs`, moving queued-or-current gating, runtime agent compatibility selection, daemon-validation gating, and assignment metadata helpers out of the runtime; rewired `prepare.rs` and `supervisor.rs`, tightened the cancellation wakeup dispatch test to use a bounded running-state wait instead of a fixed 500ms assumption, and revalidated with `cargo test -p ingot-usecases`, `cargo test -p ingot-agent-runtime --test dispatch`, and `cargo test -p ingot-agent-runtime --lib --test auto_dispatch --test reconciliation --test escalation`.
- [ ] Extract execution-preparation policy into `crates/ingot-usecases`.
- [ ] Extract Git-operation reconciliation and maintenance policy into `crates/ingot-usecases`.
- [ ] Extract execution-completion policy into `crates/ingot-usecases`.
- [ ] Reduce `JobDispatcher` further toward a thin facade over a supervisor plus explicit services, then run the broader Rust test and lint gates.

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

- Observation: several contested runtime mutations are protected by `ProjectLocks` plus re-fetch-and-check logic rather than by SQL compare-and-swap updates.
  Evidence: `prepare_run()`, `prepare_harness_validation()`, `reconcile_assigned_job()`, `reconcile_running_job()`, and `prepare_queue_head_convergence()` all acquire a project lock and re-load current rows before mutating them, while `Database::update_job()`, `update_workspace()`, `update_convergence()`, and `update_git_operation()` themselves have no revision or status guard.

- Observation: the runtime already has three internal adapter structs that bridge into `ingot-usecases`, so the cleanest extraction path is to move policy behind more ports, not to bypass those adapters.
  Evidence: `RuntimeConvergencePort`, `RuntimeFinalizePort`, and `RuntimeReconciliationPort` in `crates/ingot-agent-runtime/src/lib.rs` already implement `ConvergenceSystemActionPort`, `PreparedConvergenceFinalizePort`, and `ReconciliationPort`.

- Observation: `refresh_project_mirror()` logic is duplicated today between the runtime and the HTTP API support layer.
  Evidence: `crates/ingot-agent-runtime/src/lib.rs::refresh_project_mirror()` and `crates/ingot-http-api/src/router/support.rs::refresh_project_mirror()` both re-check unresolved finalize operations before calling `ensure_mirror`.

- Observation: retry lineage is already encoded in durable job fields and is exercised by tests adjacent to the runtime.
  Evidence: `crates/ingot-usecases/src/job.rs::dispatch_job()` and `retry_job()` set `semantic_attempt_no`, `retry_no`, and `supersedes_job_id`, and `crates/ingot-agent-runtime/tests/escalation.rs::successful_authoring_retry_clears_escalation_and_reopens_review_dispatch()` asserts successful retry behavior tied to `retry_no > 0`.

- Observation: preparation already freezes execution metadata into the durable job row before launch.
  Evidence: `prepare_run()` writes `JobAssignment` with `workspace_id`, `agent_id`, `prompt_snapshot`, and `phase_template_digest`, while `prepare_harness_validation()` assigns only the workspace and then transitions directly into `start_job_execution()`.

- Observation: `CompleteJobService` already carries retry and target-ref correctness rules that go beyond the store-layer stale-revision guard.
  Evidence: `crates/ingot-usecases/src/job.rs::CompleteJobService::execute()` calls `load_completed_job_completion()`, `verify_and_hold_target_ref()`, and `release_hold()`, and tests `completion_returns_matching_completed_job_as_idempotent_success`, `completion_holds_target_ref_through_transaction_apply`, and `completion_retry_after_post_commit_hold_release_failure_returns_job_not_active` assert those behaviors.

- Observation: abandoned-workspace recovery is not just “delete anything abandoned”; it preserves authoring and integration workspaces that still anchor unresolved findings for the workspace head.
  Evidence: `crates/ingot-agent-runtime/src/lib.rs::workspace_can_be_removed()` checks `FindingSubjectKind`, unresolved triage state, and the workspace head commit, and `crates/ingot-agent-runtime/tests/reconciliation.rs` covers both the `reconcile_startup_removes_abandoned_*_when_safe` and `reconcile_startup_retains_abandoned_*_with_untriaged_*_finding` cases.

- Observation: projected-review recovery has a failure-isolation contract, not just a dispatch contract.
  Evidence: `crates/ingot-agent-runtime/src/lib.rs::recover_projected_review_jobs()` logs and continues on per-project and per-item failures, and `crates/ingot-agent-runtime/tests/reconciliation.rs::reconcile_startup_continues_review_recovery_past_broken_project()` plus `crates/ingot-agent-runtime/tests/dispatch.rs::tick_runs_healthy_queued_job_even_when_another_project_is_broken()` assert that broken recovery candidates do not block healthy work.

- Observation: active-job recovery already has two intentionally different restart semantics that later extraction must keep separate.
  Evidence: `crates/ingot-agent-runtime/src/lib.rs::reconcile_assigned_job()` re-queues the job and releases its workspace to `Ready`, while `reconcile_running_job()` expires the job and marks the workspace `Stale`; `crates/ingot-agent-runtime/tests/reconciliation.rs::reconcile_startup_handles_mixed_inflight_states_conservatively()` asserts both outcomes in one startup pass.

- Observation: the mechanical runtime split was lower-risk than expected once the parent `dispatcher::mod.rs` owned the shared imports and helper types, because Rust allowed the `impl JobDispatcher` blocks to live across concern files with only a small number of visibility fixes.
  Evidence: after creating `crates/ingot-agent-runtime/src/dispatcher/{startup,supervisor,prepare,prompt,agent_execution,harness_execution,completion,workspace,git_ops,system_actions,projected_dispatch,ports,artifacts}.rs`, `cargo check -p ingot-agent-runtime` passed after wiring only method visibility and helper imports.

- Observation: projected-dispatch extraction did not require a new runtime-specific trait in `ingot-usecases`; a closure-based recovery helper plus `auto_dispatch_projected()` was enough to move the policy while keeping hydrated convergence loading in the runtime.
  Evidence: `crates/ingot-usecases/src/dispatch.rs` now owns `auto_dispatch_validation`, `auto_dispatch_projected`, and `recover_projected_jobs`, while `crates/ingot-agent-runtime/tests/auto_dispatch.rs` and `tests/dispatch.rs` stayed green unchanged.

- Observation: once completion activities moved into `ingot-usecases`, the daemon-only harness path no longer needed its private revision-context rebuild branch.
  Evidence: `crates/ingot-agent-runtime/src/dispatcher/harness_execution.rs` now calls `refresh_revision_context_for_ids()` after `append_job_completed_activity()` and `append_approval_requested_activity_if_needed()`, replacing the previous inline fetch or diff or `rebuild_revision_context()` sequence.

- Observation: the first preparation extraction split cleanly at “job eligibility and assignment metadata” rather than at workspace provisioning.
  Evidence: `crates/ingot-usecases/src/job_preparation.rs` now owns queued-or-current gating, runtime agent capability selection, daemon-validation gating, and assignment helpers, while `crates/ingot-agent-runtime/src/dispatcher/prepare.rs` still owns mirror refresh, harness profile loading, workspace provisioning, and prompt assembly.

- Observation: supervisor scheduling tests were already depending on tight timing assumptions, and the added preparation indirection made one of those assumptions visible.
  Evidence: `crates/ingot-agent-runtime/tests/dispatch.rs::run_forever_starts_next_job_after_running_job_cancellation` was asserting the second launch after a fixed 500ms wait; replacing that with `wait_for_job_status(..., JobStatus::Running, Duration::from_secs(2))` kept the existing behavior requirement while removing suite-load sensitivity.

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

- Decision: completion extraction must continue to route through `CompleteJobService::execute()` semantics, not a thinner wrapper around `apply_job_completion()`.
  Rationale: the current completion service already owns idempotent replay for matching completed jobs plus target-ref hold and release behavior for clean integrated validation. Re-implementing “completion” around only the apply mutation would regress retry and ref-safety behavior that existing usecase tests already lock down.
  Date/Author: 2026-03-19 / Codex

- Decision: treat projected-review recovery failure isolation and assigned-job restart behavior as explicit acceptance criteria, not incidental side effects.
  Rationale: both behaviors are already encoded in runtime tests and both are easy to regress if recovery extraction collapses all active-job handling or projected-review recovery errors into one generic usecase path.
  Date/Author: 2026-03-19 / Codex

- Decision: move projected-dispatch recovery into `ingot-usecases` with a closure-based helper instead of introducing a dedicated new port trait in the first extraction.
  Rationale: the runtime already needs to hydrate convergences against the mirror before dispatch evaluation, so a small usecase helper that accepts repositories, project locks, and a per-item async callback moved the policy without forcing a second abstraction layer that would only forward to the existing runtime facade.
  Date/Author: 2026-03-19 / Codex

- Decision: land completion extraction incrementally by moving durable non-success bookkeeping and post-completion activity rules before attempting to extract workspace-finalization or commit-materialization paths.
  Rationale: `CompleteJobService` already owns the durable report-completion semantics, while workspace cleanup and Git commit creation are explicitly runtime infrastructure. Moving the shared activity and escalation rules first removed duplication without forcing artificial usecase abstractions over filesystem-side effects.
  Date/Author: 2026-03-19 / Codex

- Decision: land preparation extraction incrementally by moving queue eligibility, runtime agent selection, and assignment metadata before attempting to move workspace or prompt assembly.
  Rationale: the durable “can this queued job launch now?” rules are application policy and duplicate across agent-backed and daemon-only paths, while mirror refresh, harness profile loading, workspace provisioning, and prompt rendering still depend directly on filesystem and subprocess infrastructure.
  Date/Author: 2026-03-19 / Codex

## Outcomes & Retrospective

The initial runtime-only split, the projected-dispatch extraction, the first completion-policy extraction slice, and the first preparation-policy extraction slice are now complete. `crates/ingot-agent-runtime/src/lib.rs` is a small crate root, the runtime implementation lives under `crates/ingot-agent-runtime/src/dispatcher/`, `crates/ingot-usecases/src/dispatch.rs` owns projected review or validation selection plus projected-dispatch recovery sequencing, `crates/ingot-usecases/src/job_lifecycle.rs` now owns shared non-success outcome bookkeeping plus job-completed, approval-requested, and escalation-cleared activity rules, and `crates/ingot-usecases/src/job_preparation.rs` now owns queued-or-current gating, compatible runtime agent selection, daemon-validation gating, and assignment metadata helpers.

The current verified state is:

- `cargo check -p ingot-agent-runtime` passes after the split
- `cargo test -p ingot-agent-runtime --test auto_dispatch --test dispatch` passes
- `cargo test -p ingot-agent-runtime --lib --test escalation --test auto_dispatch --test dispatch --test reconciliation` passes after the completion-policy extraction slice
- `cargo test -p ingot-agent-runtime --test dispatch` and `cargo test -p ingot-agent-runtime --lib --test auto_dispatch --test reconciliation --test escalation` pass after the preparation-policy extraction slice
- `cargo test -p ingot-usecases` passes

The remaining work is still substantial. Execution preparation still keeps mirror refresh, harness profile loading, workspace provisioning, and prompt assembly in the runtime; Git-operation reconciliation and the remaining completion branches around commit materialization and workspace-finalization sequencing also still live primarily in runtime modules, even though the file breakup now makes those extractions much smaller and more reviewable.

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

Plan revision note (2026-03-19 22:13Z): updated progress, discoveries, decisions, and outcomes after moving the first preparation-policy slice into `ingot-usecases::job_preparation`, rewiring runtime preparation helpers to use those shared decisions, stabilizing the cancellation wakeup dispatch test around a bounded running-state wait, and recording the expanded validation set that now passes.

## Context and Orientation

`crates/ingot-agent-runtime/src/lib.rs` is currently 5,274 lines long and defines `JobDispatcher`. A “dispatcher” in this repository is the long-running daemon component that wakes up, finds queued jobs, prepares their workspaces, launches work, refreshes heartbeats, and cleans up after interrupted or completed jobs.

The current public runtime surface is:

- `pub struct DispatcherConfig`
- `pub trait AgentRunner`
- `pub struct CliAgentRunner`
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
- `pub enum RuntimeError`

Today the runtime source tree contains only `crates/ingot-agent-runtime/src/lib.rs` and `crates/ingot-agent-runtime/src/bootstrap.rs`. Because this plan proposes files under `crates/ingot-agent-runtime/src/dispatcher/`, Milestone 1 must explicitly create `crates/ingot-agent-runtime/src/dispatcher/mod.rs` and wire `lib.rs` to it. Without that module root, the proposed split does not compile.

The current `lib.rs` also owns the bottom-of-file unit tests for `drain_until_idle`, `output_schema_for_job`, `result_schema_version`, and the schema helper functions. The split therefore has to move or preserve those tests intentionally instead of assuming that `cargo test -p ingot-agent-runtime --lib` will keep passing automatically after the file move.

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

Adjacent code that already encodes patterns this refactor should reuse includes:

`crates/ingot-http-api/src/router/jobs.rs`, which already composes `CompleteJobService`, `job_lifecycle`, revision-context refresh, and projected review dispatch after job mutations.

`crates/ingot-usecases/src/job_lifecycle.rs`, which centralizes guarded cancel, fail, and expire mutations for active jobs.

`crates/ingot-usecases/src/teardown.rs`, which centralizes lane teardown across jobs, convergences, queue entries, and Git operations.

`crates/ingot-agent-runtime/tests/common/mod.rs`, which provides the runtime test harness and fake runner patterns that the current runtime tests already use.

`crates/ingot-agent-runtime/tests/dispatch.rs`, which locks down supervisor behavior such as timeouts, capacity filling, workspace-busy skips, and the requirement that healthy queued work still runs even if projected-review recovery hits a broken project first.

`crates/ingot-agent-runtime/src/bootstrap.rs`, which already isolates default-agent bootstrap behavior and has dedicated tests for bootstrap creation, idempotence, and unavailable-agent persistence.

`apps/ingot-daemon/src/main.rs` and `crates/ingot-http-api/tests/job_routes.rs`, which both import `ingot_agent_runtime::{DispatcherConfig, JobDispatcher}` directly. The runtime split cannot treat public re-exports as an internal detail because those callers compile against the crate root today.

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

The `job_missing_workspace` branch matters directly to preparation extraction, because `prepare_run()` and `prepare_harness_validation()` are responsible for binding a workspace before `start_job_execution()`.

### Preparation freeze guard

The durable execution-freeze fields in the job row are:

- `workspace_id`
- `agent_id` when an agent-backed job is used
- `prompt_snapshot`
- `phase_template_digest`

`prepare_run()` assigns all four through `JobAssignment` before execution starts. `prepare_harness_validation()` assigns only the workspace because daemon-only validation has no agent or prompt snapshot. Any preparation extraction must preserve that distinction and must not defer these writes until after execution begins.

### Lease guard

The durable lease fields are `lease_owner_id`, `heartbeat_at`, and `lease_expires_at`.

- `run_with_heartbeats()` starts execution with `lease_owner_id = self.lease_owner_id`, then refreshes heartbeats through `heartbeat_job_execution()`.
- `prepare_harness_validation()` starts daemon-only validation with `agent_id = None` but the same `lease_owner_id = self.lease_owner_id`.
- `refresh_daemon_validation_heartbeat()` also uses `self.lease_owner_id`.
- `reconcile_running_job()` expires a running job when either `lease_expires_at` is stale or `lease_owner_id` does not match the current dispatcher.

This is important because it means a recovery extraction must preserve the current “foreign owner means expired” rule. It also means a usecase extraction cannot drop `lease_owner_id` on the floor and treat a heartbeat update as a generic running-job write.

### Retry-lineage guard

The durable retry and supersession fields are:

- `semantic_attempt_no`
- `retry_no`
- `supersedes_job_id`

These are created and advanced in `crates/ingot-usecases/src/job.rs::dispatch_job()` and `retry_job()`. They are then consumed by runtime behavior such as escalation clearing, because `should_clear_item_escalation_on_success()` depends on `job.retry_no > 0`.

Any extraction of projected dispatch or completion policy must continue using the existing usecase job-construction helpers so those lineage fields remain correct. Do not create follow-up or retry jobs by hand in the runtime or in a new service.

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

### Completion replay and target-ref hold guard

`CompleteJobService` currently preserves two extra completion invariants that any extraction must keep intact:

- matching retries against an already completed report job return idempotent success through `load_completed_job_completion()`
- clean `validate_integrated` completion acquires a target-ref hold through `verify_and_hold_target_ref()` before `apply_job_completion()`, then attempts `release_hold()` even if apply fails

The existing tests in `crates/ingot-usecases/src/job.rs` also assert one more edge: if apply succeeds but hold release fails, a retry must return `JobNotActive` instead of reapplying completion. Any new completion service or wrapper must preserve that exact retry behavior. Do not rebuild completion around an ad hoc `apply_job_completion()` call that drops replay detection or target-ref hold handling.

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

### Persisted Git-operation metadata compatibility

The runtime writes replay metadata into unresolved `git_operations` rows through `ConvergenceReplayMetadata`, especially in `fail_prepare_convergence_attempt()`. Those rows can survive a daemon restart and are then revisited by startup reconciliation.

That means this refactor must keep existing `git_operations` payload and metadata shapes readable across deploys. Do not change `ConvergenceReplayMetadata` layout or the meaning of unresolved prepare or finalize operations as part of the extraction unless there is an explicit migration and the plan is updated accordingly.

### Workspace-state guard asymmetry

Different paths deliberately leave workspaces in different states:

- `reconcile_assigned_job()` re-queues the job and releases the workspace to `Ready`.
- `reconcile_running_job()` expires the job and marks the workspace `Stale`.
- `fail_run()` resets the workspace filesystem and then releases or abandons the workspace based on `WorkspaceLifecycle`.
- `cleanup_supervised_task()` uses `fail_run()` for agent-backed jobs but writes a non-success row and marks the workspace `Stale` for daemon-only validation jobs.
- `job_lifecycle::cancel_job()`, `fail_job()`, `expire_job()`, and `teardown_revision_lane()` release workspaces through generic repository updates.

This asymmetry is real and tested. The extraction must not collapse all termination paths onto one helper unless that helper can preserve each distinct resulting state.

### Lock-and-reload guard

Not every stateful path in this codebase is protected by a single SQL compare-and-swap. Several important mutations depend on:

- acquiring `ProjectLocks`
- re-fetching the current row after the lock
- re-checking status, revision, queue-head identity, or activity state in Rust
- only then calling `update_*()`

This pattern exists in `prepare_run()`, `prepare_harness_validation()`, `reconcile_assigned_job()`, `reconcile_running_job()`, `prepare_queue_head_convergence()`, and parts of `cleanup_supervised_task()`.

Because `Database::update_job()`, `update_workspace()`, `update_convergence()`, and `update_git_operation()` do not enforce their own revision or status guard, any extraction that moves those paths must preserve the current lock-plus-reload structure. Treat that structure as part of the invariant, not as incidental style.

### Projected-review recovery failure-isolation guard

`recover_projected_review_jobs()` is not allowed to fail closed on the first broken recovery candidate. Its current contract is:

- load all projects
- hold each project lock while scanning that project’s items
- log and continue if `list_items_by_project()` fails for one project
- log and continue if `auto_dispatch_projected_review_locked()` fails for one item
- still let `tick()` or `run_supervisor_iteration()` move on to healthy queued job execution afterward

The tests that prove this are:

- `crates/ingot-agent-runtime/tests/reconciliation.rs::reconcile_startup_continues_review_recovery_past_broken_project()`
- `crates/ingot-agent-runtime/tests/dispatch.rs::tick_runs_healthy_queued_job_even_when_another_project_is_broken()`

If projected-review recovery becomes usecase-owned, preserve that “warn and continue” behavior and its current steady-state and startup coverage. Do not replace it with a bulk operation that aborts all remaining recovery work on the first repository or hydration error.

### Startup progress guard

Startup has its own progress contract:

- `reconcile_startup()` bootstraps the default agent through `bootstrap::ensure_default_agent()`
- then runs maintenance reconciliation
- then drains system actions through `drain_until_idle(|| self.tick_system_action())`
- then attempts projected review recovery

The current tests also assert two progress properties:

- blocked auto-finalize must not cause startup drain to spin forever
- `tick()` must not report progress when blocked auto-finalize made no durable change

Any startup or system-action extraction must preserve those progress semantics.

## Plan of Work

Begin with a behavior-preserving runtime-only split. Create internal runtime modules and move code out of `crates/ingot-agent-runtime/src/lib.rs` while keeping the current public API stable. This first step is mechanical on purpose. It reduces merge conflict pressure and makes later semantic extraction reviewable.

The runtime split should reflect the clusters that already exist in the code today:

`crates/ingot-agent-runtime/src/lib.rs`

- keep the public surface and re-exports only
- keep `mod bootstrap;`
- add `mod dispatcher;`
- either keep `DispatcherConfig`, `AgentRunner`, `CliAgentRunner`, `JobDispatcher`, and `RuntimeError` defined here or move them into `crates/ingot-agent-runtime/src/dispatcher/mod.rs` and re-export them from `lib.rs`; do not make integration tests chase renamed public paths during the mechanical split

`crates/ingot-agent-runtime/src/dispatcher/mod.rs`

- declare `mod startup;`, `mod ports;`, `mod supervisor;`, `mod prepare;`, `mod prompt;`, `mod agent_execution;`, `mod harness_execution;`, `mod workspace;`, `mod completion;`, `mod git_ops;`, `mod system_actions;`, `mod projected_dispatch;`, and `mod artifacts;`
- host the shared private runtime types that are used across multiple leaf modules but are not part of the public crate surface:
  - `FinalizeCompletionOutcome`
  - `FinalizeOperationContext`
  - `NonJobWorkProgress`
  - `RunningJobResult`
  - `RunningJobMeta`
- keep the leaf-module boundaries honest: if a helper type is only used by one concern, move it down into that leaf instead of recreating a second catch-all file

`crates/ingot-agent-runtime/src/dispatcher/startup.rs`

- move `reconcile_startup`
- move `drain_until_idle`
- keep `bootstrap.rs` as the bootstrap-specific helper module rather than folding that code into a generic system-actions file
- keep startup-specific tests and helper moves explicit, because current `lib.rs` unit tests already cover `drain_until_idle`

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
- move `run_prepared_agent_job`
- move `run_prepared_harness_validation_job`
- move `NonJobWorkProgress`
- move `RunningJobResult`
- move `RunningJobMeta`
- preserve the current `tick()` behavior where `system_actions_progressed` causes an early return before launching a job
- preserve the current top-level spawned-helper shape, because `JoinSet::spawn(...)` currently depends on owned wrapper futures that take `JobDispatcher`, prepared state, and `OwnedSemaphorePermit`

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
- move `HarnessCommandResult`
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
- move `FinalizeCompletionOutcome`
- move `FinalizeOperationContext`

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

After the split is green, start semantic extraction in the order that best matches the code’s current duplication and guards. Every usecase-owned extraction in the later milestones must update `crates/ingot-usecases/src/lib.rs` as part of the same change, because that file is the current re-export surface for `CompleteJobService`, `ConvergenceService`, `DispatchNotify`, `ProjectLocks`, `ReconciliationService`, and `rebuild_revision_context`.

First extract projected follow-up dispatch into `ingot-usecases`, because the runtime already delegates review auto-dispatch to `ingot_usecases::dispatch::auto_dispatch_review()` but still owns the validation half and the item-wide recovery scan. Model the new code after the existing `dispatch.rs` helpers and keep `JobDispatcher::auto_dispatch_projected_review_locked()` as a public facade that delegates into the new service.

That extraction must keep the current `hydrate_convergences()` step or an equivalent guard, because projected dispatch today evaluates on hydrated `target_head_valid` data rather than on raw convergence rows.
It must also account for the current shape of `crates/ingot-usecases/src/reconciliation.rs`: `ReconciliationPort` and `ReconciliationService` currently own only Git-operation, active-job, active-convergence, and workspace-retention maintenance. If projected-review recovery becomes usecase-owned, extend that surface deliberately instead of hiding the recovery scan behind a runtime-only helper that defeats the refactor.
It must also preserve the current failure-isolation behavior of `recover_projected_review_jobs()`, because both startup and steady-state `tick()` currently continue past broken projects or broken items and still process healthy work in the same pass.

Second extract execution completion policy into `ingot-usecases`, but compose existing helpers instead of replacing them blindly:

- reuse `CompleteJobService` for report and harness-validation completion
- reuse `CompleteJobService::execute()` semantics, including `load_completed_job_completion()` replay detection and target-ref hold or release around clean integrated validation
- reuse `job_lifecycle` helpers where the resulting status and workspace semantics match the runtime behavior
- reuse the concrete sequence already present in `crates/ingot-http-api/src/router/jobs.rs::complete_job()` and adjacent helpers for “complete job, refresh revision context, append completion or escalation activities, optionally request approval, then trigger projected review dispatch”
- do not replace `reconcile_running_job()` with `job_lifecycle::expire_job()` without first deciding whether to preserve current `heartbeat_expired` plus `WorkspaceStatus::Stale` semantics or to intentionally change them and update tests
- do not bypass the existing `PreparedConvergenceGuard` flow for clean `validate_integrated`

Third extract execution preparation policy into `ingot-usecases`. That service should decide whether the job is launchable and should return the durable execution facts that the runtime needs, but it should not perform worktree provisioning or process launching. Reuse the existing `ingot_usecases::dispatch` helpers for candidate-subject derivation instead of re-implementing them.

That extraction must also preserve the current lock-and-reload behavior for contested preparation paths, because the store-layer `update_*()` helpers do not provide their own revision or status compare-and-swap semantics for those updates. It must also preserve the current job freeze behavior, including durable `prompt_snapshot` and `phase_template_digest` assignment before agent-backed execution starts.

Fourth extract recovery policy into `ingot-usecases`. That includes:

- Git-operation adoption decisions
- active-job recovery decisions
- active-convergence recovery decisions
- projected-review recovery sequencing
- workspace-retention eligibility decisions

This extraction should compose existing `job_lifecycle` and `teardown` helpers where they match the runtime behavior, not create a second unrelated state machine.
It should also decide explicitly whether projected-review recovery becomes a new `ReconciliationPort` stage or a dedicated usecase service invoked by both `reconcile_startup()` and `drive_non_job_work()`. The existing order is code, not commentary: startup currently runs bootstrap, then `ReconciliationService::reconcile_startup()`, then `drain_until_idle(|| self.tick_system_action())`, then `recover_projected_review_jobs()`, while steady-state supervisor iterations run maintenance, then system actions, then projected-review recovery before job launch.
It must keep assigned-job recovery distinct from running-job expiry. Today `reconcile_assigned_job()` re-queues and releases to `WorkspaceStatus::Ready`, while `reconcile_running_job()` expires and marks `WorkspaceStatus::Stale`; the mixed-state startup test covers both outcomes in one pass.

The HTTP API is adjacent code but is not the primary target of this refactor. Do not widen scope into `crates/ingot-http-api/src/router/dispatch.rs` or `router/convergence.rs` unless a shared helper must move to keep runtime and HTTP behavior aligned. If that becomes necessary, record it explicitly in this plan before doing it.

## Milestones

### Milestone 1: Split the runtime file without changing behavior

At the end of this milestone, the public API of `crates/ingot-agent-runtime` should behave exactly as before, but `crates/ingot-agent-runtime/src/lib.rs` should no longer contain the whole implementation. Contributors should be able to open `ports.rs`, `supervisor.rs`, `prepare.rs`, `prompt.rs`, `agent_execution.rs`, `harness_execution.rs`, `workspace.rs`, `completion.rs`, `git_ops.rs`, `system_actions.rs`, `projected_dispatch.rs`, and `artifacts.rs` and find one coherent concern per file.

This milestone must preserve:

- the current public runtime types, not just the methods
- the current public methods on `JobDispatcher`
- the current `bootstrap.rs` boundary and behavior
- the current `DispatchNotify` watch-based wakeup semantics
- the current `tick()` early return after system-action progress
- the current test-observed output artifact paths and names
- the current lease-owner and stale-revision guards
- the current `drain_until_idle_*` and schema unit tests that still live in `crates/ingot-agent-runtime/src/lib.rs`
- a compilable module tree rooted at `crates/ingot-agent-runtime/src/dispatcher/mod.rs`
- the current crate-root imports used by `apps/ingot-daemon/src/main.rs` and `crates/ingot-http-api/tests/job_routes.rs`

Run `cargo test -p ingot-agent-runtime --lib`, `cargo test -p ingot-agent-runtime`, `cargo test -p ingot-http-api --test job_routes`, and `cargo check -p ingot-daemon` after the split. Acceptance for this milestone is behavioral parity, a much smaller `lib.rs`, and unchanged crate-root imports for the daemon and HTTP test harness.

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
- the daemon-only validation and invalid-harness cases later in the same file, because projected follow-up dispatch shares completion and recovery edges with those paths
- the current use of hydrated convergence validity rather than raw convergence rows
- `crates/ingot-agent-runtime/tests/escalation.rs::successful_authoring_retry_clears_escalation_and_reopens_review_dispatch()`
- `crates/ingot-agent-runtime/tests/dispatch.rs::tick_runs_healthy_queued_job_even_when_another_project_is_broken()`, because the projected-review recovery scan must keep logging and continuing instead of blocking unrelated queued work

Run `cargo test -p ingot-agent-runtime --test auto_dispatch`, `cargo test -p ingot-agent-runtime --test dispatch`, `cargo test -p ingot-agent-runtime --test escalation`, and `cargo test -p ingot-usecases`. Acceptance is that the runtime public facade still passes the same tests while the policy lives in `ingot-usecases`, including the “broken recovery candidate does not block healthy queued work” path.

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
- successful retry clearing escalation through `retry_no > 0`

It must also preserve the stale-state guards around `expected_item_revision_id`, `lease_owner_id`, and `PreparedConvergenceGuard`.

This milestone also includes the daemon-only validation completion surface currently covered in `crates/ingot-agent-runtime/tests/auto_dispatch.rs`, including:

- `daemon_only_validation_job_executes_on_tick`
- `run_forever_executes_daemon_only_validation_job`
- `run_forever_refreshes_heartbeat_for_daemon_only_validation_job`
- `run_forever_cancels_daemon_only_validation_command`
- `daemon_only_validation_command_completes_even_when_heartbeat_interval_exceeds_command_timeout`
- `harness_validation_timeout_kills_background_processes`
- `harness_validation_with_commands_produces_findings_on_failure`

This milestone must also preserve the completion-service behaviors already tested in `crates/ingot-usecases/src/job.rs`, especially:

- `completion_holds_target_ref_through_transaction_apply`
- `completion_retry_after_post_commit_hold_release_failure_returns_job_not_active`
- `completion_returns_matching_completed_job_as_idempotent_success`

Run `cargo test -p ingot-agent-runtime --test dispatch`, `cargo test -p ingot-agent-runtime --test escalation`, `cargo test -p ingot-agent-runtime --test auto_dispatch`, and `cargo test -p ingot-usecases`. Acceptance is that the runtime still produces the same terminal job states, activities, escalation behavior, and follow-up dispatch while the policy lives in `ingot-usecases`.

### Milestone 4: Move execution-preparation policy into usecases

At the end of this milestone, the runtime should still provision workspaces and launch work, but it should receive a prepared execution plan rather than deciding launchability itself.

This milestone must preserve:

- the current stale-revision checks in `prepare_run()` and `prepare_harness_validation()`
- the current `AdapterKind::Codex` selection filter
- the current `WorkspaceError::Busy` handling in the supervisor
- the current integration-workspace lookup from prepared convergence for daemon-only validation
- the current prompt contract, including repo-local skill inclusion and invalid-harness failure behavior
- the current job-lineage behavior for projected and retry-created jobs, including `semantic_attempt_no`, `retry_no`, and `supersedes_job_id`
- the current job freeze fields, especially `prompt_snapshot` and `phase_template_digest`, for agent-backed launches

Preparation-specific regressions that must stay green include:

- `daemon_only_validation_fails_on_invalid_harness_profile`
- `queued_authoring_job_fails_on_invalid_harness_profile`
- `authoring_prompt_includes_resolved_repo_local_skill_files`
- `queued_authoring_job_fails_when_harness_skill_glob_escapes_repo`
- `queued_authoring_job_fails_when_repo_local_skill_symlink_points_outside_repo`
- `daemon_validation_resyncs_authoring_workspace_before_running_harness`
- `daemon_validation_resyncs_integration_workspace_before_running_harness`

Run `cargo test -p ingot-agent-runtime --test dispatch`, `cargo test -p ingot-agent-runtime --test auto_dispatch`, and `cargo test -p ingot-usecases`. Acceptance is that preparation behavior is unchanged but the decision logic now lives in `ingot-usecases`.

### Milestone 5: Move recovery and convergence-system-action policy into usecases and leave JobDispatcher as a facade

At the end of this milestone, `JobDispatcher` should mainly wire services and delegate, while Git-operation recovery, active-job and active-convergence recovery, workspace-retention decisions, projected-review recovery, and convergence system-action decisions are usecase-owned.

This milestone must preserve the behaviors tested in:

- `crates/ingot-agent-runtime/tests/reconciliation.rs`
- `crates/ingot-agent-runtime/tests/convergence.rs`

That includes:

- re-queuing assigned jobs and releasing their workspaces to `Ready`
- expiring stale running jobs and marking their workspaces stale
- continuing projected review recovery past broken projects or items
- adopting create-job-commit, prepare-convergence, reset-workspace, and remove-workspace-ref operations
- finalizing prepared convergence only when target-ref and checkout state allow it
- leaving blocked finalize operations unresolved
- invalidating stale prepared convergence
- preserving `fail_prepare_convergence_attempt()` semantics, including queue-entry release and replay metadata
- preserving the current lock-plus-reload structure in contested mutating paths that use `update_job()`, `update_workspace()`, `update_convergence()`, or `update_git_operation()`
- removing abandoned review and done-item authoring workspaces only when `workspace_can_be_removed()` says they are safe
- retaining abandoned authoring or integration workspaces when unresolved candidate or integrated findings still match the workspace head
- preserving blocked-finalize progress semantics, including:
  - `reconcile_startup_does_not_spin_when_auto_finalize_is_blocked`
  - `tick_reports_no_progress_when_auto_finalize_is_blocked`
- conservative mixed-state startup recovery, specifically `reconcile_startup_handles_mixed_inflight_states_conservatively`

Run `cargo test -p ingot-agent-runtime --test reconciliation`, `cargo test -p ingot-agent-runtime --test convergence`, and `cargo test -p ingot-usecases`. Acceptance is that those flows stay green while `JobDispatcher` becomes a thin façade.

## Concrete Steps

Work from the repository root, meaning the directory that contains `Cargo.toml`, `Makefile`, `.agent/`, and `crates/`. In this checkout that root is `/Users/aa/.codex/worktrees/1cae/ingot`; keep the commands portable rather than baking in a machine-specific worktree path.

Before editing, inspect the current tree:

    git status --short

Record the pre-split crate-root size:

    wc -l crates/ingot-agent-runtime/src/lib.rs

For a fast Milestone 1 loop, run:

    cargo test -p ingot-agent-runtime
    cargo test -p ingot-http-api --test job_routes
    cargo check -p ingot-daemon

Expected success signal:

    test result: ok

For Milestone 2, after the projected-dispatch extraction, run:

    cargo test -p ingot-agent-runtime --test auto_dispatch
    cargo test -p ingot-agent-runtime --test dispatch
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

If UI dependencies are missing, run:

    make ui-install

before `make lint` or `make ci`.

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

`apps/ingot-daemon/src/main.rs` and `crates/ingot-http-api/tests/job_routes.rs` still compile against `ingot_agent_runtime::{DispatcherConfig, JobDispatcher}` without path churn or helper shims.

The runtime still exposes the same public types and passes tests for `refresh_project_mirror`, `reconcile_active_jobs`, `fail_prepare_convergence_attempt`, and `auto_dispatch_projected_review_locked`.

Startup still preserves:

- empty-registry default-agent bootstrap
- idempotent no-op when agents already exist
- persistence of an unavailable bootstrapped agent when CLI probe fails
- blocked-finalize startup drain that does not spin forever
- projected review recovery that logs and continues past broken projects or items

The runtime still launches queued mutating, read-only, and daemon-only validation jobs correctly through both `tick()` and `run_forever()`, including:

- healthy queued-job execution even when projected-review recovery hits a broken project first
- timeouts
- cancellation wakeups
- heartbeat refresh
- workspace-busy skip behavior
- stale-head queued-job skip behavior
- harness validation cancellation and timeout cleanup
- timed-out harness commands not leaving background writers alive after the job has already completed

The extracted code still preserves the current stale-state guards for:

- `expected_item_revision_id`
- `lease_owner_id`
- `PreparedConvergenceGuard`
- queue-entry identity and `Head` status
- item `current_revision_id` checks inside convergence preparation and item-closing adoption paths
- retry lineage fields `semantic_attempt_no`, `retry_no`, and `supersedes_job_id` where follow-up or retry job creation is involved
- lock-plus-reload protections on paths that currently rely on `ProjectLocks` rather than SQL compare-and-swap updates
- durable preparation freeze fields such as `prompt_snapshot` and `phase_template_digest` for agent-backed jobs

Projected review and projected validation auto-dispatch still happen after the same job outcomes as before, but the policy lives in `ingot-usecases`.

Report completion and harness-validation completion still go through the prepared-convergence-safe completion path, preserve matching completed-job replay as idempotent success, and do not regress integrated validation stale protection or target-ref hold release.

Git-operation reconciliation, active-job recovery, active-convergence recovery, and abandoned-workspace cleanup still pass the existing tests, including the authoring and integration retention cases where unresolved findings require keeping the abandoned workspace, but the runtime crate no longer owns those state-machine decisions directly.

Assigned-job and running-job recovery remain intentionally different under startup and maintenance reconciliation: assigned jobs are re-queued and release their workspaces to `Ready`, while stale running jobs expire and mark their workspaces `Stale`.

The current distinct termination outcomes remain distinct unless an intentional behavior change is made and tested. In particular, runtime running-job expiry must not silently turn into `job_lifecycle::expire_job()` behavior unless that change is deliberate and reflected in the tests.

Completion extraction also remains correct under replay and integrated-validation ref protection: matching already-completed report retries stay idempotent, clean integrated validation still holds and releases the target ref around apply, and a post-apply hold-release failure still prevents a second apply on retry.

## Idempotence and Recovery

This plan intentionally starts with a behavior-preserving file split so that later extractions have smaller, safer diffs. Each milestone should be independently mergeable and should leave the repository compiling and tests passing before the next milestone begins.

If a milestone stalls midway, recover in this order:

1. Restore compilation inside the crate currently being moved.
2. Restore the targeted test binary or crate that covers the moved path.
3. Delete any duplicate old branch only after the new path is green.

Do not leave two authoritative copies of the same policy. Temporary delegation shims are acceptable. Long-lived duplicated state-machine logic is not.

No database schema changes are required by this plan. The work is code-only and safe to retry. The main risk is semantic drift during extraction, especially when a nearby helper appears reusable but does not preserve the exact current workspace-state or error-code semantics. Whenever that happens, prefer a thin delegating wrapper over a behavior-changing substitution.

Unresolved `git_operations` can outlive a deploy, so retry safety also depends on preserving the current readable metadata shape for `ConvergenceReplayMetadata`. If that metadata shape must change, stop and extend this plan to cover compatibility or migration work before proceeding.

## Artifacts and Notes

The most important proof points to record while implementing this plan are:

- a before-and-after line count for `crates/ingot-agent-runtime/src/lib.rs`
- the exact `lib.rs` to `dispatcher/mod.rs` wiring chosen for the split
- a note showing which current runtime concern moved into which new file in Milestone 1
- a note showing where the current `drain_until_idle` and schema unit tests moved during the split
- a note showing where startup code and `bootstrap.rs` interactions ended up after the split
- a note showing which extracted usecase code owns projected dispatch, completion policy, preparation policy, and recovery policy after the later milestones
- the exact guards preserved for each extracted path, especially `expected_item_revision_id`, `lease_owner_id`, and `PreparedConvergenceGuard`
- a note showing where `CompleteJobService` replay detection and target-ref hold or release behavior live after completion extraction
- a note showing which recovery path still removes abandoned workspaces and which path intentionally retains them when unresolved findings still point at the workspace head
- a note showing where projected-review recovery now preserves the current “warn and continue” behavior for broken projects or broken items
- any behavior that had to remain runtime-owned at the end, with a code-based reason

## Interfaces and Dependencies

The final code must preserve the existing public runtime surface listed in `Context and Orientation`.

The plan’s later milestones necessarily touch `crates/ingot-usecases/src/lib.rs` and at least one of `crates/ingot-usecases/src/dispatch.rs`, `crates/ingot-usecases/src/job.rs`, `crates/ingot-usecases/src/job_lifecycle.rs`, `crates/ingot-usecases/src/reconciliation.rs`, or `crates/ingot-usecases/src/convergence.rs`, because those are the files that currently define the reusable helpers, port traits, and exported services the runtime already composes. Do not invent a parallel hidden export path inside the runtime crate.

The internal runtime split should follow existing code patterns rather than inventing a new framework. Use the current `CompleteJobService` in `crates/ingot-usecases/src/job.rs` as the model for new service extraction. Use `crates/ingot-usecases/src/job_lifecycle.rs` and `crates/ingot-usecases/src/teardown.rs` as the reference for guarded lifecycle mutations. Use `crates/ingot-usecases/src/dispatch.rs` for projected-dispatch and candidate-subject helpers instead of duplicating that logic.

The runtime crate should continue to use:

- `ingot_git` for raw Git effects
- `ingot_workspace` for worktree provisioning and removal
- `DispatchNotify` for watch-based wakeups
- `ProjectLocks` for project mutation serialization

The existing runtime test harness conventions that should remain usable through the split are:

- `TestHarness`
- `TestHarness::with_config`
- `dispatch_notify`
- `wait_for_job_status`
- `wait_for_running_jobs`
- `BlockingRunner`

The runtime crate should not add a third implementation of mirror refresh logic. If the refactor exposes a shared helper, use it to remove duplication between the runtime and `crates/ingot-http-api/src/router/support.rs`. If that deduplication is not needed for this refactor, leave both existing copies alone and do not create a new one.

When extracting completion and recovery policy, prefer composing:

- `CompleteJobService`
- `job_lifecycle`
- `teardown_revision_lane`
- `ingot_usecases::dispatch` helpers

before introducing new durable mutation helpers. Those existing utilities already encode significant parts of the repository’s guarded state-transition behavior.

After Milestone 1, parallel work should follow these write-ownership rules:

One agent owns:

- `crates/ingot-agent-runtime/src/dispatcher/startup.rs`
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

Revision note: revised this ExecPlan again after auditing the HTTP job routes, convergence queue store helpers, convergence store helpers, daemon-only validation tests, and the direct `update_*()` store methods. The update adds retry-lineage coverage, calls out the current lock-plus-reload mutation pattern as an invariant, includes the spawned `JoinSet` helper functions in the runtime split, and ties completion extraction more explicitly to the patterns already used in `crates/ingot-http-api/src/router/jobs.rs`.

Revision note: revised this ExecPlan once more after checking how preparation freezes prompt and template metadata, how projected dispatch depends on hydrated convergence validity, and how unresolved `git_operations` carry replay metadata across restarts. The update adds those concrete invariants and compatibility constraints without changing the plan’s intent.

Revision note: revised this ExecPlan after reconciling three parallel code audits. This update corrects startup ownership by moving `reconcile_startup` and `drain_until_idle` out of the generic system-actions bucket, adds `bootstrap.rs` and lib-level unit-test preservation, expands the milestone test matrix to cover retry reopening, daemon-only validation completion, preparation-specific harness and skill cases, blocked-finalize progress semantics, and broken-project recovery continuation, and adds the `job_missing_workspace` preparation guard plus the existing HTTP completion sequence as explicit extraction references.

Revision note: revised this ExecPlan again after re-checking the current runtime and usecase module boundaries. This update adds the missing `crates/ingot-agent-runtime/src/dispatcher/mod.rs` wiring required for the proposed split, names the remaining shared private helper types that must move with Milestone 1, and makes `crates/ingot-usecases/src/lib.rs` plus `reconciliation.rs` explicit touchpoints for the later projected-dispatch and recovery extractions so the plan matches the code that exists today.

Revision note: revised this ExecPlan after auditing `CompleteJobService`, the harness-timeout runtime tests, and the abandoned-workspace retention paths in reconciliation. The update adds the existing completion replay and target-ref hold invariants, includes the background-process timeout regression and safe-remove versus retain workspace recovery cases in milestone acceptance, and replaces the worktree-specific command header with a portable repository-root instruction.

Revision note: finalized this ExecPlan after reconciling three GPT-5.4 high-reasoning review passes against the current repository state. The final pass kept the plan’s intent unchanged but carried the verified runtime-test and completion-lifecycle findings through to Validation and Artifacts so the observable acceptance criteria now explicitly cover harness-timeout cleanup, completed-job replay idempotence, integrated target-ref hold release, and abandoned-workspace retention.

Revision note: reconciled five additional GPT-5.4 high-reasoning audits against the live repository and found the plan body already aligned on the runtime invariants, recovery paths, and usecase boundaries they inspected. The only remaining code-grounded gap was external compile coverage for the runtime crate root, so this update adds `apps/ingot-daemon/src/main.rs` and `crates/ingot-http-api/tests/job_routes.rs` as Milestone 1 preservation and verification targets.

Revision note: re-ran the ExecPlan audit with five isolated GPT-5.4 high-reasoning passes, then diffed their rewritten plans against the shared tree and re-checked the cited runtime, usecase, store, daemon, and HTTP files directly. After adding the crate-root import coverage above, the five-pass reconciliation found no further code-grounded corrections or lifecycle gaps to add without introducing churn.

Revision note: revised this ExecPlan again after re-checking `recover_projected_review_jobs()`, `reconcile_assigned_job()`, and the current runtime test inventory. This update adds the missing projected-review failure-isolation invariant, makes assigned-job requeue versus running-job expiry an explicit recovery acceptance criterion, and adds the existing `dispatch.rs` broken-project-tolerance test to the milestone and validation matrix so the extraction preserves both startup and steady-state continuation semantics.
