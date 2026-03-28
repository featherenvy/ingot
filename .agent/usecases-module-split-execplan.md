# Split mixed-responsibility ingot-usecases modules

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows `.agent/PLANS.md` and must be maintained in accordance with that file.

## Purpose / Big Picture

After this change, the `ingot-usecases` finding and convergence code will be organized by responsibility instead of being concentrated in two large files. Contributors should be able to find report parsing, triage, backlog promotion, finalization helpers, command orchestration, system actions, and test support in predictable places without changing how the rest of the repository imports or executes these usecases. The visible proof is that the crate compiles and its existing tests still pass while `crates/ingot-usecases/src/finding.rs` and `crates/ingot-usecases/src/convergence.rs` become directory modules with focused internal files.

## Progress

- [x] (2026-03-28 13:03Z) Re-read `.agent/PLANS.md`, claimed `ingot-8fh` and `ingot-zxm`, inspected `crates/ingot-usecases/src/finding.rs` and `crates/ingot-usecases/src/convergence.rs`, and mapped the current public API plus the embedded test scaffolding.
- [x] (2026-03-28 13:15Z) Converted `crates/ingot-usecases/src/finding.rs` into `crates/ingot-usecases/src/finding/` with `mod.rs` re-exporting the stable public API and focused `report.rs`, `triage.rs`, `auto_triage.rs`, `context.rs`, and `tests.rs` files.
- [x] (2026-03-28 13:15Z) Converted `crates/ingot-usecases/src/convergence.rs` into `crates/ingot-usecases/src/convergence/` with `mod.rs` re-exporting the stable public API and focused `types.rs`, `finalization.rs`, `command.rs`, `system_actions.rs`, `test_support.rs`, and `tests.rs` files.
- [x] (2026-03-28 13:15Z) Ran `cargo test -p ingot-usecases`, `cargo check -p ingot-http-api -p ingot-agent-runtime`, and `cargo fmt --all`.
- [ ] Close the two `bd` issues, commit, rebase, push `bd` state and git, and confirm the branch is synchronized with `origin/master`.

## Surprises & Discoveries

- Observation: `finding.rs` already has a relatively small public surface, but its internal responsibilities are split across three obvious seams: protocol report extraction and validation, triage/backlog mutation helpers, and repository-backed auto-triage orchestration.
  Evidence: the file exports `extract_findings`, `triage_finding`, `backlog_finding`, `auto_triage_findings`, `execute_auto_triage`, and `parse_revision_context_summary`, with the validation helpers and note/link normalization living near those boundaries.

- Observation: `convergence.rs` is easier to split by behavior than by visibility because one file currently mixes reusable public types and traits with pure helpers, service methods, and large inline test fakes.
  Evidence: the file defines public DTOs and traits near the top, pure helper functions in the middle, `ConvergenceService` orchestration methods below them, and a long `#[cfg(test)] mod tests` block containing `FakePort`.

## Decision Log

- Decision: preserve the existing external API paths under `crate::finding` and `crate::convergence` while performing only internal file extraction.
  Rationale: the issue scope calls for stable external APIs and low-risk cleanup, so downstream crates should not need import changes.
  Date/Author: 2026-03-28 / Codex

- Decision: keep the split as module-directory conversions (`finding/mod.rs`, `convergence/mod.rs`) rather than introducing new top-level crate modules.
  Rationale: this keeps the current public paths intact while allowing focused internal files and test support modules to live beside each feature area.
  Date/Author: 2026-03-28 / Codex

## Outcomes & Retrospective

The main implementation goal is complete: both oversized usecase files are now split into directory modules with focused internal ownership and unchanged public import paths. The validation pass shows downstream crates still compile against `crate::finding` and `crate::convergence`, which confirms the stable-API constraint held through the refactor. The remaining work is only session landing: close the `bd` issues, commit, push, and verify branch state.

## Context and Orientation

`crates/ingot-usecases/src/finding.rs` currently owns several different jobs. It parses job result payloads from `ingot_agent_protocol::report`, validates the report shape, converts findings into `ingot_domain::finding::Finding` rows, applies manual triage transitions, creates backlog items and revisions from unresolved findings, runs auto-triage against repositories, and exposes a small helper for projecting revision-context summaries. A “backlog promotion” in this repository means turning a finding into a new `Item` plus `ItemRevision` linked back to the source finding.

`crates/ingot-usecases/src/convergence.rs` currently owns the convergence command and system-action surface for the usecase layer. It defines DTOs such as `ConvergenceApprovalContext`, traits such as `ConvergenceCommandPort` and `PreparedConvergenceFinalizePort`, pure helper predicates for evaluator-driven transitions, shared finalization helpers, `ConvergenceService` methods for queue preparation and approvals, queue promotion and invalidation helpers, and a large test module with fake ports. A “prepared convergence” is the domain object representing a candidate integration result that has been prepared but not yet finalized to the target reference.

The rest of the repository imports these modules via `crate::finding::*` and `crate::convergence::*`, so `crates/ingot-usecases/src/lib.rs` should not need public path changes. The safest refactor is to convert each file into a directory module with `mod.rs` re-exporting the current external surface.

