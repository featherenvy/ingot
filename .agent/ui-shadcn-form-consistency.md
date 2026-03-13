# UI Consistency Pass: shadcn forms, destructive confirmation dialogs, and inline error alerts

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with `.agent/PLANS.md`.

## Purpose / Big Picture

After this change, the UI will present forms, destructive actions, and inline failures through one consistent shadcn-based pattern. Users will see labeled fields with accessible validation wiring, explicit confirmation dialogs before workspace-destructive actions, and `Alert`-styled inline errors instead of raw paragraphs. The visible proof is in the create/register dialogs and sheets, the workspace action flow, and the item-detail error state.

## Progress

- [x] (2026-03-13 15:45Z) Reviewed the affected UI pages, available shadcn primitives, current tests, and dependency state.
- [x] (2026-03-13 15:50Z) Wrote the implementation plan and locked scope: migrate project, board, and config forms; replace workspace timeout confirmation; standardize touched inline errors.
- [x] (2026-03-13 16:20Z) Added shadcn `label`, `form`, and `alert-dialog` wrappers, updated shared card/input/textarea primitives, and declared `react-hook-form`.
- [x] (2026-03-13 16:28Z) Migrated the project, board, and config forms to shadcn `Form` patterns with field-level required messages.
- [x] (2026-03-13 16:31Z) Replaced workspace timeout confirmation with `AlertDialog` and removed `ui/src/hooks/useConfirmAction.ts`.
- [x] (2026-03-13 16:34Z) Standardized touched inline error states on `Alert`, updated UI tests, and passed `make ui-test` and `make ui-build`.

## Surprises & Discoveries

- Observation: `react-hook-form` is not currently installed, so `FormField` adoption requires a real dependency addition rather than only view-layer edits.
  Evidence: `ui/package.json` does not list `react-hook-form`.
- Observation: the existing UI already includes `Alert`, `Card`, and `sonner`, so most of the consistency work is compositional rather than visual redesign.
  Evidence: `ui/src/components/ui/alert.tsx`, `ui/src/components/ui/card.tsx`, and `ui/src/components/ui/sonner.tsx` are already present.

## Decision Log

- Decision: Scope includes the same raw-label and inline-error patterns already present on `ProjectsPage` and Config row actions, not only the pages named in the review notes.
  Rationale: stopping at only the cited files would leave the new patterns visibly inconsistent within the same UI surface.
  Date/Author: 2026-03-13 / Codex
- Decision: Use full shadcn `Form` plus `react-hook-form` now instead of a label-only pass.
  Rationale: the review specifically called out missing validation state, `aria-describedby` wiring, and field-level message handling, which are not solved by swapping labels alone.
  Date/Author: 2026-03-13 / Codex
- Decision: Use `AlertDialog` for reset, abandon, and remove workspace actions.
  Rationale: the prior timeout-confirm flow is invisible and hard to explain; a modal confirmation is explicit and aligns with the existing design system.
  Date/Author: 2026-03-13 / Codex

## Outcomes & Retrospective

The implemented result matches the original purpose. The project, board, and config forms now share one RHF-backed shadcn form pattern; workspace reset, abandon, and remove now require explicit dialog confirmation; and the touched inline failure states render through `Alert` instead of raw text. Validation proved clean through `make ui-test` and `make ui-build`.

## Context and Orientation

The frontend lives in `ui/`. The affected pages are `ui/src/pages/ProjectsPage.tsx`, `ui/src/pages/BoardPage.tsx`, `ui/src/pages/ConfigPage.tsx`, `ui/src/pages/WorkspacesPage.tsx`, and `ui/src/pages/ItemDetailPage.tsx`. The shared shadcn primitives live in `ui/src/components/ui/`. Today, forms on the project, board, and config pages are local `useState` objects rendered with raw `<label>` tags. Workspace destructive actions use `ui/src/hooks/useConfirmAction.ts`, which sets a three-second confirmation window without visible countdown state. Several inline failures still render as raw text instead of the existing `Alert` primitive.

