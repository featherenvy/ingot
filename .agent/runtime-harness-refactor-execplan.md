# Share runtime test harness and adapter metadata fixtures

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with [.agent/PLANS.md](/Users/aa/Documents/ingot/.agent/PLANS.md).

## Purpose / Big Picture

After this refactor, runtime unit tests and integration tests will stop carrying separate copies of the same harness setup and agent-capability fixtures. The observable result is that runtime tests still pass, but `TestHarness`, `BlockingRunner`, and the default agent profiles are defined in one place, reducing drift like the previously inconsistent mutating-agent capabilities between `src/tests.rs` and `tests/common/mod.rs`.

## Progress

- [x] 2026-03-29 09:17Z Claimed bd issue `ingot-r0g`.
- [x] 2026-03-29 09:20Z Surveyed overlap between `crates/ingot-agent-runtime/src/tests.rs` and `crates/ingot-agent-runtime/tests/common/mod.rs`.
- [x] 2026-03-29 09:24Z Extracted `TestHarness`, `BlockingRunner`, and `TestAgentProfile` into `crates/ingot-agent-runtime/tests/common/shared_harness.rs`.
- [x] 2026-03-29 09:25Z Rewired `src/tests.rs`, `tests/common/mod.rs`, `tests/dispatch.rs`, and `tests/auto_dispatch.rs` to use the shared harness and shared agent fixtures.
- [x] 2026-03-29 09:28Z Ran focused runtime validation for the library tests and the `dispatch`, `auto_dispatch`, `reconciliation`, and `convergence` integration suites.
- [ ] 2026-03-29 09:28Z Close `ingot-r0g`, commit, rebase, push bd/git, and verify the branch is published.

## Surprises & Discoveries

- Observation: `register_mutating_agent` already drifted between the two harness copies. The unit-test version registered `ReadOnlyJobs` in addition to mutating capabilities, while the integration-test version did not.
  Evidence: `crates/ingot-agent-runtime/src/tests.rs` used `MutatingJobs + ReadOnlyJobs + StructuredOutput`, while `crates/ingot-agent-runtime/tests/common/mod.rs` used `MutatingJobs + StructuredOutput`.

- Observation: The include-file approach works cleanly when the shared file imports runtime types through a wrapper alias, but integration support code outside the extracted harness still needed its own `ids` and `Project` imports.
  Evidence: the first integration-test compile failed in `crates/ingot-agent-runtime/tests/common/mod.rs` until those imports were restored for helper functions outside the shared harness.

## Decision Log

- Decision: Reuse one shared include file under `crates/ingot-agent-runtime/tests/common/` instead of adding a new public library module.
  Rationale: The shared code is test-only. Using `include!` with a crate-alias wrapper lets unit tests and integration tests compile the same source without exposing test harness APIs in the production crate surface.
  Date/Author: 2026-03-29 / Codex

## Outcomes & Retrospective

This refactor removed the duplicated runtime harness definitions without adding any new production API. `shared_harness.rs` now owns the common `TestHarness`, `BlockingRunner`, and the `TestAgentProfile` capability table. The unit tests in `src/tests.rs` and the integration helpers in `tests/common/mod.rs` both compile that same file through a crate-alias wrapper.

The shared agent-profile helper also replaced several hand-written capability bundles in `dispatch` and `auto_dispatch` tests. That resolved the most obvious metadata drift while keeping the change set narrowly scoped to tests that matched one of the shared profiles.

## Context and Orientation

`crates/ingot-agent-runtime/src/tests.rs` contains unit tests for internal runtime behavior. It had a private `TestRuntimeHarness` and private `BlockingRunner`. `crates/ingot-agent-runtime/tests/common/mod.rs` is compiled by multiple integration-test binaries and had a second `TestHarness` plus another `BlockingRunner`. Both harnesses create a temp git repo, migrate a temp SQLite database, create a `JobDispatcher`, create a project row, and expose helper methods for waiting on job status. Both also hard-code common agent capability sets.

