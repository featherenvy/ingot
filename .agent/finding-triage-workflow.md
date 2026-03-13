# Add explicit finding triage to the delivery workflow

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows the repository guidance in `.agent/PLANS.md` and must be maintained in accordance with that file.

## Purpose / Big Picture

After this change, closure-relevant review and validation findings no longer force an immediate all-or-nothing `repair_candidate` loop. Instead, the item enters an explicit triage state where each finding gets a disposition. Operators can mark a finding to fix now, accept it as `wont_fix`, send it to a backlog item, link it as a duplicate of an existing item, dismiss it as invalid, or mark it as needing more investigation. The observable behavior is that an item with findings will stop in a `triaging` state, the item detail page will expose per-finding actions, and only findings marked `fix_now` will be sent into the next repair job.

## Progress

- [x] (2026-03-13 12:39Z) Re-read `.agent/PLANS.md`, inspected the current workflow graph, evaluator, finding model, HTTP routes, runtime prompt assembly, and UI item-detail page to map the full change surface.
- [x] (2026-03-13 12:42Z) Chose the implementation shape: keep the existing SQLite finding columns in place for compatibility, but generalize them in Rust and JSON as a linked item reference and triage note so the feature can ship without a risky schema table rewrite.
- [x] (2026-03-13 12:44Z) Chose a single generic `POST /api/findings/{finding_id}/triage` command instead of one route per disposition.
- [x] (2026-03-13 20:18Z) Implemented the expanded finding triage model in `crates/ingot-domain`, `crates/ingot-usecases`, `crates/ingot-store-sqlite`, and a new SQLite migration that rebuilds the `findings` table with the new dispositions and link metadata.
- [x] (2026-03-13 20:20Z) Updated the workflow evaluator and dispatch logic so closure-relevant findings enter `phase_status=triaging`, stay blocked until the latest report’s findings are fully triaged, then either dispatch repair or continue along the clean edge.
- [x] (2026-03-13 20:21Z) Updated repair prompt assembly so `repair_candidate` and `repair_after_integration` receive only the `fix_now` findings from the latest closure-relevant findings report plus the accepted non-blocking dispositions for that same report.
- [x] (2026-03-13 20:22Z) Updated `SPEC.md` with the new triage dispositions, triage hold semantics, API route, and validation-report requirement that blocking validation outcomes emit canonical `finding:v1` entries.
- [x] (2026-03-13 20:23Z) Added the generic finding-triage API route and item-detail UI controls for per-finding triage, including backlog item creation and duplicate linking.
- [x] (2026-03-13 20:25Z) Added or updated evaluator, usecase, router, runtime, and UI tests and verified them with targeted Rust and Vitest commands.
- [x] (2026-03-13 20:40Z) Addressed the follow-up review findings by fixing the integrated approval transition, allowing safe re-triage of prior decisions, rejecting self-linked backlog items, and adding focused regressions for each case.

## Surprises & Discoveries

- Observation: the current workflow already supports durable finding rows and per-finding `dismiss` and `promote`, but those actions are purely audit and follow-up tools; they do not participate in closure progression.
  Evidence: `crates/ingot-http-api/src/router.rs` exposes `/api/findings/{finding_id}/dismiss` and `/api/findings/{finding_id}/promote`, while `crates/ingot-workflow/src/evaluator.rs` computes the next closure step only from the latest job outcome.

- Observation: validation currently permits `outcome=findings` with failed checks and zero `finding:v1` objects, which is incompatible with a triage-first workflow because there is nothing durable to triage.
  Evidence: `SPEC.md` section `9.5.2 validation_report:v1` says `outcome=findings` requires at least one failed check or one finding, and `crates/ingot-usecases/src/finding.rs` extracts durable rows only from canonical `finding:v1` objects.

- Observation: several target files already contain user changes in the working tree, especially `crates/ingot-agent-runtime/src/lib.rs`, `crates/ingot-http-api/src/router.rs`, and `ui/src/pages/ItemDetailPage.tsx`.
  Evidence: `git status --short` on 2026-03-13 shows those files modified before this feature work begins.

