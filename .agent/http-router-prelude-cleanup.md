# Finish HTTP router prelude cleanup

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows [.agent/PLANS.md](/Users/aa/Documents/ingot/.agent/PLANS.md) and must be maintained in accordance with that file.

## Purpose / Big Picture

After this change, the HTTP API router should be easier to navigate because `crates/ingot-http-api/src/router/mod.rs` will only own router state and top-level assembly, while feature modules import their own dependencies directly instead of pulling a giant shared prelude through `super::*` and `super::support::*`. The observable behavior stays the same: `cargo test -p ingot-http-api` should still pass, but the router tree should be structurally clearer and less coupled.

## Progress

- [x] (2026-03-28 15:57Z) Created and claimed `ingot-uud` as the follow-up task for the unfinished router split.
- [x] (2026-03-28 15:58Z) Audited `crates/ingot-http-api/src/router/mod.rs`, `crates/ingot-http-api/src/router/support/mod.rs`, and the feature modules that still use wildcard imports.
- [x] (2026-03-28 16:02Z) Added `crates/ingot-http-api/src/router/app.rs` and `crates/ingot-http-api/src/router/deps.rs`, moved router assembly out of `router/mod.rs`, and updated feature modules to use `deps::*` plus focused support submodule imports.
- [x] (2026-03-28 16:03Z) Reduced `crates/ingot-http-api/src/router/support/mod.rs` to focused module declarations and updated non-router callers such as `crates/ingot-http-api/src/demo/mod.rs` to import helper functions from their concrete support modules.
- [x] (2026-03-28 16:05Z) Ran `cargo fmt --package ingot-http-api`, `cargo check -p ingot-http-api`, and `cargo test -p ingot-http-api`; all passed.
- [ ] Update `bd`, commit, rebase, `bd dolt push`, and `git push`.

## Surprises & Discoveries

- Observation: The original split already created focused support files, but almost every major route module still imports them through `support::*`, which keeps `support/mod.rs` acting like the old catch-all helper surface.
  Evidence: `rg -n "use super::\\*;|use super::support::\\*;" crates/ingot-http-api/src/router` reports matches in `agents.rs`, `projects.rs`, `dispatch.rs`, `jobs.rs`, `findings.rs`, `workspaces.rs`, `convergence.rs`, `item_projection.rs`, `convergence_port.rs`, and `items/mod.rs`.

- Observation: `router/mod.rs` is only 256 lines now, but most of that size comes from imports and helper re-exports needed by sibling modules, not from actual router construction.
  Evidence: `crates/ingot-http-api/src/router/mod.rs` still imports Axum extractors, domain types, use cases, and helper functions that are consumed indirectly through `use super::*`.

- Observation: Moving `AppState` into `router/app.rs` changed privacy boundaries. Sibling router modules and `test_helpers.rs` could no longer reach private struct fields until the fields were widened to `pub(crate)`.
  Evidence: the first compile after the move required field access from files such as `crates/ingot-http-api/src/router/test_helpers.rs`, `crates/ingot-http-api/src/router/jobs.rs`, and `crates/ingot-http-api/src/router/dispatch.rs`.

- Observation: `demo/mod.rs` was part of the old helper barrel surface even though it lives outside `router/`.
  Evidence: `crates/ingot-http-api/src/demo/mod.rs` imported `append_activity`, `load_effective_config`, `ensure_git_valid_target_ref`, `git_to_internal`, `repo_to_internal`, `repo_to_project_mutation`, and `resolve_default_branch` from `crate::router`.

## Decision Log

- Decision: Keep the follow-up as a structural cleanup inside `crates/ingot-http-api` instead of broadening into other crates or more route-file splits.
  Rationale: The user feedback is specifically about the router and support wiring still feeling centralized; explicit imports fix that without changing behavior or widening scope.
  Date/Author: 2026-03-28 / Codex

