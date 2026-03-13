# Freeze fresh snapshots for superseding revisions

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with `.agent/PLANS.md`.

## Purpose / Big Picture

After this change, every new item revision created by `revise`, `approval/reject`, or `reopen` will carry its own frozen execution policy snapshot instead of inheriting stale policy metadata from the superseded revision. A user can verify the fix by changing `approval_policy` through those endpoints and observing that the returned revision and the stored `policy_snapshot` agree.

## Progress

- [x] 2026-03-13 16:05Z Confirmed the bug against `SPEC.md` and the current `router.rs` handlers.
- [x] 2026-03-13 16:10Z Chose the shared-helper fix so `revise`, `approval/reject`, and `reopen` stay consistent.
- [x] 2026-03-13 16:18Z Patched superseding revision construction to regenerate `policy_snapshot` and `template_map_snapshot`, and routed approval rejection through the shared helper.
- [x] 2026-03-13 16:24Z Added regression tests for revise, approval reject, and reopen policy snapshot consistency, plus a unit test for policy budget extraction.
- [x] 2026-03-13 16:27Z Ran targeted Rust tests for the affected routes and helper; all passed.

## Surprises & Discoveries

- Observation: initial item creation already freezes a fresh snapshot in `crates/ingot-usecases/src/item.rs`, but all superseding revisions were cloning snapshots in `crates/ingot-http-api/src/router.rs`.
  Evidence: `create_manual_item` calls `default_policy_snapshot(...)`, while `build_superseding_revision` and `approval_reject` used `current_revision.policy_snapshot.clone()`.

- Observation: `reopen` uses the same superseding revision helper as `revise`, so a shared fix also changes reopen behavior.
  Evidence: `reopen_item` calls `build_superseding_revision(&project, &item, &current_revision, &jobs, request)`.

- Observation: `approval/reject` has a separate request type that is field-for-field compatible with `ReviseItemRequest`, but Rust will not infer that compatibility for the shared helper.
  Evidence: the first build failed with `expected ReviseItemRequest, found RejectApprovalRequest`, which was resolved by adding `impl From<RejectApprovalRequest> for ReviseItemRequest`.

## Decision Log

- Decision: regenerate snapshots in the shared superseding revision path instead of patching only the two reported endpoints.
  Rationale: `reopen` shares the same revision-construction logic, and the spec expects a new immutable revision contract rather than a selective exception.
  Date/Author: 2026-03-13 / Codex

- Decision: preserve budget values from the prior revision's frozen snapshot when building the new snapshot.
  Rationale: there is no request surface for changing those values on superseding revisions, so carrying them forward maintains current behavior while still freezing a fresh snapshot object.
  Date/Author: 2026-03-13 / Codex

- Decision: fall back to cloning the prior `policy_snapshot` and only updating `approval_policy` if the older snapshot does not expose rework budget fields.
  Rationale: current production-style revisions should carry valid budget fields, but this keeps the helper tolerant of sparse historical fixtures while still fixing the reported stale-policy bug.
  Date/Author: 2026-03-13 / Codex

## Outcomes & Retrospective

Implemented the shared superseding-revision fix and verified it with focused tests. `revise`, `approval/reject`, and `reopen` now all create revisions whose stored and returned `policy_snapshot.approval_policy` matches the new revision contract, while carrying forward rework budgets from the superseded revision when present.

## Context and Orientation

Item revisions are stored in `crates/ingot-domain/src/revision.rs` and persisted by the SQLite store. The HTTP layer in `crates/ingot-http-api/src/router.rs` owns the `revise`, `reopen`, and `approval/reject` command handlers. Initial manual item creation lives in `crates/ingot-usecases/src/item.rs`; that module already knows how to build the default `policy_snapshot` and `template_map_snapshot` for a fresh revision. A "policy snapshot" is the JSON blob stored on each revision that records the workflow version, approval policy, and rework budgets that later jobs read. A "template map snapshot" is the JSON object mapping workflow step ids to the default prompt template slug captured on the revision.

## Plan of Work

First, expose the snapshot-building helpers from `crates/ingot-usecases/src/item.rs` so the HTTP router can reuse the same logic as initial creation. Add one small helper to read candidate and integration rework budgets from an existing revision snapshot, because superseding revisions do not accept those values directly.

Next, update `build_superseding_revision` in `crates/ingot-http-api/src/router.rs` to create a new `policy_snapshot` and `template_map_snapshot` for the returned `ItemRevision`. Apply the same snapshot regeneration in the inline approval-reject revision creation path, or refactor that handler to use the shared helper if that keeps the code smaller and clearer.

Then extend the route tests in `crates/ingot-http-api/src/router.rs` so policy-changing `revise`, `approval/reject`, and `reopen` requests assert that the response revision and the stored `policy_snapshot` agree, while budget values are preserved.

## Concrete Steps

From `/Users/aa/Documents/ingot`, implement the Rust changes and run:

    cargo test -p ingot-http-api revise_route_creates_superseding_revision
    cargo test -p ingot-http-api approval_reject_route_supersedes_revision_and_cancels_prepared_convergence
    cargo test -p ingot-http-api dismiss_and_reopen_routes_close_and_reopen_item

After the patch, each test passes and the returned revision JSON contains matching `approval_policy` and `policy_snapshot.approval_policy` values.

## Validation and Acceptance

Acceptance is met when the three route tests above prove that changing `approval_policy` through each superseding-revision command produces a new revision whose top-level `approval_policy` matches the frozen `policy_snapshot.approval_policy`, and the carried-forward rework budgets remain unchanged.

## Idempotence and Recovery

The code and test edits are additive and safe to rerun. If a test fails midway, rerun the specific `cargo test -p ingot-http-api <test_name>` command after adjusting the assertion or helper code. No schema or migration changes are required.

## Artifacts and Notes

Expected post-fix behavior example:

    current_revision.approval_policy == "not_required"
    current_revision.policy_snapshot.approval_policy == "not_required"
    current_revision.policy_snapshot.candidate_rework_budget == <prior value>

Observed verification commands:

    cargo test -p ingot-http-api revise_route_creates_superseding_revision
    cargo test -p ingot-http-api reject_approval_route_cancels_prepared_convergence_and_creates_superseding_revision
    cargo test -p ingot-http-api dismiss_and_reopen_routes_close_and_reopen_item
    cargo test -p ingot-usecases rework_budgets_are_read_from_policy_snapshot

Each command completed successfully on 2026-03-13 after the shared helper and regression tests were in place.

## Interfaces and Dependencies

`crates/ingot-usecases/src/item.rs` should expose stable helper functions that produce revision snapshots:

    pub fn default_policy_snapshot(
        approval_policy: ApprovalPolicy,
        candidate_rework_budget: u32,
        integration_rework_budget: u32,
    ) -> Value

    pub fn default_template_map_snapshot() -> Value

    pub fn rework_budgets_from_policy_snapshot(policy_snapshot: &Value) -> Option<(u32, u32)>

`crates/ingot-http-api/src/router.rs` should use those helpers when constructing any new superseding `ItemRevision`.

Revision note: created this ExecPlan during implementation because the change touches shared revision-creation behavior and regression coverage across multiple command paths.

Revision note: updated after implementation to record the request-type conversion, the sparse-snapshot fallback, and the passing test evidence.
