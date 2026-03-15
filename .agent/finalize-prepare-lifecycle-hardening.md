# Harden Finalize, Prepare, and Revision Teardown Consistency

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows [.agent/PLANS.md](./PLANS.md) and must be maintained in accordance with that file.

## Purpose / Big Picture

After this change, auto-finalized convergences will stop leaving projects stuck in stale-mirror mode, cancelling or superseding an item revision will stop old jobs and convergence work from reviving or continuing after the item has moved on, and invalid branch names will be rejected before any Git resolution step runs. Operators should be able to revise, defer, dismiss, invalidate, approve, and reject items without hidden background work mutating stale revisions or daemon-managed refs disappearing from the canonical mirror.

## Progress

- [x] (2026-03-14 14:40Z) Re-read `.agent/PLANS.md`, inspected the dirty worktree, and confirmed the six reported bugs are still present in the current in-progress runtime/router refactor.
- [x] (2026-03-14 14:58Z) Updated runtime git-operation handling so successful in-process finalize and prepare flows no longer leave unresolved applied operations behind, and startup adoption no longer revives terminal convergences.
- [x] (2026-03-14 15:07Z) Expanded router teardown and item-mutation coverage so revise, defer, dismiss, and approval rejection terminate current-revision jobs, convergences, queue entries, and matching git operations together.
- [x] (2026-03-14 15:14Z) Hardened canonical mirror refresh to preserve `refs/ingot/*`, strengthened target-ref validation to reject Git-invalid branch names, and added focused tests in the touched crates.
- [x] (2026-03-14 15:54Z) Ran `cargo test -p ingot-agent-runtime`, `cargo test -p ingot-http-api`, `cargo test -p ingot-git`, `cargo test -p ingot-usecases`, `cargo fmt --all`, `make test`, `make lint`, and `make ci`.
- [x] (2026-03-14 21:44Z) Moved exact ref-format validation into `ingot-git`, narrowed `normalize_target_ref()` back to canonicalization, unified the finalize-success tail behind a shared runtime helper, and removed the `project-layout` React `act(...)` warning from direct Vitest and `make ci` output.

## Surprises & Discoveries

- Observation: the current worktree already contains a broad queueing and mirror-path refactor in the same runtime and router files, so this work must be layered onto that refactor rather than reimplemented from scratch.
  Evidence: `git diff -- crates/ingot-agent-runtime/src/lib.rs` and `git diff -- crates/ingot-http-api/src/router.rs` both show large, unpublished changes for queue entries, mirror paths, and startup reconciliation.

- Observation: the finalize refresh guard is still necessary because startup recovery relies on a locally advanced mirror ref remaining in place while checkout sync is blocked.
  Evidence: the current `refresh_project_mirror()` implementations in both runtime and router still intentionally skip `ensure_mirror()` when an unresolved `FinalizeTargetRef` exists and the mirror directory already exists.

- Observation: `git check-ref-format` accepts some names that the earlier manual rule set rejected, including `refs/heads/@` and `refs/heads/-leading-dash`.
  Evidence: the first `ingot-git` helper test run failed until the invalid-ref expectations were updated to match Git’s actual acceptance set.

- Observation: the stubborn `project-layout` warning was caused by test teardown mutating zustand stores while components were still mounted, not just by unsettled layout queries.
  Evidence: the warning disappeared only after wrapping the store resets in `act(...)`; pre-seeding query cache data and stubbing the connection/store functions alone was not sufficient.

## Decision Log

- Decision: keep the finalize refresh guard and instead tighten git-operation resolution so only truly unresolved finalizations hold the guard open.
  Rationale: removing the guard would reintroduce checkout-sync recovery bugs by overwriting the locally advanced mirror ref before the registered checkout catches up.
  Date/Author: 2026-03-14 / Codex

- Decision: use auto-cancel semantics for `revise`, `defer`, `dismiss`, and `invalidate` instead of rejecting those routes while current-revision work is still active.
  Rationale: this matches the agreed plan, gives operators a single successful mutation path, and aligns with the existing explicit job-cancellation flow already present in the router.
  Date/Author: 2026-03-14 / Codex