- Observation: the first findings-table migration approach broke `items.origin_finding_id` because renaming `findings` to `findings_old` caused SQLite foreign keys in `items` to keep pointing at the temporary table name.
  Evidence: the first full Rust test run failed with `SqliteError { code: 1, message: "no such table: main.findings_old" }` when inserting new items after migration. Rebuilding the table as `findings_new` with `PRAGMA foreign_keys = OFF`, copying data, dropping `findings`, and renaming `findings_new` back to `findings` resolved the issue.

- Observation: route-level regression tests for backlog re-triage require cyclic foreign keys (`finding.linked_item_id` and `item.origin_finding_id`), so test fixtures must seed one side first and then update both columns into a valid steady state.
  Evidence: the first regression attempt failed with `FOREIGN KEY constraint failed` and then `CHECK constraint failed: NOT (triage_state = 'backlog' AND linked_item_id IS NULL)` until the fixture switched to an `untriaged` insert followed by a single `UPDATE` into backlog state.

## Decision Log

- Decision: model the new operator outcomes as expanded finding dispositions: `untriaged`, `fix_now`, `wont_fix`, `backlog`, `duplicate`, `dismissed_invalid`, and `needs_investigation`.
  Rationale: these states preserve the distinctions that matter for workflow closure. `wont_fix` means accepted risk, `dismissed_invalid` means the finding should not count as real, `backlog` means the issue remains real but was spun out into tracked follow-up work, and `duplicate` means the issue is already tracked elsewhere.
  Date/Author: 2026-03-13 / Codex

- Decision: keep the existing `findings.promoted_item_id` and `findings.dismissal_reason` SQLite columns, but expose them as `linked_item_id` and `triage_note` in Rust and JSON.
  Rationale: this keeps the migration surface small and avoids rewriting table constraints during a larger workflow refactor. The storage names become implementation details inside `ingot-store-sqlite`.
  Date/Author: 2026-03-13 / Codex

- Decision: add a single generic triage command route rather than one route per disposition.
  Rationale: the backend logic for validating dispositions, required notes, and required linked-item references belongs in one place. The UI can still present disposition-specific buttons.
  Date/Author: 2026-03-13 / Codex

- Decision: tighten validation semantics so closure-relevant validation outcomes with `outcome=findings` must include at least one `finding:v1`.
  Rationale: a triage workflow needs durable, attributable findings. Failed checks can still be reported in `checks`, but if the result should block closure it must also produce at least one canonical finding row.
  Date/Author: 2026-03-13 / Codex

## Outcomes & Retrospective

The core user-visible goal is now implemented. Closure-relevant findings no longer force immediate all-findings repair. The evaluator holds the item in `triaging`, the item detail page exposes per-finding dispositions, triaged findings can create or link follow-up work, and repair prompts are scoped to the findings marked `fix_now`.

The main compromise was storage migration strategy. The implementation originally tried to avoid a full findings-table rebuild by reusing the existing schema shape, but supporting `duplicate` correctly required removing the unique promoted-item constraint and generalizing the state machine. That forced a dedicated migration after all. The final migration is still narrowly scoped to `findings`.

Residual gap: the repository still emits one dead-code warning for the `status` field on `ValidationCheckV1` in `crates/ingot-usecases/src/finding.rs`. It does not block tests or the behavior implemented here, but it should be cleaned up before a warning-free lint gate is expected.

The follow-up review pass also closed three concrete bugs: required-policy integrated findings now enter pending approval after the last non-blocking triage decision, previously triaged findings remain editable, and backlog links can no longer point back to the source item. The regression suite now covers all three.

## Context and Orientation

The backend workflow lives across four Rust crates. `crates/ingot-domain` contains the shared data types such as `Finding` and `Job`. `crates/ingot-usecases` contains pure command-side logic such as finding extraction, triage transitions, and dispatch validation. `crates/ingot-workflow` contains the pure evaluator, which turns item state and job history into the current workflow projection. `crates/ingot-http-api` exposes the REST routes used by the UI. `crates/ingot-agent-runtime` assembles prompts and runs jobs against adapters.

