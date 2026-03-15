# Introduce a narrow shared test-support crate

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows [.agent/PLANS.md](/Users/aa/Documents/ingot/.agent/PLANS.md) and must be maintained in accordance with that file.

## Purpose / Big Picture

The repository now has typed `JobInput` fixtures and more complex late-bound authoring behavior, but test setup is still duplicated across crates. After this change, backend tests will share one small crate for temporary Git repositories, migrated SQLite databases, typed domain builders, and canned report payloads. A contributor should be able to run `cargo test -p ingot-agent-runtime`, `cargo test -p ingot-http-api`, and `cargo test -p ingot-workspace` and see those crates using the same low-level fixture layer instead of each rebuilding it locally.

## Progress

- [x] (2026-03-15 10:20Z) Audited the duplicated helper surface in `crates/ingot-agent-runtime/tests/integration/helpers.rs`, `crates/ingot-http-api/src/router.rs`, `crates/ingot-workspace/src/lib.rs`, and `crates/ingot-store-sqlite/src/db.rs`.
- [x] (2026-03-15 11:05Z) Added `crates/ingot-test-support` with temp repo/DB helpers, typed builders, and report payload helpers.
- [x] (2026-03-15 11:20Z) Migrated shared low-level helpers in `ingot-agent-runtime`, `ingot-http-api`, `ingot-workspace`, and `ingot-git` to the new crate while keeping crate-specific harness behavior local.
- [x] (2026-03-15 11:35Z) Ran focused crate tests plus `make test`; the backend test gate passed.

## Surprises & Discoveries

- Observation: `ingot-store-sqlite` cannot practically consume a shared helper crate that depends on `ingot-store-sqlite::Database`, because that would create a dependency cycle.
  Evidence: the desired shared migrated-DB helper necessarily depends on `ingot-store-sqlite`, so `ingot-store-sqlite` itself must keep its local `temp_db_path()` test helper.

## Decision Log

- Decision: keep the shared crate narrow and dev-focused, and do not move route assertions or runtime harness orchestration into it.
  Rationale: the duplicated pain is in setup and typed fixtures, while the route/runtime harness code is crate-specific and easier to read when local.
  Date/Author: 2026-03-15 / Codex

- Decision: allow `ingot-store-sqlite` to keep its local DB-path helper instead of forcing every crate onto the shared crate.
  Rationale: a shared migrated-database helper needs `ingot-store-sqlite` as a normal dependency, which makes it unusable from `ingot-store-sqlite` tests without a cycle.
  Date/Author: 2026-03-15 / Codex

## Outcomes & Retrospective

The new shared crate now carries the low-level fixture layer that had been duplicated across backend tests. `ingot-agent-runtime` reuses the shared temp repo/database helpers, typed builders, and canonical report payloads. `ingot-http-api`, `ingot-workspace`, and `ingot-git` reuse the shared Git helpers, while route-specific insert helpers and runtime harness orchestration remain local as intended.

The boundary worked as planned: the shared crate reduced duplication without becoming a generic testing framework. The only notable exception is `ingot-store-sqlite`, which still keeps its own local temp DB helper because the shared migrated-database helper depends on `ingot-store-sqlite` itself.

## Context and Orientation

This repository is a Rust workspace. Low-level backend tests currently create temporary Git repositories by hand, connect to temporary SQLite files by hand, and assemble `Project`, `Item`, `ItemRevision`, `Workspace`, and `Job` structs inline.

The main duplicated locations are:

- `crates/ingot-agent-runtime/tests/integration/helpers.rs`, which contains a runtime-specific harness plus generic entity builders and temp repo/database helpers.
- `crates/ingot-http-api/src/router.rs` test module, which contains route tests, temporary repo setup, ad hoc test jobs, and a local `insert_test_job_row` helper.
- `crates/ingot-workspace/src/lib.rs` tests, which contain another temporary Git repository helper.

The new crate will be `crates/ingot-test-support`. It will be a normal library crate that other crates use only from `[dev-dependencies]`. It will export small utility modules:

- `git`: create temp repositories and run Git commands.
- `sqlite`: create a migrated temp `ingot_store_sqlite::Database`.
- `fixtures`: typed builders for `Project`, `Item`, `ItemRevision`, `Workspace`, and `Job`.
- `reports`: canonical JSON payload builders used by fake runners in tests.

The route-specific helper `insert_test_job_row` stays in `crates/ingot-http-api/src/router.rs`, but it should consume the shared builders instead of rebuilding typed `Job` setup from scratch.

## Plan of Work

First create `crates/ingot-test-support` and register it in the workspace. Its `Cargo.toml` should depend on `ingot-domain`, `ingot-store-sqlite`, `chrono`, `serde_json`, `tokio`, and `uuid`. The crate should expose a thin `lib.rs` that re-exports the modules.

