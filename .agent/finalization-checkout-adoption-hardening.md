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
- [x] (2026-04-12T16:18Z) Re-read the plan against the current repository and corrected it to cover the actual approval, auto-finalize, reconciliation, workflow, store, and UI paths that participate in the broken state.
- [x] (2026-04-12T15:09Z) Extended `ConvergenceState::Finalized` with durable checkout-adoption state, message, and timestamps; added SQLite migration `0011_finalized_checkout_adoption.sql`; and covered finalized round-tripping in domain and store tests.
- [x] (2026-04-12T15:15Z) Split usecase/runtime/HTTP finalization into target-ref advance versus checkout-adoption completion so queue release and workspace abandonment can happen before item closure without ever writing `done + unresolved finalize_target_ref`.
- [x] (2026-04-12T15:20Z) Added `FinalizationStatusResponse`, projected blocked/pending finalized convergences independently of queue state, and taught workflow/UI to render `awaiting_checkout_sync` as a working-state blocker instead of `DONE`.
- [x] (2026-04-12T15:25Z) Rewrote the permissive runtime/usecase/HTTP/UI tests around blocked finalization, added workflow and store coverage for the new state, and passed `make ci`.
- [x] (2026-04-12T16:10Z) Collapsed finalization persistence behind a single transactional store mutation used by both approval-time finalization and runtime reconciliation, removed read-model dependence on unresolved finalize operations, corrected migration backfill for historical unresolved finalize rows, and re-passed `make ci`.

## Surprises & Discoveries

- Observation: the current runtime explicitly closes the item before checkout adoption is proven.
  Evidence: `crates/ingot-agent-runtime/src/reconciliation.rs` calls `adopt_finalized_target_ref(...)` at line ~178 inside `complete_finalize_target_ref_operation(...)`, then computes `checkout_finalization_status(...)` afterward at lines ~180-229.

- Observation: the current adopter clears escalation, releases the queue entry, and sets `Lifecycle::Done` even when the finalize git operation remains unresolved.
  Evidence: `crates/ingot-agent-runtime/src/reconciliation.rs` lines ~311-380 set the convergence to `Finalized`, release the active queue entry, and clear `item.escalation`, while the caller still returns `FinalizeCompletionOutcome::Blocked` when checkout sync is not safe.

- Observation: the current UI hides checkout-adoption blockers as soon as the queue entry is released.
  Evidence: `crates/ingot-http-api/src/router/item_projection.rs` returns `empty_queue_status()` immediately when `find_active_queue_entry_for_revision(...)` returns `None` at lines ~243-249, so `checkout_sync_blocked` becomes false even if an unresolved `finalize_target_ref` row still exists.

- Observation: the current runtime tests intentionally bless the broken state.
  Evidence: `crates/ingot-agent-runtime/tests/convergence.rs` lines ~183-218 assert that blocked auto-finalize leaves the item `done`, `Escalation::None`, queue released, and exactly one unresolved `FinalizeTargetRef` operation.

- Observation: the same broken write happens in the HTTP approval path, not only in runtime reconciliation.
  Evidence: `crates/ingot-http-api/src/router/convergence_port.rs` lines ~621-719 implement `PreparedConvergenceFinalizePort::apply_successful_finalization(...)` by transitioning the convergence to `Finalized`, releasing the queue entry, and writing `Lifecycle::Done`; `crates/ingot-http-api/tests/convergence_routes.rs` lines ~654-692 assert that `approval/approve` returns success while the finalize operation remains `applied`.

- Observation: the shared usecase tests also encode "close first, sync later" as accepted behavior.
  Evidence: `crates/ingot-usecases/src/convergence/tests.rs` lines ~193-247 expect `approve_item(...)` to succeed, call `apply_successful_finalization`, and leave the finalize operation unresolved when checkout sync is blocked or when the sync retry fails.

- Observation: leaving the item open after target-ref advance is not enough by itself; the workflow evaluator currently only recognizes prepared convergences as the finalization gate.
  Evidence: `crates/ingot-workflow/src/evaluator.rs` lines ~176-180 only capture `prepared_convergence`, and `crates/ingot-workflow/src/evaluator/projection.rs` lines ~200-205 only special-case `prepared_convergence.is_some()`, so an open item with a finalized convergence would otherwise fall through to the wrong idle/operator projection.

