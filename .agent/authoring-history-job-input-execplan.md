# Extract shared authoring history and job-input helpers

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with [.agent/PLANS.md](/Users/aa/Documents/ingot/.agent/PLANS.md).

## Purpose / Big Picture

After this refactor, the usecase layer will own one implementation for deriving authoring commit history, selecting prepared convergences, and constructing subject-style `JobInput` values. `crates/ingot-usecases/src/job_dispatch.rs` and `crates/ingot-usecases/src/dispatch.rs` currently repeat related logic, while `crates/ingot-agent-runtime/src/execution.rs` depends on the `dispatch` copy. The observable outcome is that targeted usecase and runtime tests continue to pass while the helper logic lives in one internal module instead of being split across multiple files.

## Progress

- [x] 2026-03-29 17:24Z Re-read `.agent/PLANS.md`, scanned the duplicated helper sites, created and claimed bd issue `ingot-02z`.
- [x] 2026-03-29 17:25Z Added `crates/ingot-usecases/src/authoring_history.rs` and migrated `job_dispatch`, `dispatch`, and `job_completion` to the shared helpers.
- [x] 2026-03-29 17:27Z Kept the runtime-facing `dispatch` helper API stable with thin public wrapper functions that delegate to the shared module.
- [x] 2026-03-29 17:30Z Ran `cargo test -p ingot-usecases dispatch`, `cargo test -p ingot-usecases job_dispatch`, `cargo test -p ingot-agent-runtime execution`, and `cargo test -p ingot-usecases`.
- [ ] 2026-03-29 17:30Z Close `ingot-02z`, commit, rebase, push `bd` and git, and verify the branch is published.

## Surprises & Discoveries

- Observation: `job_dispatch` and `dispatch` are not exact duplicates, but they derive from the same commit-history primitives.
  Evidence: `job_dispatch` computes `current_authoring_head`, `previous_authoring_head`, and `job_input_from_range`, while `dispatch` computes `current_authoring_head_for_revision`, `current_authoring_head_for_revision_with_workspace`, `effective_authoring_base_commit_oid`, and candidate-subject filling.

- Observation: public re-export from a crate-private helper module does not work for the runtime-facing `dispatch` API.
  Evidence: the first `cargo test -p ingot-usecases dispatch` run failed with `E0364` because crate-private functions in `authoring_history` cannot be re-exported publicly from `dispatch`.

## Decision Log

- Decision: Extract one shared internal module and keep `dispatch` public helper names as re-exports instead of changing runtime callers in the same patch.
  Rationale: This removes the duplicated logic while keeping the public shape of `ingot-usecases::dispatch` stable for `ingot-agent-runtime`.
  Date/Author: 2026-03-29 / Codex

- Decision: Preserve the runtime-facing `dispatch` API with thin wrapper functions rather than `pub use`.
  Rationale: the shared module is intentionally crate-private; wrappers keep that boundary intact while satisfying the downstream public API requirement.
  Date/Author: 2026-03-29 / Codex

## Outcomes & Retrospective

The extraction landed as planned. `crates/ingot-usecases/src/authoring_history.rs` now owns the shared primitives for commit-history lookup, workspace fallback, prepared-convergence selection, and subject-style `JobInput` construction. `job_dispatch` no longer owns its own copies of those helpers, `job_completion` imports `selected_prepared_convergence` from the shared module, and `dispatch` now delegates its runtime-facing authoring helpers to the shared implementation.

Validation passed with targeted and crate-level test coverage. The runtime-facing API remained stable without making the new module public by using thin wrapper functions in `dispatch`.

## Context and Orientation

`crates/ingot-usecases/src/job_dispatch.rs` builds new queued `Job` values for manual dispatch and retry. It contains helper logic for selecting authoring heads from prior completed commit jobs, constructing candidate or integrated subject inputs from commit ranges, and locating a prepared convergence for the current revision. `crates/ingot-usecases/src/dispatch.rs` contains the auto-dispatch path plus runtime-oriented helper functions that derive the current authoring head, effective authoring base commit, and candidate-subject inputs while optionally consulting a workspace. `crates/ingot-usecases/src/job_completion.rs` already imports `selected_prepared_convergence` from `job_dispatch`, which is a sign that the helper belongs in a shared internal module instead of a dispatch-command file.

