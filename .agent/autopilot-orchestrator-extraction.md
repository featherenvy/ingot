# Extract autopilot orchestration into a bounded module

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with `.agent/PLANS.md`.

## Purpose / Big Picture

After this change, autopilot dispatch is fixed to enforce strict sequential item ordering (one item at a time, in sort_key order), and all autopilot-specific orchestration logic that currently lives scattered across the large runtime file (`crates/ingot-agent-runtime/src/lib.rs`, approximately 7600 lines — the exact count shifts with pending changes) will be consolidated into a dedicated module. An operator running Ingot in autopilot mode will see sequential item processing where a stuck/escalated item blocks subsequent items. A developer reading the codebase will find autopilot dispatch, auto-triage, throttling, and convergence queueing in one place instead of spread across seven `if execution_mode == Autopilot` branches in production code. The auto-triage transaction choreography that currently lives in the runtime will move into `crates/ingot-usecases`, matching the existing pattern where `ingot-usecases` owns state transitions and the runtime is a thin coordinator.

To verify success: run `make all` (which runs check, test, lint, build). All existing tests pass unchanged. The runtime file shrinks by approximately 200-300 lines and gains no new `execution_mode == Autopilot` branches. The new module and usecase function are exercised by the same existing tests that cover autopilot behavior today. The new sequential-dispatch test verifies that autopilot does not skip past an undispatchable item to dispatch a later one.

## Progress

- [x] Milestone 0: Fix sequential dispatch — autopilot must not skip past a stuck item to dispatch a later one.
- [x] Milestone 1: Extract autopilot methods into `crates/ingot-agent-runtime/src/autopilot.rs`.
- [x] Milestone 2: Lift auto-triage orchestration into `crates/ingot-usecases/src/finding.rs`.
- [x] Milestone 3: Consolidate auto-queue convergence logic into the autopilot module.
- [x] Final validation: `make all` passes.

## Surprises & Discoveries

- Observation: Autopilot item dispatch is not truly sequential. In `recover_projected_jobs` (lib.rs lines 4562-4570), the item loop only breaks when `dispatched == true && mode == Autopilot`. If the evaluator returns nothing-to-dispatch for an item (e.g., it is escalated after an authoring failure), `dispatched` is false and the loop falls through to the next item. This means a stuck/escalated item does NOT block subsequent items from being dispatched. For demo/greenfield projects where items have implicit ordering dependencies, this causes cascading failures: Item1 fails, Item2 starts (depends on Item1's code but it doesn't exist yet), Item2 also fails, etc. The user observes multiple failed + one active job.
  Evidence: lib.rs lines 4565-4570: `if dispatched && project.execution_mode == Autopilot { break; }` — the `dispatched &&` condition is the bug. The `project_has_active_autopilot_work` throttle at lines 4533-4537 only catches ACTIVE work (Queued/Assigned/Running jobs, active convergences, active queue entries). A failed/escalated item has NO active work, so the throttle passes and the item loop proceeds to dispatch the next item.

- Observation: `bootstrap.rs` in the same crate does NOT use `impl JobDispatcher` blocks — it defines standalone functions like `ensure_default_agents(db: &Database)`. There is no existing file in `ingot-agent-runtime` that splits `impl JobDispatcher` across modules. The approach is still valid (Rust allows multiple `impl` blocks for the same type across modules in the same crate), but there is no in-crate precedent to follow. The implementer should expect to discover the minimal set of `use` imports needed in `autopilot.rs` by trial and error.
  Evidence: `bootstrap.rs` lines 12-18 define `pub async fn ensure_default_agents(db: &Database)`, not `impl JobDispatcher`.

- Observation: The approval-state transition in `auto_triage_job_findings` is guarded by TWO conditions that were not documented in the original plan: `job.step_id == StepId::ValidateIntegrated` AND `item.current_revision_id == revision.id` (lib.rs line 4678). Omitting these guards would cause autopilot to attempt approval transitions for non-validation jobs and for stale revisions.
  Evidence: lib.rs line 4678: `if job.step_id == StepId::ValidateIntegrated && item.current_revision_id == revision.id {`

- Observation: `finding.rs` imports `crate::item::approval_state_for_policy` (line 15), but that helper returns `ApprovalState::NotRequested` for `Required` policy (item.rs line 134), not `ApprovalState::Pending`. The auto-triage approval transition needs `Pending`, so the new `execute_auto_triage` must implement its own match, not reuse that helper.
  Evidence: item.rs lines 132-136 show `Required => NotRequested`, but lib.rs line 4693 uses `Required => Pending`.

- Observation: lib.rs has staged modifications (git status shows `M crates/ingot-agent-runtime/src/lib.rs`). All line numbers in this plan are approximate and may shift by a few lines from the values stated here. During implementation, search for function names rather than relying on exact line numbers.

## Decision Log

- Decision: Fix autopilot sequential dispatch by always breaking after the first open item in `recover_projected_jobs`, regardless of whether dispatch succeeded.
  Rationale: The current code breaks only when `dispatched == true`, allowing the item loop to skip past escalated/stuck items and dispatch later items. For greenfield projects, items have implicit dependencies (Item2's code depends on Item1's output). The fix enforces strict FIFO ordering: only the first open item (by sort_key) is ever considered for dispatch. If it cannot be dispatched (escalated, awaiting approval, or has no dispatchable step), subsequent items are blocked until the first resolves. This aligns with the autopilot "run to gate" philosophy — an escalated item IS a gate. The `project_has_active_autopilot_work` throttle at the top of the function still handles the case where active work (jobs, convergences, queue entries) exists. The error case in the item loop also needs the same break to prevent fallthrough.
  Date/Author: 2026-03-22 / Claude (revision 3)

- Decision: Keep the orchestrator module in `ingot-agent-runtime`, not in `ingot-usecases`.
  Rationale: The orchestrator coordinates infrastructure concerns (project mutation locks, mirror refresh via `refresh_project_mirror`, convergence hydration via `hydrate_convergences`, DB loads) that belong in the runtime adapter layer. The pure decision logic already lives in `ingot-usecases/src/dispatch.rs` and `ingot-usecases/src/finding.rs`. Moving the orchestrator into `ingot-usecases` would force those pure modules to depend on infrastructure.
  Date/Author: 2026-03-22 / Claude