- Observation: both runtime and HTTP checkout-sync reconciliation helpers stop surfacing checkout blockers once the item is already `done`.
  Evidence: `crates/ingot-agent-runtime/src/convergence.rs` lines ~642-664 and `crates/ingot-http-api/src/router/convergence_port.rs` lines ~178-199 only set `EscalationReason::CheckoutSyncBlocked` when `!item.lifecycle.is_done()`.

- Observation: moving item closure behind git-operation reconciliation forced the usecase layer to mark the finalize operation reconciled before invoking the closure write, otherwise the HTTP path immediately tripped the new invariant on its own prepared `Convergence` snapshot.
  Evidence: the first `convergence_routes` run after the split returned HTTP 500 for the happy-path approval tests until `persist_checkout_adoption_success(...)` reloaded the stored finalized convergence and required `GitOperationStatus::Reconciled` before closing the item.

- Observation: releasing the queue at target-ref advance means the correct post-finalize queue projection is now `state = null`, not `released`, because `find_active_queue_entry_for_revision(...)` only returns active rows.
  Evidence: the updated blocked approval route test initially expected `json["queue"]["state"] == "released"` and failed; the durable blocker is now carried by `finalization.*` while queue projection correctly returns no active lane row.

- Observation: the remaining “serious” bug after the first hardening pass came from marking the finalize git operation reconciled before item closure committed, which meant a partial failure could strand an open item with no unresolved finalize row left to retry.
  Evidence: the review pass traced the sequence through `crates/ingot-usecases/src/convergence/finalization.rs`, `crates/ingot-agent-runtime/src/reconciliation.rs`, and `crates/ingot-http-api/src/router/convergence_port.rs`, then the follow-up refactor moved both reconciliation and closure into one SQLite mutation in `crates/ingot-store-sqlite/src/store/finalization.rs`.

- Observation: the cleanest boundary is “transactional DB state here, filesystem cleanup in adapters,” because integration-workspace path removal cannot participate in the SQLite transaction.
  Evidence: moving all finalize writes into `apply_finalization_mutation(...)` initially regressed the runtime convergence tests until the runtime and HTTP adapters restored `worktree remove` as a pre-mutation side effect while leaving convergence/item/git-operation state changes inside the transaction.

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

- Decision: keep the durable checkout-adoption substate inside `ConvergenceState::Finalized` instead of introducing a new top-level `ConvergenceStatus`.
  Rationale: the repository already treats `ConvergenceStatus::Finalized` as the terminal "target ref advanced" state in `crates/ingot-store-sqlite/src/store/convergence.rs`, `crates/ingot-usecases/src/job_dispatch.rs`, `crates/ingot-usecases/src/finding/report.rs`, and `crates/ingot-agent-runtime/src/reconciliation.rs`. Extending the finalized payload is the smallest change that preserves those terminal-state queries while still separating mirror advance from checkout adoption.
  Date/Author: 2026-04-12 / Codex

- Decision: widen the UI and API change rather than papering over the issue with a special-case warning string.
  Rationale: this is a state-model bug, not a copy bug. The UI needs a first-class finalization/adoption status so future features cannot regress by reading the wrong source of truth.
  Date/Author: 2026-04-12 / Codex

- Decision: keep queue release tied to target-ref advancement and make finalization truth durable outside queue rows.
  Rationale: `find_active_queue_entry_for_revision(...)` only treats `queued` and `head` entries as active in `crates/ingot-store-sqlite/src/store/convergence_queue.rs`, so holding the queue entry open until checkout sync would also hold the lane. The queue should keep describing lane ownership while the new finalization state describes post-release checkout adoption.
  Date/Author: 2026-04-12 / Codex

- Decision: move finalization persistence into a dedicated repository mutation rather than trying to keep the usecase port as the owner of multi-row updates.
  Rationale: the first hardening pass still duplicated transition sequencing across usecase, runtime, and HTTP adapters. A `FinalizationMutation` owned by `crates/ingot-store-sqlite/src/store/finalization.rs` makes convergence, git-operation, queue, workspace, escalation, and item updates atomic and removes the reconciled-before-closed hole.
  Date/Author: 2026-04-12 / Codex

- Decision: stop projecting `finalize_operation_unresolved` in the API response.
  Rationale: once finalization writes are atomic, the UI does not need git-operation rows to understand user-visible truth. The read model should project from convergence finalization state plus item state only.
  Date/Author: 2026-04-12 / Codex

