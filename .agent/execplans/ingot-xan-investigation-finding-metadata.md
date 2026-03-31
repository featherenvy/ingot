# Preserve investigation finding metadata through extraction and UI

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with [.agent/PLANS.md](/Users/aa/Documents/ingot/.agent/PLANS.md).

## Purpose / Big Picture

After this change, an investigation finding keeps the information that makes it actionable: the investigation scope, the promotion draft, and the optional grouping key. A user opening an item detail page will be able to see how the investigation was run, what paths were examined, and what item would be created if the finding is promoted, without relying on a separate raw job-log inspection flow.

The result is visible in two ways. First, promoting a finding will use the metadata already stored on the finding instead of reparsing the original job payload. Second, the item detail Findings section will show investigation context and promotion preview data for investigation findings.

## Progress

- [x] (2026-03-31 08:55Z) Investigated the current protocol, extraction path, storage schema, and UI DTOs. Confirmed that `extract_findings()` drops investigation-only fields and that promotion reparses the source job payload.
- [x] (2026-03-31 09:02Z) Created and claimed `bd` issue `ingot-xan` for this work.
- [x] (2026-03-31 09:14Z) Added optional persisted investigation metadata to `Finding`, the SQLite schema, and the shared finding test builder.
- [x] (2026-03-31 09:23Z) Updated extraction to preserve investigation metadata and switched promotion overrides to use persisted finding metadata first, with a fallback to raw job reparsing for older rows.
- [x] (2026-03-31 09:31Z) Exposed investigation metadata through the item detail DTO and rendered scope plus promotion preview in `FindingsTable`.
- [x] (2026-03-31 09:40Z) Ran focused Rust and UI tests, then passed the full `make ci` gate.
- [ ] Commit, push, close `ingot-xan`, and record the final landing details.

## Surprises & Discoveries

- Observation: The raw investigation report is not destroyed outright; it still exists on the completed job payload and the Jobs page can show it.
  Evidence: `crates/ingot-http-api/src/router/jobs.rs` serves `result.json`, and `ui/src/pages/JobsPage.tsx` already queries `/api/jobs/{job_id}/logs`.

- Observation: The loss is still structural for the finding pipeline because the stored `Finding` model and SQLite schema have no field for investigation metadata.
  Evidence: `crates/ingot-domain/src/finding.rs` and `crates/ingot-store-sqlite/migrations/0002_finding_triage.sql` only carry generic finding fields plus triage state.

- Observation: A compatibility fallback is worth keeping even after persisting the new field, because existing findings created before this migration will have `investigation = null`.
  Evidence: The investigation route tests manually seed old-style findings and still need promotion to inherit metadata without forcing fixture rewrites.

## Decision Log

- Decision: Store investigation-only metadata directly on each `Finding` as an optional typed object that includes both report-wide scope and per-finding promotion/grouping details.
  Rationale: This keeps the API and UI centered on the existing `Finding` entity, eliminates reparsing for promotion, and avoids adding a second item-detail fetch path just to recover investigation context.
  Date/Author: 2026-03-31 / Codex

- Decision: Keep `promotion_overrides_for_finding()` compatible with pre-migration rows by falling back to the source job payload only when `finding.investigation` is absent.
  Rationale: New findings no longer require reparsing, but older persisted findings would otherwise lose their promotion defaults until they are regenerated.
  Date/Author: 2026-03-31 / Codex

## Outcomes & Retrospective

Investigation findings now keep structured scope, promotion, and grouping data from extraction through persistence, API serialization, and item detail rendering. Promotion defaults use the stored metadata first, which removes the previous hard dependency on reparsing raw job payloads for newly created findings.

The broad repo gate passed with `make ci`, including Rust check/test/clippy/fmt and UI test/lint/build. The only remaining work is the landing sequence: commit, push, close the tracked issue, and verify the remote state.

## Context and Orientation

`crates/ingot-agent-protocol/src/report.rs` defines the wire-format investigation report. That report has a `scope` object describing the search and a list of findings where each finding adds `promotion` and `group_key` fields that do not exist on generic review or validation findings.

`crates/ingot-usecases/src/finding/report.rs` is the backend extraction layer that converts completed job payloads into stored domain findings. Today it flattens an `InvestigationFindingV1` into the generic `FindingV1` shape, which strips the metadata unique to investigation work.

`crates/ingot-domain/src/finding.rs` defines the stored `Finding` model and the JSON wire shape shared with the HTTP API. `crates/ingot-store-sqlite/src/store/finding.rs` reads and writes that model to SQLite. These two files must change together, plus a new migration under `crates/ingot-store-sqlite/migrations/`.

`crates/ingot-usecases/src/finding/triage.rs` currently reparses the original `InvestigationReportV1` from the source `Job` when backlog promotion needs promotion defaults. After this change, it should use the metadata already attached to the `Finding`.