- Decision: reuse existing `Cancelled`, `Failed`, and `Reconciled` git-operation and work-status enums instead of adding a migration or new terminal states.
  Rationale: the repository already has enough terminal states to model “intentionally abandoned finalize”, “observed successful prepare”, and “cancelled revision work” without schema changes.
  Date/Author: 2026-03-14 / Codex

- Decision: use plain `git check-ref-format` on normalized full refs instead of `git check-ref-format --branch`.
  Rationale: the repository already normalizes to `refs/heads/...`, and validating the full ref avoids branch-shorthand semantics while making `ingot-git` the exact source of truth for ref acceptance.
  Date/Author: 2026-03-14 / Codex

- Decision: keep the `project-layout` warning fix local to the test file by preloading query cache data, stubbing the mount-only store callbacks, and wrapping teardown store resets in `act(...)`.
  Rationale: that removes the noise without changing production code or broadening global test harness behavior.
  Date/Author: 2026-03-14 / Codex

## Outcomes & Retrospective

Startup recovery still works, successful in-process finalize and prepare flows resolve their git operations instead of leaving projects in stale-mirror mode, and the finalize-success tail is now shared so the startup and auto-finalize paths cannot drift independently. Exact ref validation now lives in `ingot-git`, so item and project ingress paths defer to Git itself instead of mirroring refname rules in `ingot-usecases`. The `project-layout` test no longer prints the React `act(...)` warning in direct Vitest output or during `make ci`.

Validation now covered the direct crate tests, targeted route/runtime regressions, the project-layout Vitest file, and a final successful `make ci` run on the completed workspace.

## Context and Orientation

The runtime dispatcher lives in `crates/ingot-agent-runtime/src/lib.rs`. It owns startup reconciliation, automatic convergence preparation, automatic finalization, and adoption of unresolved git operations. A “git operation” is the durable record in the `git_operations` table that tracks mirror ref updates, convergence prepare commits, workspace ref deletion, and similar Git-side mutations.

The HTTP item mutation routes live in `crates/ingot-http-api/src/router.rs`. Those routes already hold a per-project mutation lock while revising, deferring, closing, approving, rejecting, or cancelling work. The current teardown helper only cancels prepared convergences and queue entries; it does not cancel active jobs, queued or running convergences, or matching unresolved git operations.

The canonical mirror helper lives in `crates/ingot-git/src/project_repo.rs`. That mirror is a bare Git repository stored under the daemon state root and is the source of truth for daemon-managed worktree refs such as `refs/ingot/workspaces/*`. Refreshing the mirror must keep checkout-owned refs synchronized from the registered checkout without pruning daemon-owned refs.

Target-ref normalization currently lives in `crates/ingot-usecases/src/item.rs`. After the follow-up hardening, it remains only a canonicalizer from branch names to `refs/heads/<branch>` plus a non-branch namespace guard; exact Git ref acceptance now belongs to `crates/ingot-git/src/commands.rs`.

## Plan of Work

In `crates/ingot-agent-runtime/src/lib.rs`, finish the split between “operation applied in the mirror” and “operation fully reconciled into durable runtime state”. `auto_finalize_prepared_convergence()` should either reuse or mirror `reconcile_finalize_target_ref_operation()` so a successful in-process finalize ends with `GitOperationStatus::Reconciled`, `adopt_finalized_target_ref()` already applied, and `GitOperationReconciled` activity emitted. If checkout sync is blocked, the operation should remain unresolved so the existing refresh guard still protects the advanced mirror ref.

In the same runtime file, change the successful prepare path so `PrepareConvergenceCommit` is marked `Reconciled` immediately after the prepared convergence and workspace rows are updated. Then make `adopt_prepared_convergence()` terminal-state aware so cancelled, failed, or finalized convergences are never promoted back to prepared during startup reconciliation from an older unresolved operation.

