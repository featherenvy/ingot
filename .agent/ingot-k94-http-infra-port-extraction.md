# Finish HTTP Router Infra-Port Extraction

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows the repository requirements in `.agent/PLANS.md`.

## Purpose / Big Picture

After this change, the HTTP router code in `crates/ingot-http-api/src/router/` will consistently access mirror-backed git and workspace operations through `crates/ingot-http-api/src/router/infra_ports.rs` instead of reaching into `ingot-git` and `ingot-workspace` directly from route helpers. A reader should be able to inspect router modules and see business flow, while the filesystem and git plumbing lives behind `HttpInfraAdapter`. The observable proof is that `cargo test -p ingot-http-api` still passes after the extraction.

## Progress

- [x] (2026-03-28 15:11Z) Created and claimed bead issue `ingot-k94` for the HTTP router infra-port completion.
- [x] (2026-03-28 15:13Z) Mapped the remaining direct infrastructure calls: `items/revisions.rs`, `item_projection.rs`, and `items/convergence_prep.rs`.
- [x] (2026-03-28 15:24Z) Extended `HttpInfraAdapter` with the missing git/workspace primitives still used directly by router helpers.
- [x] (2026-03-28 15:31Z) Switched the remaining router helpers in `items/revisions.rs`, `item_projection.rs`, `items/convergence_prep.rs`, `dispatch.rs`, and `findings.rs` to use `HttpInfraAdapter` instead of raw `ingot_git` / `ingot_workspace` calls.
- [x] (2026-03-28 15:36Z) Ran `cargo fmt --all --check` and `cargo test -p ingot-http-api` successfully.
- [ ] Close `ingot-k94`, then commit and push.

## Surprises & Discoveries

- Observation: The user’s named files were directionally right, but the actual remaining direct git/workspace calls are concentrated in `items/revisions.rs`, `item_projection.rs`, and the dormant-but-compiled `items/convergence_prep.rs`; `convergence.rs` itself already delegates through `HttpConvergencePort`.
  Evidence: `rg -n "ingot_git|ingot_workspace|resolve_ref_oid|checkout_sync_status\\(|provision_integration_workspace" crates/ingot-http-api/src/router -g'*.rs'`.

- Observation: `findings.rs` and `dispatch.rs` still had route-level raw git usage after the first extraction pass, even though they were not in the original “remaining files” list.
  Evidence: A follow-up grep still found production `resolve_ref_oid` calls in `dispatch.rs` and `is_commit_reachable_from_any_ref` calls in `findings.rs` until those were moved to `HttpInfraAdapter`.

- Observation: Tests in `findings.rs` relied on `ensure_finding_subject_reachable` using the passed `Project` directly, not reloading it from the database.
  Evidence: the first post-refactor test run failed with `UseCase(Repository(NotFound))` until `HttpInfraAdapter` gained `is_commit_reachable_from_project`.

## Decision Log

- Decision: Finish the seam by expanding `HttpInfraAdapter` rather than introducing a second router-side adapter.
  Rationale: The router already has a recognized infrastructure boundary in `infra_ports.rs`; strengthening that seam is simpler and easier to understand than creating another abstraction layer.
  Date/Author: 2026-03-28 / Codex

- Decision: Keep support-layer validation helpers such as `support/errors.rs` unchanged for now even though they still call git directly.
  Rationale: The task scope is the router infrastructure seam used by route helpers and convergence/dispatch flows. The support helpers are shared validation code rather than route orchestration, and leaving them alone avoided widening the refactor beyond the issue’s intent.
  Date/Author: 2026-03-28 / Codex

## Outcomes & Retrospective

The HTTP router seam is complete for the targeted route/helper code paths. `HttpInfraAdapter` now owns the remaining mirror-backed git/workspace operations that had still been scattered through `items/revisions.rs`, `item_projection.rs`, `items/convergence_prep.rs`, `dispatch.rs`, and `findings.rs`. `cargo test -p ingot-http-api` stayed green after the move. The main lesson was that some route helpers were only incidentally outside the original report, so a final grep pass was necessary to reach a real 5/5 extraction.

