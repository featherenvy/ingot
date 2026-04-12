# Make finalization truthful by separating target-ref advance from checkout adoption

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with `.agent/PLANS.md`.

## Purpose / Big Picture

After this change, Ingot will no longer be able to show an item as integrated merely because the daemon-managed mirror moved the target ref. An item will only become `done` after the registered checkout at `Project.path` has actually adopted the final commit, and the UI will continue to show a durable checkout-adoption blocker whenever that last step is not complete.

The user-visible proof is simple. When a prepared convergence advances `refs/heads/main` in the mirror but the registered checkout is dirty, on the wrong branch, or blocked by conflicting local artifacts, the item detail page and board must show an explicit "awaiting checkout sync" state instead of `DONE`. Once the checkout becomes safe and Ingot finishes adoption, the item may close, the finalize operation may reconcile, and the UI may then show the item as integrated. The broken state observed on `itm_019d816a442c7d739d59dade1082ab29` in project `prj_019d3ab9926f7701882bdf68eb1c08cf` must become impossible to represent.

This plan deliberately excludes migration logic, repair jobs, and backwards-compatibility shims. The repository is still early in development, so the implementation may change domain types, API shapes, and persisted schema directly in the cleanest way.

## Progress

- [x] (2026-04-12T14:47Z) Investigated the live incident in `perdify`, confirmed that the mirror ref advanced to `4d2ccc10126bbc48df92f06bbb1febd71a2217ba` while `/Users/aa/Documents/perdify` remained at `8cb76ba10e60868a5cf4a83fd72f656142bf4052`, and identified the exact runtime and projection paths that make the UI show `done`.
- [x] (2026-04-12T14:55Z) Authored this ExecPlan in `.agent/finalization-checkout-adoption-hardening.md`.
- [ ] Introduce a first-class durable finalization state that records whether the target ref moved, whether checkout adoption is pending, blocked, or synced, and any current blocker message.
- [ ] Refactor runtime finalization and reconciliation so moving the mirror ref never closes the item by itself.
- [ ] Refactor item projection and UI response types so checkout-adoption truth is no longer inferred from active queue rows.
- [ ] Add invariant checks and state-machine coverage that make `done + unresolved finalize_target_ref` impossible.
- [ ] Validate the full hardening pass with focused runtime, HTTP, usecase, and UI tests plus the broader Rust gate.

## Surprises & Discoveries

- Observation: the current runtime explicitly closes the item before checkout adoption is proven.
  Evidence: `crates/ingot-agent-runtime/src/reconciliation.rs` calls `adopt_finalized_target_ref(...)` at line ~178 inside `complete_finalize_target_ref_operation(...)`, then computes `checkout_finalization_status(...)` afterward at lines ~180-229.

- Observation: the current adopter clears escalation, releases the queue entry, and sets `Lifecycle::Done` even when the finalize git operation remains unresolved.
  Evidence: `crates/ingot-agent-runtime/src/reconciliation.rs` lines ~311-380 set the convergence to `Finalized`, release the active queue entry, and clear `item.escalation`, while the caller still returns `FinalizeCompletionOutcome::Blocked` when checkout sync is not safe.

- Observation: the current UI hides checkout-adoption blockers as soon as the queue entry is released.
  Evidence: `crates/ingot-http-api/src/router/item_projection.rs` returns `empty_queue_status()` immediately when `find_active_queue_entry_for_revision(...)` returns `None` at lines ~243-249, so `checkout_sync_blocked` becomes false even if an unresolved `finalize_target_ref` row still exists.

- Observation: the current runtime tests intentionally bless the broken state.
  Evidence: `crates/ingot-agent-runtime/tests/convergence.rs` lines ~183-218 assert that blocked auto-finalize leaves the item `done`, `Escalation::None`, queue released, and exactly one unresolved `FinalizeTargetRef` operation.

- Observation: the live incident confirms the test expectation is not theoretical.
  Evidence: in `~/.ingot/ingot.db`, git operation `gop_019d817d39687312b102dcbf1d8101b4` is still `applied`, the item is `done/completed`, the queue entry is released, and activity at `2026-04-12T11:39:20Z` records `checkout_sync_blocked` with `Registered checkout has uncommitted changes; clean it before finalizing`.

- Observation: the registered checkout is still not sync-safe today because it contains many untracked and ignored local artifacts.
  Evidence: `git -C /Users/aa/Documents/perdify status --ignored --short` reports numerous `??` and `!!` entries, while the checkout still points at `8cb76ba1` and does not know commit `4d2ccc10`.

