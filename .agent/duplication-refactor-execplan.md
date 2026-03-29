# Collapse shared job, report, and test-fixture duplication

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with [.agent/PLANS.md](/Users/aa/Documents/ingot/.agent/PLANS.md).

## Purpose / Big Picture

After this refactor, the agent protocol, domain model, and shared test-support crate will each own one copy of the rules they are responsible for. Agent adapters will no longer carry a private commit-summary schema, report fixtures will always include the current required fields, SQLite mapping will reconstruct `JobState` and `ConvergenceState` through domain-owned helpers instead of restating the rules, and test crates will share one `PersistFixture` implementation instead of drifting apart. The change is observable by running the targeted Rust tests that currently depend on these duplicated contracts.

## Progress

- [x] 2026-03-29 09:06Z Claimed bd issue `ingot-ps2` for this refactor.
- [x] 2026-03-29 09:09Z Surveyed the duplication points in `ingot-agent-protocol`, `ingot-agent-adapters`, `ingot-test-support`, `ingot-domain`, `ingot-store-sqlite`, and `ingot-http-api`.
- [x] 2026-03-29 09:11Z Added shared helpers in protocol and domain crates.
- [x] 2026-03-29 09:12Z Repointed adapters, SQLite mapping, and test fixtures to the shared helpers.
- [x] 2026-03-29 09:12Z Removed duplicated `PersistFixture` implementations from `ingot-http-api` tests and deleted the store-local fixture module.
- [x] 2026-03-29 09:13Z Ran focused validation for `ingot-agent-adapters`, `ingot-domain`, `ingot-store-sqlite`, and selected `ingot-http-api` route tests.
- [x] 2026-03-29 09:13Z Filed follow-up bd issues `ingot-r0g`, `ingot-gom`, and `ingot-1je` for the deferred duplication.
- [ ] 2026-03-29 09:14Z Close `ingot-ps2`, commit, rebase, push bd/git, and verify the branch is fully published.

## Surprises & Discoveries

- Observation: `ingot-test-support` report fixtures already drifted from the protocol contract by omitting the required `extensions` field.
  Evidence: `crates/ingot-test-support/src/reports.rs` builds review and validation payloads without `extensions`, while `crates/ingot-agent-protocol/src/report.rs` requires it.

- Observation: `ingot-agent-adapters::result_from_text` wraps plain text into `{ "summary": ... }`, which does not satisfy the adapter’s own fallback schema because `validation` is required.
  Evidence: `crates/ingot-agent-adapters/src/lib.rs` had `structured_output_schema()` requiring both `summary` and `validation`, but `result_from_text()` returned only `summary`.

- Observation: `ingot-store-sqlite` unit tests cannot import `PersistFixture` impls from `ingot-test-support` directly because that dev-dependency brings in a distinct crate instance of `ingot-store-sqlite`, so `Database` types do not match.
  Evidence: the first `cargo test -p ingot-store-sqlite` run failed with `expected ingot_store_sqlite::db::Database, found db::Database` in `crates/ingot-store-sqlite/src/store/job/tests.rs`.

## Decision Log

- Decision: Scope this pass to the highest-value shared helpers and defer runtime test harness, target-ref normalization, and transparent-newtype macro work.
  Rationale: The selected refactors cross fewer ownership boundaries, fix an active contract drift, and can be validated with focused tests in one session without widening risk.
  Date/Author: 2026-03-29 / Codex

## Outcomes & Retrospective

This refactor shipped the highest-value shared helpers without widening into the lower-priority duplication. The protocol crate now owns the reusable report payload builders and the commit-summary fallback envelope, the domain crate owns lifecycle reconstruction for `JobState` and `ConvergenceState` plus `OutcomeClass::as_str()`, SQLite mapping delegates to those domain constructors, and HTTP tests now consume the shared `PersistFixture` implementation from `ingot-test-support`.

The one place that could not directly consume `ingot_test_support::sqlite::PersistFixture` was the `ingot-store-sqlite` crate’s own unit tests because of Rust crate identity during dev-dependency compilation. The practical fix was to delete the duplicated `store/test_fixtures.rs` module anyway and replace the two remaining in-crate uses with direct repository calls. That still removed the duplicated persistence implementation while respecting the crate-boundary constraint.

## Context and Orientation

`crates/ingot-agent-protocol/src/report.rs` defines the structured JSON contract for agent outputs. `crates/ingot-agent-adapters/src/lib.rs` should treat that crate as the source of truth when it needs a default schema or default result envelope. `crates/ingot-test-support/src/reports.rs` exists only to build test payloads and should therefore call protocol-owned helpers instead of recreating payloads by hand.

`crates/ingot-domain/src/job.rs` and `crates/ingot-domain/src/convergence.rs` define the lifecycle state machines for jobs and convergence records. Both files already reconstruct state from flattened wire formats for serde. `crates/ingot-store-sqlite/src/store/job/mapping.rs` and `crates/ingot-store-sqlite/src/store/convergence.rs` currently repeat that reconstruction logic when decoding SQLite rows. The refactor will expose domain-owned `from_parts` helpers so both serde and SQLite mapping consume the same rules.

