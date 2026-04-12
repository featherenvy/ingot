# Tighten finalization mutation ownership and drop backfill compatibility

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with `.agent/PLANS.md`.

## Purpose / Big Picture

After this change, finalization will have one clear owner. The SQLite store will reject illegal prior-state transitions, adapter code will not delete an integration workspace before the transactional finalization write succeeds, and the repository will stop pretending to repair historical finalized rows with a partial backfill migration. The observable result is that a failed finalization mutation leaves both the database and the integration workspace untouched, while valid finalization flows still finalize and clean up normally.

## Progress

- [x] (2026-04-12T16:36Z) Claimed `ingot-oma` and re-read the runtime, HTTP, workflow, migration, and store code paths implicated by the review findings.
- [x] (2026-04-12T16:38Z) Wrote this ExecPlan for the follow-up review fixes.
- [x] (2026-04-12T16:42Z) Tightened `crates/ingot-store-sqlite/src/store/finalization.rs` so target-ref advance now rejects non-`prepared`/`finalized` convergences, both mutations reject terminal finalize operations, and both mutations require the current item revision to still be open.
- [x] (2026-04-12T16:42Z) Moved adapter-side integration workspace path deletion to post-commit best-effort cleanup in `crates/ingot-agent-runtime/src/runtime_ports.rs` and `crates/ingot-http-api/src/router/convergence_port.rs`.
- [x] (2026-04-12T16:42Z) Removed the historical finalized-row backfill behavior by turning migration `0012_finalized_checkout_adoption_backfill_pending.sql` into a no-op while preserving migration version compatibility.
- [x] (2026-04-12T16:45Z) Added store regression tests for illegal transition rejection and passed targeted Rust suites plus the full `make ci` gate.

## Surprises & Discoveries

- Observation: the current repository already has a `bd` bug for these exact review findings.
  Evidence: `bd ready --json` returned `ingot-oma` with the same scope: transition guards, pre-transaction cleanup, and backfill removal.

- Observation: deleting a historical migration file outright is risky because the project uses `sqlx::migrate!("./migrations")` at runtime and in tests.
  Evidence: `crates/ingot-store-sqlite/src/db.rs` compiles the migration list directly into the binary, so a version that has already been applied still needs a corresponding file entry.

- Observation: abandoned-workspace cleanup already has a durable retry path in runtime reconciliation.
  Evidence: `crates/ingot-agent-runtime/src/reconciliation.rs` implements `reconcile_workspace_retention()` and `remove_abandoned_workspace(...)`, so post-commit workspace deletion can safely degrade to best-effort immediate cleanup.

## Decision Log

- Decision: keep migration version `0012` present but make it a no-op instead of deleting the file.
  Rationale: this removes the historical backfill behavior without risking migration-version mismatches for databases that have already recorded version 12.
  Date/Author: 2026-04-12 / Codex

- Decision: treat post-commit integration-workspace removal as best-effort cleanup, not as part of finalization success.
  Rationale: the database transaction is the correctness boundary. If filesystem cleanup fails after commit, runtime reconciliation can remove the abandoned workspace later without lying about finalization success.
  Date/Author: 2026-04-12 / Codex

- Decision: reject illegal prior states in the store instead of preserving permissive idempotence for old broken rows.
  Rationale: the user explicitly requested that historical backfill and backwards-compatibility paths be removed. The clean model is stricter and easier to reason about.
  Date/Author: 2026-04-12 / Codex

## Outcomes & Retrospective

This follow-up plan started from an already-refactored finalization flow and narrowed the remaining work to three review findings. The landed result matches that scope: the store now owns stricter transition guards, adapter cleanup only runs after a successful database commit, and the repository no longer ships a partial historical backfill for finalized checkout-adoption rows.

The main lesson is that the residual bugs sat at the boundary between transactional state and non-transactional filesystem cleanup. The durable state machine is now stricter and easier to reason about because correctness stops at the database commit, while workspace deletion is explicitly best-effort cleanup with existing retention reconciliation as the recovery path.

## Context and Orientation

The finalization write path is split between the usecase layer and two concrete adapters. `crates/ingot-usecases/src/convergence/finalization.rs` decides when to persist `FinalizationMutation::TargetRefAdvanced` and `FinalizationMutation::CheckoutAdoptionSucceeded`. The actual transactional state machine lives in `crates/ingot-store-sqlite/src/store/finalization.rs`, which updates convergences, git operations, queue rows, workspaces, items, and activities inside a single SQLite transaction.

Two adapter implementations currently wrap that mutation call:

- `crates/ingot-agent-runtime/src/runtime_ports.rs`
- `crates/ingot-http-api/src/router/convergence_port.rs`

Both of them currently delete the integration workspace path before calling `db.apply_finalization_mutation(...)`. That ordering is unsafe because the store mutation can still fail on stale-revision or illegal-state checks.

