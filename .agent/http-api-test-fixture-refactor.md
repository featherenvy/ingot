# Refactor ingot-http-api tests onto builder-backed fixtures

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows [.agent/PLANS.md](/Users/aa/Documents/ingot/.agent/PLANS.md) and must be maintained in accordance with that file.

## Purpose / Big Picture

`crates/ingot-http-api` currently has the noisiest backend test setup in the workspace. Many route tests seed `projects`, `items`, `item_revisions`, `workspaces`, `convergences`, and `findings` with hand-written `sqlx::query("INSERT INTO ...")` blocks or manual domain struct literals. After this refactor, the HTTP API tests should build persisted state through `ingot-test-support` builders plus small crate-local persistence helpers, so a contributor can read a route test as domain intent instead of SQL boilerplate. The proof is that the targeted HTTP API route tests and router unit tests still pass while the raw insert and manual literal count in the touched files drops sharply.

## Progress

- [x] (2026-03-16 19:59Z) Audited `crates/ingot-http-api/tests/common/mod.rs`, the targeted route tests, and the router unit tests to identify which raw inserts can be replaced by builder-backed persistence without losing exact test control.
- [x] (2026-03-16 20:03Z) Added a local builder-backed persistence layer in `crates/ingot-http-api/tests/common/mod.rs`, including typed builder entry points, a local `PersistFixture` trait, convenience persistence helpers, and a `JobBuilder`-backed `insert_test_job_row()`.
- [x] (2026-03-16 20:12Z) Migrated the targeted route tests in `crates/ingot-http-api/tests/finding_routes.rs`, `crates/ingot-http-api/tests/convergence_routes.rs`, `crates/ingot-http-api/tests/job_routes.rs`, `crates/ingot-http-api/tests/dispatch_routes.rs`, `crates/ingot-http-api/tests/item_routes.rs`, and `crates/ingot-http-api/tests/workspace_routes.rs` away from `INSERT INTO` setup for the targeted entities.
- [x] (2026-03-16 20:14Z) Replaced the targeted manual `Project` and `Workspace` literals in `crates/ingot-http-api/src/router/convergence.rs`, `crates/ingot-http-api/src/router/dispatch.rs`, and `crates/ingot-http-api/src/router/test_helpers.rs` with builder-backed setup.
- [x] (2026-03-16 20:15Z) Ran focused `cargo test -p ingot-http-api` coverage for the touched route files and router unit tests; all targeted suites passed.

## Surprises & Discoveries

- Observation: `crates/ingot-store-sqlite/src/store/test_fixtures.rs` already contains the exact persistence pattern this refactor wants, including auto-creating a workspace when a persisted `Job` references one.
  Evidence: the crate-local `PersistFixture for Job` implementation creates a builder-compatible workspace before calling `db.create_job(&self)`.
- Observation: `WorkspaceBuilder` already covers more of the HTTP route test needs than the skill summary implied, including `retention_policy`, `no_target_ref`, `no_workspace_ref`, `current_job_id`, `status`, `base_commit_oid`, and `head_commit_oid`.
  Evidence: `crates/ingot-test-support/src/fixtures/workspace.rs` exposes setters for those fields, so only cases like `parent_workspace_id` or custom `target_ref` still need post-build mutation.

## Decision Log

- Decision: keep the new persistence helpers local to `crates/ingot-http-api/tests/common/mod.rs` instead of expanding `ingot-test-support` again during this pass.
  Rationale: the skill guidance and the existing store test fixture pattern both point toward crate-local persistence wrappers when the crate already has exact route-test needs and stringly typed IDs.
  Date/Author: 2026-03-16 / Codex

- Decision: preserve the existing `TestJobInsert` surface for route tests, but implement it through `JobBuilder` plus post-build mutation instead of assembling `Job` manually.
  Rationale: this keeps the route tests stable while still moving the typed job construction onto the shared fixture layer.
  Date/Author: 2026-03-16 / Codex

## Outcomes & Retrospective

The refactor achieved its intended goal: the targeted HTTP API tests now read as domain setup rather than SQL row authoring, while still preserving exact IDs where route assertions need them. The common route harness now owns the builder-backed persistence surface, the targeted route files no longer use `INSERT INTO` setup for the core persisted entities, and the targeted router unit tests no longer open-code the listed `Project` and `Workspace` literals.

The main subtlety was foreign-key ordering in the finding retriage test. The old SQL flow inserted a finding before its backlog-linked item existed and then patched state with `UPDATE`; the builder-backed flow had to express the same final condition with a different, but still valid, ordering. Once that was corrected, the focused route and router suites passed cleanly.

## Context and Orientation

`crates/ingot-http-api/tests/common/mod.rs` is the shared harness for route integration tests. It already exposes temp repo and migrated SQLite helpers from `ingot-test-support`, plus a `TestJobInsert` helper that turns a compact test row into a persisted `Job`. The gap is that project, item, revision, workspace, convergence, and finding setup still happens in each test through open-coded SQL.

The domain builders live in `crates/ingot-test-support/src/fixtures/`. They build typed `Project`, `Item`, `ItemRevision`, `Workspace`, `Job`, `Convergence`, and `Finding` values with deterministic timestamps and domain-valid defaults. `ingot-store-sqlite::Database` already exposes `create_project`, `create_item_with_revision`, `create_revision`, `create_workspace`, `create_convergence`, `create_finding`, and `create_job`, which means route tests can seed persisted state without writing SQL for those entities.

