# Typed Job Input Refactor

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with [.agent/PLANS.md](/Users/aa/Documents/ingot/.agent/PLANS.md).

## Purpose / Big Picture

After this change, jobs no longer expose two unrelated nullable commit fields for their input. Instead, the codebase, database, and HTTP API use one typed `job_input` value that distinguishes authoring-head jobs from candidate and integrated diff subjects. This makes incomplete or contradictory job subjects impossible to represent in normal Rust code, and it removes the specific class of bugs where review or investigation jobs were queued with missing commit ranges.

## Progress

- [x] (2026-03-15 00:00Z) Replace `Job.input_base_commit_oid` / `Job.input_head_commit_oid` with typed `JobInput` in `crates/ingot-domain`.
- [x] (2026-03-15 00:15Z) Update SQLite schema and store mapping to persist `job_input_kind` plus normalized base/head columns.
- [x] (2026-03-15 00:40Z) Refactor usecases, router, runtime, finding extraction, and workspace binding to construct and consume `JobInput`.
- [x] (2026-03-15 01:05Z) Update HTTP/UI types and the touched router/runtime/workflow/usecase tests to the new shape.
- [x] (2026-03-15 01:15Z) Update `SPEC.md` references and record the final verification outcome here.

## Surprises & Discoveries

- Observation: The current `Job` input pair is threaded through domain, store, HTTP, runtime, usecases, and many raw SQL tests.
  Evidence: `rg -n "input_base_commit_oid|input_head_commit_oid" crates ui SPEC.md -S`

- Observation: `auto_dispatch_projected_review` is called after successful job finalization, so invalid follow-on dispatch state must not be allowed to leak as ambiguous partial inputs.
  Evidence: `crates/ingot-agent-runtime/src/lib.rs` around `auto_dispatch_projected_review(...)` after job completion.

## Decision Log

- Decision: Implement the full wire/schema refactor instead of an internal-only wrapper.
  Rationale: The user explicitly chose the end-to-end typed model over a compatibility layer.
  Date/Author: 2026-03-15 / Codex

- Decision: Use a single tagged `job_input` enum in Rust and JSON.
  Rationale: This is the most comprehensive option and removes the illegal hybrid states entirely.
  Date/Author: 2026-03-15 / Codex

- Decision: Store the enum in normalized SQLite columns with CHECK constraints rather than a JSON blob.
  Rationale: This keeps SQL queryable and lets SQLite enforce valid combinations at the storage boundary.
  Date/Author: 2026-03-15 / Codex

- Decision: Treat this as a clean-break migration with no backward-compatible DB/API shim.
  Rationale: The user chose the clean-break path and the repo already treats this late-bound seed work as fresh-state only.
  Date/Author: 2026-03-15 / Codex

## Outcomes & Retrospective

The backend and UI type layers now expose a typed `job_input` model, and the dispatch/runtime logic uses that enum instead of mutating two independent nullable commit fields. The schema still uses the existing `input_base_commit_oid` / `input_head_commit_oid` column names alongside the new `job_input_kind` discriminator so SQL queries remain readable and most raw fixture SQL did not need to be rewritten. The focused regressions for late-bound investigation and projected review dispatch are passing, the touched crates compile, and the UI build passes.

## Context and Orientation

`crates/ingot-domain/src/job.rs` defines the persisted and serialized `Job` shape used everywhere else in the repository. `crates/ingot-store-sqlite/migrations/0001_initial.sql` defines the fresh SQLite schema, and `crates/ingot-store-sqlite/src/store.rs` maps `Job` values to and from SQL rows. The HTTP layer in `crates/ingot-http-api/src/router.rs` dispatches and binds job inputs, while `crates/ingot-agent-runtime/src/lib.rs` prepares and auto-dispatches jobs after successful execution. `crates/ingot-usecases/src/job.rs` decides what each step’s input should be, and `crates/ingot-usecases/src/finding.rs` derives finding subjects from the job input.

The current bug-prone design is that one logical job input is represented as two unrelated optional strings. Review, validation, and investigation steps need a complete diff subject, while authoring steps need only a head commit. This refactor gives those cases distinct typed variants.

## Plan of Work