In `crates/ingot-test-support/src/git.rs`, implement `unique_temp_path`, `run_git`, `git_output`, `write_file`, and `temp_git_repo`. The repo helper should create a repository with `main`, configure a local author identity, and commit an initial `tracked.txt` file so callers can immediately resolve `HEAD`.

In `crates/ingot-test-support/src/sqlite.rs`, implement `temp_db_path` and `migrated_test_db`. `migrated_test_db` must create a unique temp file, connect with `ingot_store_sqlite::Database::connect`, run `migrate`, and return the ready database.

In `crates/ingot-test-support/src/fixtures.rs`, implement small builders for `Project`, `Item`, `ItemRevision`, `Workspace`, and `Job`. The builders should produce valid domain objects by default and expose only the mutators needed by current tests, such as setting ids, timestamps, `seed_commit_oid`, `job_input`, `workspace_kind`, `output_artifact_kind`, and `workspace_ref`. The builders should default to deterministic timestamps by using a shared `parse_timestamp` helper.

In `crates/ingot-test-support/src/reports.rs`, add canonical JSON builders for the fake review and validation reports used by runtime tests. These should cover the current clean-review, review-with-findings, and clean-validation shapes so runtime test runners can reuse them instead of rebuilding JSON blobs inline.

After the crate exists, update `crates/ingot-agent-runtime/Cargo.toml`, `crates/ingot-http-api/Cargo.toml`, and `crates/ingot-workspace/Cargo.toml` to add `ingot-test-support` as a dev-dependency. Then migrate the generic setup code in those crates to the new helper crate:

- `crates/ingot-agent-runtime/tests/integration/helpers.rs` should keep `TestHarness` and runtime-only orchestration local, but use the shared temp repo/database helpers, builders, and report payload builders.
- `crates/ingot-http-api/src/router.rs` tests should use the shared temp repo and migrated DB helpers. The local `TestJobInsert` helper should stay, but it should build jobs through the shared `JobBuilder`.
- `crates/ingot-workspace/src/lib.rs` tests should use the shared temp repo and Git command helpers.

`crates/ingot-store-sqlite/src/db.rs` should be left alone except for any incidental cleanup, because using the new shared database helper there would create a dependency cycle.

## Concrete Steps

From `/Users/aa/Documents/ingot`:

    cargo test -p ingot-test-support
    cargo test -p ingot-workspace
    cargo test -p ingot-http-api --no-run
    cargo test -p ingot-agent-runtime --no-run
    make test

Expected signs of success:

    test result: ok. ... passed; 0 failed
    Finished `test` profile ...

If `make test` exposes additional local helper duplication or import breakage, patch the affected test module to use the shared crate and rerun the targeted crate before rerunning `make test`.

## Validation and Acceptance

Acceptance is:

1. `crates/ingot-test-support` exists and has unit tests covering at least temp repo creation and one typed builder.
2. `ingot-agent-runtime`, `ingot-http-api`, and `ingot-workspace` tests compile and run using the shared helper crate.
3. The local helper surface shrinks in those crates, while route-specific insert/assertion helpers remain local.
4. `make test` passes from the repository root.

## Idempotence and Recovery

The temp repo and temp DB helpers only write into unique paths under the system temp directory, so rerunning tests is safe. If a test run leaves temp files behind, they are disposable and do not affect later runs because the paths are unique. If a migration or compile step fails halfway through, rerun the same command after fixing the compile error; no persistent project data is mutated.

## Artifacts and Notes

Validation that was run from `/Users/aa/Documents/ingot`:

    cargo fmt
    cargo test -p ingot-test-support
    cargo test -p ingot-workspace
    cargo test -p ingot-git ensure_mirror_preserves_daemon_refs_while_pruning_checkout_refs -- --nocapture
    cargo test -p ingot-http-api target_head_valid_tracks_ref_movement -- --nocapture
    cargo test -p ingot-agent-runtime authoring_success_auto_dispatches_incremental_review --test integration -- --nocapture
    make test

Observed result:

    test result: ok. ... passed; 0 failed

Update 2026-03-15: implemented the shared crate, migrated the targeted crates, and recorded the `ingot-store-sqlite` dependency-cycle exception after the full backend test gate passed.

## Interfaces and Dependencies

`crates/ingot-test-support/src/lib.rs` must publicly expose:

    pub mod fixtures;
    pub mod git;
    pub mod reports;
    pub mod sqlite;

`crates/ingot-test-support/src/sqlite.rs` must define:

    pub async fn migrated_test_db(prefix: &str) -> ingot_store_sqlite::Database

`crates/ingot-test-support/src/git.rs` must define:

    pub fn temp_git_repo(prefix: &str) -> std::path::PathBuf
    pub fn run_git(path: &std::path::Path, args: &[&str])
    pub fn git_output(path: &std::path::Path, args: &[&str]) -> String

`crates/ingot-test-support/src/fixtures.rs` must define builder types for:

    ProjectBuilder
    ItemBuilder
    RevisionBuilder
    WorkspaceBuilder
    JobBuilder

When this plan is updated, append a note below describing what changed and why.