`crates/ingot-agent-runtime/src/execution.rs` currently calls `ingot_usecases::dispatch::current_authoring_head_for_revision_with_workspace` and `ingot_usecases::dispatch::effective_authoring_base_commit_oid`. Those callers should keep compiling after the refactor, which is why the public `dispatch` surface needs to stay stable even if the logic moves.

## Plan of Work

Create `crates/ingot-usecases/src/authoring_history.rs` as a crate-internal module. Move the shared helper primitives there: selecting completed commit OIDs for a revision, deriving current and previous authoring heads, resolving the current authoring head with an optional workspace fallback, deriving the effective authoring base commit with workspace fallback, selecting a prepared convergence for a revision, building subject-style `JobInput` values from a commit range or prepared convergence, and constructing candidate-subject input for auto-dispatch from an existing job input plus current state.

Update `crates/ingot-usecases/src/lib.rs` to register the new internal module. In `crates/ingot-usecases/src/job_dispatch.rs`, replace the local helper implementations with imports from the new module. In `crates/ingot-usecases/src/job_completion.rs`, import `selected_prepared_convergence` from the new module instead of `job_dispatch`. In `crates/ingot-usecases/src/dispatch.rs`, import the shared module for internal use and re-export the runtime-facing helper functions so `ingot-agent-runtime` keeps its current call sites.

Add or preserve focused tests in the new module or the existing dispatch tests so the extraction proves that commit ordering, seed fallback, workspace fallback, and prepared-convergence selection behave the same as before.

## Concrete Steps

From `/Users/aa/Documents/ingot`:

    cargo test -p ingot-usecases dispatch
    cargo test -p ingot-usecases job_dispatch
    cargo test -p ingot-agent-runtime execution

If the test target names differ, run the crate-level tests for `ingot-usecases` and `ingot-agent-runtime` instead and record the exact commands used in this plan.

## Validation and Acceptance

Acceptance is:

1. `crates/ingot-usecases/src/authoring_history.rs` exists and contains the shared authoring-history and subject-input helpers.
2. `crates/ingot-usecases/src/job_dispatch.rs`, `crates/ingot-usecases/src/dispatch.rs`, and `crates/ingot-usecases/src/job_completion.rs` no longer each own private copies of the same helper logic.
3. `crates/ingot-agent-runtime/src/execution.rs` still compiles against `ingot_usecases::dispatch`.
4. Focused Rust tests pass and prove no behavior regression in authoring-head or prepared-convergence selection.

## Idempotence and Recovery

This refactor is source-only and safe to repeat. If the new module causes a compile failure, the safe recovery path is to keep the shared helper in place and temporarily restore the failing caller to a forwarding wrapper until all imports line up. Do not change user-visible behavior in the same patch.

## Artifacts and Notes

Expected structural proof after the refactor:

    crates/ingot-usecases/src/authoring_history.rs
    crates/ingot-usecases/src/job_dispatch.rs imports shared helpers instead of defining them locally
    crates/ingot-usecases/src/job_completion.rs imports selected_prepared_convergence from authoring_history
    crates/ingot-usecases/src/dispatch.rs re-exports the runtime-facing authoring helper functions

## Interfaces and Dependencies

At the end of this refactor, `crates/ingot-usecases/src/authoring_history.rs` defines crate-internal helpers equivalent to:

    pub(crate) fn current_authoring_head_for_revision(...)
    pub(crate) fn previous_authoring_head_for_revision(...)
    pub(crate) fn current_authoring_head_for_revision_with_workspace(...)
    pub(crate) fn effective_authoring_base_commit_oid(...)
    pub(crate) fn selected_prepared_convergence(...)
    pub(crate) fn subject_input_from_range(...)
    pub(crate) fn job_input_from_prepared_convergence(...)
    pub(crate) fn build_candidate_subject_input(...)

The `crates/ingot-usecases/src/dispatch.rs` module continues exposing the authoring-head helpers that `ingot-agent-runtime` already calls via thin forwarding wrappers.

Revision note: 2026-03-29 / Codex. Created this ExecPlan at implementation start because the repository requires a living plan for significant refactors and this change crosses multiple backend crates through shared helper APIs.

Revision note: 2026-03-29 / Codex. Updated after implementation and validation to record the switch from attempted public re-export to thin wrappers and the exact test commands that passed.