## Outcomes & Retrospective

This ExecPlan records the intended hardening before implementation. The key outcome target is that the system will stop equating "the mirror moved" with "the project integrated successfully". Instead, convergence state, item lifecycle, unresolved git operations, HTTP projections, and UI labels will all agree on one rule: the item is not integrated until the registered checkout is on the final commit.

The most important lesson from the incident is that a local fix in one layer would not be enough. The current bug is enforced by domain semantics, runtime sequencing, queue projection, and tests at the same time. The implementation therefore needs to change the model, not just one condition.

Implementation completed on 2026-04-12. Finalized convergences now persist checkout-adoption truth directly, item closure only happens after the finalize git operation reconciles, and the API/UI expose an explicit `awaiting_checkout_sync` state even after the queue row is released. The focused commands in this plan plus the full `make ci` gate passed after the hardening landed.

Follow-up refactor completed later the same day. Finalization now uses one transactional store mutation for target-ref advance and checkout-adoption success, git-operation reconciliation and item closure succeed or fail together, and item/detail projection no longer consults unresolved finalize operations. The main remaining complexity is unavoidable split between transactional database state and non-transactional filesystem cleanup of integration worktrees.

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
- `crates/ingot-usecases/src/convergence/types.rs`, `crates/ingot-usecases/src/convergence/finalization.rs`, `crates/ingot-usecases/src/convergence/command.rs`, and `crates/ingot-usecases/src/convergence/system_actions.rs`, which route both approval and auto-finalize through the shared prepared-convergence finalizer.
- `crates/ingot-agent-runtime/src/runtime_ports.rs` and `crates/ingot-http-api/src/router/convergence_port.rs`, which are the two concrete `PreparedConvergenceFinalizePort` implementations that currently persist convergence, queue, and item closure together.
- `crates/ingot-agent-runtime/src/reconciliation.rs`, which adopts and retries unresolved finalize operations.
- `crates/ingot-agent-runtime/src/convergence.rs`, which currently clears or raises `CheckoutSyncBlocked` escalation only while the item is still open.
- `crates/ingot-usecases/src/convergence/finalization.rs`, which defines the shared approval/auto-finalize flow.
- `crates/ingot-http-api/src/router/item_projection.rs`, which currently exposes checkout blockers only when a queue row is still active.
- `crates/ingot-http-api/src/router/types.rs`, which defines `QueueStatusResponse`, `ItemSummaryResponse`, and `ItemDetailResponse`.
- `crates/ingot-workflow/src/evaluator.rs`, `crates/ingot-workflow/src/evaluator/projection.rs`, and `crates/ingot-workflow/src/recommended_action.rs`, which currently model finalization only through the presence of a prepared convergence and queue-owned checkout blockers.
- `ui/src/types/domain.ts`, `ui/src/lib/status.ts`, `ui/src/components/StatusBadge.tsx`, `ui/src/components/item-detail/OperatorActions.tsx`, `ui/src/pages/BoardPage.tsx`, `ui/src/pages/DashboardPage.tsx`, and `ui/src/pages/ItemDetailPage.tsx`, which render the board/detail state that operators actually see.
- `crates/ingot-store-sqlite/src/db.rs` and `crates/ingot-store-sqlite/src/store/convergence.rs`, plus `crates/ingot-store-sqlite/migrations/0001_initial.sql`, which define and load the persisted `convergences` table and the migration chain that tests execute.

This plan uses a few terms of art:

- "Target-ref advance" means the compare-and-swap update of the mirror's branch ref, usually `refs/heads/main`.
- "Checkout adoption" means the registered checkout successfully reaches the same final commit and is safe to use.
- "Unresolved finalize operation" means a `git_operations` row with `operation_kind = finalize_target_ref` and `status IN ('planned', 'applied')`.
- "Impossible state" means a persisted state combination that the system should never write, such as `item.lifecycle = done` while a finalize operation for the current revision remains unresolved.

## Plan of Work

### Milestone 1: Make finalization state explicit and durable

The first milestone introduces the missing domain concept: finalization is not one bit. Edit `crates/ingot-domain/src/convergence.rs` so `ConvergenceState::Finalized` carries durable checkout-adoption information instead of treating `completed_at` as the only finalization fact. The most direct shape is a new `CheckoutAdoptionState` enum with values `Pending`, `Blocked`, and `Synced`, plus the blocker message and separate timestamps needed to explain and audit the transition. Keep this as extra payload on `ConvergenceState::Finalized`; do not add a new top-level `ConvergenceStatus`, because the rest of the repository already uses `Finalized` as the terminal "target ref advanced" status.