The router unit tests in `crates/ingot-http-api/src/router/dispatch.rs` and `crates/ingot-http-api/src/router/convergence.rs` are not HTTP integration tests, but they still create manual `Project` and `Workspace` values in test code. Those literals should be replaced with builders or small helper constructors where possible so the test setup matches the rest of the workspace style.

## Plan of Work

First, extend `crates/ingot-http-api/tests/common/mod.rs`. Import the shared builders from `ingot_test_support::fixtures` and add a local `PersistFixture` trait mirroring the pattern already used inside `ingot-store-sqlite`. Implement it for `Project`, `(Item, ItemRevision)`, `Item`, `ItemRevision`, `Workspace`, `Job`, `Convergence`, and `Finding`. The `Job` implementation must keep the current convenience behavior of auto-creating a workspace when the job state points at one that does not yet exist.

Next, add small builder entry-point helpers that accept the route tests’ fixed string IDs and return builders with parsed IDs already applied. The target names are `test_project_builder`, `test_item_builder`, `test_revision_builder`, `test_workspace_builder`, `test_convergence_builder`, and `test_finding_builder`. These helpers should let the route tests stay concise while preserving exact IDs for response assertions.

Then update `seeded_route_test_app()` and `insert_test_job_row()` to consume that builder-backed persistence layer. `insert_test_job_row()` should build `Job` values with `JobBuilder`, mutate any fields the builder does not expose, and then persist through the new local trait so tests keep their current `TestJobInsert` API.

After the shared layer is stable, convert the targeted route files away from raw inserts. Each test should express the entities it needs through builders, override only the behavior-relevant fields such as approval state, escalation, target ref, retention policy, convergence status, or finding triage, and then persist through the local helper trait. Only leave direct SQL where the test is explicitly asserting on a low-level mutation like a `DELETE`, `UPDATE`, or aggregate query result.

Finally, replace the manual `Project` and `Workspace` literals in router unit tests with `ProjectBuilder` and `WorkspaceBuilder`, plus post-build mutation for any remaining fields the shared builders do not expose directly, and run focused `cargo test -p ingot-http-api` commands over the touched tests.

## Concrete Steps

Run from `/Users/aa/Documents/ingot`:

    cargo test -p ingot-http-api finding_routes -- --nocapture
    cargo test -p ingot-http-api convergence_routes -- --nocapture
    cargo test -p ingot-http-api job_routes -- --nocapture
    cargo test -p ingot-http-api dispatch_routes -- --nocapture
    cargo test -p ingot-http-api item_routes -- --nocapture
    cargo test -p ingot-http-api workspace_routes -- --nocapture
    cargo test -p ingot-http-api router::dispatch -- --nocapture
    cargo test -p ingot-http-api router::convergence -- --nocapture

Expected signs of success are the usual Rust test summaries:

    test result: ok. ... passed; 0 failed

If a route file still contains raw inserts after conversion, use `rg -n 'sqlx::query\\(\"INSERT INTO' crates/ingot-http-api/tests` to confirm whether the remaining SQL is in or out of scope for this pass before moving on.

## Validation and Acceptance

Acceptance means:

1. The targeted route tests seed their main persisted entities through builders and `db.create_*` calls rather than `INSERT INTO` SQL.
2. `crates/ingot-http-api/tests/common/mod.rs` becomes the single place where builder-backed persistence helpers for route tests live.
3. The targeted router unit tests no longer open-code the listed `Project` and `Workspace` literals.
4. Focused `cargo test -p ingot-http-api` runs over the touched files pass.

## Idempotence and Recovery

This refactor is source-only. Re-running the tests is safe because every helper uses a unique temp repo or temp database path. If a conversion turns out to require exact low-level SQL semantics for one edge case, keep that one query local and continue migrating the surrounding setup; the acceptance bar is to remove boilerplate setup, not to ban every SQL statement in tests.

## Artifacts and Notes

Implementation notes to preserve while editing:

    crates/ingot-store-sqlite/src/store/test_fixtures.rs

is the reference for the local `PersistFixture` pattern, especially the `Job` implementation that auto-creates a workspace when needed.

    crates/ingot-http-api/tests/common/mod.rs

already centralizes `TestJobInsert` and should remain the only route-test harness file after this refactor.

Focused verification that passed from `/Users/aa/Documents/ingot`:

    cargo test -p ingot-http-api --test finding_routes --test workspace_routes
    cargo test -p ingot-http-api --test convergence_routes
    cargo test -p ingot-http-api --test item_routes --test dispatch_routes --test job_routes
    cargo test -p ingot-http-api router::dispatch -- --nocapture
    cargo test -p ingot-http-api router::convergence -- --nocapture

## Interfaces and Dependencies

In `crates/ingot-http-api/tests/common/mod.rs`, define a local async trait:

    trait PersistFixture: Sized {
        async fn persist(self, db: &Database) -> Result<Self, RepositoryError>;
    }

and implement it for:

    Project
    Item
    ItemRevision
    (Item, ItemRevision)
    Workspace
    Job
    Convergence
    Finding

Also expose helper constructors named:

    test_project_builder(...)
    test_item_builder(...)
    test_revision_builder(...)
    test_workspace_builder(...)
    test_convergence_builder(...)
    test_finding_builder(...)

They should accept the string IDs already used throughout the HTTP route tests and return the corresponding `ingot-test-support` builder with parsed typed IDs applied.

Revision note: Created this plan after auditing the targeted HTTP API route and router tests so the ongoing implementation can be tracked against `.agent/PLANS.md`.

Revision note: Updated after implementation to record the completed helper layer, the migrated route and router tests, and the focused passing verification commands.
