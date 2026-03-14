# Move convergence and reconciliation ownership into ingot-usecases

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows `.agent/PLANS.md` and must be maintained in accordance with that file.

## Purpose / Big Picture

After this change, the convergence state machine and startup recovery logic will live in `ingot-usecases` instead of being split across the HTTP router and the runtime loop. The daemon and HTTP API should become thin adapters that parse requests, call services, map errors, and render responses, while the usecase layer owns queue prepare, approval transitions, convergence prepare/finalize/invalidate, checkout-sync escalation, and startup maintenance decisions.

## Progress

- [x] (2026-03-14 20:58Z) Re-read `.agent/PLANS.md`, inspected the dirty worktree, and confirmed `crates/ingot-usecases/src/convergence.rs` and `crates/ingot-usecases/src/reconciliation.rs` are still placeholders while mutating convergence logic remains in `router.rs` and `ingot-agent-runtime`.
- [x] (2026-03-14 21:23Z) Implemented `ConvergenceService` and `ReconciliationService`, exported them from `crates/ingot-usecases/src/lib.rs`, and added queue-prepare, approval, reject-approval, system-action, and maintenance orchestration coverage in `ingot-usecases` tests.
- [x] (2026-03-14 21:31Z) Extended `UseCaseError` and the API error mapping for the new convergence queue cases, and added the supporting convergence/reconciliation port shapes in `ingot-domain::ports`.
- [x] (2026-03-14 21:39Z) Wired the HTTP router queue-prepare, approve, and reject-approval mutations through `ConvergenceService` using a local `HttpConvergencePort` adapter while leaving read-only response shaping in the router.
- [x] (2026-03-14 21:46Z) Wired the runtime startup and per-tick orchestration through `ConvergenceService` and `ReconciliationService` using local runtime adapters, leaving the lower-level Git/workspace helpers in the runtime for now.
- [x] (2026-03-14 21:56Z) Ran `cargo test -p ingot-usecases`, `cargo test -p ingot-agent-runtime`, `cargo test -p ingot-http-api`, and `make test`.
- [x] (2026-03-14 22:18Z) Performed a post-implementation review, fixed the four concrete issues found in the first pass, split convergence command and system-action ports, restored HTTP not-found/project scoping behavior, and added focused regression tests for those cases plus runtime maintenance progress reporting.

## Surprises & Discoveries

- Observation: the worktree already contains a large in-progress convergence queue and mirror refactor, so this extraction must build on that state instead of assuming the last committed baseline.
  Evidence: `git status --short` and `git diff --stat -- crates/ingot-agent-runtime/src/lib.rs crates/ingot-http-api/src/router.rs ...` show thousands of lines of unpublished changes in the exact adapter files this refactor touches.

- Observation: the current code has a queue-aware approval state machine with `ApprovalState::Granted` in addition to `Pending` and `Approved`.
  Evidence: `crates/ingot-domain/src/item.rs` defines `Granted`, and both router and runtime branch on it during approval/finalization.

- Observation: the practical extraction boundary in this dirty tree is “move orchestration and route-level mutations into usecases, keep the low-level Git/workspace substeps behind adapter ports for now”.
  Evidence: the runtime already contains many intertwined helper methods for prepare/finalize/adoption, but the new services can still own the top-level state-machine sequencing without duplicating that in the adapters.

## Decision Log

- Decision: preserve the current queue-aware convergence behavior exactly, including `ApprovalState::Granted`, queue-head gating, and checkout-sync escalation.
  Rationale: this task is about architectural ownership, not reworking the runtime state model while the tree is already mid-refactor.
  Date/Author: 2026-03-14 / Codex

- Decision: keep read-only API shaping in the HTTP layer and move only mutating convergence and maintenance logic into `ingot-usecases`.
  Rationale: the architectural complaint is about ownership of system actions and recovery, not about moving every projection helper out of the adapters.
  Date/Author: 2026-03-14 / Codex

## Outcomes & Retrospective

The usecase placeholders are now real services. `ConvergenceService` owns queue prepare, approval grant, approval rejection, and system-action sequencing; `ReconciliationService` owns startup and per-tick maintenance sequencing. The HTTP router now delegates its main convergence mutations to the usecase service, and the runtime now delegates startup/tick orchestration to the usecase services.

