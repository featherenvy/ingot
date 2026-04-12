# Make checkout synchronization safe for untracked files and remove it from the finalization critical path

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with `.agent/PLANS.md`.

## Purpose / Big Picture

After this change, Ingot will stop escalating or leaving items open merely because the registered checkout contains harmless untracked files. A user will be able to finish an item even if their working checkout contains unrelated scratch files such as notes, generated output, or editor artifacts, as long as advancing the checkout to the prepared commit would not overwrite those files.

The stronger follow-up is part of this plan, not a separate idea. Once the prepared convergence has safely compare-and-swap updated the target branch ref, the item should be finalized and closed even if the registered checkout cannot be synchronized immediately. Checkout synchronization will become best-effort post-finalization cleanup with durable retry behavior instead of a correctness gate for convergence.

The user-visible proof is:

1. A prepared convergence finalizes successfully when the registered checkout is on the target branch and has only non-conflicting untracked files.
2. A prepared convergence still protects local data by refusing to overwrite conflicting untracked paths.
3. If the target ref is already updated but the checkout cannot yet be synchronized safely, the item still ends in `done`, the convergence ends in `finalized`, and the daemon keeps enough durable state to retry checkout synchronization later.

## Progress

- [x] 2026-04-12T12:20:00Z Wrote the initial ExecPlan after investigating the failure mode in `crates/ingot-git/src/project_repo.rs` and the finalization sequencing in `crates/ingot-usecases/src/convergence/finalization.rs`.
- [x] 2026-04-12T11:51:47Z Re-read the referenced Rust, SQL, and test files and corrected the plan where command shapes, lifecycle coverage, and checkout-block propagation were too vague.
- [x] 2026-04-12T13:18:00Z Implemented a path-aware checkout synchronization preflight in `crates/ingot-git/src/project_repo.rs` that separates tracked dirtiness from harmless untracked files and from destination-path collisions.
- [x] 2026-04-12T13:24:00Z Threaded the commit-aware checkout-block result through runtime escalation helpers, HTTP finalization readiness, and prepared-lane queue projection.
- [x] 2026-04-12T13:20:00Z Added focused `ingot-git` tests proving non-conflicting untracked files survive checkout synchronization while conflicting untracked paths still block.
- [x] 2026-04-12T13:24:00Z Changed prepared-convergence finalization so compare-and-swap success closes the item before best-effort checkout synchronization runs, and late sync problems leave the finalize operation unresolved instead of surfacing as a protocol error.
- [x] 2026-04-12T13:31:00Z Preserved durable retry behavior for post-finalization checkout synchronization using the existing `finalize_target_ref` git operation lifecycle.
- [x] 2026-04-12T13:31:00Z Updated runtime, HTTP, and usecase tests to reflect the new semantics: blocked checkout sync after CAS no longer keeps the item open, and later retries do not re-escalate done items.
- [x] 2026-04-12T13:32:00Z Updated `SPEC.md` to describe checkout synchronization as post-finalization cleanup.
- [x] 2026-04-12T13:37:00Z Ran the focused suites plus `make test`.

## Surprises & Discoveries

- Observation: the current checkout dirtiness test is much broader than the intended safety rule.
  Evidence: `crates/ingot-git/src/commit.rs` defines `working_tree_has_changes()` as `git status --porcelain`, which treats `??` untracked files the same as tracked edits.

- Observation: a blanket "ignore untracked files" change would risk data loss.
  Evidence: local reproduction showed that `git reset --hard <target>` preserves unrelated untracked files such as `note.tmp`, but it silently overwrites or replaces untracked paths when the target commit tracks the same path or a parent/child path. An untracked file `collide.txt` was replaced by the tracked file from the target commit, and an untracked directory `foo/` was replaced by a tracked file `foo`.

- Observation: checkout synchronization is currently on the correctness path after the target ref has already moved.
  Evidence: `crates/ingot-usecases/src/convergence/finalization.rs` calls `finalize_target_ref(...)`, marks the git operation `Applied`, and only then checks `checkout_finalization_readiness(...)`. If checkout sync is blocked, the helper returns an error before `apply_successful_finalization(...)`, so the branch head has moved but the item remains open.

- Observation: the written specification does not require checkout synchronization before closure.
  Evidence: `SPEC.md` sections `12.5 Finalize Prepared Convergence` and `12.7 Approve Pending Item` stop at compare-and-swap, mark convergence finalized, and close the item. They do not mention checkout synchronization as a closure gate.

