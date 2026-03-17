# Move Pure Fixture Builders into `ingot-domain`

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows [`.agent/PLANS.md`](/Users/aa/Documents/ingot/.agent/PLANS.md).

## Purpose / Big Picture

After this change, `ingot-domain` unit tests will use local builder and timestamp helpers directly instead of depending on `ingot-test-support`. Other crates will keep using the existing `ingot_test_support::fixtures::*` API, but that API will become a thin re-export of `ingot-domain`’s feature-gated test-support module. The user-visible proof is that `cargo test -p ingot-domain` still passes, `cargo tree -p ingot-domain --edges normal,dev` no longer pulls `ingot-test-support`, and the workspace crates that already import `ingot_test_support::fixtures::*` still compile and test cleanly.

## Progress

- [x] (2026-03-16 21:49Z) Wrote the implementation plan and confirmed the target design: pure fixtures move into `ingot-domain`, exposed behind a `test-support` Cargo feature; `ingot-test-support` keeps `git`, `sqlite`, and `reports`.
- [x] (2026-03-16 21:56Z) Created `crates/ingot-domain/src/test_support/`, moved the pure fixture modules there, and exposed them from `ingot-domain` via `#[cfg(any(test, feature = "test-support"))]`.
- [x] (2026-03-16 21:57Z) Replaced `ingot-test-support::fixtures` with re-exports from `ingot_domain::test_support`, removed the duplicated local fixture source files, and pruned now-unused `chrono` and `tokio` dependencies from `ingot-test-support`.
- [x] (2026-03-16 21:58Z) Removed `ingot-domain`’s dev-dependency on `ingot-test-support`, switched the inline `ingot-domain` tests to `crate::test_support::*`, and deleted every temporary `bridge()` helper.
- [x] (2026-03-16 22:01Z) Ran formatting and validation: `cargo fmt --package ingot-domain --package ingot-test-support`, `cargo test -p ingot-domain`, `cargo test -p ingot-test-support`, `cargo check --workspace`, and `cargo test -p ingot-workflow -p ingot-usecases`.

## Surprises & Discoveries

- Observation: `ingot-domain` unit tests cannot use `ingot-test-support` builders directly because `ingot-test-support` compiles its own dependency copy of `ingot-domain`.
  Evidence: `cargo test -p ingot-domain` previously failed with mismatched-type errors between local `job::Job` and dependency `ingot_domain::job::Job`, which led to the temporary serde bridge workaround.
- Observation: `ingot-test-support` currently pulls in `ingot-store-sqlite`, `chrono`, `tokio`, `serde_json`, and `uuid`, but the non-fixture modules only need a subset after the fixture move.
  Evidence: `rg -n "chrono|serde_json|tokio|uuid" crates/ingot-test-support/src/{git.rs,lib.rs,reports.rs,sqlite.rs}` showed `serde_json` only in `reports.rs` and `uuid` only in `git.rs`.
- Observation: The feature-gated `ingot-domain::test_support` design still works for inline `ingot-domain` unit tests because `cfg(test)` exposes the module without enabling the Cargo feature.
  Evidence: `cargo test -p ingot-domain` passed after removing the `ingot-test-support` dev-dependency entirely.
- Observation: The downstream fixture API remained stable enough that a workspace compile pass and two high-usage crate test suites passed without changing external import sites.
  Evidence: `cargo check --workspace` passed, and `cargo test -p ingot-workflow -p ingot-usecases` passed after the re-export switch.

## Decision Log

- Decision: Use a feature-gated public module in `ingot-domain` named `test_support`.
  Rationale: The user explicitly chose feature-gated exposure over an always-on public module. `cfg(test)` will still make the module visible to `ingot-domain`’s own unit tests without turning it into unconditional API surface.
  Date/Author: 2026-03-16 / Codex
- Decision: Move the entire pure fixture family, not only the five builders from the first patch.
  Rationale: Leaving some builders in `ingot-test-support` would create two sources of truth for domain fixtures and keep the architecture muddled.
  Date/Author: 2026-03-16 / Codex
- Decision: Keep the downstream `ingot_test_support::fixtures::*` API fully intact and satisfy the refactor entirely behind that facade.
  Rationale: This avoids noisy changes across the workspace and keeps the change focused on ownership and dependency direction rather than consumer churn.
  Date/Author: 2026-03-16 / Codex

## Outcomes & Retrospective

The refactor achieved the intended end state. `ingot-domain` now owns the pure fixture builders and deterministic timestamp helpers in `crates/ingot-domain/src/test_support/`, exposed behind the `test-support` feature and `cfg(test)`. `ingot-test-support` now re-exports that API from `src/fixtures/mod.rs` while continuing to own the heavier `git`, `reports`, and `sqlite` helpers.

The original low-severity issues are resolved. `ingot-domain` no longer has a dev-dependency on `ingot-test-support`, `cargo tree -p ingot-domain --edges normal,dev` is back to a light graph, and `rg -n "fn bridge<|serialize bridge|deserialize bridge" crates/ingot-domain/src -g '*.rs'` returns no matches. The broader workspace still compiles and the two highest-value pure Rust consumers (`ingot-workflow` and `ingot-usecases`) still pass their test suites.

## Context and Orientation