## Plan of Work

Start with `crates/ingot-usecases/src/finding.rs`. Convert it into `crates/ingot-usecases/src/finding/mod.rs` and move code into internal files that match the existing responsibility boundaries. The expected split is a report-focused module for `extract_findings`, schema validation, unique key checks, and subject classification; a triage module for `BacklogFindingOverrides`, `TriageFindingInput`, `triage_finding`, `backlog_finding`, and note/link normalization; an auto-triage module for `AutoTriagedFinding`, `auto_triage_findings`, and `execute_auto_triage`; and a small context module for `parse_revision_context_summary`. Keep the public API re-exported from `finding/mod.rs`.

Then convert `crates/ingot-usecases/src/convergence.rs` into `crates/ingot-usecases/src/convergence/mod.rs`. Put the shared public DTOs and traits in a `types.rs` module, the pure evaluator helpers and finalization operation helper in a `finalization.rs` module, the `ConvergenceService` plus its command methods in `command.rs`, and the queue-head and invalidation helpers plus `tick_system_actions` support in `system_actions.rs`. Move the existing `FakePort` and related test helpers into a `test_support.rs` module compiled only for tests, and keep the actual tests in `mod.rs` or dedicated `tests.rs` files if that is simpler.

After both splits compile, run targeted tests for `ingot-usecases` and at least one dependent compile check so path visibility regressions are caught. Update this ExecPlan with the exact outcomes, then close `ingot-8fh` and `ingot-zxm`, commit the refactor, rebase, push `bd dolt push`, push git, and verify the branch is synchronized.

## Concrete Steps

From `/Users/aa/Documents/ingot`, run:

    cargo test -p ingot-usecases
    cargo check -p ingot-http-api -p ingot-agent-runtime

If those pass after the refactor, complete the session landing steps:

    bd close ingot-8fh --reason "Completed" --json
    bd close ingot-zxm --reason "Completed" --json
    git pull --rebase
    bd dolt push
    git push
    git status -sb

## Validation and Acceptance

Acceptance is met when `crates/ingot-usecases/src/finding.rs` and `crates/ingot-usecases/src/convergence.rs` no longer exist as single large files and are replaced by directory modules with focused internal files, while existing imports continue to compile through the same public paths. `cargo test -p ingot-usecases` must pass, and `cargo check -p ingot-http-api -p ingot-agent-runtime` must pass to show the stable external API claim is true for dependent crates.

## Idempotence and Recovery

This refactor is source-only and should not change schemas, stored data, or network behavior. Re-running `cargo fmt`, `cargo test -p ingot-usecases`, and the dependent `cargo check` is safe. If a partial split leaves unresolved imports, restore the missing re-export in `finding/mod.rs` or `convergence/mod.rs` rather than changing downstream crates; the public path stability requirement is part of the task definition.

## Artifacts and Notes

Initial scope evidence:

    rg -n "^(pub )?(async )?fn |^pub struct |^pub enum |^pub trait " \
      crates/ingot-usecases/src/finding.rs crates/ingot-usecases/src/convergence.rs

Expected validation commands:

    cargo test -p ingot-usecases
    cargo check -p ingot-http-api -p ingot-agent-runtime

Observed validation results:

    cargo test -p ingot-usecases
    test result: ok. 72 passed; 0 failed

    cargo check -p ingot-http-api -p ingot-agent-runtime
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 4.36s

## Interfaces and Dependencies

At the end of this refactor, the following public items must still be available from `crate::finding`: `BacklogFindingOverrides`, `TriageFindingInput`, `ExtractedFindings`, `AutoTriagedFinding`, `extract_findings`, `triage_finding`, `backlog_finding`, `auto_triage_findings`, `execute_auto_triage`, and `parse_revision_context_summary`.

At the end of this refactor, the following public items must still be available from `crate::convergence`: `SystemActionItemState`, `SystemActionProjectState`, `ConvergenceApprovalContext`, `ApprovalFinalizeReadiness`, `FinalizePreparedTrigger`, `FinalizationTarget`, `CheckoutFinalizationReadiness`, `FinalizeTargetRefResult`, `RejectApprovalTeardown`, `RejectApprovalContext`, `ConvergenceCommandPort`, `ConvergenceSystemActionPort`, `PreparedConvergenceFinalizePort`, `should_prepare_convergence`, `should_invalidate_prepared_convergence`, `should_auto_finalize_prepared_convergence`, `find_or_create_finalize_operation`, `ConvergenceService`, `finalize_prepared_convergence`, `promote_queue_heads`, and `invalidate_prepared_convergence`.

The refactor should continue depending only on the existing domain, protocol, workflow, repository, and tracing crates already used by these modules. No new external dependencies are needed.

Revision note: created this ExecPlan before implementation because the task is a significant internal refactor spanning two large Rust modules and the repository requires an ExecPlan for significant refactors.

Revision note: updated this ExecPlan after implementation and validation to record the exact module tree produced, the successful test and compile commands, and the fact that only session landing steps remain.
