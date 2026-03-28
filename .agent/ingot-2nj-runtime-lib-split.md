# Finish `ingot-2nj` by splitting runtime support code out of `crates/ingot-agent-runtime/src/lib.rs`

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows the repository requirements in `.agent/PLANS.md`.

## Purpose / Big Picture

After this change, the public runtime entry point in `crates/ingot-agent-runtime/src/lib.rs` will stop acting as a catch-all file for test scaffolding, usecase adapter implementations, and internal job-support helpers. A reader should be able to open `lib.rs` and see the public runtime shell quickly, while the test-only pause hooks, usecase port adapters, and job preparation/execution support code live in focused internal modules. The observable proof is that `cargo test -p ingot-agent-runtime` still passes without behavior changes while the runtime internals are easier to navigate.

## Progress

- [x] (2026-03-28 14:52Z) Extracted the test-only pause-hook machinery from `crates/ingot-agent-runtime/src/lib.rs` into `crates/ingot-agent-runtime/src/test_support.rs` and verified the crate tests still passed.
- [x] (2026-03-28 15:06Z) Mapped the remaining `lib.rs` responsibilities and confirmed the next extractions are the runtime usecase ports and the shared job-support types/helpers.
- [x] (2026-03-28 15:18Z) Extracted the runtime usecase port adapters and their error/idle helpers into `crates/ingot-agent-runtime/src/runtime_ports.rs`.
- [x] (2026-03-28 15:21Z) Extracted `PreparedRun`, `PrepareRunOutcome`, `WorkspaceLifecycle`, and the shared job-support/helper functions into `crates/ingot-agent-runtime/src/job_support.rs`.
- [x] (2026-03-28 15:25Z) Re-ran formatting and `cargo test -p ingot-agent-runtime`; the runtime crate stayed green after the split.
- [x] (2026-03-28 15:25Z) Reduced `crates/ingot-agent-runtime/src/lib.rs` to the public runtime shell and its core dispatcher helpers.

## Surprises & Discoveries

- Observation: The first pause-hook extraction already reduced `lib.rs` from roughly 1.1k+ lines to 955 lines, but the file still held three separate runtime port adapters and a large shared helper tail.
  Evidence: `wc -l crates/ingot-agent-runtime/src/lib.rs` returned `955`.

- Observation: The runtime crate already has clean internal seams because `preparation.rs`, `execution.rs`, `supervisor.rs`, `convergence.rs`, and `autopilot.rs` all reference a small set of shared types and helpers rather than depending on arbitrary `lib.rs` internals.
  Evidence: `rg -n "PreparedRun|PrepareRunOutcome|WorkspaceLifecycle|RuntimeConvergencePort|RuntimeFinalizePort|RuntimeReconciliationPort|usecase_from_runtime_error|usecase_to_runtime_error|drain_until_idle" crates/ingot-agent-runtime/src -g'*.rs'` shows the shared surface is narrow.

- Observation: The compiler forced several test imports to become explicit because the tests had been relying on names that happened to be imported in `lib.rs`.
  Evidence: The first post-extraction compile failed in `crates/ingot-agent-runtime/src/tests.rs` for `AgentRequest`, `CommitOid`, `WorkspaceKind`, `OutcomeClass`, and trait-method visibility until those imports were spelled out.

- Observation: After the full split, `lib.rs` dropped from 955 lines to 317 lines while keeping the runtime crate tests green.
  Evidence: `wc -l crates/ingot-agent-runtime/src/lib.rs` returned `317` after the extraction.

## Decision Log

- Decision: Keep `JobDispatcher` itself in `crates/ingot-agent-runtime/src/lib.rs` and move only focused internal support layers into helper modules.
  Rationale: The public API surface of this crate is still centered on `DispatcherConfig`, `AgentRunner`, and `JobDispatcher`; moving those would obscure the public entry point instead of clarifying it.
  Date/Author: 2026-03-28 / Codex