- Decision: Prefer direct imports from `router::support::{activity, config, errors, io, normalize, path, project_repo}` over adding another curated prelude.
  Rationale: A new prelude would preserve the same coupling under a different name. Direct imports force each feature module to declare its own dependencies and make `support/mod.rs` genuinely thin.
  Date/Author: 2026-03-28 / Codex

- Decision: Introduce `crates/ingot-http-api/src/router/app.rs` and `crates/ingot-http-api/src/router/deps.rs` instead of keeping router assembly and the shared non-support import surface in `router/mod.rs`.
  Rationale: This keeps `router/mod.rs` as a true module index while still giving the large route files one stable place for shared Axum and domain imports during this refactor.
  Date/Author: 2026-03-28 / Codex

## Outcomes & Retrospective

The intended structural outcome landed. `crates/ingot-http-api/src/router/mod.rs` is now a 24-line module index and public surface, router state and assembly live in `crates/ingot-http-api/src/router/app.rs`, and the shared non-support imports moved to `crates/ingot-http-api/src/router/deps.rs`. The support tree is now a 7-line module declaration file, and callers import helpers from specific support modules instead of the old barrel.

The behavior stayed intact. `cargo check -p ingot-http-api` and `cargo test -p ingot-http-api` both passed after the refactor. A remaining small compromise is that a few local test modules inside `dispatch.rs`, `findings.rs`, and `convergence_port.rs` still use `use super::*`; the production router modules no longer do.

## Context and Orientation

The HTTP API router lives in `crates/ingot-http-api/src/router`. The top-level file, `crates/ingot-http-api/src/router/mod.rs`, currently declares the child modules, defines `AppState`, builds the Axum `Router`, applies the dispatch notification middleware, and exposes a shared import surface for many sibling modules. The support helpers live under `crates/ingot-http-api/src/router/support/`, with focused files like `errors.rs`, `normalize.rs`, `path.rs`, and `project_repo.rs`, but `crates/ingot-http-api/src/router/support/mod.rs` still re-exports a broad set of those helpers. Major feature modules such as `dispatch.rs`, `items/mod.rs`, and `findings.rs` then import `super::*` and `super::support::*`, which makes the module boundaries look cleaner than they really are.

For this plan, “prelude-style import” means a wildcard or broad re-export that hides where a type or function truly comes from. In this repository, that pattern shows up as route modules relying on `router/mod.rs` and `support/mod.rs` to pull in Axum types, domain types, helper functions, and error adapters that belong to more specific modules.

## Plan of Work

First, update the support tree so sibling modules can import focused helper modules directly. That means making the support child modules visible to the router parent and then changing feature modules to import the exact helper functions and types they use from `support::activity`, `support::config`, `support::errors`, `support::io`, `support::normalize`, `support::path`, and `support::project_repo`.

Second, replace `use super::*` across the route modules with explicit imports from Axum, domain crates, use cases, and local router modules. Keep the `routes()` builders and handler function names stable so the HTTP surface does not change. This step should let `crates/ingot-http-api/src/router/mod.rs` drop most of its current import list and remove the helper re-exports that only exist for sibling consumption.

Third, reduce `crates/ingot-http-api/src/router/support/mod.rs` to a thin module declaration file with no wide helper re-export surface except where a small targeted re-export is clearly justified. Then run formatting and tests, update this plan with the final observations, close `ingot-uud`, and land the session according to the repository workflow.

## Concrete Steps

From `/Users/aa/Documents/ingot`:

1. Edit `.agent/http-router-prelude-cleanup.md` as work progresses.
2. Edit `crates/ingot-http-api/src/router/support/mod.rs` so support submodules can be imported directly by sibling route modules.
3. Edit the affected route modules to replace wildcard imports with explicit imports.
4. Split `crates/ingot-http-api/src/router/mod.rs` into a thin module surface plus `app.rs` and `deps.rs`.
5. Run `cargo fmt --package ingot-http-api`.
6. Run `cargo test -p ingot-http-api`.
7. Update `bd`, commit, rebase if needed, `bd dolt push`, `git push`, and confirm `git status` reports the branch is up to date.