## Decision Log

- Decision: introduce a durable finalization state separate from queue state.
  Rationale: queue rows describe lane ownership and scheduling. They are transient and may be released once the target ref moves. Checkout adoption is a different lifecycle that must remain visible after the queue row is gone.
  Date/Author: 2026-04-12 / Codex

- Decision: keep "mirror target ref advanced" and "registered checkout adopted the final commit" as two separate facts in the persisted convergence model.
  Rationale: the incident happened because those facts were collapsed into one implicit notion of "finalized". The design must model them separately so code, tests, and UI cannot confuse them again.
  Date/Author: 2026-04-12 / Codex

- Decision: item closure must move behind checkout adoption, not behind mirror compare-and-swap.
  Rationale: the registered checkout is the human-visible repository root and the product promise is about what operators can actually inspect and continue from. A mirror-only ref move is not enough to call the item integrated.
  Date/Author: 2026-04-12 / Codex

- Decision: remove `checkout_sync_blocked` and `checkout_sync_message` from the conceptual ownership of `QueueStatusResponse`, even if the final wire shape keeps compatibility-friendly field names during implementation.
  Rationale: the queue projection is the wrong abstraction boundary. The blocker belongs to finalization status, not to lane position.
  Date/Author: 2026-04-12 / Codex

- Decision: add invariant tests and transition guards instead of startup repair or compatibility code.
  Rationale: the project is still early, so it is better to make the bad state unrepresentable than to spend scope on repairing historical rows or preserving ambiguous semantics.
  Date/Author: 2026-04-12 / Codex

- Decision: widen the UI and API change rather than papering over the issue with a special-case warning string.
  Rationale: this is a state-model bug, not a copy bug. The UI needs a first-class finalization/adoption status so future features cannot regress by reading the wrong source of truth.
  Date/Author: 2026-04-12 / Codex

## Outcomes & Retrospective

This ExecPlan records the intended hardening before implementation. The key outcome target is that the system will stop equating "the mirror moved" with "the project integrated successfully". Instead, convergence state, item lifecycle, unresolved git operations, HTTP projections, and UI labels will all agree on one rule: the item is not integrated until the registered checkout is on the final commit.

The most important lesson from the incident is that a local fix in one layer would not be enough. The current bug is enforced by domain semantics, runtime sequencing, queue projection, and tests at the same time. The implementation therefore needs to change the model, not just one condition.

## Context and Orientation

Ingot uses three Git locations during convergence.

The "registered checkout" is the real project repository path stored in `projects.path`, such as `/Users/aa/Documents/perdify`. This is the repository a human sees in their shell.

The "mirror" is the daemon-managed bare Git repository under `~/.ingot/repos/<project-id>.git`. Ingot updates target refs there first because it is the daemon's canonical internal Git control plane.

The "integration workspace" is the isolated worktree under `~/.ingot/worktrees/<project-id>/<workspace-id>`. It is where replay and integrated validation happen before finalization.

Today the runtime and HTTP layers confuse these locations in one critical way. A `FinalizeTargetRef` git operation moving `refs/heads/main` inside the mirror is treated as enough to finalize the convergence, close the item, release the queue, and clear escalation, even if the registered checkout cannot adopt the commit yet. The files most directly involved are:

- `crates/ingot-domain/src/convergence.rs`, which defines `ConvergenceState` and `ConvergenceStatus`.
- `crates/ingot-domain/src/item.rs`, which defines `Lifecycle` and item escalation.
- `crates/ingot-domain/src/git_operation.rs`, which defines `GitOperationStatus`, `OperationKind`, and `OperationPayload::FinalizeTargetRef`.
- `crates/ingot-git/src/project_repo.rs`, which computes checkout readiness and performs `sync_checkout_to_commit(...)`.
- `crates/ingot-agent-runtime/src/reconciliation.rs`, which adopts and retries unresolved finalize operations.
- `crates/ingot-usecases/src/convergence/finalization.rs`, which defines the shared approval/auto-finalize flow.
- `crates/ingot-http-api/src/router/item_projection.rs`, which currently exposes checkout blockers only when a queue row is still active.
- `crates/ingot-http-api/src/router/types.rs`, which defines `QueueStatusResponse`, `ItemSummaryResponse`, and `ItemDetailResponse`.
- `ui/src/types/domain.ts`, `ui/src/pages/BoardPage.tsx`, `ui/src/pages/DashboardPage.tsx`, and `ui/src/pages/ItemDetailPage.tsx`, which render the board/detail state that operators actually see.
- `crates/ingot-store-sqlite/migrations/0001_initial.sql` plus later migrations, which define the persisted `convergences`, `items`, `convergence_queue_entries`, `git_operations`, and `activity` tables.