In this repository, a “form primitive” means a reusable component such as `FormField`, `FormLabel`, `FormControl`, and `FormMessage` that connects a field to validation state and accessibility attributes. A “destructive confirmation dialog” means a modal that forces the user to explicitly confirm an action such as deleting or abandoning a workspace before the mutation runs.

## Plan of Work

First, add the missing shadcn wrappers in `ui/src/components/ui/` and declare `react-hook-form` in `ui/package.json`. Keep these wrappers aligned with the rest of the repo’s `radix-ui`-based components so they share utility helpers and styling conventions.

Next, migrate the three user-input flows from local `useState` to `useForm` and shadcn `FormField` composition. Preserve the current API payloads, pending-button behavior, success toasts, and dialog-close behavior. Required fields should keep their current required semantics only; do not introduce new product validation rules.

After that, refactor `ui/src/pages/WorkspacesPage.tsx` to replace `useConfirmAction` with a single per-row `AlertDialog` that is populated from the selected action. Keep the dropdown menu as the entry point, but selecting reset, abandon, or remove should open the dialog and only run the mutation from the confirm button.

Finally, convert touched inline failures to `Alert` and update tests in `ui/src/test/` so they exercise the new dialog and form wiring. Run `make ui-test` and `make ui-build` from the repository root to prove the UI still works.

## Concrete Steps

From `/Users/aa/Documents/ingot`:

    bun install --cwd ui
    make ui-test
    make ui-build

Expected validation outcome after the code changes:

    $ make ui-test
    ...
    Test Files  ... passed

    $ make ui-build
    ...
    vite v... building for production...
    ...
    ✓ built in ...

## Validation and Acceptance

Open the UI and verify these behaviors:

1. The project registration dialog, item creation sheet, and agent registration dialog all render labels through the shared form primitives and show inline field messages when required fields are missing.
2. Selecting Reset, Abandon, or Remove from a workspace row opens an `AlertDialog` with clear destructive-action copy; confirming runs the mutation and success still appears via toast.
3. If item detail loading fails, the page renders a destructive `Alert` instead of raw text.
4. If a workspace or agent row action fails, the failure renders through `Alert`.
5. Board lane cards remain full-row links and now show focus-visible treatment through the shared card primitive.

## Idempotence and Recovery

The dependency install and test commands are safe to rerun. If the lockfile changes unexpectedly, review only the `ui` package portion; do not revert unrelated workspace edits. If a partial implementation leaves `useConfirmAction` unused, delete the file in the same change to avoid dead code.

## Artifacts and Notes

Relevant existing files before the change:

    ui/src/pages/BoardPage.tsx
    ui/src/pages/ConfigPage.tsx
    ui/src/pages/ProjectsPage.tsx
    ui/src/pages/WorkspacesPage.tsx
    ui/src/pages/ItemDetailPage.tsx
    ui/src/hooks/useConfirmAction.ts

## Interfaces and Dependencies

In `ui/package.json`, add:

    "react-hook-form": "^7.x"

In `ui/src/components/ui/form.tsx`, define reusable exports compatible with the shadcn pattern:

    Form
    FormField
    FormItem
    FormLabel
    FormControl
    FormDescription
    FormMessage
    useFormField

In `ui/src/components/ui/alert-dialog.tsx`, define the standard wrapper set:

    AlertDialog
    AlertDialogTrigger
    AlertDialogContent
    AlertDialogHeader
    AlertDialogFooter
    AlertDialogTitle
    AlertDialogDescription
    AlertDialogAction
    AlertDialogCancel

At the bottom of this file, contributors must append a note when the plan changes and why.

Revision note: Initial implementation plan created on 2026-03-13 to cover form primitive adoption, destructive confirmation dialogs, and touched inline error-state standardization.
Revision note: Updated on 2026-03-13 after implementation to record completed milestones, successful validation, and the final behavior outcome.