Update `ConvergenceStateParts`, `ConvergenceWire`, `transition_to_finalized(...)`, any new setters needed to move between pending/blocked/synced adoption, and builders in `crates/ingot-domain/src/test_support/convergence.rs` so code can construct and round-trip the richer finalized state. Keep the existing `prepared_commit_oid` and `final_target_commit_oid` semantics intact.

Persist that state in SQLite. Add a new numbered migration under `crates/ingot-store-sqlite/migrations/` instead of rewriting historical migrations, because `crates/ingot-store-sqlite/src/db.rs` runs the full `sqlx::migrate!("./migrations")` chain in tests and local startup. Extend `crates/ingot-store-sqlite/src/store/convergence.rs` and `crates/ingot-store-sqlite/tests/convergence.rs` so the new finalized payload round-trips cleanly and the schema enforces the required fields for finalized rows.

At the end of this milestone, the data model itself must be able to say "the mirror ref moved, but the checkout is still blocked because of local dirtiness" without consulting queue rows or item escalation.

### Milestone 2: Refactor runtime finalization into two explicit phases

The second milestone changes behavior. Edit `crates/ingot-usecases/src/convergence/types.rs`, `crates/ingot-usecases/src/convergence/finalization.rs`, `crates/ingot-agent-runtime/src/runtime_ports.rs`, `crates/ingot-http-api/src/router/convergence_port.rs`, and `crates/ingot-agent-runtime/src/reconciliation.rs` so finalization becomes two explicit phases across both the approval path and the daemon path.

Phase one is target-ref advance. When the mirror compare-and-swap succeeds, persist the convergence as finalized with checkout adoption `Pending` or `Blocked`, abandon the integration workspace if appropriate, and optionally release the queue lane if lane release is still desired once the branch ref moved.

Phase two is checkout adoption. Only when `checkout_finalization_status(...)` returns `Synced`, or when `NeedsSync` and the subsequent `sync_checkout_to_commit(...)` succeeds, may the code close the item, clear escalation, mark the finalize operation `Reconciled`, and emit the final integrated activity.

To make that precise, split the current sink method `PreparedConvergenceFinalizePort::apply_successful_finalization(...)` into two narrower responsibilities and update both concrete implementations. In `crates/ingot-http-api/src/router/convergence_port.rs` and `crates/ingot-agent-runtime/src/runtime_ports.rs`, one method should persist target-ref advancement and convergence/workspace/queue state, and a second method should persist checkout adoption success and item closure. In `crates/ingot-agent-runtime/src/reconciliation.rs`, split the current [reconciliation.rs](/Users/aa/Documents/ingot/crates/ingot-agent-runtime/src/reconciliation.rs:311) adopter into the same two phases and update both `complete_finalize_target_ref_operation(...)` and `adopt_reconciled_git_operation(...)` so blocked or failed checkout adoption can never fall into the item-close helper.

This milestone must also centralize closure. Search for every direct write of `Lifecycle::Done { reason: DoneReason::Completed, ... }` in convergence/finalization paths and route them through one narrow helper that first checks "no unresolved finalize op for this current revision" and "convergence finalization state says checkout adoption is synced". In this repository, that means at least `crates/ingot-agent-runtime/src/reconciliation.rs` and `crates/ingot-http-api/src/router/convergence_port.rs`. Update the checkout-sync escalation helpers in `crates/ingot-agent-runtime/src/convergence.rs` and `crates/ingot-http-api/src/router/convergence_port.rs` at the same time so blocked adoption remains visible while the item stays open.

At the end of this milestone, `done + unresolved finalize_target_ref` must be impossible to write from either approval-time finalization or reconciliation-time finalization.

### Milestone 3: Decouple UI/API finalization projection from queue state

The third milestone changes the read model so operators can see the truth. Edit `crates/ingot-http-api/src/router/types.rs` to introduce a first-class `FinalizationStatusResponse` and add it to both `ItemSummaryResponse` and `ItemDetailResponse`, because the board and dashboard render `ItemSummaryResponse` while the detail page renders `ItemDetailResponse`. It should carry the current finalization phase, the checkout adoption state, the blocker message, the final target commit OID when known, and whether a finalize git operation is still unresolved. Keep `QueueStatusResponse` focused on queue position and lane ownership only.

