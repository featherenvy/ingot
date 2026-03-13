# Unify status badges, selection UIs, and mutation confirmations

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows the repository guidance in `.agent/PLANS.md` and must be maintained in accordance with that file.

## Purpose / Big Picture

After this change, operators will see the same status pill everywhere a job, workspace, item, convergence, or agent state is shown, including icon cues for quick scanning. The config dialog will use searchable comboboxes that still support freeform model entry, and the mutation-heavy item-detail actions will ask for confirmation before destructive or hard-to-reverse operations. The result should be visible on the config page, jobs page, workspaces page, board page, projects page, and item-detail page.

## Progress

- [x] (2026-03-13 17:55Z) Audited current status badge usage, visual indicator duplication, config selection controls, and mutation actions across pages and shared item-detail components.
- [x] (2026-03-13 18:00Z) Wrote this ExecPlan and fixed the implementation scope to shared UI primitives first, page migrations second, and targeted frontend validation last.
- [x] (2026-03-13 18:12Z) Added shared `StatusBadge`, `ProjectColorDot`, and `ActivityPulse` components and migrated the relevant status pills and raw dot/ping markup across dashboard, jobs, workspaces, config, project layout, projects, and item-detail views.
- [x] (2026-03-13 18:18Z) Added local `Popover` and `Command` primitives plus a reusable combobox, installed `cmdk`, and upgraded the config registration dialog to searchable provider/model selection with custom model entry.
- [x] (2026-03-13 18:22Z) Added confirmation dialogs to job cancellation and approval rejection in item-detail actions.
- [x] (2026-03-13 18:26Z) Updated frontend tests, added the required `ResizeObserver` Vitest shim for `cmdk`, and passed `make ui-test`, `make ui-build`, and `bun run lint`.

## Surprises & Discoveries

- Observation: The current repo has `Popover` support through the `radix-ui` aggregator package, but no local `popover.tsx` wrapper and no `Command` primitive or `cmdk` dependency.
  Evidence: `node -e "const r=require('./ui/node_modules/radix-ui'); console.log(Object.keys(r).filter(k=>/Popover/i.test(k)).sort().join('\n'))"` prints `Popover`, while the same query for `Command` prints `NO_COMMAND`.

- Observation: “Item status” is currently fragmented across several fields rather than one badge.
  Evidence: `ui/src/components/item-detail/OverviewPanels.tsx` renders lifecycle, parking, approval, escalation, and board state separately, mostly as raw strings or ad-hoc badges.

- Observation: The most obvious raw visual indicator duplication is between `ProjectsPage` and `ProjectLayout`, not `RootLayout`.
  Evidence: both files render the same colored dot markup with `className="size-3 rounded-full border border-black/10"` and an inline `backgroundColor` style.

- Observation: `cmdk` requires `ResizeObserver`, which is not available in the current Vitest jsdom setup by default.
  Evidence: the first `make ui-test` run after adding the combobox failed in `src/test/config-page.test.tsx` with `ReferenceError: ResizeObserver is not defined` until `ui/src/test/setup.ts` defined a stub.

## Decision Log

- Decision: Keep the existing `statusVariant()` helper as a compatibility layer, but move the canonical mapping into a richer status-presentation helper used by `StatusBadge`.
  Rationale: This lets the new badge own icons and semantics without forcing every existing caller to update at once.
  Date/Author: 2026-03-13 / Codex

- Decision: Use a searchable combobox with optional custom entry for model selection, while keeping provider selection constrained to known values.
  Rationale: Provider values are bounded and should stay explicit; model names are open-ended and need a “use custom value” escape hatch.
  Date/Author: 2026-03-13 / Codex

- Decision: Add confirmation only to actions that are destructive or hard to reverse in the current UI pass.
  Rationale: The user called out cancel-job and rework-style operator actions specifically. Dispatch and approve stay one-click to avoid unnecessary friction.
  Date/Author: 2026-03-13 / Codex

## Outcomes & Retrospective

The UI now has one shared status-pill abstraction with icon treatment, and the obvious one-off visual markers have been replaced by shared components. Jobs, workspaces, agents, item state, and convergence state now render through `StatusBadge`, while project color markers and the board activity pulse are no longer page-local `<span>` fragments.

The config registration dialog now uses a proper searchable combobox stack built from local shadcn-style `Popover` and `Command` wrappers, with provider selection constrained to known values and model selection supporting custom typed entries. Item-detail cancellation and approval rejection now require confirmation. Validation finished cleanly with passing tests, build, and lint.

## Context and Orientation

The frontend route pages live under `ui/src/pages/`, shared item-detail components live under `ui/src/components/item-detail/`, and low-level local shadcn wrappers live under `ui/src/components/ui/`. In this repository, a “status badge” means a pill-shaped label used for a machine state such as `running`, `failed`, `available`, or `prepared`. A “combobox” means the shadcn pattern that combines a popover trigger with a searchable command list. A “confirmation dialog” means a blocking `AlertDialog` that the operator must accept before a mutation request is sent.

The main files involved in this pass are:

- `ui/src/lib/status.ts` for the shared status-to-variant and icon mapping.
- `ui/src/pages/JobsPage.tsx`, `ui/src/pages/WorkspacesPage.tsx`, `ui/src/pages/ConfigPage.tsx`, and `ui/src/components/item-detail/JobsTable.tsx` for current raw badge usage.
- `ui/src/components/item-detail/OverviewPanels.tsx` and `ui/src/components/item-detail/ConvergencesTable.tsx` for item and convergence status display.
- `ui/src/pages/BoardPage.tsx`, `ui/src/pages/ProjectsPage.tsx`, and `ui/src/layouts/ProjectLayout.tsx` for duplicated visual indicators.
- `ui/src/components/item-detail/JobActions.tsx` and `ui/src/components/item-detail/OperatorActions.tsx` for the mutation confirmation work.
- `ui/src/test/config-page.test.tsx`, `ui/src/test/item-detail-page.test.tsx`, `ui/src/test/jobs-page.test.tsx`, and `ui/src/test/project-layout.test.tsx` for regression coverage.

## Plan of Work

First, add shared presentation components. `StatusBadge` will live in `ui/src/components/` and consume a richer status-presentation helper from `ui/src/lib/status.ts`. Small visual indicators will also move into shared components so the board active ping and project color dot stop being raw `<span>` markup inside pages.

Next, add local `ui/src/components/ui/popover.tsx` and `ui/src/components/ui/command.tsx` wrappers and install `cmdk` in the `ui/` package. Then create a reusable combobox component that supports filtering and, for model selection, optional custom values. `ConfigPage.tsx` will use those controls instead of the current `Select` plus freeform text input pairing.

Then, add confirmation dialogs to `JobActions.tsx` and `OperatorActions.tsx` for cancel-job and reject/rework style item actions. The dialogs should follow the same visual language already used in `WorkspacesPage.tsx`.

Finally, update the route and item-detail tests, plus add a focused shared-component test where it materially reduces regression risk. Then run the frontend test, build, and lint commands.

## Concrete Steps

From the repository root `/Users/aa/Documents/ingot`, edit the affected files and this plan. Because the combobox needs `cmdk`, install it from the `ui/` directory:

    cd /Users/aa/Documents/ingot/ui && bun add cmdk

Then run the frontend validation targets:

    cd /Users/aa/Documents/ingot && make ui-test
    cd /Users/aa/Documents/ingot && make ui-build
    cd /Users/aa/Documents/ingot/ui && bun run lint

Commands actually run during implementation:

    cd /Users/aa/Documents/ingot/ui && bun add cmdk
    cd /Users/aa/Documents/ingot && make ui-test
    cd /Users/aa/Documents/ingot && make ui-build
    cd /Users/aa/Documents/ingot/ui && bunx @biomejs/biome check --write src/components/ConfirmActionButton.tsx src/components/ProjectColorDot.tsx src/components/StatusBadge.tsx src/components/item-detail/JobsTable.tsx src/components/ui/command.tsx src/lib/status.ts src/pages/ConfigPage.tsx src/pages/JobsPage.tsx
    cd /Users/aa/Documents/ingot/ui && bun run lint
    cd /Users/aa/Documents/ingot && make ui-test

## Validation and Acceptance

Acceptance is behavioral.

- Job, workspace, agent, item, and convergence statuses render through one shared `StatusBadge` component with consistent color and icon treatment.
- The project color dot and board active pulse no longer use page-local raw indicator markup.
- The config registration dialog uses searchable comboboxes for provider and model selection, and model selection still allows a custom value.
- Cancel-job and reject-approval style actions now open confirmation dialogs before sending the mutation.
- Updated frontend tests pass, and the production build plus Biome lint remain green.

Observed result: all acceptance conditions above are implemented. `make ui-test`, `make ui-build`, and `bun run lint` all pass as of 2026-03-13.

## Idempotence and Recovery

These edits are confined to `ui/` and `.agent/`. Re-running `bun add cmdk` is safe because Bun will leave the dependency unchanged when already installed. If the combobox work fails midway, recovery is to keep the previous field inputs until the shared command/popover primitives compile; avoid broad resets because the UI working tree is already dirty in adjacent files.

## Artifacts and Notes

Key inspection commands used before implementation:

    rg -n "<Badge|statusVariant\\(|board_status|lifecycle_state|approval_state|animate-ping|rounded-full border border-black/10" ui/src/pages ui/src/components -g '!ui/src/components/ui/*'
    sed -n '1,260p' ui/src/lib/status.ts
    sed -n '1,260p' ui/src/components/item-detail/JobActions.tsx
    sed -n '1,260p' ui/src/pages/ConfigPage.tsx
    sed -n '1,260p' ui/src/components/item-detail/OverviewPanels.tsx

## Interfaces and Dependencies

At the end of this change, the frontend should contain:

- `ui/src/components/StatusBadge.tsx`
- `ui/src/components/ProjectColorDot.tsx`
- `ui/src/components/ActivityPulse.tsx`
- `ui/src/components/ui/popover.tsx`
- `ui/src/components/ui/command.tsx`

`ui/package.json` and `ui/bun.lock` should include `cmdk`. No backend API contracts change in this pass.

Revision note: created before implementation to capture the shared-status, combobox, and confirmation scope for the second UI consistency pass.

Revision note: updated after implementation to record the shared status and indicator components, the `cmdk`-backed combobox stack, the new confirmation dialogs, the `ResizeObserver` test shim, and the final passing frontend validation commands.
