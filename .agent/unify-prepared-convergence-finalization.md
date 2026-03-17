# Unify prepared-convergence finalization

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with `.agent/PLANS.md`.

## Purpose / Big Picture

After this change, manual approval and daemon auto-finalization will use the same prepared-convergence finalize protocol. That protocol will make the same decisions about target-ref idempotency, checkout synchronization, git-operation reuse, and final DB state regardless of whether the finalize was triggered by the HTTP route or the runtime loop.

The user-visible proof is straightforward: approving an item through `/approval/approve` and auto-finalizing a not-required item will both update the checkout correctly, treat an already-final target ref as success, record checkout-blocked state consistently, and avoid creating duplicate unresolved `finalize_target_ref` operations for the same convergence.

## Progress

- [x] 2026-03-16T12:50:00Z Captured the target refactor shape and implementation plan.
- [x] 2026-03-16T13:40:00Z Replaced approval boolean state with typed readiness and added the shared finalize helper in `crates/ingot-usecases`.
- [x] 2026-03-16T14:05:00Z Routed HTTP approval and runtime auto-finalize through the shared helper, removing adapter-owned finalize sequencing.
- [x] 2026-03-16T14:15:00Z Added the forward SQLite migration and repository helpers for unresolved finalize-operation uniqueness and get-or-create lookup.
- [x] 2026-03-16T14:35:00Z Expanded usecase, HTTP, runtime, and store tests to cover parity, blocked checkout, and duplicate-op retry cases.

## Surprises & Discoveries

- Observation: the current usecase boundary still models approval readiness as `Option<Convergence>` plus `bool prepared_target_valid`, so the type system does not force callers to distinguish "already finalized ref" from "stale ref".
  Evidence: `crates/ingot-usecases/src/convergence.rs` currently defines `ConvergenceApprovalContext { prepared_convergence, prepared_target_valid, queue_entry }`.

- Observation: the runtime already has get-or-create behavior for finalize git operations, but the HTTP route does not.
  Evidence: `crates/ingot-agent-runtime/src/lib.rs` calls `find_unresolved_finalize_operation_for_convergence(...)` before creating a new op; `crates/ingot-http-api/src/router/convergence.rs` currently inserts directly.

- Observation: the usecase-level system-action loop did not need more workflow knowledge to share finalization; only the finalize sequence needed to move. The queue-head/evaluator decision logic could stay where it was.
  Evidence: `tick_system_actions()` still decides when to finalize, but the runtime path now delegates the finalize protocol itself to the shared helper.

- Observation: carrying a precomputed target-ref decision (`PreparedTargetState`) across the usecase/adapter boundary reintroduced the already-finalized race. The decision had to move down into the git primitive that actually reads and mutates the ref.
  Evidence: the follow-up review found that a target advanced to the prepared OID after readiness was computed but before CAS execution was still reported as stale.

## Decision Log

- Decision: implement this as one refactor instead of a phased rollout.
  Rationale: the goal is to eliminate finalize drift, not just patch the current HTTP path. Landing the shared helper, typed readiness, and DB uniqueness together gives one stable protocol boundary.
  Date/Author: 2026-03-16 / Codex

- Decision: add a forward migration for finalize-op uniqueness instead of editing an existing migration.
  Rationale: existing migrations are immutable in this repository because deployed SQLite databases depend on their checksums.
  Date/Author: 2026-03-16 / Codex

- Decision: remove `PreparedTargetState` from the shared finalize contract and add a git-level `finalize_target_ref(...)` helper that resolves current ref state at execution time.
  Rationale: this is cleaner than patching the race locally in both adapters, and it keeps the idempotency decision next to the actual ref read/CAS operations.
  Date/Author: 2026-03-17 / Codex

## Outcomes & Retrospective

The refactor succeeded in collapsing the finalize protocol into one shared helper while preserving the existing HTTP surface and runtime control flow. Manual approval and auto-finalize now share the same git-operation lifecycle, already-final-target behavior, checkout-blocked handling, and success sequencing.

The strongest improvement is that the old boolean readiness split is gone in favor of typed approval readiness, and unresolved finalize-op dedup is now enforced both in code and in SQLite. The remaining gap is breadth, not design: the new safety rails cover approval and finalize paths directly, but if similar duplication appears in other workflows the same pattern should be applied there instead of reintroducing adapter-owned orchestration.

## Context and Orientation