Then update `crates/ingot-http-api/src/router/item_projection.rs` so `evaluate_item_snapshot(...)` and its helpers derive finalization status from convergences plus unresolved finalize operations, not from `find_active_queue_entry_for_revision(...)`. The function currently returns `empty_queue_status()` as soon as no queue row exists; that early return must stop suppressing checkout blockers. The projection logic should surface a blocked checkout-adoption state even when the queue is already released and the integration workspace is already abandoned, and `overlay_evaluation_with_queue_state(...)` should stop being the only place that can produce `ResolveCheckoutSync`.

Update the workflow layer so an item whose target ref advanced but checkout adoption is not synced does not land in `BoardStatus::DONE` and does not fall through to an unrelated idle/operator state. The cleanest design is to add a dedicated `PhaseStatus::AwaitingCheckoutSync` in `crates/ingot-workflow/src/evaluator.rs`, teach `crates/ingot-workflow/src/evaluator/projection.rs` and `crates/ingot-workflow/src/recommended_action.rs` how to project it, and keep such items in `BoardStatus::WORKING`. Then update `ui/src/types/domain.ts`, `ui/src/lib/status.ts`, `ui/src/components/StatusBadge.tsx`, `ui/src/components/item-detail/OperatorActions.tsx`, `ui/src/pages/BoardPage.tsx`, and `ui/src/pages/ItemDetailPage.tsx` so the UI renders a human-readable label such as "Awaiting checkout sync" and the precise blocker message instead of raw snake_case or a false `DONE`.

At the end of this milestone, the user should never need to infer the truth from Git manually. If the checkout has not adopted the final commit, the API and UI must say so directly.

### Milestone 4: Replace permissive tests with invariant-driven state-machine coverage

The fourth milestone removes the test scaffolding that currently blesses the fatal bug. Rewrite the blocked-auto-finalize expectations in `crates/ingot-agent-runtime/tests/convergence.rs`, especially `assert_blocked_auto_finalize_state(...)`, so the correct blocked state is: mirror target ref advanced, finalize operation unresolved, item still open, and checkout adoption state blocked with a durable message. The old assertion that `Escalation::None` and `item.lifecycle.is_done()` are acceptable must be deleted.

Add targeted tests across the stack:

- A runtime reconciliation test for "mirror ref advanced, checkout blocked" that proves the item stays open and visible as blocked.
- A runtime reconciliation test for "checkout becomes safe later" that proves the item closes only after the finalize operation reconciles.
- A usecase test update in `crates/ingot-usecases/src/convergence/tests.rs` that inverts the current "approval succeeds and closes immediately" expectations.
- An HTTP route test update in `crates/ingot-http-api/tests/convergence_routes.rs` that proves `approval/approve` leaves the item open when checkout adoption is blocked.
- An HTTP projection test, ideally in `crates/ingot-http-api/src/router/item_projection.rs` and reinforced by `crates/ingot-http-api/tests/item_routes.rs`, that proves a released queue entry does not hide a blocked checkout-adoption state.
- A workflow evaluator test in `crates/ingot-workflow/src/evaluator/tests.rs` that proves open-plus-finalized-with-blocked-adoption projects the new waiting phase instead of `DONE` or generic operator intervention.
- A usecase or runtime invariant test that rejects or panics on any attempt to produce `done + unresolved finalize op`.
- UI tests in `ui/src/test/domain-contract.test.ts`, `ui/src/test/item-detail-page.test.tsx`, and `ui/src/test/board-page.test.tsx` that show the new finalization payload and the "Awaiting checkout sync" label instead of `DONE`.

Add one higher-level state-machine style test that drives the real transition sequence: prepare convergence, advance target ref, block checkout adoption, release queue, render item detail, clean checkout, reconcile, and render again. That single test should prove the cross-layer contract rather than only local helper behavior.

At the end of this milestone, the old broken state is both impossible to write and impossible for tests to accidentally re-approve.

## Concrete Steps

Work from `/Users/aa/Documents/ingot`.

