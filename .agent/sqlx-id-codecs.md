# Add SQLX codecs for prefixed domain IDs

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with [.agent/PLANS.md](/Users/aa/Documents/ingot/.agent/PLANS.md).

## Purpose / Big Picture

After this change, the SQLite store can bind and read the repository’s prefixed identifier types such as `ItemId`, `JobId`, and `WorkspaceId` directly through SQLX instead of converting them to `String` on every write and parsing strings on every read. The visible proof is that `cargo check -p ingot-store-sqlite` and `cargo test -p ingot-store-sqlite` still pass while the store code loses most of its remaining `.to_string()` and `parse_id(...)` database plumbing.

## Progress

- [x] (2026-03-21 09:36Z) Created and claimed `bd` task `ingot-6wr` for the second SQLX cleanup pass.
- [x] (2026-03-21 09:36Z) Confirmed the workspace uses SQLX `0.8` and that `ingot-domain` already gates SQLX support behind the `sqlx` feature.
- [x] (2026-03-21 09:43Z) Implemented SQLX `Type`, `Encode`, and `Decode` support for the prefixed ID newtypes in `crates/ingot-domain/src/ids.rs`.
- [x] (2026-03-21 09:48Z) Replaced manual `.to_string()` and `parse_id(...)` usage in `crates/ingot-store-sqlite/src/store/` for prefixed ID columns and removed the now-dead `parse_id(...)` helper.
- [x] (2026-03-21 09:49Z) Validated with `cargo check -p ingot-store-sqlite`, `cargo test -p ingot-store-sqlite`, and a repository grep showing no remaining `parse_id(...)` calls in the store.

## Surprises & Discoveries

- Observation: SQLX enum support was already enough for the first pass, so the second pass can focus almost entirely on ID newtypes without revisiting the enum work.
  Evidence: `rg -n "to_string\\(|parse_id\\(" crates/ingot-store-sqlite/src/store -g '*.rs'` still reports dense ID conversions after the enum cleanup.

- Observation: SQLX `Encode` for SQLite can be implemented by delegating to the existing `String` codec, which keeps the custom ID code short and aligned with SQLX `0.8.6`.
  Evidence: local source inspection of `sqlx-sqlite-0.8.6/src/types/str.rs` shows `String` encoding by pushing `SqliteArgumentValue::Text(Cow::Owned(...))`.

- Observation: once IDs stop decoding through `String`, `map_job` needs explicit `Option<WorkspaceId>` and `Option<AgentId>` annotations because type inference no longer has the old `parse_id(...)` closure to anchor it.
  Evidence: `cargo check -p ingot-store-sqlite` initially failed with `type annotations needed` on `workspace_id` in `crates/ingot-store-sqlite/src/store/job.rs`.

## Decision Log

- Decision: Implement the ID codecs in `crates/ingot-domain/src/ids.rs` rather than creating store-local wrappers.
  Rationale: The IDs are domain types used across the repository, and the store already depends on `ingot-domain` with the `sqlx` feature enabled. Centralizing the codec logic keeps the SQL representation tied to the type definition.
  Date/Author: 2026-03-21 / Codex

- Decision: Keep the database representation as the existing prefixed text format like `itm_<uuid>` rather than switching to raw UUID storage.
  Rationale: The schema and persisted data already use prefixed text keys, and this pass is explicitly about removing manual conversions without changing on-disk format.
  Date/Author: 2026-03-21 / Codex

- Decision: Implement the SQLX support directly inside the `define_id!` macro instead of per-type hand-written impls.
  Rationale: Every prefixed ID type has the same storage contract and parse behavior, so putting the SQLX impls in the macro keeps the representations uniform and avoids copy-paste drift across eleven ID types.
  Date/Author: 2026-03-21 / Codex

## Outcomes & Retrospective

The second-pass goal was met. The domain ID newtypes now encode and decode directly through SQLX when `ingot-domain` is built with the `sqlx` feature, and the SQLite store no longer uses `parse_id(...)`. The remaining `.to_string()` calls in the store are intentional non-ID cases such as JSON serialization, string error messages, and test assertions.

This change did not require a schema migration because the stored representation stayed as the existing prefixed text form. The main lesson was that the domain type owns the database representation here; once that is true, the repository layer becomes much smaller and easier to read.

## Context and Orientation

The repository keeps its domain identifier types in `crates/ingot-domain/src/ids.rs`. Each ID is a small wrapper around `uuid::Uuid` with a stable human-readable prefix such as `itm`, `job`, or `wrk`. The wrapper’s `Display` implementation writes values in the prefixed text format that the SQLite schema stores in `TEXT` columns. The SQLite repository implementation lives in `crates/ingot-store-sqlite/src/store/`. After the enum cleanup, that store still converts ID values to `String` for binds and fetches `String` values that it then parses back into IDs with `parse_id(...)`.