Prepared convergence is the state where an integration commit exists and is ready to become the target branch head. Today that finalize protocol is split across `crates/ingot-http-api/src/router/convergence.rs` for manual approval and `crates/ingot-agent-runtime/src/lib.rs` for daemon auto-finalize. The usecase layer in `crates/ingot-usecases/src/convergence.rs` validates high-level preconditions for approval, but it still delegates the actual finalize sequence to adapter-owned methods.

Git operations are persisted in the `git_operations` table and are used as the durable recovery log for partially completed ref updates and checkout synchronization. The current schema lives in `crates/ingot-store-sqlite/migrations/0001_initial.sql`, and unresolved operations are queried by `crates/ingot-store-sqlite/src/store/git_operation.rs`.

The refactor will introduce one shared finalize helper in the usecase layer, plus a tighter repository boundary for unresolved finalize operations, while keeping the HTTP surface unchanged.

## Plan of Work

First, update `crates/ingot-usecases/src/convergence.rs` to replace the approval readiness booleans with typed readiness and define a shared finalize helper that drives the prepared-convergence finalize protocol. That helper will own idempotent target-ref handling, checkout-sync decisions, git-operation status transitions, and final success persistence while leaving only primitive git/DB operations to adapter-provided methods.

Next, rework the HTTP and runtime adapters to implement the new primitive finalize port instead of each encoding its own finalize sequence. The HTTP route will become a thin wrapper around the shared helper for approval-triggered finalization. The runtime auto-finalize path will continue to decide when to finalize, but once it commits to finalization it will invoke the same shared helper.

Then, add a new SQLite migration creating a partial unique index for unresolved `finalize_target_ref` operations scoped to convergence entities. Extend the git-operation repository layer with a lookup and get-or-create path that re-queries after uniqueness conflicts so retries converge on one durable operation row.

Finally, extend tests across usecases, HTTP routes, runtime, and SQLite store code so the finalize protocol is verified through the same scenario matrix in each layer.

## Concrete Steps

From `/Users/aa/Documents/ingot`:

    cargo test -p ingot-usecases convergence
    cargo test -p ingot-http-api --test convergence_routes
    cargo test -p ingot-agent-runtime convergence reconciliation
    cargo test -p ingot-store-sqlite git_operation

During implementation, rerun the narrowest affected suite after each subsystem change, then run all four commands before considering the refactor complete.

## Validation and Acceptance

Acceptance requires all of the following:

1. `cargo test -p ingot-http-api --test convergence_routes` proves manual approval updates the checkout, treats an already-final target ref as success, records blocked-checkout behavior consistently, and does not create duplicate unresolved finalize ops on retry.
2. `cargo test -p ingot-agent-runtime convergence reconciliation` proves auto-finalize uses the same finalize protocol and keeps blocked-checkout recovery and operation reuse intact.
3. `cargo test -p ingot-usecases convergence` proves the typed readiness and finalize helper distinguish missing, stale, needs-ref-update, already-finalized, and checkout-blocked cases explicitly.
4. `cargo test -p ingot-store-sqlite git_operation` proves the new unique index and repository logic enforce one unresolved finalize op per convergence.

## Idempotence and Recovery

The implementation must preserve `git_operations` as the recovery boundary for git-plus-database finalization. The shared helper must reuse unresolved finalize ops instead of inserting duplicates, and retries after partial failure must be safe. The migration must be additive and rerunnable through the normal migration flow.

## Artifacts and Notes

Important evidence to capture while implementing:

    - the new partial unique index definition in the forward migration
    - the usecase test asserting already-finalized target refs are modeled explicitly
    - the HTTP/runtime tests proving retry reuses a single unresolved finalize op

## Interfaces and Dependencies

At the end of this refactor:

- `crates/ingot-usecases/src/convergence.rs` must define typed approval readiness instead of the current `prepared_target_valid` boolean.
- The usecase layer must expose one shared prepared-convergence finalize helper callable by both the HTTP approval path and the runtime auto-finalize path.
- The adapter boundary must provide primitive finalize operations only: git-operation lookup/creation, target-ref application, checkout-sync inspection/sync, success persistence, and checkout-blocked reconciliation.
- `crates/ingot-store-sqlite/src/store/git_operation.rs` must support unresolved finalize-op lookup for a convergence and safe get-or-create semantics on top of the new unique index.

Revision note: created this ExecPlan at implementation start to satisfy the repository requirement for significant refactors and to pin the intended protocol unification before code changes begin.