This plan uses a few terms of art:

- "Target-ref advance" means the compare-and-swap update of the mirror's branch ref, usually `refs/heads/main`.
- "Checkout adoption" means the registered checkout successfully reaches the same final commit and is safe to use.
- "Unresolved finalize operation" means a `git_operations` row with `operation_kind = finalize_target_ref` and `status IN ('planned', 'applied')`.
- "Impossible state" means a persisted state combination that the system should never write, such as `item.lifecycle = done` while a finalize operation for the current revision remains unresolved.

## Plan of Work

### Milestone 1: Make finalization state explicit and durable

The first milestone introduces the missing domain concept: finalization is not one bit. Edit `crates/ingot-domain/src/convergence.rs` so the finalized convergence state carries durable checkout-adoption information. The most direct shape is a new `CheckoutAdoptionState` enum with values `Pending`, `Blocked`, and `Synced`, plus the blocker message and timestamps needed to explain and audit the transition. `ConvergenceState::Finalized` should stop being just "the target ref moved at time X" and become "the target ref moved, and checkout adoption is currently in state Y". Update `ConvergenceStateParts`, serde/sqlx wiring, builders in `crates/ingot-domain/src/test_support/convergence.rs`, and any helper methods so code can read and mutate this state explicitly.

Persist that state in SQLite. Add a new migration under `crates/ingot-store-sqlite/migrations/` that extends `convergences` with durable checkout-adoption columns. Update the store mapping and repository files that read and write convergences so the new fields round-trip cleanly. Because backwards compatibility is not in scope, the migration may assume a development-only schema evolution and does not need fallback logic for legacy rows beyond what is required for the new schema to load in tests.

At the end of this milestone, the data model itself must be able to say "the mirror ref moved, but the checkout is still blocked because of local dirtiness" without consulting queue rows or item escalation.

### Milestone 2: Refactor runtime finalization into two explicit phases

The second milestone changes behavior. Edit `crates/ingot-agent-runtime/src/reconciliation.rs` and `crates/ingot-usecases/src/convergence/finalization.rs` so finalization becomes two explicit phases.

Phase one is target-ref advance. When the mirror compare-and-swap succeeds, persist the convergence as finalized with checkout adoption `Pending` or `Blocked`, abandon the integration workspace if appropriate, and optionally release the queue lane if lane release is still desired once the branch ref moved.

Phase two is checkout adoption. Only when `checkout_finalization_status(...)` returns `Synced`, or when `NeedsSync` and the subsequent `sync_checkout_to_commit(...)` succeeds, may the code close the item, clear escalation, mark the finalize operation `Reconciled`, and emit the final integrated activity.

To make that precise, split the current [reconciliation.rs](/Users/aa/Documents/ingot/crates/ingot-agent-runtime/src/reconciliation.rs:311) adopter into two narrower helpers. One helper should adopt target-ref advancement and convergence/workspace state. A second helper should adopt checkout synchronization and item closure. The current sequencing in `complete_finalize_target_ref_operation(...)` at lines ~152-229 must be inverted so the item-close helper is unreachable on blocked or failed checkout adoption paths.

This milestone must also centralize closure. Search for every direct write of `Lifecycle::Done { reason: DoneReason::Completed, ... }` in convergence/finalization paths and route them through one narrow helper that first checks "no unresolved finalize op for this current revision" and "convergence finalization state says checkout adoption is synced". That helper becomes the only legal closure path for convergence completion.

At the end of this milestone, `done + unresolved finalize_target_ref` must be impossible to write from either approval-time finalization or reconciliation-time finalization.

### Milestone 3: Decouple UI/API finalization projection from queue state

The third milestone changes the read model so operators can see the truth. Edit `crates/ingot-http-api/src/router/types.rs` to introduce a first-class `FinalizationStatusResponse`. It should carry the current finalization phase, the checkout adoption state, the blocker message, the final target commit OID when known, and whether a finalize git operation is still unresolved. Keep `QueueStatusResponse` focused on queue position and lane ownership only.