In SQLX terms, `Type` declares which SQL column family a Rust type maps to, `Encode` describes how the Rust value is written into a SQL query argument buffer, and `Decode` describes how the SQL cell is converted back into the Rust value. Because these IDs are not transparent wrappers over the literal database payload, a derive-based transparent mapping is not enough. The codec must encode to the prefixed string and decode from that same prefixed string.

The key files for this work are `crates/ingot-domain/src/ids.rs`, where the ID macro lives; `crates/ingot-store-sqlite/src/store/helpers.rs`, which currently provides `parse_id(...)`; and the many store modules under `crates/ingot-store-sqlite/src/store/` that bind or read IDs.

## Plan of Work

First, extend the `define_id!` macro in `crates/ingot-domain/src/ids.rs` so that, when the `sqlx` feature is enabled, each generated ID type implements SQLX support for SQLite text columns. The implementation will advertise itself as compatible with SQLite `TEXT`, encode by formatting the existing prefixed string form, and decode by reading a string and feeding it through the type’s existing `FromStr` implementation.

Second, update the SQLite store modules so any query argument or row extraction that corresponds to an ID type uses direct `.bind(id)` or `row.try_get::<IdType, _>(...)` patterns. Optional IDs should become `.bind(option_id)` and `row.try_get::<Option<IdType>, _>(...)`. The remaining uses of `parse_id(...)` should disappear from the store implementation, and `parse_id(...)` can then be removed from `helpers.rs` if nothing else needs it.

Third, validate at the crate level with `cargo check -p ingot-store-sqlite` and `cargo test -p ingot-store-sqlite`. If the refactor leaves any intentional manual conversions behind, record them explicitly in this document so the next contributor does not have to rediscover them.

## Concrete Steps

From the repository root `/Users/aa/Documents/ingot`:

1. Update `crates/ingot-domain/src/ids.rs` so the ID macro emits SQLX trait implementations behind `#[cfg(feature = "sqlx")]`.
2. Search the store with:

       rg -n "to_string\\(|parse_id\\(" crates/ingot-store-sqlite/src/store -g '*.rs'

   and replace ID-specific database binds and fetches with direct SQLX ID usage.
3. Re-run:

       cargo check -p ingot-store-sqlite
       cargo test -p ingot-store-sqlite

4. Update this plan’s `Progress`, `Surprises & Discoveries`, and `Outcomes & Retrospective` sections with the actual results.

## Validation and Acceptance

Acceptance means all of the following are true:

1. `cargo check -p ingot-store-sqlite` succeeds.
2. `cargo test -p ingot-store-sqlite` succeeds.
3. `rg -n "parse_id\\(" crates/ingot-store-sqlite/src/store -g '*.rs'` returns no results.
4. The remaining `.to_string()` occurrences in `crates/ingot-store-sqlite/src/store/` are limited to non-ID cases such as JSON serialization, human-readable messages, or comparisons that still intentionally work with strings.

Completed evidence:

    cargo check -p ingot-store-sqlite
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.62s

    cargo test -p ingot-store-sqlite
    test result: ok. 19 passed; 0 failed

    rg -n "parse_id\\(" crates/ingot-store-sqlite/src/store -g '*.rs'
    # no output

## Idempotence and Recovery

This change is source-only and can be repeated safely. If a partial edit breaks compilation, the safe recovery path is to rerun `cargo check -p ingot-store-sqlite`, fix the next reported type mismatch, and continue until the store compiles cleanly. No schema migration or database rewrite is part of this plan, so there is no on-disk rollback concern.

## Artifacts and Notes

The relevant issue tracker entry for this work is `ingot-6wr`, titled `Add SQLX codecs for prefixed domain IDs`.

## Interfaces and Dependencies

In `crates/ingot-domain/src/ids.rs`, each generated ID type must continue to expose the current constructors, `Display`, `Serialize`, `Deserialize`, and `FromStr` behavior. With the `sqlx` feature enabled it must additionally support:

    impl sqlx::Type<sqlx::Sqlite> for <IdType> { ... }
    impl<'q> sqlx::Encode<'q, sqlx::Sqlite> for <IdType> { ... }
    impl<'r> sqlx::Decode<'r, sqlx::Sqlite> for <IdType> { ... }

The SQLite store must then use these ID types directly in query binds and row decodes instead of routing through `String`.

Revision note: created this plan at the start of implementation to satisfy the repository requirement that significant refactors use a maintained ExecPlan, and seeded it with the SQLX version check plus the claimed `bd` task context.