- Decision: Split the remaining `lib.rs` internals into two modules, one for runtime usecase port adapters and one for shared job-support code, instead of many tiny files.
  Rationale: This finishes the issue with a meaningful readability gain while keeping the internal structure simple enough for a novice to navigate.
  Date/Author: 2026-03-28 / Codex

- Decision: Re-export the extracted crate-internal types and helpers from `lib.rs` with `pub(crate) use` instead of forcing every internal module to import the new modules directly.
  Rationale: This keeps the rest of the runtime crate stable while still moving ownership of the code out of `lib.rs`; the crate root remains the obvious place to discover the shared internal surface.
  Date/Author: 2026-03-28 / Codex

## Outcomes & Retrospective

`ingot-2nj` is complete. The pause-hook machinery now lives in `crates/ingot-agent-runtime/src/test_support.rs`, the runtime usecase adapters live in `crates/ingot-agent-runtime/src/runtime_ports.rs`, and the shared prepared-run/helper layer lives in `crates/ingot-agent-runtime/src/job_support.rs`. `crates/ingot-agent-runtime/src/lib.rs` now reads as the public runtime shell plus the remaining `JobDispatcher` helpers that genuinely belong there. The main lesson was that the old crate root had been providing incidental imports to the tests; once the boundaries were made explicit, the tests needed a few direct imports but behavior did not change.

## Context and Orientation

`crates/ingot-agent-runtime` is the daemon-side runtime crate that prepares jobs, runs agent and daemon validation work, reconciles startup and in-flight state, and drives convergence system actions. The key files for this refactor are:

- `crates/ingot-agent-runtime/src/lib.rs`: the crate root and current public shell. It currently defines `DispatcherConfig`, `AgentRunner`, `JobDispatcher`, `RuntimeError`, and some internal helper types/functions.
- `crates/ingot-agent-runtime/src/preparation.rs`: prepares queued jobs into executable runs. It depends on `PreparedRun`, `PrepareRunOutcome`, `WorkspaceLifecycle`, and several helper functions.
- `crates/ingot-agent-runtime/src/execution.rs`: executes prepared jobs and depends on `PreparedRun`, `WorkspaceLifecycle`, and formatting/escalation helpers.
- `crates/ingot-agent-runtime/src/supervisor.rs`: drives the main runtime loop and depends on the usecase port adapters and `drain_until_idle`.
- `crates/ingot-agent-runtime/src/convergence.rs` and `crates/ingot-agent-runtime/src/autopilot.rs`: use the runtime usecase adapters and usecase/runtime error conversions.
- `crates/ingot-agent-runtime/src/test_support.rs`: already owns the test-only pause hooks.

In this repository, a “usecase port” means a small adapter type that implements an interface from `ingot_usecases` by forwarding calls into `JobDispatcher`. A “prepared run” means the fully assembled job execution context produced by `prepare_run`, including the selected agent, workspace, prompt, and bookkeeping needed to finish the job later.

## Plan of Work

Create `crates/ingot-agent-runtime/src/runtime_ports.rs` and move the three runtime port structs (`RuntimeConvergencePort`, `RuntimeFinalizePort`, `RuntimeReconciliationPort`) there along with `usecase_to_runtime_error`, `usecase_from_runtime_error`, and `drain_until_idle`. Re-export those items from `lib.rs` for the rest of the crate.

Create `crates/ingot-agent-runtime/src/job_support.rs` and move the shared job-support types and pure helper functions there: `PreparedRun`, `PrepareRunOutcome`, `WorkspaceLifecycle`, `is_supported_runtime_job`, `supports_job`, `is_inert_assigned_authoring_dispatch_residue`, `built_in_template`, `format_revision_context`, `commit_subject`, `non_empty_message`, `outcome_class_name`, `template_digest`, `failure_escalation_reason`, and `should_clear_item_escalation_on_success`. Re-export those items from `lib.rs` so the existing module call sites stay easy to read.