Then update `crates/ingot-http-api/src/router/item_projection.rs` so `load_item_detail(...)` and item summary loading derive finalization status from convergences plus unresolved finalize operations, not from `find_active_queue_entry_for_revision(...)`. The function currently returns `empty_queue_status()` as soon as no queue row exists; that early return must stop suppressing checkout blockers. The projection logic should surface a blocked checkout-adoption state even when the queue is already released and the integration workspace is already abandoned.

Update the workflow overlay so an item whose target ref advanced but checkout adoption is not synced does not land in `BoardStatus::DONE`. The cleanest design is to add a dedicated `PhaseStatus::AwaitingCheckoutSync` in the workflow/UI surface and map such items to `BoardStatus::WORKING`. This requires coordinated edits in `ingot_workflow`, the API response types, `ui/src/types/domain.ts`, and the status rendering helpers in `ui/src/lib/status.ts`. `BoardPage.tsx`, `DashboardPage.tsx`, and `ItemDetailPage.tsx` should then render a dedicated label such as "Awaiting checkout sync" and the precise blocker message.

At the end of this milestone, the user should never need to infer the truth from Git manually. If the checkout has not adopted the final commit, the API and UI must say so directly.

### Milestone 4: Replace permissive tests with invariant-driven state-machine coverage

The fourth milestone removes the test scaffolding that currently blesses the fatal bug. Rewrite the blocked-auto-finalize expectations in `crates/ingot-agent-runtime/tests/convergence.rs`, especially `assert_blocked_auto_finalize_state(...)`, so the correct blocked state is: mirror target ref advanced, finalize operation unresolved, item still open, and checkout adoption state blocked with a durable message. The old assertion that `Escalation::None` and `item.lifecycle.is_done()` are acceptable must be deleted.

Add targeted tests across the stack:

- A runtime reconciliation test for "mirror ref advanced, checkout blocked" that proves the item stays open and visible as blocked.
- A runtime reconciliation test for "checkout becomes safe later" that proves the item closes only after the finalize operation reconciles.
- An HTTP item projection test that proves a released queue entry does not hide a blocked checkout-adoption state.
- A usecase or runtime invariant test that rejects or panics on any attempt to produce `done + unresolved finalize op`.
- A UI page test that shows "Awaiting checkout sync" instead of `DONE` for an item with blocked checkout adoption.

Add one higher-level state-machine style test that drives the real transition sequence: prepare convergence, advance target ref, block checkout adoption, release queue, render item detail, clean checkout, reconcile, and render again. That single test should prove the cross-layer contract rather than only local helper behavior.

At the end of this milestone, the old broken state is both impossible to write and impossible for tests to accidentally re-approve.

## Concrete Steps

Work from `/Users/aa/Documents/ingot`.

During implementation, keep the plan updated after every milestone and after any design change. Run focused commands as each layer lands:

    cargo test -p ingot-domain convergence
    cargo test -p ingot-store-sqlite convergence
    cargo test -p ingot-usecases convergence::tests
    cargo test -p ingot-agent-runtime --test convergence
    cargo test -p ingot-agent-runtime --test reconciliation
    cargo test -p ingot-http-api --test convergence_routes
    cargo test -p ingot-http-api --test item_routes
    bun test ui/src/test/item-detail-page.test.tsx ui/src/test/board-page.test.tsx ui/src/test/dashboard-page.test.tsx

After the focused commands pass, run the broader Rust gate:

    make test

If UI changes are substantial, also run:

    make ui-test

Expected checkpoints while implementing:

    - Before the change, `crates/ingot-agent-runtime/tests/convergence.rs` accepts `done + unresolved finalize op`.
    - Before the change, `crates/ingot-http-api/src/router/item_projection.rs` drops checkout blockers whenever there is no active queue row.
    - After Milestone 2, blocked checkout adoption leaves the finalize operation unresolved but keeps the item open.
    - After Milestone 3, item detail and board responses include explicit finalization/adoption state even with `queue.state = null`.
    - After Milestone 4, the previous blocked-auto-finalize test is inverted and a cross-layer sequence proves the new contract.

## Validation and Acceptance

Acceptance requires all of the following observable behavior:

1. When the mirror target ref advances but the registered checkout is blocked, the convergence is marked finalized with checkout adoption `Blocked`, the finalize git operation remains unresolved, and the item is not `done`.
2. When the queue entry is released after target-ref advance, item detail still shows a finalization/adoption blocker and does not fall back to a neutral queue status.
3. When the registered checkout later becomes safe, the runtime reconciliation path synchronizes it, marks the finalize operation `Reconciled`, and only then closes the item.
4. The board and detail UI render a dedicated non-`DONE` status for this waiting period, including the durable blocker message.
5. No code path in approval, auto-finalize, or reconciliation can write `Lifecycle::Done` while a `FinalizeTargetRef` operation for the current revision is still unresolved.
6. The test suite no longer contains any assertions that intentionally bless `done + unresolved finalize_target_ref`.
7. The original incident shape is covered by an automated test that fails before this hardening and passes after it.

## Idempotence and Recovery

All implementation steps should be safe to rerun. The new finalization state should be written deterministically, and retrying checkout adoption while the checkout is still blocked must leave the same durable blocker state rather than duplicating rows or flipping the item between open and done.

The runtime must remain idempotent when repeatedly reconciling an unresolved finalize operation. Re-running the reconcile loop against a blocked checkout should update timestamps only when necessary and must not emit duplicate closure side effects, duplicate activity that implies success, or duplicate queue releases.

Because backwards compatibility is explicitly out of scope, do not add fallback parsing branches, compatibility fields, or one-off repair logic for older rows. The schema and domain types may move directly to the hardened design.

## Artifacts and Notes

Capture these artifacts while implementing:

    - A focused test transcript showing a blocked finalize operation leaves the item open and the finalize row unresolved.
    - A focused test transcript showing a later checkout cleanup reconciles the same finalize row and only then closes the item.
    - An HTTP response example, from a test fixture, where `queue.state` is null but the finalization status still says checkout adoption is blocked.
    - A UI assertion proving the board/detail status label is "Awaiting checkout sync" rather than `DONE`.
    - A short excerpt from the old blocked-auto-finalize test and the new inverted assertion that replaces it.

If implementation requires a different finalization response shape than the one proposed here, update `Decision Log`, `Interfaces and Dependencies`, and the acceptance text in this plan before landing code.

## Interfaces and Dependencies

At the end of this hardening pass, the following interfaces or equivalent concrete behavior must exist:

- In `crates/ingot-domain/src/convergence.rs`, define a durable checkout-adoption enum, for example:

    pub enum CheckoutAdoptionState {
        Pending,
        Blocked,
        Synced,
    }

  and extend finalized convergence state so it can store:

    - the final target commit OID,
    - the checkout adoption state,
    - the current blocker message,
    - the timestamp when the target ref advanced,
    - the timestamp when checkout adoption completed, if any.

- In `crates/ingot-agent-runtime/src/reconciliation.rs`, split the current finalize adopter into one helper that adopts target-ref advancement and one helper that closes the item after checkout adoption. The latter must be the only helper allowed to write `Lifecycle::Done` for convergence completion.

- In `crates/ingot-usecases/src/convergence/finalization.rs`, the shared finalization flow must persist "mirror advanced but checkout not yet adopted" without returning a false success/failure combination to callers.

- In `crates/ingot-http-api/src/router/types.rs`, define a first-class `FinalizationStatusResponse` (name may differ if the final implementation chooses a better one) and stop treating checkout-adoption fields as queue-owned data.

- In `crates/ingot-http-api/src/router/item_projection.rs`, projections must derive finalization state from convergences and unresolved `FinalizeTargetRef` operations rather than active queue rows alone.

- In `ui/src/types/domain.ts`, add the matching finalization status type and the new phase/status literals needed to render waiting-for-checkout-adoption correctly.

- In `ingot_workflow` and the UI status helpers, introduce a dedicated waiting state such as `AwaitingCheckoutSync` instead of reusing `DONE` or generic convergence-waiting copy.

- In runtime and HTTP tests, add invariant assertions equivalent to:

    done item for current revision => no unresolved finalize_target_ref operation for that revision

  and

    unresolved finalize_target_ref operation for current revision => finalization status is visible in the API even when queue.state is null

Revision note: created on 2026-04-12 after investigating the live `perdify` incident where Ingot showed `itm_019d816a442c7d739d59dade1082ab29` as integrated even though the mirror alone had advanced `refs/heads/main` to `4d2ccc10126bbc48df92f06bbb1febd71a2217ba` and the registered checkout remained at `8cb76ba10e60868a5cf4a83fd72f656142bf4052`.
