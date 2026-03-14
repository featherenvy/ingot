# Move convergence and reconciliation ownership into ingot-usecases

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows `.agent/PLANS.md` and must be maintained in accordance with that file.

## Purpose / Big Picture

After this change, convergence queueing, approval, convergence system actions, and startup reconciliation will no longer be defined primarily inside the HTTP router and runtime dispatcher. The application behavior should stay the same, but the state-machine ownership will move into `crates/ingot-usecases`, leaving the router and runtime as adapters that parse requests, run polling loops, and invoke explicit usecase services.

## Progress

- [x] (2026-03-14 20:30Z) Investigated the current workflow and confirmed that `crates/ingot-usecases/src/convergence.rs` and `crates/ingot-usecases/src/reconciliation.rs` are still placeholders while convergence and reconciliation mutations live in `crates/ingot-http-api/src/router.rs` and `crates/ingot-agent-runtime/src/lib.rs`.
- [x] (2026-03-14 21:10Z) Implemented `ConvergenceService` and `ReconciliationService` in `crates/ingot-usecases`, added the supporting convergence/reconciliation port surface, and extended the usecase error mapping for convergence queue and lane-head failures.
- [x] (2026-03-14 21:18Z) Delegated runtime maintenance/startup sequencing and router convergence queue/approval/reject entrypoints to the new usecase services while preserving the current queue-aware behavior.
- [x] (2026-03-14 21:25Z) Added focused `ingot-usecases` tests for convergence and reconciliation sequencing and validated `cargo test -p ingot-usecases`, `cargo test -p ingot-agent-runtime --lib`, `cargo test -p ingot-http-api --lib`, and `make test`.

## Surprises & Discoveries

- Observation: the current worktree already contains a broad, unpublished convergence queue and mirror-management refactor, so this extraction must layer onto that state instead of assuming a clean baseline.
  Evidence: `git status --short` shows large in-progress edits in `crates/ingot-http-api/src/router.rs` and `crates/ingot-agent-runtime/src/lib.rs`, plus new queue and project-repo files.

- Observation: the cleanest safe extraction in this dirty tree is to move the top-level state-machine sequencing into `ingot-usecases` first, while leaving many low-level Git/SQLite/workspace helper methods in the runtime and router as adapter-port implementations.
  Evidence: the runtime and router already carry large unpublished helper surfaces for queue management, mirror refresh, checkout sync, and teardown; extracting the sequencing still thins the adapters without forcing a second large rewrite of those helpers in one turn.

## Decision Log

- Decision: preserve current behavior, including the queue-aware `ApprovalState::Granted` intermediate state and checkout-sync escalation behavior, instead of trying to realign the full state model in the same change.
  Rationale: the task is about crate ownership and adapter thinning; changing convergence semantics at the same time would make the refactor unsafe in the already-dirty tree.
  Date/Author: 2026-03-14 / Codex

- Decision: keep read-only evaluation overlay and response shaping in the adapter layer unless moving a helper is necessary to remove duplicated mutation logic.
  Rationale: the architectural gap is about system-action and reconciliation ownership, not about where HTTP-specific response assembly lives.
  Date/Author: 2026-03-14 / Codex

- Decision: move the convergence and reconciliation orchestration into usecase services now, but leave low-level adapter methods as port implementations in `router.rs` and `ingot-agent-runtime` for this step.
  Rationale: this satisfies the architectural ownership change without destabilizing the already-dirty runtime/router helper surfaces in the same turn.
  Date/Author: 2026-03-14 / Codex

## Outcomes & Retrospective

`crates/ingot-usecases/src/convergence.rs` and `crates/ingot-usecases/src/reconciliation.rs` now contain concrete services with tests. The runtime no longer owns the top-level startup/maintenance/system-action sequencing directly; it delegates those branches to the new services. The HTTP router no longer owns the queue-prepare, approval-grant, and approval-reject entrypoint logic directly; those routes now delegate to `ConvergenceService` and keep response loading and HTTP-specific activity payload shaping locally.

The remaining gap is that many low-level convergence and reconciliation helper methods still live in the adapter crates as port implementations. That is smaller and more explicit than before, but it is still adapter code that could be pushed further down into shared infrastructure in a follow-up refactor if desired.

## Context and Orientation

`crates/ingot-usecases` already owns item creation, finding triage, and job completion. `crates/ingot-usecases/src/job.rs` is the best current example of the intended style: a service defined over narrow ports plus a project mutation lock, with transaction-heavy state changes hidden behind a repository port.

`crates/ingot-http-api/src/router.rs` currently owns mutating convergence routes and helpers such as queue prepare, approval grant/reject, revision lane teardown, and an alternate convergence prepare path. `crates/ingot-agent-runtime/src/lib.rs` currently owns startup reconciliation, maintenance reconciliation, convergence queue-head preparation, stale prepared invalidation, checkout-sync escalation, and auto-finalization.

The extraction target is to move those state transitions into `crates/ingot-usecases/src/convergence.rs` and `crates/ingot-usecases/src/reconciliation.rs` while leaving request parsing, polling loops, subprocess execution, and response loading in the adapter crates.

## Plan of Work

Add the new usecase-facing APIs and ports first so there is a stable boundary to wire against. Then replace the router and runtime mutation entrypoints with thin delegation layers that call the services and keep only adapter-specific concerns locally. Keep the service logic behavior-preserving by copying the current queue, approval, convergence, checkout-sync, and reconciliation branches into the usecase layer with minimal semantic change.

## Concrete Steps

From `/Users/aa/Documents/ingot`, iterate with:

    cargo test -p ingot-usecases
    cargo test -p ingot-http-api
    cargo test -p ingot-agent-runtime

Before closing out, run:

    make test

## Validation and Acceptance

The change is acceptable when the mutating convergence and reconciliation call sites in the router and runtime are reduced to service invocation plus adapter work, the new usecase services are covered by focused tests, and the existing router/runtime behavior still passes its Rust tests.

## Idempotence and Recovery

This refactor is code-only. If an intermediate edit fails, fix the compile or test failure and rerun the same targeted command. No schema migration or destructive rollback is expected in this phase.

## Artifacts and Notes

The key architectural proof after implementation is that `crates/ingot-usecases/src/convergence.rs` and `crates/ingot-usecases/src/reconciliation.rs` are no longer placeholders and the router/runtime no longer contain the primary state-machine branches for these flows.

Validation completed with:

    cargo test -p ingot-usecases
    cargo test -p ingot-agent-runtime --lib
    cargo test -p ingot-http-api --lib
    make test

## Interfaces and Dependencies

The main public additions should be `ConvergenceService` and `ReconciliationService` exported from `crates/ingot-usecases/src/lib.rs`, plus the supporting ports and error variants required to preserve the current convergence, queue, approval, checkout-sync, and reconciliation behavior without depending directly on HTTP, SQLite, or process-layer implementations.

Revision note: created this ExecPlan before code edits because the refactor spans multiple backend crates and the repository requires an ExecPlan for significant changes.

Revision note: updated this ExecPlan after implementation to record the final extraction boundary, the top-level ownership that moved into `ingot-usecases`, and the exact validation commands that passed.