- Observation: conflicting-untracked detection cannot live only inside `checkout_finalization_status(...)` if the UI and escalation paths still call `checkout_sync_status(...)` directly.
  Evidence: `crates/ingot-agent-runtime/src/convergence.rs`, `crates/ingot-http-api/src/router/convergence_port.rs`, and `crates/ingot-http-api/src/router/item_projection.rs` all call `checkout_sync_status(...)` today to decide whether to escalate the item or surface `queue.checkout_sync_blocked`.

- Observation: after the stronger sequencing change, returning the current post-sync error would produce a false HTTP failure even though the item was already closed.
  Evidence: `crates/ingot-usecases/src/convergence/finalization.rs` currently converts blocked readiness into `UseCaseError::ProtocolViolation(...)`, and `crates/ingot-http-api/src/error.rs` maps `UseCaseError::ProtocolViolation` to HTTP 422.

- Observation: no new database schema is required to keep retry state durable.
  Evidence: `crates/ingot-domain/src/git_operation.rs` already stores `FinalizeTargetRef { workspace_id, ref_name, expected_old_oid, new_oid, commit_oid }`, `crates/ingot-store-sqlite/src/store/git_operation.rs` already reloads unresolved `planned` and `applied` rows, and `crates/ingot-store-sqlite/migrations/0004_finalize_target_ref_uniqueness.sql` already enforces one unresolved finalize row per convergence.

- Observation: once the item is closed, later maintenance can still leave unrelated unresolved git operations behind.
  Evidence: the new runtime retry test showed that `finalize_target_ref` reconciles once checkout sync becomes safe, while a separate `remove_workspace_ref` operation can remain unresolved afterward. The assertions had to target the finalize operation specifically instead of assuming the entire unresolved set becomes empty.

## Decision Log

- Decision: implement the safe untracked-file handling as a path-aware preflight instead of weakening `checkout_dirty` globally.
  Rationale: unrelated untracked files are safe to keep, but untracked files or directories that overlap the destination tree can be overwritten by `git reset --hard`. The preflight must encode the real safety rule, not a coarse approximation.
  Date/Author: 2026-04-12 / Codex

- Decision: compare untracked paths against the destination commit tree, not only against the diff from current `HEAD`.
  Rationale: the risk is "would the destination tree need this path," not merely "did this path change in the last step." Using the full destination tree is simpler to reason about and safe for a novice maintainer to verify.
  Date/Author: 2026-04-12 / Codex

- Decision: remove checkout synchronization from the finalization critical path once the target ref compare-and-swap succeeds.
  Rationale: moving the target ref is the repository truth. Blocking item closure on a convenience synchronization of the registered checkout makes already-landed work appear unfinished and contradicts the spec's stated finalization flow.
  Date/Author: 2026-04-12 / Codex

- Decision: keep durable post-finalization checkout retry behavior by reusing unresolved `finalize_target_ref` operations instead of inventing a brand-new persistence mechanism in this change.
  Rationale: the current recovery loop already knows how to find `Applied` finalize operations and retry checkout synchronization. The semantics need to change, but the durable journal can stay.
  Date/Author: 2026-04-12 / Codex

- Decision: leave `working_tree_has_changes()` unchanged and add the finer-grained checkout classifier next to the other checkout-sync helpers in `crates/ingot-git/src/project_repo.rs`.
  Rationale: `working_tree_has_changes()` is still used by unrelated execution and workspace code paths in `crates/ingot-agent-runtime/src/execution.rs`, `crates/ingot-agent-runtime/src/convergence.rs`, and `crates/ingot-http-api/src/router/items/convergence_prep.rs`, so weakening it globally would have broader consequences than this plan intends.
  Date/Author: 2026-04-12 / Codex

- Decision: the done-item invariant is explicit: once `apply_successful_finalization(...)` succeeds, later checkout retries may keep the finalize operation unresolved but must not set `EscalationReason::CheckoutSyncBlocked` back onto the item.
  Rationale: `apply_successful_finalization(...)` in both adapters already clears escalation and closes the item, while the existing retry helpers in `crates/ingot-agent-runtime/src/convergence.rs` and `crates/ingot-http-api/src/router/convergence_port.rs` would otherwise re-escalate the same item on every blocked retry.
  Date/Author: 2026-04-12 / Codex

## Outcomes & Retrospective