During implementation, keep the plan updated after every milestone and after any design change. Run focused commands as each layer lands:

    cargo test -p ingot-domain convergence
    cargo test -p ingot-store-sqlite --test convergence
    cargo test -p ingot-workflow evaluator
    cargo test -p ingot-usecases convergence::tests
    cargo test -p ingot-agent-runtime --test convergence
    cargo test -p ingot-agent-runtime --test reconciliation
    cargo test -p ingot-http-api item_projection
    cargo test -p ingot-http-api --test convergence_routes
    cargo test -p ingot-http-api --test item_routes
    cd ui && bun run test -- src/test/domain-contract.test.ts src/test/item-detail-page.test.tsx src/test/board-page.test.tsx

After the focused commands pass, run the full repository gate from `/Users/aa/Documents/ingot`:

    make ci

Expected checkpoints while implementing:

    - Before the change, `crates/ingot-agent-runtime/tests/convergence.rs` accepts `done + unresolved finalize op`.
    - Before the change, `crates/ingot-usecases/src/convergence/tests.rs` and `crates/ingot-http-api/tests/convergence_routes.rs` accept approval success while checkout adoption is still unresolved.
    - Before the change, `crates/ingot-http-api/src/router/item_projection.rs` drops checkout blockers whenever there is no active queue row.
    - Before the change, `crates/ingot-workflow/src/evaluator.rs` only treats `Prepared` convergences as the finalization gate.
    - After Milestone 2, blocked checkout adoption leaves the finalize operation unresolved, keeps the item open, and leaves queue release separate from item closure.
    - After Milestone 3, item summary and item detail responses include explicit finalization/adoption state even with `queue.state = null`.
    - After Milestone 4, the runtime, usecase, HTTP, workflow, and UI tests all describe the same contract and no test still blesses `done + unresolved finalize_target_ref`.

## Validation and Acceptance

Acceptance requires all of the following observable behavior:

1. When the mirror target ref advances but the registered checkout is blocked, the convergence is marked finalized with checkout adoption `Blocked`, the finalize git operation remains unresolved, and the item is not `done`.
2. When the queue entry is released after target-ref advance, item detail still shows a finalization/adoption blocker and does not fall back to a neutral queue status.
3. When `POST /api/projects/:project_id/items/:item_id/approval/approve` finalizes a prepared convergence but the checkout is blocked, the response shows the item still open and the durable finalization state reports blocked checkout adoption.
4. When the registered checkout later becomes safe, the runtime reconciliation path synchronizes it, marks the finalize operation `Reconciled`, and only then closes the item.
5. The board and detail UI render a dedicated non-`DONE` status for this waiting period, including the durable blocker message.
6. No code path in approval, auto-finalize, or reconciliation can write `Lifecycle::Done` while a `FinalizeTargetRef` operation for the current revision is still unresolved.
7. The test suite no longer contains any assertions that intentionally bless `done + unresolved finalize_target_ref`.
8. The original incident shape is covered by an automated test that fails before this hardening and passes after it.

## Idempotence and Recovery

All implementation steps should be safe to rerun. The new finalization state should be written deterministically, and retrying checkout adoption while the checkout is still blocked must leave the same durable blocker state rather than duplicating rows or flipping the item between open and done.

The runtime and approval paths must remain idempotent when repeatedly touching the same unresolved finalize operation. Re-running the reconcile loop against a blocked checkout should update timestamps only when necessary and must not emit duplicate closure side effects, duplicate queue releases, or a second success write from `PreparedConvergenceFinalizePort`. If the implementation keeps `ActivityEventType::ConvergenceFinalized` at target-ref advance time, that activity should also remain single-shot.

Because backwards compatibility is explicitly out of scope, do not add fallback parsing branches, compatibility fields, or one-off repair logic for older rows. The schema and domain types may move directly to the hardened design.

## Artifacts and Notes

Plan revision note (2026-04-12): updated the plan after the transactional refactor to record the second-pass progress, the reconciled-before-closed hole that motivated the repository mutation, the adapter/store split for workspace-path cleanup, and the fact that the full repository gate passed again.

Capture these artifacts while implementing:

    - A focused test transcript showing a blocked finalize operation leaves the item open and the finalize row unresolved.
    - A focused test transcript showing a later checkout cleanup reconciles the same finalize row and only then closes the item.
    - An HTTP response example, from a test fixture, where `queue.state` is null but the new `finalization` payload still says checkout adoption is blocked.
    - A UI assertion proving the board/detail status label is "Awaiting checkout sync" rather than `DONE`.
    - A short excerpt from the old blocked-auto-finalize, approval, and convergence-route tests and the new inverted assertions that replace them.

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