Update `lib.rs` to declare the new modules, remove the extracted definitions, and keep only the public shell plus `JobDispatcher` helper methods that truly belong on the root type. Adjust crate imports only where the compiler requires it.

Run formatting and the runtime crate tests. If behavior is unchanged and `lib.rs` now reads as the public runtime shell, update the plan, close `ingot-2nj`, and push the result. This step is now complete.

## Concrete Steps

From the repository root `/Users/aa/Documents/ingot`:

1. Create the living ExecPlan in `.agent/ingot-2nj-runtime-lib-split.md`.
2. Add `crates/ingot-agent-runtime/src/runtime_ports.rs` and move the usecase adapter code there.
3. Add `crates/ingot-agent-runtime/src/job_support.rs` and move the shared run/helper code there.
4. Update `crates/ingot-agent-runtime/src/lib.rs` to declare and re-export the new modules.
5. Run:

    cargo fmt --all --check
    cargo test -p ingot-agent-runtime

The expected success signal is that formatting passes with no diff and the runtime crate test suites complete successfully.

## Validation and Acceptance

Acceptance is:

1. `crates/ingot-agent-runtime/src/lib.rs` no longer contains the runtime usecase adapter impl blocks or the tail of shared job-support/helper functions.
2. The extracted code lives in focused internal modules with names that match their responsibilities.
3. `cargo test -p ingot-agent-runtime` passes without behavioral regressions.
4. The bead issue `ingot-2nj` can be closed because both the pause-hook extraction and the broader dispatcher-internal split are complete. This condition is now satisfied.

## Idempotence and Recovery

This refactor is safe to repeat because it is structural, not behavioral. If a module extraction fails midway, the safest recovery path is to keep the old code in place until the new module compiles, then remove the original definitions in the same change. Validation is local to the runtime crate, so a failed attempt can be corrected by re-running `cargo test -p ingot-agent-runtime` after each edit.

## Artifacts and Notes

Important baseline evidence before the remaining split:

    $ wc -l crates/ingot-agent-runtime/src/lib.rs
         955 crates/ingot-agent-runtime/src/lib.rs

    $ cargo test -p ingot-agent-runtime
    test result: ok. 26 passed; 0 failed; ...
    test result: ok. 22 passed; 0 failed; ...
    test result: ok. 8 passed; 0 failed; ...
    test result: ok. 10 passed; 0 failed; ...
    test result: ok. 2 passed; 0 failed; ...
    test result: ok. 20 passed; 0 failed; ...

## Interfaces and Dependencies

At the end of this refactor, these internal crate interfaces must exist:

In `crates/ingot-agent-runtime/src/runtime_ports.rs`, define and export to the crate:

    pub(crate) struct RuntimeConvergencePort {
        pub(crate) dispatcher: JobDispatcher,
    }

    pub(crate) struct RuntimeFinalizePort {
        pub(crate) dispatcher: JobDispatcher,
    }

    pub(crate) struct RuntimeReconciliationPort {
        pub(crate) dispatcher: JobDispatcher,
    }

    pub(crate) fn usecase_to_runtime_error(error: ingot_usecases::UseCaseError) -> RuntimeError
    pub(crate) fn usecase_from_runtime_error(error: RuntimeError) -> ingot_usecases::UseCaseError
    pub(crate) async fn drain_until_idle<F, Fut>(step: F) -> Result<(), RuntimeError>

In `crates/ingot-agent-runtime/src/job_support.rs`, define and export to the crate the prepared-run types and helper functions currently shared across `preparation.rs`, `execution.rs`, `supervisor.rs`, and `reconciliation.rs`.

Revision note: 2026-03-28 / Codex. Created the initial ExecPlan after the pause-hook extraction and before the remaining runtime adapter/helper split so the rest of the refactor is recorded in the repository’s required living-plan format.

Revision note: 2026-03-28 / Codex. Updated the plan after completing the runtime port extraction, helper extraction, and validation so the final module layout, discoveries, and completion state are recorded.