## Context and Orientation

`crates/ingot-http-api/src/router/infra_ports.rs` already adapts mirror-backed git/workspace operations for dispatch and workspace usecases. Route helpers such as `dispatch.rs` and `workspaces.rs` already use it. The remaining direct infrastructure calls live in:

- `crates/ingot-http-api/src/router/items/revisions.rs` for resolving target refs and validating seed reachability.
- `crates/ingot-http-api/src/router/item_projection.rs` for checkout sync status and convergence target-head validity.
- `crates/ingot-http-api/src/router/items/convergence_prep.rs` for integration workspace provisioning and convergence replay git operations.

In this repository, a “mirror-backed” operation means a git or workspace action that should run against the project mirror/worktree layout computed from the HTTP app’s state root, not against arbitrary paths assembled in each route helper.

## Plan of Work

Add the missing git/workspace helpers to `HttpInfraAdapter`: commit reachability, convergence target-head validity, integration workspace provisioning, commit listing/message lookup, cherry-pick/reset helpers, working tree dirtiness checks, daemon convergence commit creation, and workspace-ref update. Then replace the direct `ingot_git` / `ingot_workspace` calls in the remaining router helper files with those adapter methods.

Keep the route helper control flow unchanged. The goal is to move infrastructure ownership, not to redesign convergence or revision behavior. Where a helper already takes `AppState` and `Project`, create a local `HttpInfraAdapter` and reuse it instead of recomputing raw mirror operations inline.

## Concrete Steps

From `/Users/aa/Documents/ingot`:

1. Update `crates/ingot-http-api/src/router/infra_ports.rs` with the missing helper methods.
2. Update `crates/ingot-http-api/src/router/items/revisions.rs`, `crates/ingot-http-api/src/router/item_projection.rs`, and `crates/ingot-http-api/src/router/items/convergence_prep.rs` to use `HttpInfraAdapter`.
3. Run:

    cargo fmt --all --check
    cargo test -p ingot-http-api

4. Close `ingot-k94`, then commit and push. Closing remains as the final repository workflow step.

## Validation and Acceptance

Acceptance is:

1. The remaining router helpers no longer call `ingot_git` / `ingot_workspace` directly for mirror-backed operations.
2. `HttpInfraAdapter` owns the router-side infrastructure seam for those operations.
3. `cargo test -p ingot-http-api` passes.
4. `ingot-k94` is closed. The code portion is complete; only tracker/commit/push workflow remains.

## Idempotence and Recovery

This refactor is safe to repeat because it is internal structure work. If a method move fails midway, the safe recovery path is to keep the old direct call in place until the new `HttpInfraAdapter` method compiles and tests pass.

## Artifacts and Notes

Initial evidence:

    $ rg -n "ingot_git|ingot_workspace|resolve_ref_oid|checkout_sync_status\\(|provision_integration_workspace" crates/ingot-http-api/src/router -g'*.rs'
    ... items/revisions.rs ...
    ... item_projection.rs ...
    ... items/convergence_prep.rs ...

## Interfaces and Dependencies

At the end of this change, `crates/ingot-http-api/src/router/infra_ports.rs` should expose the router-local git/workspace helper methods needed by the remaining route helpers, and those helpers should call `HttpInfraAdapter` instead of raw `ingot_git` / `ingot_workspace` functions.

Revision note: 2026-03-28 / Codex. Created the initial ExecPlan when claiming `ingot-k94`, before the remaining router infrastructure calls were moved behind `HttpInfraAdapter`.

Revision note: 2026-03-28 / Codex. Updated the plan after the adapter expansion, caller migrations, validation, and final seam cleanup so the completed HTTP extraction is fully recorded.