The extraction is not a full low-level Git/workspace cutover yet. The runtime still contains the underlying prepare/finalize/invalidate/adoption helper implementations, but those helpers are now called through usecase-owned orchestration instead of the adapters owning the orchestration themselves.

The follow-up review fixed a real project-scoping bug in the approval path, restored correct `project_not_found` / `item_not_found` mapping for the new HTTP convergence adapter, made runtime maintenance progress reporting truthful, and split the previously muddled convergence port boundary into separate command and system-action traits.

## Context and Orientation

`crates/ingot-http-api/src/router.rs` currently owns queue prepare, approval grant/reject, and one copy of convergence prepare logic. `crates/ingot-agent-runtime/src/lib.rs` currently owns automatic system-action progression and startup maintenance: queue-head preparation, stale prepared invalidation, auto-finalization, git-operation adoption, active job/convergence cleanup, and workspace-retention cleanup.

`crates/ingot-usecases/src/job.rs` already demonstrates the target shape. It owns job dispatch and completion logic behind explicit service types and repository/Git ports, while the adapters only provide infrastructure and response mapping. This refactor should give convergence and reconciliation the same shape.

“Queue head” means the active item at the front of the target-ref lane in `convergence_queue_entries`. “Checkout sync” means reconciling the registered project checkout with the daemon-managed mirror after finalization. “Maintenance recovery” means startup and per-tick repair of durable state after interrupted jobs, convergences, or Git operations.

## Plan of Work

First, add service implementations to `crates/ingot-usecases/src/convergence.rs` and `crates/ingot-usecases/src/reconciliation.rs`, following the `CompleteJobService` pattern where feasible. Those services should own the mutating state-machine decisions and accept the repository/Git/workspace abstractions they need as generic parameters.

Next, add or extend the supporting traits and errors so the services can operate without depending on infrastructure crates. The service APIs should be public and stable enough for both the router and the runtime to use directly.

Then replace the mutating convergence and reconciliation code in `router.rs` and `ingot-agent-runtime/src/lib.rs` with thin calls into the new services. Keep request parsing, polling, runner orchestration, response shaping, and bootstrap behavior in the adapters.

## Concrete Steps

From `/Users/aa/Documents/ingot`, run:

    cargo test -p ingot-usecases
    cargo test -p ingot-http-api
    cargo test -p ingot-agent-runtime

If those pass, run:

    make test

## Validation and Acceptance

Acceptance is met when:

`crates/ingot-usecases/src/convergence.rs` and `crates/ingot-usecases/src/reconciliation.rs` contain real service implementations instead of placeholder comments.

The HTTP router no longer directly mutates queue/convergence/approval state for queue prepare and approval routes; it delegates to `ConvergenceService`.

The runtime no longer directly owns the top-level system-action and maintenance state-machine logic; it delegates to `ConvergenceService` and `ReconciliationService`.

Existing convergence queue, approval, finalization, and startup reconciliation tests still pass.

## Idempotence and Recovery

This refactor should not change durable schemas or API shapes. Re-running the tests should be safe. If a partial implementation fails to compile, fix the service interfaces first, then re-run the targeted crate tests before the repository-wide test command.

## Artifacts and Notes

Validation results:

    cargo test -p ingot-usecases
    test result: ok. 40 passed; 0 failed

    cargo test -p ingot-agent-runtime
    test result: ok. 33 passed; 0 failed

    cargo test -p ingot-http-api
    test result: ok. 47 passed; 0 failed

    make test
    test result: ok

## Interfaces and Dependencies

The public `ingot-usecases` surface must export:

`ConvergenceService`

`ReconciliationService`

The adapters should continue using `ProjectLocks`, `Database`, the existing git/workspace helpers, and the current route shapes. The main interface change is that mutating convergence and reconciliation behavior will be invoked through new usecase services instead of being embedded in adapter-local helper functions.

Revision note: created this ExecPlan before implementation because the work spans the usecase layer, runtime, HTTP API, and persistence interfaces and must stay coordinated with the dirty in-progress convergence refactor already in this tree.

Revision note: updated this ExecPlan after implementation to record the final extraction boundary, the adapter-based compromise used in the dirty tree, and the exact Rust validation commands that now pass.