In this repository, a “closure-relevant” step means a review or validation step that advances or rewinds delivery progress. Those steps currently emit `OutcomeClass::Findings`, and the evaluator follows the workflow graph directly to `repair_candidate` or `repair_after_integration`. This plan changes that behavior. A closure-relevant findings result should instead put the item into a `triaging` state where the latest report’s findings must be dispositioned before progress continues.

The UI relevant to this change lives under `ui/src/`. `ui/src/pages/ItemDetailPage.tsx` is the route container. `ui/src/components/item-detail/FindingsTable.tsx` renders the findings list. `ui/src/api/client.ts` contains the REST client helpers and `ui/src/types/domain.ts` mirrors backend JSON payloads.

## Plan of Work

First, generalize the finding model. In `crates/ingot-domain/src/finding.rs`, expand `FindingTriageState` to the new dispositions and rename the public struct fields from the promotion-specific names to generic triage names. In `crates/ingot-usecases/src/finding.rs`, replace the narrow dismiss and promote helpers with a generic triage command validator plus a helper that creates a linked follow-up item when the disposition is `backlog`. In `crates/ingot-store-sqlite/src/store.rs`, keep reading and writing the current SQLite columns but map them to the new logical field names and add a generic persistence method for triage updates.

Second, make triage closure-relevant. In `crates/ingot-workflow/src/evaluator.rs`, extend `Evaluator::evaluate` to accept the current item’s findings. When the latest closure-relevant review or validation job ended with `OutcomeClass::Findings`, load only the findings sourced from that job. If any are `untriaged` or `needs_investigation`, project `phase_status=triaging`, `next_recommended_action=triage_findings`, and no dispatchable step. If all are triaged and at least one is `fix_now`, dispatch the corresponding repair step. If all are triaged and none is `fix_now`, continue along the same edge the graph would have taken on `clean`. Keep report-only investigations unchanged.

Third, scope repair prompts to the chosen findings. In `crates/ingot-agent-runtime/src/lib.rs`, when assembling a prompt for `repair_candidate` or `repair_after_integration`, load the current item’s findings and include only the `fix_now` findings from the latest closure-relevant findings report. Also include accepted non-blocking dispositions from that same report so the repair agent does not re-open them in the same attempt.

Fourth, update the API. In `crates/ingot-http-api/src/router.rs`, add a `POST /api/findings/{finding_id}/triage` route that accepts a disposition, an optional note, an optional linked item id, and optional backlog item overrides when the disposition should create a new backlog item. Replace item-detail evaluation calls so they pass findings into the evaluator. Keep the old reachability checks for finding-linked item creation.

Fifth, update the frontend. In `ui/src/api/client.ts`, add a `triageFinding` mutation helper. In `ui/src/types/domain.ts`, mirror the new finding fields and the new `triaging` phase status. In `ui/src/components/item-detail/FindingsTable.tsx`, render each finding’s state, triage note, linked item, and action controls. In `ui/src/pages/ItemDetailPage.tsx`, wire the mutation and refresh behavior into the findings table.

Finally, update the spec and tests. `SPEC.md` must describe the new dispositions, the triage state, and the changed closure rules. `ARCHITECTURE.md` should be updated only where it summarizes finding triage responsibilities. Add or update targeted tests in the evaluator, finding usecase, HTTP router, and UI route suite.

## Concrete Steps

From the repository root `/Users/aa/Documents/ingot`, edit these files:

1. `.agent/finding-triage-workflow.md`
2. `SPEC.md`
3. `ARCHITECTURE.md`
4. `crates/ingot-domain/src/finding.rs`
5. `crates/ingot-usecases/src/finding.rs`
6. `crates/ingot-usecases/src/error.rs`
7. `crates/ingot-store-sqlite/src/store.rs`
8. `crates/ingot-http-api/src/error.rs`
9. `crates/ingot-http-api/src/router.rs`
10. `crates/ingot-workflow/src/evaluator.rs`
11. `crates/ingot-usecases/src/job.rs`
12. `crates/ingot-agent-runtime/src/lib.rs`
13. `ui/src/api/client.ts`
14. `ui/src/types/domain.ts`
15. `ui/src/components/item-detail/FindingsTable.tsx`
16. `ui/src/pages/ItemDetailPage.tsx`
17. Focused Rust and UI test files that cover findings and item detail behavior.