The migration chain lives in `crates/ingot-store-sqlite/migrations/`. Migration `0011_finalized_checkout_adoption.sql` adds the finalized checkout-adoption columns. Migration `0012_finalized_checkout_adoption_backfill_pending.sql` currently attempts to rewrite existing finalized rows. This plan removes that repair behavior because the repository no longer wants historical compatibility semantics.

## Plan of Work

First, tighten the store-owned state machine in `crates/ingot-store-sqlite/src/store/finalization.rs`. `apply_target_ref_advanced(...)` must only accept convergences in `prepared` or `finalized`, and only accept finalize operations in unresolved states that are still legal to adopt. It must reject failed, cancelled, reconciled, and other impossible inputs with explicit conflict reasons. `apply_checkout_adoption_succeeded(...)` must require a finalized convergence whose checkout adoption is not already synced, an unresolved finalize operation, and an open current item revision that is still eligible to close.

Second, change both adapter implementations of `PreparedConvergenceFinalizePort::apply_finalization_mutation(...)` so they call the database mutation first and only then attempt integration-workspace deletion. The path deletion should use the workspace row that the transaction already marked abandoned. If the path removal fails, log a warning and return success so the durable finalization result is not rolled back in the caller’s view.

Third, neutralize the historical compatibility layer by replacing the SQL body of migration `0012_finalized_checkout_adoption_backfill_pending.sql` with a no-op comment. This keeps migration versioning intact while removing any claim that the repository repairs old finalized rows.

Finally, add focused regression tests in `crates/ingot-store-sqlite/tests/` for illegal transition rejection and update any runtime or HTTP tests whose expectations depend on pre-transaction workspace deletion or migration backfill behavior.

## Concrete Steps

Work from `/Users/aa/Documents/ingot`.

Run focused validation as each layer lands:

    cargo test -p ingot-store-sqlite --test finalization
    cargo test -p ingot-agent-runtime convergence -- --nocapture
    cargo test -p ingot-http-api convergence_routes -- --nocapture

Then run the local gate most relevant to the touched code:

    cargo test -p ingot-store-sqlite -p ingot-agent-runtime -p ingot-http-api -p ingot-usecases -p ingot-workflow

If time permits and the tree stays clean enough, finish with:

    make ci

## Validation and Acceptance

Acceptance is behavior-based:

1. A `TargetRefAdvanced` mutation against a failed or cancelled convergence returns a repository conflict and leaves convergence, git-operation, item, queue, and workspace state unchanged.
2. A `CheckoutAdoptionSucceeded` mutation against a failed or reconciled finalize operation returns a repository conflict and does not close the item.
3. If the adapter cannot delete the abandoned integration workspace path after the database mutation commits, the mutation still returns success and the workspace remains available for later retention cleanup.
4. Fresh databases created from the checked-in migration chain no longer execute any finalized-row backfill SQL beyond the schema added in migration `0011`.

## Idempotence and Recovery

These edits are safe to rerun. The tighter store guards only reject state combinations that should already be impossible in the clean model. Post-commit cleanup warnings are recoverable because runtime reconciliation already knows how to delete abandoned workspaces later. Leaving migration `0012` in place as a no-op preserves version compatibility for any database that has already recorded it.

## Artifacts and Notes

Important evidence to capture during validation:

    cargo test -p ingot-store-sqlite --test finalization
    cargo test -p ingot-agent-runtime convergence
    cargo test -p ingot-http-api convergence_routes

Also capture `git diff --stat` before closing the task so the scope stays limited to finalization, adapter cleanup, migrations, and regression tests.

## Interfaces and Dependencies

The existing public mutation interface remains in `crates/ingot-domain/src/ports/mutations.rs`:

    pub enum FinalizationMutation {
        TargetRefAdvanced(FinalizationTargetRefAdvancedMutation),
        CheckoutAdoptionSucceeded(FinalizationCheckoutAdoptionSucceededMutation),
    }

The implementation changes must preserve that wire shape. The stronger invariants live behind `FinalizationRepository::apply_finalization_mutation(...)` in `crates/ingot-store-sqlite/src/store/finalization.rs`. The runtime and HTTP adapters should keep using `ingot_workspace::remove_workspace(...)` and `InfraPorts::remove_workspace_path(...)` respectively, but only after the database call has succeeded.

Revision note (2026-04-12): created for `ingot-oma` after review feedback identified three remaining issues in the post-refactor finalization flow: pre-transaction workspace deletion, permissive store mutation guards, and an unnecessary historical backfill migration.

Revision note (2026-04-12T16:45Z): updated after implementation to record the completed guard tightening, post-commit cleanup ordering, no-op migration replacement, added regression tests, and passing `make ci`.