- Decision: Move auto-triage transaction choreography (DB writes, activity logging, approval-state transition) from runtime into `ingot-usecases/src/finding.rs` as a new async function `execute_auto_triage`.
  Rationale: The runtime method `auto_triage_job_findings` (lines 4622-4724 of lib.rs) performs finding persistence, activity appending, and approval-state transitions — all state-machine work that matches the `ingot-usecases` ownership pattern established by `CompleteJobService` and `ConvergenceService`. The runtime should call a single usecase function, not orchestrate the full triage-then-transition flow. Note that `finding.rs` currently contains only synchronous pure functions. The new `execute_auto_triage` is async and port-dependent, which breaks that pattern. This is acceptable because `dispatch.rs` in the same crate already mixes pure helpers (like `current_authoring_head_for_revision`) with async port-dependent functions (like `auto_dispatch_autopilot`), establishing a precedent within `ingot-usecases`.
  Date/Author: 2026-03-22 / Claude

- Decision: Do not introduce a strategy trait or enum-dispatch for execution modes yet.
  Rationale: There are only two modes today (Manual, Autopilot). A trait would add indirection without reducing complexity. If a third mode is added later, the autopilot module is the natural place to introduce a strategy pattern.
  Date/Author: 2026-03-22 / Claude

- Decision: Use `&JobDispatcher` reference in the autopilot module rather than a generic type parameter.
  Rationale: `JobDispatcher` is a concrete struct (not generic) with fields `db: Database`, `project_locks: ProjectLocks`, etc. Introducing a generic `<DB>` would not match the codebase pattern — the runtime layer uses concrete types, and generics-over-ports is an `ingot-usecases` pattern. The autopilot module will hold `&JobDispatcher` and call methods on it directly, just like existing private methods do today.
  Date/Author: 2026-03-22 / Claude

- Decision: Pass `step_id: StepId` as a parameter to `execute_auto_triage` rather than adding a `JobRepository` bound.
  Rationale: The approval-state transition in `auto_triage_job_findings` is guarded by `job.step_id == StepId::ValidateIntegrated`. The runtime currently loads the job via `self.db.get_job(job_id)` (lib.rs line 4677) to access `step_id`. Rather than adding a 5th generic repository bound to the usecase function, the runtime thin wrapper loads the job and passes `step_id` to the usecase. This keeps the usecase's port surface minimal (4 repos: Finding, Item, Revision, Activity) and avoids an unnecessary DB round-trip inside the usecase. The runtime callsites (`finish_report_run` and `run_prepared_harness_validation`) either already have the job object or can cheaply load it.
  Date/Author: 2026-03-22 / Claude (revision 2)

## Outcomes & Retrospective

Implementation completed 2026-03-22. All four milestones delivered in a single pass.

**M0**: Fixed sequential dispatch by removing `dispatched &&` from the break condition in `recover_projected_jobs` and adding a break in the error branch. Added test `recover_projected_jobs_does_not_skip_escalated_item_to_dispatch_next`.

**M1**: Extracted `auto_dispatch_autopilot_locked`, `project_has_active_autopilot_work`, and `auto_triage_job_findings` into `autopilot.rs`. No changes needed to callers — Rust resolves `self.method()` across `impl` blocks.

