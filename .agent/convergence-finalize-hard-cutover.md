# Convergence finalize hard cutover

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with `.agent/PLANS.md`.

## Purpose / Big Picture

Ingot convergence now has to behave correctly when a prepared commit exists only inside the daemon-managed mirror, when a target ref has already been moved by an applied finalize operation, and when replaying a source commit produces no file changes because the patch is already present. After this change, a granted prepared convergence can finalize without assuming the registered checkout already knows the prepared object, a partially applied finalize will reconcile instead of being invalidated as stale, and a replay no-op will advance cleanly instead of wedging the lane.

## Progress

- [x] (2026-03-14 20:36Z) Added failing runtime regressions for mirror-only finalize sync, applied-finalize reconciliation, and empty replay cleanup.
- [x] (2026-03-14 20:48Z) Implemented mirror-aware checkout sync, per-tick git-operation reconciliation, shared convergence target validity semantics, and empty replay handling in the runtime.
- [x] (2026-03-14 20:52Z) Updated the duplicated HTTP convergence validity and replay path to match the runtime cutover.
- [x] (2026-03-14 20:56Z) Validated `ingot-agent-runtime`, targeted `ingot-http-api`, and `ingot-git` tests after the cutover.

## Surprises & Discoveries

- Observation: A successful no-op `git cherry-pick --no-commit` on an already-integrated patch leaves a clean worktree and no `CHERRY_PICK_HEAD`.
  Evidence: Manual repro in the stranded integration worktree showed `status=0` and `cherry_pick_head=absent`.
- Observation: The runtime did not reconcile unresolved git operations during normal ticks, only during startup.
  Evidence: The applied finalize operation in the live incident stayed unresolved until the item was re-evaluated and invalidated.
- Observation: `git commit --no-verify -F -` reports “nothing to commit” on stdout, not stderr, in the empty replay case.
  Evidence: The regression reproduced `Git(CommandFailed(""))` until stdout fallback was added.

## Decision Log

- Decision: Keep the convergence status model unchanged and fix the partial-finalize state by reconciling git operations during normal ticks.
  Rationale: This removes the invalidation window without introducing a new persisted status and keeps the cutover focused on real state transitions.
  Date/Author: 2026-03-14 / Codex

- Decision: Treat target-head validity as matching either the original input target or the convergence result commit.
  Rationale: A convergence should remain valid after its own prepared or finalized commit is at the target ref; only unrelated target movement should invalidate it.
  Date/Author: 2026-03-14 / Codex

- Decision: Fetch the target ref from the mirror into a temporary local ref before resetting the registered checkout.
  Rationale: The registered checkout cannot reset to a mirror-only object until that object exists in its local object database.
  Date/Author: 2026-03-14 / Codex

- Decision: Treat empty replay as a no-op and fail all other post-cherry-pick replay errors through explicit convergence/workspace/git-operation cleanup.
  Rationale: Empty replay is a normal outcome when the patch is already integrated; other replay failures must not strand `running` / `busy` / `planned` rows.
  Date/Author: 2026-03-14 / Codex

## Outcomes & Retrospective

The cutover fixed all three observed failure modes without carrying forward compatibility logic for the pre-fix state machine. The new regression tests now pass, the runtime crate passes in full, and the duplicated HTTP path uses the same validity and no-op replay rules. No migration work was added because the user explicitly allowed a hard reset of runtime state.

## Context and Orientation

The core daemon loop lives in `crates/ingot-agent-runtime/src/lib.rs`. It evaluates item state, prepares convergence workspaces, replays author commits into integration worktrees, and finalizes target refs. The Git checkout/mirror coordination lives in `crates/ingot-git/src/project_repo.rs`. The convergence domain type lives in `crates/ingot-domain/src/convergence.rs`. The HTTP API duplicates parts of convergence validity and convergence preparation in `crates/ingot-http-api/src/router.rs`.

“Mirror” means the daemon-managed bare Git repository under `~/.ingot/repos/<project>.git`. “Registered checkout” means the user-facing project repository path stored in the `projects` table. “Finalize” means moving the target ref in the mirror and then syncing the registered checkout to that exact commit.

## Plan of Work

Update the domain convergence type so target-head validity accepts either the original target head or the convergence result head. Update the Git checkout sync helper so it imports the target ref from the mirror into the registered checkout before resetting to the prepared commit. Update the runtime tick loop to reconcile unresolved git operations before item evaluation, and route both finalize paths through the new mirror-aware checkout sync helper.

In the runtime replay loop, detect no-op replay immediately after `cherry-pick --no-commit` by checking for worktree changes. Skip commit creation when there are no changes. For all later replay errors, mark the integration workspace as errored, release the lane head, fail the git operation, fail or conflict the convergence, and escalate the item instead of bubbling the error and leaving durable rows active.

Mirror the same validity and no-op replay semantics in the HTTP API convergence helpers so the route path and daemon path stay aligned.

## Concrete Steps

From `/Users/aa/Documents/ingot`, run:

    cargo test -p ingot-agent-runtime
    cargo test -p ingot-http-api target_head_valid_tracks_ref_movement
    cargo test -p ingot-http-api prepare_convergence_route_queues_lane_head_for_async_prepare
    cargo test -p ingot-git

Expected outcomes:

    ingot-agent-runtime: 33 passed
    ingot-http-api targeted tests: passed
    ingot-git: 5 passed

## Validation and Acceptance

Acceptance for this cutover is entirely test-driven:

1. The three runtime regressions added for the incident now pass.
2. Full `ingot-agent-runtime` passes, proving the cutover does not break the existing daemon workflow tests.
3. The duplicated HTTP convergence code compiles and its targeted convergence tests pass.
4. `ingot-git` passes, confirming the mirror/check-out helper changes compile and behave with the existing Git-layer tests.

## Idempotence and Recovery

All commands above are safe to rerun. The tests create disposable repos and SQLite files under the system temp directory. No production data migration is required because this work assumes runtime state can be reset.

## Artifacts and Notes

Important observable outcomes from validation:

    cargo test -p ingot-agent-runtime
    test result: ok. 33 passed; 0 failed

    cargo test -p ingot-http-api target_head_valid_tracks_ref_movement
    test result: ok. 1 passed; 0 failed

    cargo test -p ingot-http-api prepare_convergence_route_queues_lane_head_for_async_prepare
    test result: ok. 1 passed; 0 failed

    cargo test -p ingot-git
    test result: ok. 5 passed; 0 failed

## Interfaces and Dependencies

The cutover keeps public HTTP routes and persisted table names unchanged. The main interface changes are internal:

- `ingot_domain::convergence::Convergence` now owns the shared target-head validity rule through `target_head_valid_for_resolved_oid`.
- `ingot_git::project_repo::sync_checkout_to_commit` now requires the mirror path and target ref so it can fetch the target ref into the registered checkout before reset.
- `ingot_agent_runtime::JobDispatcher::tick` now reconciles unresolved git operations during normal daemon ticks, not only during startup.