The agent metadata duplication matters because runtime dispatch behavior depends on declared capabilities such as `MutatingJobs`, `ReadOnlyJobs`, and `StructuredOutput`. If tests disagree about the default capability bundles for “mutating”, “review-only”, or “full” agents, they can accidentally validate different behavior.

## Plan of Work

Create a shared test-only source file at `crates/ingot-agent-runtime/tests/common/shared_harness.rs` that defines `TestHarness`, `BlockingRunner`, and a small `TestAgentProfile` enum plus an `agent_fixture(...)` helper. The file will import runtime types through a wrapper alias named `runtime_crate`, so it can be included from both unit tests and integration tests.

In `crates/ingot-agent-runtime/tests/common/mod.rs`, replace the local harness and blocking-runner definitions with a wrapper module that aliases `ingot_agent_runtime` to `runtime_crate` and then includes the shared file. Re-export the shared helpers from `common` so existing integration tests can keep using `use common::*;`.

In `crates/ingot-agent-runtime/src/tests.rs`, add a similar wrapper module that aliases `crate` to `runtime_crate`, includes the shared file, and imports `TestHarness` as `TestRuntimeHarness`. Remove the now-duplicated local harness and blocking runner. Update the remaining obvious manual review-agent registration to call the shared helper method.

In selected integration tests such as `crates/ingot-agent-runtime/tests/dispatch.rs` and `crates/ingot-agent-runtime/tests/auto_dispatch.rs`, replace hand-written `AgentBuilder::new(...)` capability bundles with `agent_fixture(..., TestAgentProfile::...)` where the metadata matches one of the shared profiles.

## Concrete Steps

From `/Users/aa/Documents/ingot`:

    cargo test -p ingot-agent-runtime --lib
    cargo test -p ingot-agent-runtime --test dispatch --test auto_dispatch --test reconciliation --test convergence

If additional duplicated agent rows remain after the main extraction, leave them for a later pass unless they can be converted to `TestAgentProfile` without widening the diff.

## Validation and Acceptance

Acceptance is:

1. `cargo test -p ingot-agent-runtime --lib` passes, proving unit tests can compile and use the shared include file.
2. The selected runtime integration tests pass, proving `tests/common/mod.rs` still provides the same harness API after the extraction.
3. The new shared `TestAgentProfile` is used by both harness registration methods and at least the most obvious manual agent fixtures in dispatch/auto-dispatch tests.

## Idempotence and Recovery

This refactor is source-only. If the include-file approach causes path or import failures, keep the shared file and fix the wrapper modules rather than re-copying the harness. The goal is one source file for the shared test harness logic.

## Artifacts and Notes

Expected proof points after the refactor:

    crates/ingot-agent-runtime/tests/common/shared_harness.rs exists and defines TestHarness, BlockingRunner, and TestAgentProfile.
    crates/ingot-agent-runtime/src/tests.rs no longer defines its own BlockingRunner or TestRuntimeHarness.
    crates/ingot-agent-runtime/tests/common/mod.rs re-exports the shared harness helpers instead of defining them inline.

## Interfaces and Dependencies

The shared file should define:

    pub enum TestAgentProfile { Mutating, ReviewOnly, Full }
    pub fn agent_fixture(name: &str, profile: TestAgentProfile) -> Agent
    pub struct TestHarness { ... }
    pub struct BlockingRunner { ... }

`TestHarness` must continue to expose `new`, `with_config`, `register_mutating_agent`, `register_review_agent`, `register_full_agent`, `wait_for_job_status`, and `wait_for_running_jobs` because existing tests already depend on that surface.

Revision note: created this ExecPlan before the code extraction so the include-file approach and the mutating-agent capability drift are recorded up front.

Revision note: updated after implementation and validation to record the successful include-file extraction, the restored non-harness imports in `tests/common/mod.rs`, and the focused test suites that passed.