The implementation now ships both halves of the fix together. `crates/ingot-git/src/project_repo.rs` distinguishes tracked dirtiness from untracked collisions against the destination tree, so harmless scratch files no longer block checkout synchronization. `crates/ingot-usecases/src/convergence/finalization.rs`, `crates/ingot-agent-runtime/src/reconciliation.rs`, and the HTTP/runtime adapters now treat compare-and-swap plus persistence as the closure boundary, leaving checkout sync as best-effort cleanup tied to the unresolved finalize operation.

The most important lesson from implementation was that the stronger follow-up was not just about reordering one helper call. The item-level escalation paths and queue projection were separate consumers of checkout-block state, so they also had to become commit-aware and lifecycle-aware or the system would still misreport safe vs. unsafe blocked states after closure.

## Context and Orientation

Ingot tracks three different Git locations during convergence, and this plan touches all three. The "registered checkout" is the project's real working tree at `Project.path`; it is the checkout a human sees and may leave local scratch files in. The "mirror" is the bare Git repository stored under the daemon state root and used for compare-and-swap updates of branch refs. The "integration workspace" is the isolated worktree where convergence replay and integrated validation happen.

The relevant code is spread across a few crates:

- `crates/ingot-git/src/commit.rs` contains generic Git working-tree helpers such as `working_tree_has_changes()`.
- `crates/ingot-git/src/project_repo.rs` contains checkout synchronization logic, including `checkout_sync_status()`, `checkout_finalization_status()`, and `sync_checkout_to_commit()`.
- `crates/ingot-git/src/commands.rs` contains `finalize_target_ref(...)`, the compare-and-swap helper that actually moves the mirror ref.
- `crates/ingot-usecases/src/convergence/finalization.rs` defines both `find_or_create_finalize_operation(...)` and the shared prepared-convergence finalization helper used by approval and daemon auto-finalize logic.
- `crates/ingot-usecases/src/convergence/tests.rs` and `crates/ingot-usecases/src/convergence/test_support.rs` hold the unit-level behavior checks for approval and blocked auto-finalize progress.
- `crates/ingot-agent-runtime/src/runtime_ports.rs` and `crates/ingot-http-api/src/router/convergence_port.rs` adapt runtime and HTTP code to the usecase finalization port.
- `crates/ingot-agent-runtime/src/convergence.rs` and `crates/ingot-http-api/src/router/convergence_port.rs` also maintain checkout-block escalation state on the item.
- `crates/ingot-agent-runtime/src/reconciliation.rs` is the daemon recovery loop that adopts and retries unresolved Git operations after restart.
- `crates/ingot-http-api/src/router/item_projection.rs` populates `queue.checkout_sync_blocked` and `queue.checkout_sync_message` for prepared queue heads.
- `crates/ingot-agent-runtime/tests/reconciliation.rs`, `crates/ingot-agent-runtime/tests/convergence.rs`, and `crates/ingot-http-api/tests/convergence_routes.rs` contain the blocked-checkout tests whose expected outcomes will change.
- `crates/ingot-store-sqlite/migrations/0004_finalize_target_ref_uniqueness.sql` and `crates/ingot-store-sqlite/src/store/git_operation.rs` prove that one unresolved finalize operation is already durable and retryable without a schema change.

Two terms are important here:

- "Prepared convergence" means a convergence record whose integrated commit already exists and is ready to become the target branch head.
- "Checkout synchronization" means updating the registered checkout at `Project.path` so its `HEAD` and working tree match the prepared commit after the target ref moves.
- An "applied" finalize operation means the compare-and-swap already succeeded and the journal row is waiting for adoption or checkout reconciliation. In this repository, `git_operations.status IN ('planned', 'applied')` is the unresolved set that startup reconciliation reloads.

Today the code treats checkout synchronization as part of finalization. The desired end state is different: finalization is branch-ref truth, while checkout synchronization is post-finalization cleanup that must still be safe and durable.

## Plan of Work

The first milestone is to make checkout synchronization safety precise without weakening unrelated dirtiness checks. Leave `working_tree_has_changes()` in `crates/ingot-git/src/commit.rs` alone for the existing execution and workspace flows that call it today, and add the finer-grained checkout classifier next to `checkout_sync_status()` and `checkout_finalization_status()` in `crates/ingot-git/src/project_repo.rs`. That helper must answer three repository-specific questions separately: whether the checkout is on the expected branch, whether there are tracked or staged changes, and whether any untracked paths would collide with the destination commit tree. The destination-tree comparison should use real Git inventory commands, such as `git ls-files --others --exclude-standard -z` for untracked paths and `git ls-tree -r --name-only -z <commit>` for tracked paths. Treat a path as conflicting when the destination needs the exact path, when an untracked directory is an ancestor of a destination path, or when an untracked file sits under a destination path that must be a file. Return enough detail to build a specific blocked message instead of the current generic `checkout_dirty` text.

