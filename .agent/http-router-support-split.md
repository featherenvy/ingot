# Split HTTP router wiring and support helpers

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows [.agent/PLANS.md](/Users/aa/Documents/ingot/.agent/PLANS.md) and must be maintained in accordance with that file.

## Purpose / Big Picture

After this change, the HTTP API router in `crates/ingot-http-api/src/router` should be easier to navigate because route registration lives with each feature module instead of one long chain in `router/mod.rs`, and shared helpers live in smaller modules under `router/support/` instead of one 600-line `support.rs` catch-all. The observable behavior should stay the same: `cargo test -p ingot-http-api` should still pass, and the same HTTP endpoints should remain mounted.

## Progress

- [x] (2026-03-28 13:36Z) Claimed `ingot-94m` and reviewed `router/mod.rs`, `router/support.rs`, and route-module dependencies.
- [x] (2026-03-28 13:37Z) Added feature-local `routes()` builders across the router feature modules and reduced `router/mod.rs` to router composition, shared state, middleware, and cross-feature helpers.
- [x] (2026-03-28 13:38Z) Replaced `crates/ingot-http-api/src/router/support.rs` with `crates/ingot-http-api/src/router/support/` and split helpers into `activity.rs`, `config.rs`, `errors.rs`, `io.rs`, `normalize.rs`, `path.rs`, and `project_repo.rs` while keeping stable re-exports through `support/mod.rs`.
- [x] (2026-03-28 13:39Z) Ran `cargo fmt --package ingot-http-api` and `cargo test -p ingot-http-api`; all tests passed. Closed `ingot-94m`.

## Surprises & Discoveries

- Observation: `bd ready --json` already contains the exact cleanup as `ingot-94m`, so no new issue is needed.
  Evidence: `bd ready --json` listed `Split ingot-http-api router wiring and support helpers into focused modules`.

- Observation: The first compile failure after the split came from visibility, not logic. Child helpers under `support/` needed visibility wide enough to be re-exported back to `router::*`, especially `ApiPath`.
  Evidence: `cargo test -p ingot-http-api` initially failed with re-export errors such as ``ApiPath is private, and cannot be re-exported`` and handler trait errors disappeared after widening those items to `pub(crate)`.

## Decision Log

- Decision: Keep the public helper names stable by re-exporting from `router::support`, even after splitting it into multiple files.
  Rationale: That gives the structural cleanup the user asked for without forcing a large, risky rename across every handler in one step.
  Date/Author: 2026-03-28 / Codex

- Decision: Put route registration on the existing feature modules as `routes()` builders and merge them from `router/mod.rs`.
  Rationale: The feature modules already exist, so this removes the central route manifest instead of moving it to another central file.
  Date/Author: 2026-03-28 / Codex

- Decision: Add a small `router/core.rs` module for the non-feature endpoints (`/api/health`, `/api/config`, and demo routes).
  Rationale: Those endpoints do not belong to one existing feature module, and keeping them in `mod.rs` would leave the same central route-registration smell in place.
  Date/Author: 2026-03-28 / Codex

## Outcomes & Retrospective

The router is structurally cleaner now. `crates/ingot-http-api/src/router/mod.rs` dropped from 392 lines to 255 lines and no longer contains the long endpoint-by-endpoint registration chain. Route registration now lives beside the handlers in the existing feature modules plus `crates/ingot-http-api/src/router/core.rs`. The old 636-line `crates/ingot-http-api/src/router/support.rs` is gone and replaced by a directory of focused helper modules.

The intended behavior stayed intact. `cargo test -p ingot-http-api` passed after the refactor, covering unit tests plus route tests across agents, convergence, demo project, dispatch, findings, harness, items, jobs, projects, and workspaces. A remaining lesson is that route modules still rely on `use super::*` for many shared imports, so there is more cleanup available if someone wants to keep shrinking `router/mod.rs`, but the main structural goals of this task are complete.

## Context and Orientation

The HTTP API lives in `crates/ingot-http-api/src/router`. Today `crates/ingot-http-api/src/router/mod.rs` both constructs `AppState` and mounts almost every route in the crate. The same directory already has feature modules such as `agents.rs`, `projects.rs`, `dispatch.rs`, `jobs.rs`, `findings.rs`, `convergence.rs`, `workspaces.rs`, and `items/mod.rs`. The support layer currently lives in one file, `crates/ingot-http-api/src/router/support.rs`, which mixes unrelated responsibilities: Axum path extraction, path and config filesystem helpers, git/repository helper functions, normalization, error mapping, activity writes, and optional log-file reads.