In `crates/ingot-http-api/src/router.rs`, replace the current `RevisionLaneTeardown` helper with a broader current-revision cancellation helper. It must cancel active same-revision jobs through `finish_job_non_success`, clear attached workspaces, cancel queued, running, and prepared convergences, abandon or remove integration workspaces, cancel active queue entries, and resolve matching unresolved git operations for the affected convergence. `PrepareConvergenceCommit` should become `Reconciled` because the prepare result was observed and intentionally cancelled; `FinalizeTargetRef` should become `Failed` because the finalization was abandoned before adoption completed. The existing item mutation routes should call this helper before changing the item or revision state.

In `crates/ingot-git/src/project_repo.rs`, replace the all-refs mirror fetch so it refreshes only checkout-owned refs from origin and leaves `refs/ingot/*` alone. In `crates/ingot-usecases/src/item.rs`, strengthen branch-name validation so invalid Git branch names fail during normalization with `UseCaseError::InvalidTargetRef`.

## Concrete Steps

Work from the repository root at `/Users/aa/Documents/ingot`.

Run targeted runtime and router tests while iterating:

    cargo test -p ingot-agent-runtime
    cargo test -p ingot-http-api

Run focused crate validation for the shared helpers:

    cargo test -p ingot-git
    cargo test -p ingot-usecases

Before closing out, run the repository gates relevant to this Rust-only change:

    make test
    make lint

If those pass cleanly in the dirty worktree, run:

    make ci

## Validation and Acceptance

The change is complete when these behaviors are observable:

Successful auto-finalize no longer leaves an unresolved finalize operation behind, and a later mirror refresh can update checkout-owned refs again instead of leaving the project stuck in stale-mirror mode.

Cancelling or superseding a revision through revise, defer, dismiss, invalidate, or approval rejection leaves no active same-revision jobs, prepared or active convergences, or lane queue entries behind, and startup reconciliation does not revive cancelled prepared convergences from legacy unresolved operations.

Refreshing the canonical mirror preserves daemon-managed refs such as `refs/ingot/workspaces/*` while still updating branch and tag refs from the registered checkout.

Creating or revising an item with a Git-invalid branch name returns the existing `invalid_target_ref` API error instead of a later unresolved-ref or internal Git error.

## Idempotence and Recovery

All state transitions in this change should stay safe under retries. Re-running startup reconciliation after a successful fix should find no unresolved prepare or finalize operations to adopt. Re-running the item mutation routes after a partial failure should either find the route already completed or encounter only terminal current-revision work records, not live background state that must be manually cleaned first.

If a test or compile step fails midway, fix the code and rerun the same targeted crate tests before moving back to repo-level gates. No database migration or destructive rollback should be required.

## Artifacts and Notes

The key proof points for this work are:

    A runtime test that successful auto-finalize leaves no unresolved finalize operation and allows a later mirror refresh to refetch checkout heads.

    A runtime or router test that a cancelled prepared convergence plus a legacy applied prepare operation stays cancelled after startup reconciliation.

    Router tests that revise, defer, dismiss, or invalidate an item with active same-revision work and then verify that jobs, convergences, queue entries, and matching git operations are all terminal.

    A git mirror test that creates a daemon-managed `refs/ingot/workspaces/*` ref in the mirror, refreshes from origin, and verifies that the workspace ref still exists.

## Interfaces and Dependencies

No external HTTP route shapes or database schemas should change.

The runtime should continue using `ingot_store_sqlite::Database::list_unresolved_git_operations()` and `update_git_operation()` for durable git-operation state. The router teardown helper should keep using `FinishJobNonSuccessParams` for job cancellation so running subprocesses observe `JobStatus::Cancelled` and stop on the next heartbeat tick.

The target-ref normalization interface should remain:

    pub fn normalize_target_ref(target_ref: &str) -> Result<String, UseCaseError>

and it must still return fully qualified `refs/heads/<branch>` strings on success.

Revision note: created this ExecPlan during implementation because the change spans runtime startup reconciliation, router item-mutation semantics, the git mirror helper, and shared target-ref validation.

Revision note: updated this ExecPlan after the follow-up hardening pass to record the move to Git-backed exact ref validation, the finalize-helper deduplication, the project-layout warning fix, and the final successful `make ci` run.