Do not stop at `checkout_finalization_status(...)`. In this repository, `checkout_sync_status(...)` is also consumed by `JobDispatcher::reconcile_checkout_sync_state(...)` in `crates/ingot-agent-runtime/src/convergence.rs`, by `reconcile_checkout_sync_state_http(...)` in `crates/ingot-http-api/src/router/convergence_port.rs`, and by `hydrate_queue_status(...)` in `crates/ingot-http-api/src/router/item_projection.rs`. The implementation therefore needs one commit-aware status path that those callers can use whenever a prepared convergence is in view, otherwise conflicting untracked files would block finalization without producing the item escalation, activity rows, or queue warning that operators rely on. Preserve the current wrong-branch and tracked-dirty behavior for callers that do not have a prepared commit in hand.

The second milestone is the stronger follow-up: remove checkout synchronization from the finalization critical path. In `crates/ingot-usecases/src/convergence/finalization.rs`, keep `find_or_create_finalize_operation(...)` as the idempotent entry point backed by the existing uniqueness index, but change the sequencing so that a non-stale `finalize_target_ref(...)` result is enough to mark the operation `Applied` and then call `apply_successful_finalization(...)`. After that durability boundary, attempt checkout synchronization as best-effort cleanup. If the checkout is already synchronized or the sync succeeds, mark the operation `Reconciled`. If the checkout is on the wrong branch, has tracked or staged dirtiness, has conflicting untracked paths, or the sync attempt fails for another Git reason, leave the existing finalize operation unresolved and return success from approval or auto-finalize instead of propagating `UseCaseError::ProtocolViolation(...)` or another late failure after the item is already closed.

Because the unresolved `finalize_target_ref` operation will now outlive item closure in some cases, update both adapters and both retry paths deliberately. `crates/ingot-agent-runtime/src/runtime_ports.rs` and `crates/ingot-http-api/src/router/convergence_port.rs` implement `PreparedConvergenceFinalizePort`, and both already clear escalation and close the item inside `apply_successful_finalization(...)`. Keep that as the single closure boundary. Then update `JobDispatcher::complete_finalize_target_ref_operation(...)` and `JobDispatcher::adopt_finalized_target_ref(...)` in `crates/ingot-agent-runtime/src/reconciliation.rs` so an already-finalized convergence plus an unresolved finalize row is treated as normal recovery state. The retry loop may keep the operation `Applied`, but it must not re-open or re-escalate the item. In practice that means `JobDispatcher::reconcile_checkout_sync_state(...)` and `reconcile_checkout_sync_state_http(...)` need an explicit done-item guard, because today they will set `EscalationReason::CheckoutSyncBlocked` whenever `checkout_sync_status(...)` reports blocked.

The test milestone has to cover all three layers that currently encode the old semantics. In `crates/ingot-usecases/src/convergence/tests.rs`, update the unit tests that presently assert blocked auto-finalize makes no progress and add assertions that `apply_successful_finalization(...)` still runs even when checkout sync remains pending. In `crates/ingot-http-api/tests/convergence_routes.rs`, rewrite `approve_route_reuses_existing_finalize_op_when_checkout_is_blocked` so it expects a successful approval, a single unresolved finalize op, a finalized convergence, a released queue entry, and no lingering checkout-sync escalation on the item. In `crates/ingot-agent-runtime/tests/convergence.rs` and `crates/ingot-agent-runtime/tests/reconciliation.rs`, replace the current "item stays open" expectations with "item is done but finalize op remains unresolved until a later retry succeeds." Rename tests whose names encode the old ordering or open-item semantics, such as `tick_reports_no_progress_when_auto_finalize_is_blocked`, `reconcile_startup_leaves_finalize_open_when_checkout_sync_is_blocked`, and `reconcile_startup_syncs_checkout_before_adopting_finalize`.

The final milestone is specification and regression proof. Update `SPEC.md` so sections `12.5` and `12.7` say that compare-and-swap plus successful persistence closes the item, while checkout synchronization continues afterward as cleanup tied to the unresolved finalize operation. No schema migration is planned in this change; the proof of durability is the existing `git_operations` payload shape and unresolved-row lookup.