**M2**: Created `execute_auto_triage` in `finding.rs` with 4 generic repo bounds + `step_id: StepId` parameter. Replaced `auto_triage_job_findings` body with thin delegation. Removed the redundant `item_id` parameter (always equal to `item.id`). Added 2 tests: happy path (FixNow findings don't trigger approval) and guard test (non-ValidateIntegrated step skips approval).

**M3**: Split `auto_queue_convergence` into thin port impl (lock + mode check in lib.rs) + `auto_queue_convergence_inner` (core logic in autopilot.rs).

**Test counts**: 25 unit (24 baseline + 1 new M0), 22 integration (unchanged), 16 finding tests (14 baseline + 2 new M2). `make all` passes.

**Surprise during implementation**: Baseline test count was 24, not the plan's estimate of 18. The `auto_triage_job_findings` `item_id` parameter was redundant (always `item.id`) — removed it to avoid an unused-variable warning, which was a clean simplification not anticipated in the plan.

## Context and Orientation

Ingot is a local code-delivery daemon that orchestrates AI coding work against Git repositories. The system has two execution modes configured per project: Manual (operator clicks dispatch buttons in the UI) and Autopilot (the daemon automatically dispatches every safe workflow step until it hits a human gate like approval, escalation, or findings triage).

The codebase is a Rust workspace. The relevant crates and their roles are:

**`crates/ingot-domain`** — Pure types with no infrastructure dependencies. Contains `ExecutionMode` (enum: `Manual` | `Autopilot`), `AutoTriagePolicy` (maps finding severity to `AutoTriageDecision`: `FixNow`, `Backlog`, or `Skip`), and `AgentRouting` (per-phase agent preferences), all as fields on the `Project` struct in `crates/ingot-domain/src/project.rs`. Repository port traits are in `crates/ingot-domain/src/ports.rs` — each entity has its own trait: `FindingRepository`, `ItemRepository`, `RevisionRepository`, `ActivityRepository`, `JobRepository`, `WorkspaceRepository`, `ConvergenceRepository`, `ConvergenceQueueRepository`. These are the interfaces that `ingot-usecases` functions accept as generic parameters.

**`crates/ingot-usecases`** — Command handlers and transaction boundaries. Contains:
- `auto_dispatch_autopilot()` in `crates/ingot-usecases/src/dispatch.rs` (lines 286-352): async function generic over `J: JobRepository, W: WorkspaceRepository, A: ActivityRepository` that evaluates item state and dispatches the next safe step. Also contains pure helpers like `current_authoring_head_for_revision()` and `should_fill_candidate_subject_from_workspace()`.
- `auto_triage_findings()` in `crates/ingot-usecases/src/finding.rs` (lines 405-462): synchronous pure function that applies `AutoTriagePolicy` to findings and returns `Vec<AutoTriagedFinding>`. All existing functions in `finding.rs` are synchronous and pure — they take domain types in and return domain types out, with no repository port imports.
- `ConvergenceService` in `crates/ingot-usecases/src/convergence.rs`: a `struct ConvergenceService<P>` generic over `P: ConvergenceSystemActionPort`. Its `tick_system_actions()` method (lines 540-620) auto-queues convergence for autopilot projects at lines 606-615 by calling `self.port.auto_queue_convergence()`.
- `CompleteJobService` in `crates/ingot-usecases/src/job.rs`: a `struct CompleteJobService<R, G, L>` (three generics: repository, git, locks) that provides the style reference for how services are structured. Bounds are applied on `impl` blocks, not on the struct definition.
- Re-exports from `crates/ingot-usecases/src/lib.rs`: `ConvergenceService`, `UseCaseError`, `CompleteJobService`, `CompleteJobCommand`, `ProjectLocks`, `DispatchNotify`, `ReconciliationService`.

**`crates/ingot-agent-runtime`** — The runtime dispatcher. Two source files: `crates/ingot-agent-runtime/src/lib.rs` (approximately 7600 lines; exact count depends on pending changes) and `crates/ingot-agent-runtime/src/bootstrap.rs` (standalone functions for agent initialization, NOT `impl JobDispatcher` methods). The main struct is `JobDispatcher` (lines 166-179), a concrete (non-generic) `#[derive(Clone)]` struct with these fields:
- `db: Database` (from `ingot_store_sqlite::Database`, a SQLite connection pool wrapper)
- `project_locks: ProjectLocks`
- `config: DispatcherConfig`
- `lease_owner_id: LeaseOwnerId`
- `runner: Arc<dyn AgentRunner>`
- `dispatch_notify: DispatchNotify`
- Three `#[cfg(test)]` pause hook fields: `pre_spawn_pause_hook`, `auto_queue_pause_hook`, `projected_recovery_pause_hook`

The `ConvergenceSystemActionPort` trait (defined in `crates/ingot-usecases/src/convergence.rs` lines 159-193) is implemented for `RuntimeConvergencePort` (a wrapper struct at `lib.rs` line 290 that holds a cloned `JobDispatcher`), NOT directly for `JobDispatcher`.

Autopilot logic is currently scattered across these methods, all on `impl JobDispatcher`:

1. **`auto_dispatch_autopilot_locked`** (lines ~4801-4852, `pub async fn`): Loads item, revision, jobs, findings, convergences. If the evaluator says `AUTHOR_INITIAL` is dispatchable and the revision has an implicit seed, calls `self.refresh_project_mirror()` to resolve the current target ref head. Delegates to `ingot_usecases::dispatch::auto_dispatch_autopilot()`. Returns `Result<bool, RuntimeError>`.

2. **`auto_dispatch_projected_review`** (lines ~4726-4749, `async fn`, private): Acquires project mutation lock, reloads project from DB (to get fresh `execution_mode`), then switches: Autopilot branch checks `project_has_active_autopilot_work()` and calls `auto_dispatch_autopilot_locked()`; Manual branch calls `auto_dispatch_projected_review_locked()`.

3. **`auto_dispatch_projected_review_locked`** (lines ~4751-4799, `pub async fn`): Manual-mode dispatch path. Calls `ingot_usecases::dispatch::auto_dispatch_review()` for closure-relevant reviews, falls through to `auto_dispatch_projected_validation_job()` for daemon-only validation jobs. Returns `Result<bool, RuntimeError>`.

4. **`auto_dispatch_projected_validation_job`** (lines ~4854-4934, `async fn`, private): Dispatches daemon-only harness validation jobs. Only called from the Manual path in `auto_dispatch_projected_review_locked`. This method stays in `lib.rs` — it is NOT part of the autopilot extraction.

5. **`auto_triage_job_findings`** (lines ~4622-4724, `async fn`, private): The method that Milestone 2 moves to usecases. Loads findings for the item, filters to unresolved ones from the specified job, calls `ingot_usecases::finding::auto_triage_findings()`, then orchestrates: persist each triage/backlog-link via `FindingRepository::triage()`/`FindingRepository::link_backlog()`, append activity per finding. Then checks whether to transition approval state — this transition is guarded by two conditions: `job.step_id == StepId::ValidateIntegrated` (the job must be a validate-integrated step, loaded via `self.db.get_job(job_id)` at line ~4677) AND `item.current_revision_id == revision.id` (the item's revision must not have changed since the job started). Only when both guards pass does it reload findings, check if all are resolved non-blocking, and set `ApprovalState::Pending` (for `Required` policy) or `ApprovalState::NotRequired` (for `NotRequired` policy).

6. **`project_has_active_autopilot_work`** (lines 4591-4620, `async fn`, private): Returns true if the project has any active jobs, active convergences, or active queue entries. Used as a throttle to enforce serial dispatch.

7. **`recover_projected_jobs`** (lines 4512-4589, `async fn`, private): Startup/recovery loop. Iterates all projects, acquires mutation lock per project, reloads project from DB (to get fresh `execution_mode`). For Autopilot projects, checks active work and calls `auto_dispatch_autopilot_locked()`; breaks after first dispatch to avoid starving other projects. For Manual projects, calls `auto_dispatch_projected_review_locked()`.

8. **`auto_queue_convergence`** closure (lines 626-767, in `impl ConvergenceSystemActionPort for RuntimeConvergencePort`): Acquires project mutation lock, reloads project to verify still Autopilot, loads item/revision/jobs/findings/convergences, evaluates, builds `ConvergenceQueueEntry`, inserts with conflict-retry logic. Uses test pause hooks `pause_before_auto_queue_guard()` (line 634) and `pause_before_auto_queue_insert()` (line 716).

9. **Two auto-triage callsites**: In `finish_report_run` (lines 3167-3177, called after report-type agent jobs complete) and in `run_prepared_harness_validation` (lines ~4222-4229, called after daemon-only harness validation jobs complete with findings). Both check `execution_mode == Autopilot` before calling `auto_triage_job_findings`.

**`crates/ingot-workflow`** — The pure evaluator. `Evaluator::new().evaluate(item, revision, jobs, findings, convergences)` returns an `Evaluation` with `dispatchable_step_id` and `next_recommended_action`. Autopilot reuses the same evaluator as manual mode — it does not weaken the evaluation; it just acts on the result unconditionally instead of filtering to closure-relevant steps.

**`crates/ingot-test-support`** — Shared test infrastructure crate. Provides `migrated_test_db(prefix)` for temporary SQLite databases with migrations, and re-exports fixture builders from `ingot_domain::test_support`: `ProjectBuilder`, `ItemBuilder`, `RevisionBuilder`, `JobBuilder`, `FindingBuilder`, `ConvergenceBuilder`, `ConvergenceQueueEntryBuilder`, etc. Also provides `git::temp_git_repo()` and `git::unique_temp_path()`.

**Test infrastructure in the runtime**: Two parallel harness patterns:
- `TestRuntimeHarness` (lib.rs line ~6200): used by unit tests in `lib.rs`'s `#[cfg(test)] mod tests` block (starts at line ~6008). Fields: `db`, `dispatcher`, `dispatch_notify`, `project`, `repo_path`. Six autopilot-specific tests here: `auto_queue_convergence_treats_conflicting_insert_as_noop`, `tick_system_action_does_not_queue_stale_autopilot_prepare_decision`, `tick_system_action_does_not_queue_after_execution_mode_switches_to_manual`, `recover_projected_jobs_reloads_execution_mode_after_lock`, `recover_projected_jobs_only_queues_one_autopilot_item_while_another_is_active`, `auto_dispatch_projected_review_does_not_queue_autopilot_item_while_project_has_active_work`.
- `TestHarness` in `crates/ingot-agent-runtime/tests/common/mod.rs`: used by integration tests in `tests/auto_dispatch.rs` (22 tests total, 2 autopilot-relevant: `autopilot_author_initial_binds_current_target_ref_head_after_branch_advances` at line 310 and `idle_item_auto_dispatches_candidate_review_after_nonblocking_incremental_triage` at line 2096). Tests access `JobDispatcher` via `h.dispatcher`.

Key commands:
- `make check` — type-check the Rust workspace
- `make test` — run all Rust tests
- `make lint` — clippy + biome + fmt
- `make all` — check + test + lint + build (the CI gate)
- `cargo test -p ingot-agent-runtime --lib` — runtime unit tests only (18 tests)
- `cargo test -p ingot-agent-runtime --test auto_dispatch` — runtime integration tests (22 tests)
- `cargo test -p ingot-usecases` — usecase tests only

## Plan of Work

The work proceeds in three milestones. Each milestone is independently verifiable with `make check && make test`.

### Milestone 0: Fix sequential dispatch for autopilot projects

At the end of this milestone, autopilot projects process items in strict FIFO order by sort_key. If the first open item cannot be dispatched (escalated after failure, awaiting approval, or evaluator returns nothing-to-dispatch), subsequent items are blocked. Previously, the item loop in `recover_projected_jobs` would fall through to dispatch later items when an earlier item had nothing to dispatch.

The bug is in `recover_projected_jobs` (lib.rs lines ~4562-4570). The item loop currently breaks only when `dispatched == true && mode == Autopilot`:

    match result {
        Ok(dispatched) => {
            dispatched_any |= dispatched;
            if dispatched
                && project.execution_mode == ExecutionMode::Autopilot
            {
                break;
            }
        }
        Err(error) => {
            warn!(...);
        }
    }

The fix changes both the `Ok` branch and the `Err` branch to always break for autopilot after the first open item:

    match result {
        Ok(dispatched) => {
            dispatched_any |= dispatched;
            if project.execution_mode == ExecutionMode::Autopilot {
                break;
            }
        }
        Err(error) => {
            warn!(...);
            if project.execution_mode == ExecutionMode::Autopilot {
                break;
            }
        }
    }

The change is minimal: remove `dispatched &&` from the `Ok` branch condition, and add an equivalent break in the `Err` branch. Items with `lifecycle.is_open() == false` (completed items) are already skipped by the `continue` at line 4550-4551, so the first open item is the first item that has unfinished work.

This preserves the existing item ordering guarantee: `list_items_by_project` returns items sorted by `sort_key ASC, created_at ASC` (SQL in `crates/ingot-store-sqlite/src/store/item.rs` line 19). Demo items are created with incrementing sort_keys, so Item1 (lowest sort_key) is always processed first.

The `project_has_active_autopilot_work` check at lines 4533-4537 remains unchanged — it still short-circuits the entire project when active jobs/convergences/queue-entries exist. The Milestone 0 fix addresses the different case where the project has NO active work but the first open item is stuck.

Add a new test in the `#[cfg(test)] mod tests` block of lib.rs:

    #[tokio::test]
    async fn recover_projected_jobs_does_not_skip_escalated_item_to_dispatch_next() {
        // 1. Create TestRuntimeHarness with 2 items in autopilot project
        // 2. Escalate Item1 (set escalation field, e.g., via ItemBuilder with escalation)
        // 3. Call dispatcher.recover_projected_jobs()
        // 4. Assert Item2 has NO jobs (Item1's escalation blocks it)
        // 5. Assert returns false (no dispatch happened)
    }

The test creates two items in an autopilot project, escalates Item1 so the evaluator returns nothing-to-dispatch for it, then verifies that `recover_projected_jobs` does NOT dispatch Item2. The existing test `recover_projected_jobs_only_queues_one_autopilot_item_while_another_is_active` covers the case where Item1 HAS active work; the new test covers the case where Item1 has NO active work but IS stuck.

Validation: `cargo test -p ingot-agent-runtime --lib` passes with test count increased by 1. All existing autopilot tests still pass — the break-on-success behavior is unchanged (the condition `dispatched && Autopilot` is a subset of `Autopilot`). `make check` passes.

### Milestone 1: Extract autopilot methods into `crates/ingot-agent-runtime/src/autopilot.rs`

At the end of this milestone, a new file `crates/ingot-agent-runtime/src/autopilot.rs` exists. Since `JobDispatcher` is a concrete struct (not generic), the autopilot module does NOT define a new struct with generic DB bounds. Instead, the moved functions become methods on `impl JobDispatcher` inside the new module, using `pub(crate)` visibility. This is the simplest extraction: the methods move files but keep the same receiver. Rust allows multiple `impl` blocks for the same type across modules in the same crate — this is standard language behavior. No existing file in `ingot-agent-runtime` uses this pattern today, so the implementer will be the first to split `impl JobDispatcher` across files.

The following methods move from `lib.rs` to `autopilot.rs`:

1. **`auto_dispatch_autopilot_locked`** (lines ~4801-4852) — moves as-is. Calls `self.refresh_project_mirror()` (stays in `lib.rs`), `self.hydrate_convergences()` (stays in `lib.rs`), and `ingot_usecases::dispatch::auto_dispatch_autopilot()`. All of these remain accessible because the receiver is still `&self` on `JobDispatcher`.

2. **`project_has_active_autopilot_work`** (lines 4591-4620) — moves as-is. Queries `self.db` for active jobs, convergences, and queue entries.

3. **`auto_triage_job_findings`** (lines ~4622-4724) — moves as-is in this milestone, then its body is replaced in Milestone 2 with a single delegation to the new usecase function.

The following methods stay in `lib.rs` but are simplified:

4. **`auto_dispatch_projected_review`** (lines ~4726-4749) — stays. Its Autopilot branch calls `self.project_has_active_autopilot_work()` and `self.auto_dispatch_autopilot_locked()` which now resolve to the autopilot module. No code change needed — Rust resolves `self.method()` across `impl` blocks in different modules.

5. **`recover_projected_jobs`** (lines 4512-4589) — stays. Same: the autopilot branch calls `self.project_has_active_autopilot_work()` and `self.auto_dispatch_autopilot_locked()`, resolving to the autopilot module.

6. **The two auto-triage callsites** (lines 3167-3177 in `finish_report_run` and lines ~4222-4229 in `run_prepared_harness_validation`) — stay in `lib.rs`. They call `self.auto_triage_job_findings()` which resolves to the autopilot module. No code change needed in this milestone.

What changes in `lib.rs`: add `mod autopilot;` near the top. Delete the three method bodies that moved. The `auto_dispatch_projected_review`, `recover_projected_jobs`, `finish_report_run`, and `run_prepared_harness_validation` methods do NOT change — they call `self.*` methods that now happen to live in a different file. The `auto_dispatch_projected_review_locked()` and `auto_dispatch_projected_validation_job()` methods stay in `lib.rs` (they are manual-mode only).

The `auto_queue_convergence` closure (lines 626-767 in the `ConvergenceSystemActionPort` impl for `RuntimeConvergencePort`) stays in `lib.rs` for this milestone because it is an `impl` for a different struct (`RuntimeConvergencePort`, not `JobDispatcher`). It will be consolidated in Milestone 3.

Test hooks: the `#[cfg(test)]` pause hooks `auto_queue_pause_hook` and `projected_recovery_pause_hook` remain on `JobDispatcher` and are not affected — they are accessed via `self` in methods that either stay in `lib.rs` or that call through to methods still on `self`. The `pause_before_auto_queue_guard()` and `pause_before_auto_queue_insert()` calls live in the `auto_queue_convergence` closure (which doesn't move until M3). The `pause_before_projected_recovery_guard()` call lives in `recover_projected_jobs()` which stays in `lib.rs`.

Validation: `cargo test -p ingot-agent-runtime --lib` (18 tests) and `cargo test -p ingot-agent-runtime --test auto_dispatch` (22 tests) pass. `make check` passes.

### Milestone 2: Lift auto-triage orchestration into `crates/ingot-usecases/src/finding.rs`

At the end of this milestone, the body of `auto_triage_job_findings` in `autopilot.rs` is replaced by a thin delegation call to a new `execute_auto_triage` async function in `crates/ingot-usecases/src/finding.rs`.

The new function is defined in `finding.rs` and follows the generic-over-ports pattern used by `auto_dispatch_review` and `auto_dispatch_autopilot` in `dispatch.rs`. It is async and takes four repository port bounds plus a `step_id: StepId` parameter (see Decision Log for why `step_id` is passed as a parameter rather than loading the job via a 5th repository bound). This introduces the first async port-dependent function in `finding.rs`, which previously contained only synchronous pure functions. This is an intentional pattern extension justified by the precedent in `dispatch.rs` (see Decision Log).

The new function orchestrates the full triage flow that currently lives in `auto_triage_job_findings`:

1. Load findings for the item via `FindingRepository::list_by_item(item_id)`.
2. Filter to findings from the specified job that are unresolved. Return early if none.
3. Load the current revision via `RevisionRepository::get(item.current_revision_id)`.
4. Load existing items via `ItemRepository::list_by_project(item.project_id)`.
5. Call the existing `auto_triage_findings()` pure function.
6. For each result: if backlog, call `FindingRepository::link_backlog(&result.finding, linked_item, linked_revision, None)` — the 4th parameter `detached_item_id` is always `None` for auto-triage; otherwise call `FindingRepository::triage(&result.finding)`. Append activity via `ActivityRepository::append()` with event type `FindingTriaged` and subject `Finding(result.finding.id)`.
7. Guard: only proceed to approval transition if `step_id == StepId::ValidateIntegrated` AND `item.current_revision_id == revision.id`. If either condition is false, return `Ok(())` after logging the triage count. The `ValidateIntegrated` guard ensures only validate-integrated jobs trigger approval transitions. The revision freshness guard prevents stale revisions from corrupting approval state.
8. Reload findings via `FindingRepository::list_by_item()`. Filter to findings from this job with `source_item_revision_id == revision.id`. Check if all are resolved and none have `FixNow` triage state.
9. If all resolved non-blocking: load current item via `ItemRepository::get(item_id)`, compute `next_approval_state` from `revision.approval_policy` (`Required` → `ApprovalState::Pending`, `NotRequired` → `ApprovalState::NotRequired`). If `current_item.approval_state != next_approval_state`, update item and append `ApprovalRequested` activity if transitioning to `Pending`.

Note on trait vs inherent method names: the runtime currently calls `self.db.triage_finding()` and `self.db.link_backlog_finding()` — these are inherent methods on `Database`. The `execute_auto_triage` usecase function calls the same operations via the port trait methods `FindingRepository::triage()` and `FindingRepository::link_backlog()`. The trait impls delegate to the inherent methods, so behavior is identical.

Error propagation: the function uses `?` (early return) on each persistence call. If a finding's triage or link fails, subsequent findings are NOT processed. This preserves the existing behavior in the runtime method.

The runtime method in `autopilot.rs` becomes:

    pub(crate) async fn auto_triage_job_findings(
        &self,
        project: &Project,
        item_id: ingot_domain::ids::ItemId,
        job_id: ingot_domain::ids::JobId,
        item: &ingot_domain::item::Item,
    ) -> Result<(), RuntimeError> {
        let policy = project.auto_triage_policy.clone().unwrap_or_default();
        let job = self.db.get_job(job_id).await?;
        ingot_usecases::finding::execute_auto_triage(
            &self.db, &self.db, &self.db, &self.db,
            project, item, job_id, job.step_id, &policy,
        ).await.map_err(|e| RuntimeError::InvalidState(format!("auto-triage failed: {e}")))
    }

The `&self.db` is passed four times because `Database` implements `FindingRepository`, `ItemRepository`, `RevisionRepository`, and `ActivityRepository` — the generic parameters resolve to the same concrete type. This matches how `auto_dispatch_review` in `dispatch.rs` is called from the runtime: `ingot_usecases::dispatch::auto_dispatch_review(&self.db, &self.db, &self.db, ...)`. The runtime thin wrapper loads the job (`self.db.get_job(job_id)`) to extract `job.step_id` and passes it to the usecase function.

Validation: `cargo test -p ingot-usecases` passes with at least one new test that exercises `execute_auto_triage` end-to-end against a `migrated_test_db`. The test should: create a project/item/revision/job with findings (using `FindingBuilder` from `ingot_domain::test_support`), call `execute_auto_triage` with `step_id = StepId::ValidateIntegrated`, verify findings are triaged and approval state is transitioned to `Pending`. A second test should verify the `ValidateIntegrated` guard: call with a non-ValidateIntegrated step_id and assert approval state is NOT changed. `cargo test -p ingot-agent-runtime --lib` and `cargo test -p ingot-agent-runtime --test auto_dispatch` still pass.

### Milestone 3: Consolidate auto-queue convergence into the autopilot module

At the end of this milestone, the `auto_queue_convergence` method body in the `impl ConvergenceSystemActionPort for RuntimeConvergencePort` block (lines 626-767 of `lib.rs`) is split into two parts:

1. **Thin port impl** (stays in `lib.rs`): the `fn auto_queue_convergence(...)` on `RuntimeConvergencePort` remains as the trait impl, but its body acquires the project mutation lock, then delegates to a new `pub(crate)` method on `JobDispatcher`.

2. **Core logic** (moves to `autopilot.rs`): a new method `auto_queue_convergence_inner` on `impl JobDispatcher` that receives the already-acquired lock guard (or takes `&self` after the lock is held by the caller) and contains the entity loading, evaluation check, queue entry construction, conflict-retry loop, and activity append.

The `#[cfg(test)]` pause hooks are a complication. `pause_before_auto_queue_guard()` (line 634) runs before lock acquisition — it stays in the port impl in `lib.rs`. `pause_before_auto_queue_insert()` (line 716) runs inside the queue-insert loop — it moves with the core logic to `autopilot.rs`. Both hooks call methods on `self.dispatcher` (i.e., on `JobDispatcher`), so they remain accessible.

The port impl body in `lib.rs` becomes approximately:

    fn auto_queue_convergence(&self, project_id, item_id) -> impl Future<...> + Send {
        let dispatcher = self.dispatcher.clone();
        async move {
            #[cfg(test)]
            dispatcher.pause_before_auto_queue_guard().await;
            let _guard = dispatcher.project_locks.acquire_project_mutation(project_id).await;
            let project = dispatcher.db.get_project(project_id).await
                .map_err(ingot_usecases::UseCaseError::Repository)?;
            if project.execution_mode != ExecutionMode::Autopilot {
                return Ok(false);
            }
            dispatcher.auto_queue_convergence_inner(project_id, item_id, &project).await
        }
    }

The lock acquisition and mode check stay in the port impl because they guard the entry point. The bulk of the logic (~100 lines) moves to `autopilot.rs`.

Validation: `make all` passes. The six autopilot-specific runtime unit tests pass: `auto_queue_convergence_treats_conflicting_insert_as_noop`, `tick_system_action_does_not_queue_stale_autopilot_prepare_decision`, `tick_system_action_does_not_queue_after_execution_mode_switches_to_manual`, `recover_projected_jobs_reloads_execution_mode_after_lock`, `recover_projected_jobs_only_queues_one_autopilot_item_while_another_is_active`, `auto_dispatch_projected_review_does_not_queue_autopilot_item_while_project_has_active_work`. The integration tests `autopilot_author_initial_binds_current_target_ref_head_after_branch_advances` and `idle_item_auto_dispatches_candidate_review_after_nonblocking_incremental_triage` also pass.

## Concrete Steps

All commands are run from the repository root `/Users/aa/Documents/ingot`.

Milestone 0 — fix sequential dispatch:

    # Baseline: confirm existing tests pass
    cargo test -p ingot-agent-runtime --lib 2>&1 | tail -5
    # Expect: "test result: ok. 18 passed; 0 failed"

    # After applying the fix and adding the new test:
    cargo test -p ingot-agent-runtime --lib 2>&1 | tail -5
    # Expect: "test result: ok. 19 passed; 0 failed"

    make check
    # Expect: exit 0

Milestone 1 — baseline, then extract:

    # Baseline: confirm all tests pass before touching anything
    cargo test -p ingot-agent-runtime --lib 2>&1 | tail -5
    # Expect: "test result: ok. 18 passed; 0 failed"

    cargo test -p ingot-agent-runtime --test auto_dispatch 2>&1 | tail -5
    # Expect: "test result: ok. 22 passed; 0 failed"

    # After creating autopilot.rs and moving methods:
    make check
    # Expect: exit 0

    cargo test -p ingot-agent-runtime --lib
    cargo test -p ingot-agent-runtime --test auto_dispatch
    # Expect: same counts as baseline

Milestone 2 — add usecase function, replace runtime body:

    cargo test -p ingot-usecases 2>&1 | tail -5
    # Expect: test count increased by at least 2 (happy path + guard test), all pass

    cargo test -p ingot-agent-runtime --lib
    cargo test -p ingot-agent-runtime --test auto_dispatch
    # Expect: same counts as M1

Milestone 3 — consolidate convergence queueing:

    make all
    # Expect: exit 0 (check + test + lint + build)

## Validation and Acceptance

Milestone 0 is a behavior fix; Milestones 1-3 are behavior-preserving refactoring. Acceptance criteria:

1. `make all` passes with zero failures.
2. In `recover_projected_jobs`, the autopilot item loop breaks after the first open item regardless of dispatch result. A new test `recover_projected_jobs_does_not_skip_escalated_item_to_dispatch_next` verifies that an escalated Item1 blocks dispatch of Item2.
3. `crates/ingot-agent-runtime/src/autopilot.rs` exists and contains `impl JobDispatcher` with methods for autopilot dispatch, triage, throttling, and convergence queueing.
4. `crates/ingot-agent-runtime/src/lib.rs` production code contains no more than three references to `ExecutionMode::Autopilot`: one in `auto_dispatch_projected_review` (line ~4738), one in `recover_projected_jobs` (lines ~4533, ~4554, ~4567 — all in the same function), and one in the `auto_queue_convergence` port impl mode guard (line ~644). Currently there are seven in production code.
5. `crates/ingot-usecases/src/finding.rs` exports a public `execute_auto_triage` async function with a `step_id: StepId` parameter and has at least two tests: one for the happy path (ValidateIntegrated step, all findings resolved non-blocking, approval transitions) and one verifying the `ValidateIntegrated` guard (non-ValidateIntegrated step, approval NOT transitioned).
6. The seven autopilot-specific runtime unit tests pass (6 existing + 1 new from M0): `recover_projected_jobs_does_not_skip_escalated_item_to_dispatch_next`, `auto_queue_convergence_treats_conflicting_insert_as_noop`, `tick_system_action_does_not_queue_stale_autopilot_prepare_decision`, `tick_system_action_does_not_queue_after_execution_mode_switches_to_manual`, `recover_projected_jobs_reloads_execution_mode_after_lock`, `recover_projected_jobs_only_queues_one_autopilot_item_while_another_is_active`, `auto_dispatch_projected_review_does_not_queue_autopilot_item_while_project_has_active_work`.
7. The integration tests `autopilot_author_initial_binds_current_target_ref_head_after_branch_advances` and `idle_item_auto_dispatches_candidate_review_after_nonblocking_incremental_triage` pass.
8. The four existing `auto_triage_findings` unit tests in `finding.rs` pass unchanged.

## Idempotence and Recovery

This refactor is code-only. No database migrations, no schema changes, no destructive operations. If any step fails to compile or test, fix the error and rerun the same command. All changes are additive (new file, new function) followed by subtractions (removing moved code from lib.rs) — the additive phase can be validated before the subtractive phase.

For Milestone 1 specifically: since the methods just move to a new `impl JobDispatcher` block in a new file, and Rust's `self.method()` resolution spans all impl blocks, the calling code in `lib.rs` does not need editing at all. If a compile error occurs, it is likely a missing import in `autopilot.rs` — check the `use` statements at the top of `lib.rs` and mirror the relevant ones. Note that no existing file in the crate splits `impl JobDispatcher` across modules, so expect to discover the minimal import set empirically.

## Artifacts and Notes

Key line references in the current codebase (these will shift during implementation; line numbers are approximate due to pending staged changes to lib.rs):

- `lib.rs:166-179` — `JobDispatcher` struct definition (concrete, non-generic, `#[derive(Clone)]`)
- `lib.rs:290-292` — `RuntimeConvergencePort` struct (holds cloned `JobDispatcher`)
- `lib.rs:479-767` — `impl ConvergenceSystemActionPort for RuntimeConvergencePort`
- `lib.rs:626-767` — `auto_queue_convergence` closure (delegates to orchestrator in M3)
- `lib.rs:3071-3192` — `finish_report_run` (auto-triage callsite at ~3167-3177)
- `lib.rs:~4060-4280` — `run_prepared_harness_validation` (auto-triage callsite at ~4222-4229)
- `lib.rs:4512-4589` — `recover_projected_jobs` (M0 fix at lines ~4562-4570: autopilot break condition; stays in lib.rs, dispatches to autopilot methods in M1)
- `lib.rs:4591-4620` — `project_has_active_autopilot_work` (moves in M1)
- `lib.rs:~4622-4724` — `auto_triage_job_findings` (moves in M1, body replaced in M2)
- `lib.rs:~4726-4749` — `auto_dispatch_projected_review` (stays, calls autopilot methods)
- `lib.rs:~4751-4799` — `auto_dispatch_projected_review_locked` (stays, manual-mode only)
- `lib.rs:~4801-4852` — `auto_dispatch_autopilot_locked` (moves in M1)
- `lib.rs:~4854-4934` — `auto_dispatch_projected_validation_job` (stays, manual-mode only)
- `lib.rs:~5370-5378` — `complete_job_service()` helper (stays)
- `lib.rs:~5380-5398` — `append_activity()` helper (stays)
- `lib.rs:~6008-7618` — `#[cfg(test)] mod tests` block
- `dispatch.rs:286-352` — `auto_dispatch_autopilot` usecase function (unchanged)
- `finding.rs:405-462` — `auto_triage_findings` pure function (unchanged, called by new `execute_auto_triage`)
- `convergence.rs:540-620` — `tick_system_actions` (unchanged, calls port's `auto_queue_convergence`)
- `convergence.rs:159-193` — `ConvergenceSystemActionPort` trait definition (unchanged)
- `ports.rs:185-214` — `FindingRepository` trait (methods used by M2: `list_by_item`, `triage`, `link_backlog`; note `link_backlog` takes 4 params: `finding`, `linked_item`, `linked_revision`, `detached_item_id: Option<ItemId>` — auto-triage always passes `None` for the last)
- `ports.rs:41-54` — `ItemRepository` trait (methods used by M2: `list_by_project`, `get`, `update`)
- `ports.rs:56-69` — `RevisionRepository` trait (method used by M2: `get`)
- `ports.rs:234-245` — `ActivityRepository` trait (method used by M2: `append`)

## Interfaces and Dependencies

In `crates/ingot-agent-runtime/src/autopilot.rs`, define an `impl JobDispatcher` block containing the moved methods:

    // crates/ingot-agent-runtime/src/autopilot.rs
    //
    // Autopilot-mode orchestration methods on JobDispatcher.
    // These methods are called from lib.rs when project.execution_mode == Autopilot.

    use ingot_domain::project::Project;
    use ingot_domain::ids;
    // ... additional imports as needed from the moved method bodies

    use crate::{JobDispatcher, RuntimeError};

    impl JobDispatcher {
        pub(crate) async fn auto_dispatch_autopilot_locked(
            &self,
            project: &Project,
            item_id: ids::ItemId,
        ) -> Result<bool, RuntimeError> {
            // Body moved from lib.rs lines ~4801-4852
        }

        pub(crate) async fn project_has_active_autopilot_work(
            &self,
            project_id: ids::ProjectId,
        ) -> Result<bool, RuntimeError> {
            // Body moved from lib.rs lines 4591-4620
        }

        pub(crate) async fn auto_triage_job_findings(
            &self,
            project: &Project,
            item_id: ids::ItemId,
            job_id: ids::JobId,
            item: &ingot_domain::item::Item,
        ) -> Result<(), RuntimeError> {
            // M1: body moved from lib.rs lines ~4622-4724
            // M2: replaced with:
            let policy = project.auto_triage_policy.clone().unwrap_or_default();
            let job = self.db.get_job(job_id).await?;
            ingot_usecases::finding::execute_auto_triage(
                &self.db, &self.db, &self.db, &self.db,
                project, item, job_id, job.step_id, &policy,
            ).await.map_err(|e| RuntimeError::InvalidState(
                format!("auto-triage failed: {e}")
            ))
        }

        // Added in M3:
        pub(crate) async fn auto_queue_convergence_inner(
            &self,
            project_id: ids::ProjectId,
            item_id: ids::ItemId,
            project: &Project,
        ) -> Result<bool, ingot_usecases::UseCaseError> {
            // Core logic moved from lib.rs lines ~647-760
        }
    }

In `crates/ingot-usecases/src/finding.rs`, add (Milestone 2):

    /// Orchestrate auto-triage for findings from a completed job.
    ///
    /// Applies the project's auto-triage policy to unresolved findings from the
    /// specified job: persists triage decisions, creates backlog items for Backlog
    /// findings, appends activity per finding, and transitions approval state if
    /// the job is a ValidateIntegrated step, the revision is still current, and
    /// all findings from the job are resolved as non-blocking.
    ///
    /// The `step_id` parameter controls the approval guard: only
    /// `StepId::ValidateIntegrated` triggers the approval-state transition.
    /// All other step IDs skip approval entirely.
    pub async fn execute_auto_triage<F, I, R, A>(
        finding_repo: &F,
        item_repo: &I,
        revision_repo: &R,
        activity_repo: &A,
        project: &Project,
        item: &Item,
        job_id: JobId,
        step_id: StepId,
        policy: &AutoTriagePolicy,
    ) -> Result<(), UseCaseError>
    where
        F: FindingRepository,
        I: ItemRepository,
        R: RevisionRepository,
        A: ActivityRepository;

The port trait names `FindingRepository`, `ItemRepository`, `RevisionRepository`, and `ActivityRepository` are the exact names from `crates/ingot-domain/src/ports.rs`. The `FindingRepository` methods used are: `list_by_item` (to load and reload findings), `triage` (to persist triage state — corresponds to inherent `Database::triage_finding`), and `link_backlog` (to persist backlog-linked findings with their new item and revision — corresponds to inherent `Database::link_backlog_finding`; takes 4 params with `detached_item_id: Option<ItemId>` always passed as `None`). The `ItemRepository` methods used are: `get` (to reload item for approval check), `update` (to persist approval state transition), and `list_by_project` (to get existing items for sort-key computation). `RevisionRepository::get` is used to load the current revision. `ActivityRepository::append` is used to log each triage event (`FindingTriaged`) and any approval-requested event (`ApprovalRequested`). `StepId` is from `ingot_domain::step_id::StepId`.

Revision note (2026-03-22): Rewrote plan after deep code audit. Key changes: (1) Fixed JobDispatcher from "generic over DB" to concrete struct — the Interfaces section now correctly uses `impl JobDispatcher` instead of `AutopilotOrchestrator<'a, DB>`. (2) Added complete signatures and line ranges for all referenced methods, correcting several inaccuracies (e.g., auto_dispatch_autopilot_locked is ~4801-4852 not 4797-4830; auto_queue_convergence is 626-767 not 626-760). (3) Added missing `RuntimeConvergencePort` struct (the actual ConvergenceSystemActionPort implementor). (4) Added `auto_dispatch_projected_validation_job` which the original plan omitted — it stays in lib.rs as a manual-mode-only method. (5) Documented all three `#[cfg(test)]` pause hooks and which milestone affects each. (6) Added the pattern-break acknowledgment for introducing async port-dependent code into finding.rs. (7) Named all six autopilot-specific runtime tests and the autopilot integration tests in acceptance criteria. (8) Documented the exact port trait method names from ports.rs that execute_auto_triage will use. (9) Corrected the ExecutionMode reference count from "approximately 10" to 7 in production code. (10) Added the two test harness types (TestRuntimeHarness vs TestHarness) and how tests access the dispatcher.

Revision note (2026-03-22, pass 3): Added Milestone 0 to fix sequential dispatch bug. Root cause: in `recover_projected_jobs` (lib.rs lines ~4562-4570), the autopilot item loop only breaks when `dispatched == true`. When an item is escalated/stuck (evaluator returns nothing-to-dispatch, `dispatched == false`), the loop falls through to dispatch the next item. For demo/greenfield projects where items have implicit ordering dependencies, this causes cascading failures. Fix: remove `dispatched &&` from the break condition and add a break in the error case, so autopilot always stops at the first open item. Added Decision Log entry, Surprises entry, new test, updated Concrete Steps, and updated Validation criteria.

Revision note (2026-03-22, pass 2): Corrections after second deep code audit. (1) CRITICAL: Added `StepId::ValidateIntegrated` guard to M2 — the approval-state transition in `auto_triage_job_findings` is conditional on `job.step_id == StepId::ValidateIntegrated` (lib.rs line ~4678). The previous plan omitted this guard entirely, which would have caused the usecase function to attempt approval transitions for all job types, violating the invariant. Added `step_id: StepId` parameter to `execute_auto_triage` signature and corresponding Decision Log entry. (2) CRITICAL: Added revision freshness guard `item.current_revision_id == revision.id` — the approval transition must skip if the item's current revision has changed. (3) Fixed bootstrap.rs pattern claim — bootstrap.rs does NOT define `impl JobDispatcher` blocks (it has standalone functions like `ensure_default_agents(db: &Database)`). Moved to Surprises & Discoveries and corrected M1 prose. (4) Corrected all line numbers shifted +4 after `auto_triage_job_findings` due to staged changes: auto_dispatch_projected_review 4722→4726, auto_dispatch_projected_review_locked 4747→4751, auto_dispatch_autopilot_locked 4797→4801, auto_dispatch_projected_validation_job 4850→4854 (end 4920→4934), complete_job_service 5366→5370, append_activity 5376→5380, test module 6004→6008. All line numbers now prefixed with ~ to indicate they are approximate. (5) Corrected auto_triage_job_findings end line from 4720 to 4724. (6) Corrected auto_triage_findings line range from 405-456 to 405-462. (7) Added `link_backlog`'s `detached_item_id: Option<ItemId>` 4th parameter (always `None`) to M2 steps and Interfaces. (8) Documented trait method name vs Database inherent method name mapping (triage vs triage_finding, link_backlog vs link_backlog_finding). (9) Noted `approval_state_for_policy` returns `NotRequested`, not `Pending` — cannot be reused for auto-triage approval transition. (10) Added second autopilot integration test `idle_item_auto_dispatches_candidate_review_after_nonblocking_incremental_triage` to acceptance criteria. (11) M2 validation now requires 2 tests (happy path + guard test). (12) Added note about staged changes making all lib.rs line numbers approximate.