The target shape keeps `router/mod.rs` responsible for shared `AppState`, router construction, middleware, and a few cross-feature helpers such as `teardown_revision_lane_state`. Each feature module gets a `routes()` function that returns its own Axum router fragment. Shared helpers move under `crates/ingot-http-api/src/router/support/` and are grouped by responsibility while preserving the existing `super::support::*` call sites through re-exports.

## Plan of Work

First, add a small `core` router module for `/api/health`, `/api/config`, and the demo endpoints, then add `routes()` builders to the existing feature modules. Update `router/mod.rs` so it only merges those builders, installs the dispatch-notify middleware, and owns shared state setup.

Second, replace `router/support.rs` with `router/support/mod.rs` plus focused files such as `activity.rs`, `config.rs`, `errors.rs`, `io.rs`, `normalize.rs`, `path.rs`, and `project_repo.rs`. Move functions without changing their signatures, then re-export them from `support/mod.rs` so feature modules can keep compiling with the current imports.

Third, run `cargo fmt` for the crate and `cargo test -p ingot-http-api`. If the refactor exposes any follow-up cleanup that is clearly separate from this structural split, capture it in `bd` instead of broadening the patch.

## Concrete Steps

From `/Users/aa/Documents/ingot`:

1. Edit `.agent/http-router-support-split.md` as the plan and keep it current.
2. Edit `crates/ingot-http-api/src/router/mod.rs` plus the feature route modules to add `routes()` builders and router merging.
3. Replace `crates/ingot-http-api/src/router/support.rs` with a `support/` directory and focused modules.
4. Run `cargo fmt --package ingot-http-api`.
5. Run `cargo test -p ingot-http-api`.
6. Update `bd` status based on the result.

## Validation and Acceptance

Acceptance is:

1. `crates/ingot-http-api/src/router/mod.rs` no longer contains the long endpoint-by-endpoint route chain.
2. `crates/ingot-http-api/src/router/support.rs` no longer exists as a large catch-all file; the helpers live in smaller files under `crates/ingot-http-api/src/router/support/`.
3. `cargo test -p ingot-http-api` passes, proving the HTTP surface still compiles and the route helper tests still work.

## Idempotence and Recovery

The refactor is source-only and safe to repeat. If compilation fails mid-way, rerun `cargo test -p ingot-http-api` after each module move to identify missing re-exports or imports. Because helper signatures stay stable, recovery should be limited to import or visibility fixes rather than behavior changes.

## Artifacts and Notes

Initial evidence:

    $ wc -l crates/ingot-http-api/src/router/mod.rs crates/ingot-http-api/src/router/support.rs
         392 crates/ingot-http-api/src/router/mod.rs
         636 crates/ingot-http-api/src/router/support.rs
        1028 total

Final evidence:

    $ wc -l crates/ingot-http-api/src/router/mod.rs crates/ingot-http-api/src/router/support/mod.rs crates/ingot-http-api/src/router/support/*.rs
         255 crates/ingot-http-api/src/router/mod.rs
          28 crates/ingot-http-api/src/router/support/mod.rs
          29 crates/ingot-http-api/src/router/support/activity.rs
          24 crates/ingot-http-api/src/router/support/config.rs
         241 crates/ingot-http-api/src/router/support/errors.rs
          25 crates/ingot-http-api/src/router/support/io.rs
         102 crates/ingot-http-api/src/router/support/normalize.rs
         189 crates/ingot-http-api/src/router/support/path.rs
          51 crates/ingot-http-api/src/router/support/project_repo.rs
         944 total

    $ cargo test -p ingot-http-api
    test result: ok. 17 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
    ...
    test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
    ...
    test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
    ...
    test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

## Interfaces and Dependencies

Each route feature module should expose:

    pub(super) fn routes() -> Router<AppState>

The support module should continue exporting the existing helper names used by route handlers, including:

    ApiPath
    append_activity
    load_effective_config
    ensure_git_valid_target_ref
    git_to_internal
    repo_to_internal
    repo_to_project_mutation
    resolve_default_branch

Revision note: created this ExecPlan before implementation so the structural split can be tracked and updated as work proceeds.

Revision note: updated after implementation to record the final module split, the visibility fix needed for re-exports, the passing `cargo test -p ingot-http-api` run, and closure of `ingot-94m`.