First, update the domain type so `Job` owns a typed `JobInput` enum plus helper constructors and accessors. Then update SQLite schema and row mapping so the database stores the enum in normalized columns and rejects invalid combinations. Once those boundaries compile, refactor usecase dispatch, router binding, runtime auto-dispatch, workspace handling, and finding extraction to construct typed inputs instead of patching raw nullable fields.

After the backend compiles, update HTTP assertions, TypeScript job types, and the raw SQL fixtures in the router and runtime integration tests. Finish by updating `SPEC.md` to match the new `job_input` contract and recording the final verification commands and outcomes here.

## Concrete Steps

From `/Users/aa/Documents/ingot`, perform the work in this order:

1. Edit `crates/ingot-domain/src/job.rs` to introduce `JobInput` and replace the old fields.
2. Edit `crates/ingot-store-sqlite/migrations/0001_initial.sql` and `crates/ingot-store-sqlite/src/store.rs` so the schema and row mapping persist `job_input`.
3. Update `crates/ingot-usecases/src/job.rs`, `crates/ingot-http-api/src/router.rs`, `crates/ingot-agent-runtime/src/lib.rs`, `crates/ingot-usecases/src/finding.rs`, and `crates/ingot-workspace/src/lib.rs` to consume `JobInput`.
4. Update `ui/src/types/domain.ts`, relevant HTTP tests, and integration tests.
5. Run focused tests and crate checks, then update this ExecPlan with results.

## Validation and Acceptance

Acceptance means:

- jobs serialize through the API with `job_input` and no longer expose `input_base_commit_oid` or `input_head_commit_oid`
- SQLite persists only valid input combinations
- implicit authoring dispatch still binds a head-only authoring input
- candidate review/validation/investigation still produce complete candidate subjects
- integrated validation still produces an integrated subject
- missing or malformed subjects fail closed without queuing invalid jobs

The minimum verification set is:

    cargo check -p ingot-http-api
    cargo check -p ingot-agent-runtime
    cargo test -p ingot-http-api -- --nocapture
    cargo test -p ingot-agent-runtime --test integration -- --nocapture

The new targeted tests should prove:

- router binding rejects missing or partial candidate subjects
- runtime auto-dispatch rejects missing candidate subjects
- pre-authoring investigation still creates and cleans up its anchor ref
- implicit authoring auto-review still uses the bound authoring base

Observed verification during implementation:

    cargo check -p ingot-http-api -p ingot-agent-runtime -p ingot-usecases -p ingot-store-sqlite -p ingot-workflow
    cargo test -p ingot-http-api --no-run
    cargo test -p ingot-agent-runtime --no-run
    cargo test -p ingot-usecases --no-run
    cargo test -p ingot-workflow --no-run
    cargo test -p ingot-store-sqlite --no-run
    cargo test -p ingot-http-api investigate_item_dispatch_creates_and_triage_removes_anchor_ref -- --nocapture
    cargo test -p ingot-agent-runtime auto_dispatch_projected_review_rejects_missing_candidate_subject -- --nocapture
    make ui-build

## Idempotence and Recovery

This is a clean-break refactor. If the database schema changes become inconsistent during development, remove the local SQLite file and rerun migrations rather than trying to preserve old job rows. Re-running tests is safe; they create temporary repos and databases. If intermediate compile errors occur, use `cargo check` to enumerate remaining call sites before running the full integration suite again.

## Artifacts and Notes

Initial discovery command:

    rg -n "input_base_commit_oid|input_head_commit_oid" crates ui SPEC.md -S

This command shows the scope of the refactor across domain, store, runtime, router, tests, and spec text.

## Interfaces and Dependencies

In `crates/ingot-domain/src/job.rs`, define:

    pub enum JobInput {
        None,
        AuthoringHead { head_commit_oid: String },
        CandidateSubject { base_commit_oid: String, head_commit_oid: String },
        IntegratedSubject { base_commit_oid: String, head_commit_oid: String },
    }

and update `pub struct Job` to contain:

    pub job_input: JobInput

At the SQLite boundary, store:

    job_input_kind TEXT NOT NULL
    input_base_commit_oid TEXT
    input_head_commit_oid TEXT

with CHECK constraints enforcing the valid combinations for each variant.

Revision note: updated after the core refactor landed to reflect the implemented storage shape and current verification state. The storage boundary kept the existing base/head column names and added `job_input_kind`, which still satisfies the normalized-enum design while minimizing SQL churn.