- In `crates/ingot-store-sqlite/migrations/`, add a new migration that extends `convergences`, and update `crates/ingot-store-sqlite/src/store/convergence.rs` plus `crates/ingot-store-sqlite/tests/convergence.rs` so the richer finalized state is enforced and round-tripped.

- In `crates/ingot-usecases/src/convergence/types.rs`, split the current `PreparedConvergenceFinalizePort` success sink into separate phase-one and phase-two methods so approval and auto-finalize cannot persist closure as part of "mirror ref advanced".

- In `crates/ingot-agent-runtime/src/runtime_ports.rs` and `crates/ingot-http-api/src/router/convergence_port.rs`, implement those split phase methods. These two files are both mutation owners for the bad state today and both must stop writing convergence finalization, queue release, and item closure as one indivisible action.

- In `crates/ingot-agent-runtime/src/reconciliation.rs`, split the current finalize adopter into one helper that adopts target-ref advancement and one helper that closes the item after checkout adoption. The latter must be the only helper allowed to write `Lifecycle::Done` for convergence completion, and `adopt_reconciled_git_operation(...)` must call the correct phase-specific helper.

- In `crates/ingot-usecases/src/convergence/finalization.rs`, `crates/ingot-usecases/src/convergence/command.rs`, and `crates/ingot-usecases/src/convergence/system_actions.rs`, the shared finalization flow must persist "mirror advanced but checkout not yet adopted" without returning a false success/failure combination to callers.

- In `crates/ingot-agent-runtime/src/convergence.rs` and `crates/ingot-http-api/src/router/convergence_port.rs`, checkout-sync escalation logic must remain truthful while an item is open and waiting for checkout adoption.

- In `crates/ingot-http-api/src/router/types.rs`, define a first-class `FinalizationStatusResponse` (name may differ if the final implementation chooses a better one), add it to `ItemSummaryResponse` and `ItemDetailResponse`, and stop treating checkout-adoption fields as queue-owned data.

- In `crates/ingot-http-api/src/router/item_projection.rs`, projections must derive finalization state from convergences and unresolved `FinalizeTargetRef` operations rather than active queue rows alone.

- In `crates/ingot-workflow/src/evaluator.rs`, `crates/ingot-workflow/src/evaluator/projection.rs`, and `crates/ingot-workflow/src/recommended_action.rs`, introduce the explicit waiting-for-checkout-adoption read-side state so open finalized convergences project consistently.

- In `ui/src/types/domain.ts`, add the matching finalization status type and the new phase/status literals needed to render waiting-for-checkout-adoption correctly.

- In `ui/src/lib/status.ts` and `ui/src/components/StatusBadge.tsx`, make the new waiting state render human-readable copy rather than raw snake_case, then update `ui/src/components/item-detail/OperatorActions.tsx`, `ui/src/pages/BoardPage.tsx`, and `ui/src/pages/ItemDetailPage.tsx` to read the new finalization payload instead of queue-owned blocker fields.

- In runtime and HTTP tests, add invariant assertions equivalent to:

    done item for current revision => no unresolved finalize_target_ref operation for that revision

  and

    unresolved finalize_target_ref operation for current revision => finalization status is visible in the API even when queue.state is null

Revision note: created on 2026-04-12 after investigating the live `perdify` incident where Ingot showed `itm_019d816a442c7d739d59dade1082ab29` as integrated even though the mirror alone had advanced `refs/heads/main` to `4d2ccc10126bbc48df92f06bbb1febd71a2217ba` and the registered checkout remained at `8cb76ba10e60868a5cf4a83fd72f656142bf4052`.

Revision note (2026-04-12, plan review): corrected the implementation surface so the plan now covers both `PreparedConvergenceFinalizePort` implementations (`crates/ingot-agent-runtime/src/runtime_ports.rs` and `crates/ingot-http-api/src/router/convergence_port.rs`), the workflow evaluator/projection files that currently only understand prepared convergences, the actual SQLite store and migration files that must carry the new durable state, and the existing usecase/HTTP tests that currently bless "close first, sync later". Replaced the guessed UI test command with the repository's real `bun run test -- ...` form from `ui/package.json`, added `cargo test -p ingot-workflow evaluator`, and tightened the API plan so `ItemSummaryResponse` and `ItemDetailResponse` both carry explicit finalization state instead of hiding it behind queue rows.
