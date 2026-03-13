# Refactor the item detail page into focused UI sections

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows the repository guidance in `.agent/PLANS.md` and must be maintained in accordance with that file.

## Purpose / Big Picture

After this change, `ui/src/pages/ItemDetailPage.tsx` becomes a small orchestration layer that loads item detail data, wires mutations, and delegates rendering to focused sub-components. A contributor reading the page should be able to understand the data flow without scrolling through every table and definition list. The observable behavior remains the same: loading the item detail route still shows operator actions, state and evaluation panels, jobs, findings, convergences, revision context, and diagnostics when present.

## Progress

- [x] (2026-03-13 09:46Z) Inspected `ui/src/pages/ItemDetailPage.tsx`, the related test file, and repository guidance to confirm the scope of the refactor and the requirement to maintain an ExecPlan.
- [x] (2026-03-13 09:46Z) Chose a component extraction strategy that keeps query and mutation ownership in the page while moving presentational sections into `ui/src/components/item-detail/ItemDetailSections.tsx`.
- [x] (2026-03-13 09:48Z) Implemented the extraction into `ui/src/components/item-detail/ItemDetailSections.tsx`, reduced `ui/src/pages/ItemDetailPage.tsx` to orchestration logic, and expanded `ui/src/test/item-detail-page.test.tsx` with a data-rich render test.
- [x] (2026-03-13 09:49Z) Ran `bunx vitest run src/test/item-detail-page.test.tsx` in `ui/` and confirmed all three item-detail tests pass.
- [x] (2026-03-13 09:49Z) Earlier validation found `bun run build` blocked by a TypeScript error in `src/test/root-layout.test.tsx`; that failure was recorded as the last known build state before this follow-up split.
- [x] (2026-03-13 10:39Z) Re-read the current component tree and confirmed the remaining sizing problem is `ui/src/components/item-detail/ItemDetailSections.tsx`, which had grown into a 452-line module with 8 exports and 3 private helpers.
- [x] (2026-03-13 10:39Z) Replaced `ui/src/components/item-detail/ItemDetailSections.tsx` with focused files under `ui/src/components/item-detail/` and switched the page to import from a barrel file.
- [x] (2026-03-13 10:41Z) Ran `bun run format src/pages/ItemDetailPage.tsx src/components/item-detail src/test/item-detail-page.test.tsx`, `bunx vitest run src/test/item-detail-page.test.tsx`, and `bun run build` in `ui/`; formatting succeeded, the focused test suite passed, and the production build completed successfully.

## Surprises & Discoveries

- Observation: `ui/src/pages/ItemDetailPage.tsx` already contains recent uncommitted feature work, so the refactor must preserve that newer behavior rather than compare against the older baseline.
  Evidence: `git diff -- ui/src/pages/ItemDetailPage.tsx` shows additions for operator actions, findings, convergences, revision context, and job retry and cancel actions.

- Observation: The existing test file only covers initial loading and the queued-job blocker message, leaving most of the page body unexercised.
  Evidence: `ui/src/test/item-detail-page.test.tsx` contains two tests and neither asserts on findings, convergences, revision context, or diagnostics.

- Observation: The general UI build currently fails in an unrelated test file, not in the item detail code that was refactored here.
  Evidence: `bun run build` reports `TS2786` in `src/test/root-layout.test.tsx` for `ExplodingPage` being inferred as `() => void`.

- Observation: Re-running the UI build after the file split no longer reproduces the earlier `root-layout` TypeScript failure; the current tree builds successfully.
  Evidence: `bun run build` completed with Vite output ending in `✓ built in 705ms`.

## Decision Log

- Decision: Keep all data fetching and mutation setup in `ui/src/pages/ItemDetailPage.tsx` and extract only rendering-oriented UI sections into a new component module.
  Rationale: This keeps hook ownership and cache invalidation logic in one place while still shrinking the page into a readable orchestration component.
  Date/Author: 2026-03-13 / Codex

- Decision: Use one new component module at `ui/src/components/item-detail/ItemDetailSections.tsx` instead of scattering each extracted section across many tiny files.
  Rationale: The user-visible goal is to reduce the page size and isolate section rendering. A single module keeps related item-detail UI together without over-fragmenting the folder.
  Date/Author: 2026-03-13 / Codex

- Decision: Supersede the single-module split and keep each item-detail section in its own file under `ui/src/components/item-detail/`, with `ui/src/components/item-detail/index.ts` as the import surface.
  Rationale: The combined module still concentrates unrelated concerns in one place. Per-file sections make ownership clearer, reduce scroll depth, and match the requested folder structure without changing page behavior.
  Date/Author: 2026-03-13 / Codex

## Outcomes & Retrospective

The user-facing goal was achieved with the requested folder structure. `ui/src/pages/ItemDetailPage.tsx` remains a small orchestration component, and the item-detail UI now lives in focused files under `ui/src/components/item-detail/` rather than one large section module. The existing route test coverage still exercises loading, queue-blocker messaging, and a populated detail view after the split.

Validation is now stronger than in the earlier pass: the formatter succeeded, the focused item-detail tests passed, and `bun run build` completed successfully on the current tree.

## Context and Orientation