Then run these commands:

    cd /Users/aa/Documents/ingot && cargo test -p ingot-workflow -p ingot-usecases -p ingot-http-api
    cd /Users/aa/Documents/ingot/ui && bunx vitest run src/test/item-detail-page.test.tsx src/test/domain-contract.test.ts

If the touched modules compile cleanly, run the broader gate most likely to expose integration mistakes:

    cd /Users/aa/Documents/ingot && make test
    cd /Users/aa/Documents/ingot && make ui-test

## Validation and Acceptance

Acceptance is behavioral.

Start from an item whose latest closure-relevant review or validation job completed with findings. The item detail route must show the item in a `triaging` state rather than offering immediate repair dispatch. Each finding in the latest report must expose an action that records a disposition. After marking one or more findings `fix_now` and resolving all others as non-blocking, the item must expose only the appropriate repair step, and the next repair prompt must mention only the chosen `fix_now` findings. If all findings are triaged into non-blocking dispositions and none is `fix_now`, the item must continue to the next clean-edge workflow step without creating a repair job.

Run `cargo test -p ingot-workflow -p ingot-usecases -p ingot-http-api` and expect the evaluator, finding-usecase, and router suites to pass. Run `bunx vitest run src/test/item-detail-page.test.tsx src/test/domain-contract.test.ts` in `ui/` and expect the item-detail tests to pass with the new finding actions present.

## Idempotence and Recovery

This plan avoids destructive repository operations. Re-running the new triage commands against a non-`untriaged` finding should fail safely with a conflict. Because the working tree already contains unrelated in-progress work, recovery should be done by inspecting the touched files and re-running targeted tests rather than resetting the tree.

## Artifacts and Notes

Useful inspection commands used while preparing this plan:

    git status --short
    git diff -- SPEC.md ARCHITECTURE.md crates/ingot-agent-runtime/src/lib.rs crates/ingot-http-api/src/router.rs ui/src/pages/ItemDetailPage.tsx
    rg -n "repair_candidate|triage_state|promote_finding|dismiss_finding" crates ui/src SPEC.md

## Interfaces and Dependencies

At the end of this work, these interfaces must exist conceptually even if helper names vary slightly:

In `crates/ingot-domain/src/finding.rs`, `FindingTriageState` must include:

    Untriaged
    FixNow
    WontFix
    Backlog
    Duplicate
    DismissedInvalid
    NeedsInvestigation

`Finding` must expose generic triage metadata:

    pub triage_state: FindingTriageState
    pub linked_item_id: Option<ItemId>
    pub triage_note: Option<String>

In `crates/ingot-http-api/src/router.rs`, define a request type equivalent to:

    pub struct TriageFindingRequest {
        pub triage_state: FindingTriageState,
        pub triage_note: Option<String>,
        pub linked_item_id: Option<String>,
        pub create_backlog_item: Option<bool>,
        pub target_ref: Option<String>,
        pub approval_policy: Option<ApprovalPolicy>,
    }

In `ui/src/api/client.ts`, add a client helper equivalent to:

    export const triageFinding = (
      findingId: string,
      payload: {
        triage_state: FindingTriageState
        triage_note?: string
        linked_item_id?: string
        create_backlog_item?: boolean
      },
    ) => ...

Revision note: created this ExecPlan before implementation to satisfy the repository requirement for significant workflow and spec refactors and to record the design constraints discovered during the initial investigation.

Revision note: updated after implementation to record the completed migration, evaluator/API/UI behavior, the SQLite foreign-key migration discovery, and the targeted Rust and Vitest validation commands that passed.