## Concrete Steps

Work from `/Users/aa/Documents/ingot`.

Begin by implementing and testing the low-level Git safety helper:

    cargo test -p ingot-git project_repo::tests

After editing the usecase finalization sequencing and adapter code, rerun the unit and integration targets that currently encode the old behavior:

    cargo test -p ingot-usecases convergence::tests
    cargo test -p ingot-http-api --test convergence_routes
    cargo test -p ingot-agent-runtime --test convergence
    cargo test -p ingot-agent-runtime --test reconciliation

Before declaring the work complete, run the broader gate that is realistic for this repository after Rust changes:

    make test

If `make test` is too broad during iteration, use the narrower crate suites above while developing and run the broader command once at the end.

Expected checkpoints while implementing:

    - Before the change, `approve_route_reuses_existing_finalize_op_when_checkout_is_blocked` in `crates/ingot-http-api/tests/convergence_routes.rs` expects HTTP 422, `approval_state = pending`, `escalation_reason = checkout_sync_blocked`, and a single finalize operation left in `applied`.
    - Before the change, `blocked_auto_finalize_does_not_count_as_progress` in `crates/ingot-usecases/src/convergence/tests.rs`, `tick_reports_no_progress_when_auto_finalize_is_blocked` in `crates/ingot-agent-runtime/tests/convergence.rs`, and `reconcile_startup_leaves_finalize_open_when_checkout_sync_is_blocked` in `crates/ingot-agent-runtime/tests/reconciliation.rs` all encode the old open-item behavior.
    - After the sequencing change, those scenarios should instead show a done item, a finalized convergence, a released queue entry, and a single unresolved finalize operation that survives until checkout sync later succeeds.
    - Before the low-level Git fix, `crates/ingot-git/src/project_repo.rs` treats a checkout with only `?? note.tmp` as `checkout_dirty` because it delegates to `working_tree_has_changes()`.
    - After the fix, the same checkout should classify as syncable, while a checkout with `?? collide.txt` or an untracked `foo/` directory that would be replaced by the destination commit should still report `Blocked` with a collision-specific message.

## Validation and Acceptance

Acceptance requires all of the following observable behaviors:

1. In a crate-level Git test, a repository whose registered checkout contains only unrelated untracked files can still be synchronized to a prepared commit. The untracked files remain on disk after synchronization, and the tracked files match the destination commit.
2. In a crate-level Git test, a repository whose registered checkout contains untracked paths that overlap the destination tree is still blocked, and the message identifies that Ingot refused to overwrite local untracked files rather than saying only that the checkout is dirty.
3. In `crates/ingot-http-api/tests/convergence_routes.rs`, `approve_route_reuses_existing_finalize_op_when_checkout_is_blocked` now returns HTTP 200, leaves exactly one `finalize_target_ref` row unresolved in `applied`, writes `convergences.status = finalized`, writes `convergence_queue_entries.status = released`, and clears `items.escalation_reason`.
4. In `crates/ingot-usecases/src/convergence/tests.rs`, blocked auto-finalize now counts as progress because durable finalization succeeded even though checkout sync is still pending.
5. In `crates/ingot-agent-runtime/tests/convergence.rs` and `crates/ingot-agent-runtime/tests/reconciliation.rs`, blocked checkout sync after compare-and-swap no longer keeps the item open, and later retries do not restore `EscalationReason::CheckoutSyncBlocked` to an already-done item.
6. The recovery loop can eventually reconcile the unresolved finalize operation once the checkout becomes safe to synchronize, at which point `GitOperationStatus` moves from `applied` to `reconciled` and the checkout `HEAD` matches the prepared commit.
7. The updated `SPEC.md` text matches the implemented behavior by describing checkout synchronization as post-finalization cleanup instead of a prerequisite for closure.

## Idempotence and Recovery

This work must stay safe to retry. The low-level preflight may be run repeatedly because it only reads Git state. The post-finalization checkout synchronization attempt must also be repeatable because the recovery loop may rerun it after a crash or after a human cleans up the checkout manually.

Do not add destructive cleanup such as `git clean -fd` to make tests pass. The entire point of this change is to preserve local untracked data unless Ingot can prove the destination commit does not need those paths.

If checkout synchronization remains blocked after finalization, the item must remain done. Recovery should proceed by leaving the unresolved `finalize_target_ref` operation in place and retrying synchronization later, not by reopening the item or synthesizing a new convergence.