`crates/ingot-test-support` currently exposes four helper families from `src/lib.rs`: `fixtures`, `git`, `reports`, and `sqlite`. The `fixtures` family is implemented in `crates/ingot-test-support/src/fixtures/` and contains only in-memory builders and deterministic timestamp helpers for `ingot-domain` types. The heavy helpers live elsewhere: `git.rs` shells out to Git and uses `uuid`, `reports.rs` builds JSON payloads, and `sqlite.rs` depends on `ingot-store-sqlite`.

`crates/ingot-domain` is the core type crate. Inline unit tests in files such as `crates/ingot-domain/src/job.rs`, `crates/ingot-domain/src/convergence.rs`, `crates/ingot-domain/src/finding.rs`, `crates/ingot-domain/src/workspace.rs`, and `crates/ingot-domain/src/git_operation.rs` currently import `ingot_test_support::fixtures::*` and use temporary serde bridge helpers because the builders come from a separate compiled copy of `ingot-domain`. The goal of this refactor is to make those builders local to `ingot-domain` while preserving the existing `ingot_test_support::fixtures::*` facade for all other crates.

## Plan of Work

Create a new module tree under `crates/ingot-domain/src/test_support/` that mirrors the current fixture layout from `crates/ingot-test-support/src/fixtures/`. Move the builder implementations, timestamp helpers, and `nil_item` / `nil_revision` into that module with minimal code changes so their defaults and method names remain stable. Update `crates/ingot-domain/src/lib.rs` to expose `pub mod test_support` behind `#[cfg(any(test, feature = "test-support"))]`, and add a `test-support` feature to `crates/ingot-domain/Cargo.toml`.

Then simplify `crates/ingot-test-support`: change its `Cargo.toml` so the `ingot-domain` dependency enables the `test-support` feature, rewrite `src/fixtures/mod.rs` as re-exports from `ingot_domain::test_support`, delete the old local fixture source files, and remove now-unused dependencies that were only required by those files.

Finally, clean up `ingot-domain` itself by removing the `ingot-test-support` dev-dependency, updating inline unit tests to import from `crate::test_support`, and replacing the temporary serde bridge helpers with direct local builder use. Preserve the deterministic `default_timestamp()` calls that already replaced `Utc::now()`.

## Concrete Steps

From the repository root `/Users/aa/Documents/ingot`, make the refactor in this order:

1. Add the `test-support` feature to `crates/ingot-domain/Cargo.toml` and expose `test_support` from `crates/ingot-domain/src/lib.rs`.
2. Create `crates/ingot-domain/src/test_support/` with modules matching the current fixture set.
3. Point `crates/ingot-test-support/src/fixtures/mod.rs` at `ingot_domain::test_support::*`, then remove the duplicated local implementations.
4. Update `crates/ingot-domain` unit tests to use `crate::test_support::*` and delete the local `bridge()` helpers.
5. Run `cargo fmt --package ingot-domain --package ingot-test-support`.
6. Run the validation commands in the order listed below.

Expected command outcomes:

    cargo test -p ingot-domain
    # expect all ingot-domain unit tests to pass with no type-mismatch errors

    cargo test -p ingot-test-support
    # expect the support crate to compile and its own tests, if any, to pass

    cargo check --workspace
    # expect downstream crates that still import ingot_test_support::fixtures::* to compile unchanged

## Validation and Acceptance

Acceptance is complete when all of the following are true:

`cargo test -p ingot-domain` passes and there are no `bridge()` helpers or serde round-trip conversion helpers left in `crates/ingot-domain/src/`.

`cargo tree -p ingot-domain --edges normal,dev` no longer shows `ingot-test-support` in `ingot-domain`’s dev dependency graph.

`cargo check --workspace` passes without changing downstream import sites that use `ingot_test_support::fixtures::*`.

`ingot-test-support` still exports the same fixture names and helper functions it exported before this refactor.

## Idempotence and Recovery

This refactor is additive until the old fixture modules are removed from `ingot-test-support`. If validation fails after the new `ingot-domain::test_support` module is added, keep the moved code in place and repair the re-export wiring rather than trying to reintroduce the bridge helpers. If dependency cleanup in `ingot-test-support/Cargo.toml` proves too aggressive, re-add only the missing dependency and rerun the targeted crate tests before widening to the workspace.

## Artifacts and Notes

Key pre-change evidence:

    cargo tree -p ingot-domain --edges normal,dev
    ...
    [dev-dependencies]
    └── ingot-test-support
        ├── ingot-domain
        ├── ingot-store-sqlite
        ├── chrono
        ├── serde_json
        ├── tokio
        └── uuid

## Interfaces and Dependencies

At the end of this refactor, `crates/ingot-domain/src/lib.rs` must contain:

    #[cfg(any(test, feature = "test-support"))]
    pub mod test_support;

`crates/ingot-domain/Cargo.toml` must define a feature named `test-support`.

`crates/ingot-test-support/src/fixtures/mod.rs` must re-export the exact fixture API from `ingot_domain::test_support`, including builder types, timestamp helpers, `nil_item()`, and `nil_revision()`.

Revision note: created this ExecPlan at implementation start to record the migration steps and the known temporary bridge workaround that this refactor is meant to remove.

Revision note: updated at implementation completion with the actual command results, the dependency-pruning outcome, and confirmation that the bridge workaround was fully removed.