## Validation and Acceptance

Acceptance is:

1. `crates/ingot-http-api/src/router/mod.rs` is a thin module index instead of the old router assembly plus import-prelude file.
2. `crates/ingot-http-api/src/router/support/mod.rs` is a thin focused module declaration file.
3. Production route modules no longer use `super::*` or `super::support::*`; they use `deps::*` and focused support module imports.
4. `cargo test -p ingot-http-api` passes.

## Idempotence and Recovery

This refactor is source-only. If a route module fails to compile after import cleanup, rerun `cargo test -p ingot-http-api` and fix the missing import or visibility error in place. The changes are safe to repeat because they do not change migrations, persisted state, or endpoint behavior. If a direct support import turns out to require broader visibility than expected, widen the specific module or item only as far as `pub(super)` or `pub(crate)` rather than restoring a broad re-export surface.

## Artifacts and Notes

Initial evidence:

    $ rg -n "use super::\\*;|use super::support::\\*;" crates/ingot-http-api/src/router
    crates/ingot-http-api/src/router/jobs.rs:6:use super::support::*;
    crates/ingot-http-api/src/router/jobs.rs:8:use super::*;
    ...

    $ wc -l crates/ingot-http-api/src/router/mod.rs crates/ingot-http-api/src/router/support/mod.rs
         256 crates/ingot-http-api/src/router/mod.rs
          26 crates/ingot-http-api/src/router/support/mod.rs

Final evidence:

    $ wc -l crates/ingot-http-api/src/router/mod.rs crates/ingot-http-api/src/router/app.rs crates/ingot-http-api/src/router/deps.rs crates/ingot-http-api/src/router/support/mod.rs
          24 crates/ingot-http-api/src/router/mod.rs
         191 crates/ingot-http-api/src/router/app.rs
          45 crates/ingot-http-api/src/router/deps.rs
           7 crates/ingot-http-api/src/router/support/mod.rs

    $ rg -n "use super::\\*;|use super::support::\\*;" crates/ingot-http-api/src/router crates/ingot-http-api/src/demo
    crates/ingot-http-api/src/router/convergence_port.rs:736:    use super::*;
    crates/ingot-http-api/src/router/dispatch.rs:362:    use super::*;
    crates/ingot-http-api/src/router/findings.rs:467:    use super::*;

    $ cargo check -p ingot-http-api --message-format=short
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.54s

    $ cargo test -p ingot-http-api
    test result: ok. 17 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
    ...
    test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
    ...
    test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
    ...
    test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

## Interfaces and Dependencies

At the end of this work, the existing external router entry points must still exist:

    pub fn build_router(db: Database) -> Router
    pub fn build_router_with_project_locks(db: Database, project_locks: ProjectLocks) -> Router
    pub fn build_router_with_project_locks_and_state_root(
        db: Database,
        project_locks: ProjectLocks,
        state_root: PathBuf,
        dispatch_notify: DispatchNotify,
    ) -> Router

The support tree should expose focused sibling-importable modules under `crates/ingot-http-api/src/router/support/`, including:

    pub(super) mod activity;
    pub(super) mod config;
    pub(super) mod errors;
    pub(super) mod io;
    pub(super) mod normalize;
    pub(super) mod path;
    pub(super) mod project_repo;

Revision note: created this ExecPlan for the follow-up cleanup after the first router split left `router/mod.rs` and `router/support/mod.rs` acting as broad preludes.

Revision note: updated after implementation to record the new `app.rs` and `deps.rs` split, the `AppState` field visibility adjustment, the direct support imports from router and demo modules, and the passing `cargo check -p ingot-http-api` and `cargo test -p ingot-http-api` runs.