This plan intentionally relies on the existing finalize-operation durability model. `git_operations.status IN ('planned', 'applied')` is already the unresolved set, `find_or_create_finalize_operation(...)` already reuses the existing row for repeat approval attempts, and the uniqueness index in `crates/ingot-store-sqlite/migrations/0004_finalize_target_ref_uniqueness.sql` already prevents duplicate active finalize rows for the same convergence. No data migration or deploy-order choreography is required beyond shipping the new semantics.

Be careful with late failures after `apply_successful_finalization(...)`. Once the item is closed and the convergence is finalized, a blocked or transiently failing checkout sync must leave the finalize row unresolved for retry, not bubble a late error back to HTTP or daemon callers as if the whole command had failed. The retry helpers also need to avoid re-applying checkout-blocked escalation to `Lifecycle::Done` items.

## Artifacts and Notes

Capture these pieces of evidence while implementing:

    - A small test transcript or assertion showing that `note.tmp` survives synchronization to a new prepared commit.
    - A small test transcript or assertion showing that `collide.txt` or `foo/` is detected as a conflicting untracked path.
    - The updated assertions from the HTTP, usecase, and runtime tests showing that blocked checkout sync after compare-and-swap no longer leaves the item open.
    - An activity or state assertion showing that `ConvergenceFinalized` is recorded before `GitOperationReconciled` when sync is delayed, and that no new `ItemEscalated` event is written for a done item during retry.
    - The `SPEC.md` excerpt that now describes compare-and-swap as the closure boundary and checkout sync as cleanup.

If additional UI or projection work becomes necessary to surface pending checkout synchronization for done items, record that here as a follow-up rather than silently broadening the scope mid-flight.

## Interfaces and Dependencies

At the end of this change, the following interfaces or equivalent concrete behavior must exist:

- In `crates/ingot-git/src/project_repo.rs`, a helper that enumerates untracked paths and determines whether they conflict with a destination commit tree, while leaving `crates/ingot-git/src/commit.rs::working_tree_has_changes()` available for the broader "any change at all" callers that already depend on it.
- In `crates/ingot-git/src/project_repo.rs`, checkout readiness and finalization readiness that can distinguish `wrong branch`, `tracked or staged changes`, `conflicting untracked paths`, `needs sync`, and `already synced`, with a specific blocked message for untracked collisions.
- In `crates/ingot-usecases/src/convergence/finalization.rs`, prepared-convergence finalization that treats compare-and-swap success as sufficient to apply successful finalization before best-effort checkout sync runs, and that does not return `UseCaseError::ProtocolViolation` merely because the post-finalization sync is still pending.
- In `crates/ingot-agent-runtime/src/convergence.rs` and `crates/ingot-http-api/src/router/convergence_port.rs`, checkout-block tracking that does not set `EscalationReason::CheckoutSyncBlocked` on items whose `Lifecycle` is already `Done`.
- In `crates/ingot-http-api/src/router/item_projection.rs`, prepared-lane queue status that can surface the same blocked message finalization would produce for conflicting untracked files.
- In `crates/ingot-agent-runtime/src/reconciliation.rs`, reconciliation logic that remains idempotent when a `finalize_target_ref` operation is still unresolved after the item is already done and the convergence is already finalized.
- In the relevant tests across `ingot-git`, `ingot-usecases`, `ingot-http-api`, and `ingot-agent-runtime`, scenario coverage for both harmless and conflicting untracked files, for post-finalization unresolved finalize rows, and for the eventual retry that reconciles them.

Revision note: created on 2026-04-12 after investigating a convergence case where harmless untracked files in the registered checkout appeared to block convergence. The plan explicitly includes the stronger sequencing follow-up so the implementation fixes both the immediate safety bug and the broader finalization design flaw together.

Revision note (2026-04-12 review pass): corrected the concrete command list to match the repository's actual test entrypoints, added the missing callers that consume checkout-block state (`reconcile_checkout_sync_state`, `reconcile_checkout_sync_state_http`, and `item_projection`), named the existing finalize-operation durability and uniqueness guarantees that make a schema change unnecessary, and made the done-item invariant explicit so post-finalization retries cannot re-escalate or falsely fail an already-closed item.

Revision note (2026-04-12 implementation pass): implemented the plan in `ingot-git`, `ingot-usecases`, `ingot-agent-runtime`, and `ingot-http-api`; updated `SPEC.md`; and verified the shipped behavior with focused crate suites plus `make test`.
