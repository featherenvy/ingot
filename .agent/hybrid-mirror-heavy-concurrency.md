# Hybrid Mirror Heavy Concurrency

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document is maintained in accordance with `.agent/PLANS.md`.

## Purpose / Big Picture

After this change, Ingot no longer treats the registered project checkout as the Git mutation authority. Instead, the daemon owns a hidden mirror under `~/.ingot/repos/<project_id>.git`, provisions daemon worktrees from that mirror, queues convergence per `target_ref`, and only closes an item after the visible checkout is synced to the finalized commit. The user-visible effect is that queued convergence is explicit, approval can be granted before finalization completes, and the registered checkout no longer lands in the broken “empty tree with staged deletions” state after closure.

## Progress

- [x] (2026-03-13 21:00Z) Added mirror-backed repo path helpers and queue schema/domain types.
- [x] (2026-03-13 21:25Z) Switched HTTP project registration, item/revision seed resolution, workspace provisioning, and convergence validity checks to the mirror.
- [x] (2026-03-13 21:55Z) Implemented queue admission, sticky `approval_state=granted`, and asynchronous daemon-owned finalize behavior.
- [x] (2026-03-13 22:10Z) Updated runtime reconciliation and workspace cleanup tests to use mirror-backed worktrees.
- [x] (2026-03-13 22:26Z) Verified `make ci` passes end to end.

## Surprises & Discoveries

- Observation: The old tests encoded checkout-owned worktrees directly in their fixtures, so mirror adoption broke cleanup/reconciliation tests even when the production code was correct.
  Evidence: runtime tests failed with `Workspace(Git(Io(... "No such file or directory")))` until the fixtures created worktrees from the mirror instead of the checkout.

- Observation: The HTTP helper `build_router(db)` needed test-specific state-root isolation.
  Evidence: full HTTP test runs passed individually but failed as a suite until `default_state_root()` used a unique temp directory under `#[cfg(test)]`.

## Decision Log

- Decision: Keep `Project.path` as the user-visible checkout path and derive the mirror path from `DispatcherConfig.state_root` plus `project_id`.
  Rationale: This preserves the existing project registration UX while avoiding persistent mirror-path state in SQLite.
  Date/Author: 2026-03-13 / Codex

- Decision: Make `POST /items/:item_id/convergence/prepare` queue-only and let the daemon prepare asynchronously after lane acquisition.
  Rationale: This keeps convergence work in the daemon loop and avoids duplicating prepare/finalize orchestration across HTTP and runtime.
  Date/Author: 2026-03-13 / Codex

- Decision: Introduce `approval_state=granted` instead of reusing `approved` while the item is still open.
  Rationale: `approved` already semantically meant “done and finalized”; `granted` cleanly represents sticky approval before checkout sync and finalization complete.
  Date/Author: 2026-03-13 / Codex

## Outcomes & Retrospective

The refactor now matches the intended clean-slate model: mirror-backed daemon workspaces, strict convergence lanes, sticky queue-head approval, and checkout-sync-aware finalization. The heaviest lift was not the queue table itself, but systematically removing hidden assumptions that `project.path` was always the canonical Git repo. The remaining gaps are polish-level, not functional: the old unused helper `prepare_convergence_workspace` can be deleted in a cleanup pass, and some test-only local variables can be cleaned up further if desired.

## Context and Orientation

The key backend entry points are:

- `crates/ingot-git/src/project_repo.rs` for mirror path derivation, mirror refresh, and checkout sync checks.
- `crates/ingot-http-api/src/router.rs` for project registration, queue admission, approval/reject commands, item detail loading, and test fixtures.
- `crates/ingot-agent-runtime/src/lib.rs` for queue-head promotion, async prepare/finalize, checkout-sync blocking, and startup reconciliation.

The new persistent queue model lives in SQLite via `convergence_queue_entries`, and the public item/detail contract exposes queue state plus checkout-sync blocking details.

## Plan of Work

The implemented sequence was:

1. Extend domain enums and IDs for `granted`, `checkout_sync_blocked`, queue entries, and new activity events.
2. Add the `0003_convergence_queue.sql` migration to widen the item approval/escalation checks and create the queue table.
3. Add mirror helpers in `ingot-git` and wire them into HTTP, runtime, and job-completion repo-path resolution.
4. Rework the prepare route into queue admission only, then teach the dispatcher to promote queue heads, prepare convergence, and finalize only after checkout sync preconditions pass.
5. Update the UI contract and item detail page to show queue state and checkout-sync blocking.
6. Rewrite the affected HTTP/runtime tests around mirror-backed worktrees and queue-head approval.

## Concrete Steps

From the repository root:

    cargo check
    cargo test
    make ui-test
    make ui-build
    make lint
    make ci

Expected outcome: all commands exit `0`. The HTTP and runtime tests now exercise mirror-backed worktrees; the UI tests expect `queue` in item summary/detail payloads.

## Validation and Acceptance

Acceptance is satisfied when all of the following are true:

- Project registration creates and refreshes a hidden mirror under the daemon state root.
- Queueing convergence no longer prepares synchronously in the HTTP route; the queue head is visible in item detail and the daemon later prepares it.
- Approval on a prepared queue head moves the item to `approval_state=granted` without immediately closing it.
- Auto-finalization for approval-not-required items and sticky finalization for granted queue heads only complete after the visible checkout is synced to the finalized commit.
- The original failure mode is gone: a repo finalized from an empty checkout ends with files materialized and a clean `git status`, not an empty tree with staged deletions.

## Idempotence and Recovery

Mirror refresh is safe to rerun and is performed repeatedly before Git-sensitive operations. Queue admission is idempotent per active revision via a partial unique index. Runtime reconciliation now checks mirror-backed worktrees and queue heads conservatively; when uncertain, it leaves work open instead of assuming success.

## Artifacts and Notes

Key implementation artifacts:

- New mirror helper module: `crates/ingot-git/src/project_repo.rs`
- New queue migration: `crates/ingot-store-sqlite/migrations/0003_convergence_queue.sql`
- Queue-aware async converge/finalize: `crates/ingot-agent-runtime/src/lib.rs`

## Interfaces and Dependencies

The final implementation adds:

- `ingot_domain::convergence_queue::{ConvergenceQueueEntry, ConvergenceQueueEntryStatus}`
- `ApprovalState::Granted`
- `EscalationReason::CheckoutSyncBlocked`
- `ingot_git::project_repo::{project_repo_paths, ensure_mirror, checkout_sync_status, sync_checkout_to_commit}`

The router now exposes queue data in `ItemSummaryResponse` and `ItemDetailResponse` through a `queue` payload, and the runtime depends on `DispatcherConfig.state_root` for mirror/worktree ownership.