`crates/ingot-test-support/src/sqlite.rs` already owns the reusable temp-DB and fixture persistence helpers. `crates/ingot-store-sqlite/src/store/test_fixtures.rs` and `crates/ingot-http-api/tests/common/mod.rs` copied that logic instead of importing it. Those local copies will be removed and all tests will use `ingot_test_support::sqlite::PersistFixture`.

## Plan of Work

In `crates/ingot-agent-protocol/src/report.rs`, add reusable payload constructors for commit summaries and report fixtures, keeping the JSON schema and example payload rules adjacent. In `crates/ingot-agent-adapters/src/lib.rs`, replace the private fallback schema with the protocol helper and make the plain-text fallback emit a valid commit-summary payload.

In `crates/ingot-domain/src/job.rs`, add a public `OutcomeClass::as_str()` and a public `JobState::from_parts(...)` API that accepts the flattened fields needed to rebuild a state from storage. Convert the serde `TryFrom<JobWire>` path to call the new helper. Mirror that pattern in `crates/ingot-domain/src/convergence.rs` with `ConvergenceState::from_parts(...)`.

In `crates/ingot-store-sqlite/src/store/job/mapping.rs` and `crates/ingot-store-sqlite/src/store/convergence.rs`, decode flat row fields, then call the domain-owned `from_parts(...)` helper and map any returned string error into `RepositoryError::Database`. In `crates/ingot-test-support/src/reports.rs` and `crates/ingot-test-support/src/sqlite.rs`, expose any missing reusable helpers needed by external tests and keep the default placeholder workspace creation logic there.

In `crates/ingot-http-api/tests/common/mod.rs`, remove the local `PersistFixture` trait and import the shared trait instead. In `crates/ingot-store-sqlite/src/store/job/tests.rs`, replace the two remaining in-crate persistence calls with direct repository methods so the duplicated `store/test_fixtures.rs` module can be deleted without crossing the self-dev-dependency boundary.

## Concrete Steps

From `/Users/aa/Documents/ingot`:

    bd update ingot-ps2 --claim --json
    cargo test -p ingot-agent-adapters
    cargo test -p ingot-domain
    cargo test -p ingot-store-sqlite
    cargo test -p ingot-http-api --test job_routes --test convergence_routes --test item_routes --test dispatch_routes

If a targeted crate test reveals fallout in another duplicated area that this plan intentionally deferred, create a follow-up bd issue with `--deps discovered-from:ingot-ps2` and leave the wider refactor out of this patch.

## Validation and Acceptance

Acceptance is:

1. `cargo test -p ingot-agent-adapters` passes, proving the adapter fallback schema and plain-text result envelope now match the shared protocol contract.
2. `cargo test -p ingot-domain` passes, proving serde reconstruction still works after introducing domain-owned `from_parts` helpers.
3. `cargo test -p ingot-store-sqlite` passes, proving SQLite decoding and crate-local test fixtures now use the shared helpers correctly.
4. The selected `ingot-http-api` route tests pass, proving external tests can import the shared fixture persistence helpers without behavior regressions.

## Idempotence and Recovery

All edits are source-only and safe to repeat. If a helper extraction causes a compile failure, restore the broken consumer by pointing it temporarily back at the old logic, then continue shrinking duplication one consumer at a time. Do not delete the old copy until the shared helper is compiling in at least one caller.

## Artifacts and Notes

Expected proof points after the refactor:

    crates/ingot-agent-adapters/src/lib.rs no longer contains a hand-written commit summary schema.
    crates/ingot-test-support/src/reports.rs always emits `extensions: null` for report payload fixtures.
    crates/ingot-store-sqlite/src/store/job/mapping.rs and crates/ingot-store-sqlite/src/store/convergence.rs call domain-owned reconstruction helpers.
    crates/ingot-http-api/tests/common/mod.rs no longer defines its own `PersistFixture` trait.

## Interfaces and Dependencies

In `crates/ingot-domain/src/job.rs`, define a public parts carrier and constructor with this shape:

    pub struct JobStateParts { ... }
    impl JobState {
        pub fn from_parts(status: JobStatus, parts: JobStateParts) -> Result<Self, String>;
    }
    impl OutcomeClass {
        pub fn as_str(self) -> &'static str;
    }

In `crates/ingot-domain/src/convergence.rs`, define:

    pub struct ConvergenceStateParts { ... }
    impl ConvergenceState {
        pub fn from_parts(status: ConvergenceStatus, parts: ConvergenceStateParts) -> Result<Self, String>;
    }

In `crates/ingot-agent-protocol/src/report.rs`, add protocol-owned payload helpers for commit summaries and report fixtures so test-support and adapters do not assemble those payloads independently.

Revision note: created this ExecPlan at the start of implementation to bound the session around the highest-value duplication only and to record the already-observed schema drift.

Revision note: updated after implementation and validation to record the crate-identity constraint on `ingot-store-sqlite` unit tests, the shipped helper extractions, and the follow-up bd issues for deferred duplication.