The UI code lives in `ui/src/`. The item detail route is implemented in `ui/src/pages/ItemDetailPage.tsx`. After this refactor, that file is intentionally limited to route parameters, React Query data loading, cache invalidation, and the top-level operator mutations. The section rendering now lives in focused files under `ui/src/components/item-detail/`, with `ui/src/components/item-detail/index.ts` acting as the import surface. The supporting domain types for the page live in `ui/src/types/domain.ts`. The current test coverage for this route lives in `ui/src/test/item-detail-page.test.tsx`.

In this repository, an "item detail" page is the single-page view for one work item inside a project. The route path is `/projects/:projectId/items/:itemId`. The page shows a mix of summary panels and tables for jobs, findings, and convergence attempts. "Convergence" here means the integration attempt that prepares or finalizes changes against a target commit. "Revision context" is a summary object from the backend that reports changed paths and the latest validation or review results for the current revision.

## Plan of Work

Replace `ui/src/components/item-detail/ItemDetailSections.tsx` with focused files for each section: `OperatorActions.tsx`, `OverviewPanels.tsx`, `JobsTable.tsx`, `JobActions.tsx`, `FindingsTable.tsx`, `ConvergencesTable.tsx`, `DetailList.tsx`, and the existing supporting sections that were previously exported from the large module. Add `ui/src/components/item-detail/index.ts` so the page can keep one import statement while the folder owns the file split. Keep the item page responsible for route parameter resolution, React Query data loading, refresh invalidation, and top-level mutation wiring for dispatch, prepare convergence, approve, and reject.

Edit `ui/src/pages/ItemDetailPage.tsx` so it imports the extracted section components and renders them conditionally based on the loaded data. Keep the current user-visible content and wording intact. The page should continue to compute `activeJob`, `retryableJobs`, and the queued-job blocker message before passing the necessary values down.

Extend `ui/src/test/item-detail-page.test.tsx` with a rendered-detail test that populates jobs, findings, convergences, revision context, and diagnostics. The purpose is to prove that the extracted sections still render the expected headings and representative values.

## Concrete Steps

From the repository root `/Users/aa/Documents/ingot`, update or create these files:

1. `.agent/item-detail-page-refactor.md`
2. `ui/src/components/item-detail/index.ts`
3. `ui/src/components/item-detail/*.tsx` for the focused section files
4. `ui/src/pages/ItemDetailPage.tsx`
5. `ui/src/test/item-detail-page.test.tsx`

Then run these commands from the repository root:

    cd /Users/aa/Documents/ingot/ui && bun run format src/pages/ItemDetailPage.tsx src/components/item-detail src/test/item-detail-page.test.tsx
    cd /Users/aa/Documents/ingot/ui && bunx vitest run src/test/item-detail-page.test.tsx
    cd /Users/aa/Documents/ingot/ui && bun run build

Expected result after implementation: the formatter exits successfully, the Vitest run reports all item-detail tests passing, and the production build completes successfully.

## Validation and Acceptance

Acceptance is behavioral. Open the item detail route through the test harness or the running UI and confirm that the page still shows:

- the title and description for the current revision,
- operator action buttons when the evaluation allows them,
- the three summary panels for state, evaluation, and revision,
- the jobs table with retry and cancel controls when applicable,
- findings and convergences tables when data is present,
- revision context and diagnostics sections when returned by the backend.

Run `cd /Users/aa/Documents/ingot/ui && bunx vitest run src/test/item-detail-page.test.tsx` and expect the focused route tests to pass. The new test should fail before the extraction if any section is accidentally omitted and pass after the extraction is complete.

Run `cd /Users/aa/Documents/ingot/ui && bun run build` and expect the production bundle to complete successfully. A failure in an item-detail file would indicate the split introduced a broken import or export.

## Idempotence and Recovery

This refactor is safe to repeat because it only reorganizes UI component structure without schema or backend changes. If a step fails, rerun the formatter or the targeted test command after correcting the file. Recovery is by normal source control inspection rather than destructive reset commands because the working tree already contains unrelated in-progress work.

## Artifacts and Notes

Useful inspection commands used while preparing this refactor:

    git diff -- ui/src/pages/ItemDetailPage.tsx
    sed -n '1,260p' ui/src/test/item-detail-page.test.tsx

## Interfaces and Dependencies

`ui/src/pages/ItemDetailPage.tsx` must continue to use `@tanstack/react-query` for `useQuery`, `useMutation`, and `useQueryClient`, and it must continue to use `react-router` for `useParams`.

`ui/src/components/item-detail/index.ts` should export at least these components:

    OperatorActions
    OverviewPanels
    AcceptanceCriteriaSection
    JobsTable
    FindingsTable
    ConvergencesTable
    RevisionContextPanel
    DiagnosticsSection

`JobsTable` should keep using the existing retry and cancel APIs from `ui/src/api/client.ts` through `ui/src/components/item-detail/JobActions.tsx` so the button behavior stays isolated from the page.

Revision note: created this ExecPlan before implementation to satisfy the repository requirement for significant refactors and to capture the intended extraction boundary.

Revision note: updated after implementation to record the successful item-detail test run and the unrelated `root-layout` TypeScript build failure discovered during validation.

Revision note: updated again after the component module grew too large, replacing the single-file section module with focused item-detail component files and refreshing the validation steps to match.