`ui/src/types/domain.ts` mirrors the HTTP API response types. `ui/src/components/item-detail/FindingsTable.tsx` renders the current Findings section. That table already groups findings by job; it is the right place to display investigation methodology, examined paths, grouping keys, and promotion preview data.

## Plan of Work

First, extend the domain `Finding` model with an optional `investigation` field that contains a scope object and a promotion object. Mirror the new field through the serde wire struct, the test builder, and the SQLite schema. Use a single JSON column in SQLite so the repository can evolve the investigation payload without repeated schema churn for nested attributes.

Next, change `extract_findings()` so investigation reports produce standard findings plus the optional metadata instead of discarding it. Update `promotion_overrides_for_finding()` to read the typed metadata directly from the `Finding`. This removes the need to scan `source_jobs` for matching report entries just to recover promotion defaults.

Then, expose the new field through existing API responses by updating the shared `Finding` DTO type in the UI. Update `FindingsTable.tsx` so investigation findings show the scope and promotion preview inline. Keep the current layout and triage interactions intact; this is an additive presentation change, not a redesign.

Finally, add or update focused tests in the usecase, HTTP route, SQLite, and UI type/render layers to prove persistence, promotion behavior, and visible rendering.

## Concrete Steps

From `/Users/aa/Documents/ingot`:

1. Add a new SQLite migration after `0009_investigation_workflow.sql` that adds a nullable JSON column for persisted investigation metadata on `findings`.
2. Update `crates/ingot-domain/src/finding.rs` and `crates/ingot-domain/src/test_support/finding.rs` to include the new optional typed metadata.
3. Update `crates/ingot-store-sqlite/src/store/finding.rs` to read and write the new JSON column in `create`, `update`, `upsert`, and row mapping.
4. Update `crates/ingot-usecases/src/finding/report.rs` and `crates/ingot-usecases/src/finding/triage.rs` so extraction populates the field and promotion consumes it.
5. Update `ui/src/types/domain.ts`, `ui/src/components/item-detail/FindingsTable.tsx`, and any affected tests so the metadata is visible in item detail.
6. Run the focused test commands listed below and record the outcome in this plan.

## Validation and Acceptance

Run these commands from `/Users/aa/Documents/ingot`:

    cargo test -p ingot-usecases finding::
    cargo test -p ingot-http-api investigation_routes
    cargo test -p ingot-store-sqlite job_completion
    bun test ui/src/test/domain-contract.test.ts

Acceptance means:

- An investigation report extracted into findings keeps a typed metadata payload with scope, promotion, and group key.
- Backlog promotion inherits title, description, acceptance criteria, and classification from the persisted finding metadata without reparsing the source job payload.
- The item detail Findings UI type accepts the new metadata shape and renders investigation context/promotion preview for investigation findings.

## Idempotence and Recovery

The migration is additive and safe to rerun only through the normal migrated test database flow; if it fails during development, recreate the test database instead of editing old migrations. The code changes are idempotent because the optional metadata field serializes as `null` for non-investigation findings and existing callers can continue treating normal findings exactly as before.

## Artifacts and Notes

Expected backend behavior after the code change:

    finding.investigation.scope.methodology == "AST comparison"
    finding.investigation.group_key == Some("helper-dedup")
    promotion_overrides_for_finding(&finding, ..) reads from finding.investigation

Expected UI behavior after the code change:

    Findings
      Methodology: AST comparison
      Paths examined: crates/
      Promotion preview: Extract shared temp_git_repo helper

Validation transcripts used during implementation:

    cargo test -p ingot-usecases --lib
    cargo test -p ingot-http-api --test investigation_routes
    cargo test -p ingot-store-sqlite --test job_completion finding_round_trip_preserves_investigation_metadata
    cd ui && bun x vitest run src/test/domain-contract.test.ts src/test/item-detail-page.test.tsx
    make ci

## Interfaces and Dependencies

At the end of this work, `crates/ingot-domain/src/finding.rs` should define a stable optional investigation payload on `Finding`, using nested structs for scope and promotion.

`crates/ingot-usecases/src/finding/triage.rs` should continue exporting:

    pub fn promotion_overrides_for_finding(
        finding: &Finding,
        source_jobs: &[Job],
    ) -> Option<PromotionOverrides>

but the implementation should no longer depend on reparsing `InvestigationReportV1` from `source_jobs` when the `Finding` already carries the needed metadata.

Revision note: Created this ExecPlan to cover a cross-layer fix spanning persistence, usecases, API, and UI so a future contributor can resume from one document if the work stops midstream.

Revision note: Updated the plan after implementation to record the compatibility fallback, the validation evidence, and the remaining landing steps.
