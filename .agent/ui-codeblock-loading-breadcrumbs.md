# Add shared code blocks, loading affordances, and item breadcrumbs

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows the repository guidance in `.agent/PLANS.md` and must be maintained in accordance with that file.

## Purpose / Big Picture

After this change, operators will see the same styled code surface everywhere the UI shows JSON, YAML-like config text, acceptance criteria, or agent logs. Every active data-fetch boundary touched by this pass will show a skeleton or spinner instead of silently changing state, and the nested item-detail route will expose breadcrumbs so it is obvious how to get back to the board. The result should be visible by opening the config page, jobs page, activity page, and an item detail page in the UI.

## Progress

- [x] (2026-03-13 17:23Z) Audited the current UI for duplicated `<pre>` rendering, current loading treatment, nested item routing, and the relevant Vitest coverage.
- [x] (2026-03-13 17:31Z) Wrote this ExecPlan and fixed the scope to the concrete files already using raw code surfaces or missing visible loading state.
- [x] (2026-03-13 17:42Z) Added a shared `CodeBlock` component and migrated `ActivityPage`, `ConfigPage`, `LogBlock`, and `AcceptanceCriteriaSection` onto it.
- [x] (2026-03-13 17:42Z) Added breadcrumb primitives, rendered breadcrumbs on `ItemDetailPage`, and aligned project-tab highlighting so item routes keep the board tab active.
- [x] (2026-03-13 17:43Z) Added explicit loading affordances for root health, project header resolution, activity pagination, and item-detail agent availability checks.
- [x] (2026-03-13 17:47Z) Updated Vitest coverage, ran `make ui-test`, `make ui-build`, and `bun run lint`, and confirmed all frontend validation targets pass.

## Surprises & Discoveries

- Observation: The current route tree has no standalone item list route, so item-detail breadcrumbs cannot link to an `Items` index page.
  Evidence: `ui/src/App.tsx` defines `/projects/:projectId/items/:itemId`, but the available sibling project routes are dashboard, board, jobs, workspaces, activity, and config.

- Observation: The project tabs currently fall back to `dashboard` for item-detail routes, which makes the nested page look detached from the board flow.
  Evidence: `ui/src/layouts/ProjectLayout.tsx` assigns `dashboard` unless the pathname starts with `/board`, `/jobs`, `/workspaces`, `/activity`, or `/config`.

- Observation: The raw code rendering duplication is limited and well-bounded.
  Evidence: `rg -n "<pre" ui/src` only returns `ui/src/pages/ActivityPage.tsx`, `ui/src/pages/ConfigPage.tsx`, `ui/src/components/LogBlock.tsx`, and `ui/src/components/item-detail/AcceptanceCriteriaSection.tsx`.

- Observation: Biome rejects `role="status"` on generic `div` and `span` elements in this codebase and prefers semantic output elements for short live status text.
  Evidence: `bun run lint` reported `lint/a11y/useSemanticElements` on the first loading-state implementation until those regions were changed to `<output aria-live="polite">`.

## Decision Log

- Decision: Use a shared `CodeBlock` component with copy-to-clipboard and keep activity payload disclosure logic outside the component.
  Rationale: The duplicated styling and clipboard behavior should be centralized, but `ActivityPage` still needs its own expand/collapse rule based on payload length.
  Date/Author: 2026-03-13 / Codex

- Decision: Treat the item-detail route as part of the board flow for both breadcrumbs and tab highlighting.
  Rationale: There is no dedicated items index route, and the board is the only in-product list that leads into item detail.
  Date/Author: 2026-03-13 / Codex

- Decision: Limit the loading-state pass to boundaries that currently show plain text or no visible loading affordance.
  Rationale: Several pages already have full skeleton screens. This pass should close the obvious gaps without rewriting already-consistent sections.
  Date/Author: 2026-03-13 / Codex

## Outcomes & Retrospective

The frontend now has one shared structured-text surface with copy-to-clipboard behavior, and the duplicated raw `<pre>` blocks are gone from the audited pages and components. Item detail now shows breadcrumbs back to the board, and project tabs no longer misleadingly highlight the dashboard when an item route is open.

The loading-state pass stayed scoped to the gaps that were actually inconsistent: the daemon health badge, the project header query, activity pagination, and item-detail agent-availability checks. Existing full-page skeletons were kept in place. Validation finished cleanly with passing Vitest, production build, and Biome lint runs.

## Context and Orientation

The frontend lives under `ui/src/`. Route components are under `ui/src/pages/`, route shells are under `ui/src/layouts/`, and shared presentational components live in `ui/src/components/`. In this repository, a “code block” means any scrollable or wrapped monospace surface used to show logs, JSON, or structured text. A “loading boundary” means a distinct query-backed region where the user should see either placeholder skeletons during the first load or a spinner while a smaller subsection refreshes.

The files directly involved in this pass are:

- `ui/src/pages/ActivityPage.tsx` for JSON payload display and page-to-page fetch feedback.
- `ui/src/pages/ConfigPage.tsx` for the project-defaults JSON panel.
- `ui/src/components/LogBlock.tsx` for job prompt/stdout/stderr/result rendering.
- `ui/src/components/item-detail/AcceptanceCriteriaSection.tsx` for revision acceptance criteria.
- `ui/src/pages/ItemDetailPage.tsx` for breadcrumbs and agent-availability loading.
- `ui/src/layouts/ProjectLayout.tsx` for tab selection and project-header loading treatment.
- `ui/src/layouts/RootLayout.tsx` for the daemon health badge loading treatment.
- `ui/src/test/activity-page.test.tsx`, `ui/src/test/config-page.test.tsx`, and `ui/src/test/item-detail-page.test.tsx` for regression coverage.

## Plan of Work

First, add two shared UI primitives under `ui/src/components/`: a `CodeBlock` for consistent structured-text display and a local shadcn-style breadcrumb primitive under `ui/src/components/ui/`. `CodeBlock` will render a bordered surface, optional wrapping, a copy button, and a scrollable content area. The breadcrumb primitive will provide the semantic building blocks for the item-detail header.

Next, migrate the existing raw `<pre>` call sites. `ConfigPage` will render project defaults through `CodeBlock` with horizontal scrolling preserved. `LogBlock` and `AcceptanceCriteriaSection` will use the same component with wrapping enabled. `ActivityPage` will keep its collapsible disclosure logic, but each payload branch will render a `CodeBlock` instead of hand-written `<pre>` tags.

Then, address the missing loading states. `RootLayout` will show a compact spinner in the daemon health badge while the health query is unresolved. `ProjectLayout` will show skeletons for the project title/description while the projects query resolves. `ActivityPage` pagination will show a compact spinner when changing pages, and `ItemDetailPage` will surface a small loading indicator while the agent list is still resolving for queue-blocker logic.

Finally, add breadcrumbs to `ItemDetailPage`, update `ProjectLayout` so item routes highlight the board tab, expand tests for breadcrumbs and copy affordances, and run targeted frontend validation.

## Concrete Steps

From the repository root `/Users/aa/Documents/ingot`, edit the affected files under `ui/src/` and this plan file. Then run:

    cd /Users/aa/Documents/ingot && bun test --version
    cd /Users/aa/Documents/ingot && make ui-test
    cd /Users/aa/Documents/ingot && make ui-build

If linting is needed for touched UI files, also run:

    cd /Users/aa/Documents/ingot/ui && bun run lint

Commands actually run during implementation:

    cd /Users/aa/Documents/ingot && make ui-test
    cd /Users/aa/Documents/ingot && make ui-build
    cd /Users/aa/Documents/ingot/ui && bun run lint
    cd /Users/aa/Documents/ingot/ui && bunx @biomejs/biome check --write src/components/item-detail/OperatorActions.tsx src/components/ui/breadcrumb.tsx src/pages/ActivityPage.tsx
    cd /Users/aa/Documents/ingot/ui && bun run lint
    cd /Users/aa/Documents/ingot && make ui-test

## Validation and Acceptance

Acceptance is behavioral.

- The config page, job log panels, activity payloads, and acceptance-criteria panel render through one shared `CodeBlock` surface and each offers a copy action.
- The activity page exposes a visible spinner when paginating, instead of only mutating footer text.
- The root header and project header show explicit loading treatment while their backing queries are unresolved.
- The item-detail page shows breadcrumbs leading back to the project board and highlights the board tab while viewing an item.
- The item-detail page shows a visible loading affordance while agent availability is still being resolved.
- Updated Vitest coverage passes, and the UI production build still succeeds.

Observed result: all acceptance conditions above are implemented. `make ui-test`, `make ui-build`, and `bun run lint` all pass as of 2026-03-13.

## Idempotence and Recovery

These edits are confined to `ui/` and `.agent/`. The work is safe to repeat by reapplying the same patches. If a test fails after partial changes, the recovery path is to finish migrating all call sites to the shared component or temporarily revert only the touched hunk with a targeted patch instead of resetting unrelated local work.

## Artifacts and Notes

Key inspection commands used before implementation:

    rg -n "<pre|whitespace-pre|font-mono" ui/src
    rg -n "useQuery\\(|isLoading|isFetching" ui/src/pages ui/src/layouts ui/src/components -g '!ui/src/components/ui/*'
    sed -n '1,260p' ui/src/pages/ActivityPage.tsx
    sed -n '1,260p' ui/src/pages/ItemDetailPage.tsx
    sed -n '1,260p' ui/src/layouts/ProjectLayout.tsx

## Interfaces and Dependencies

At the end of this change, the frontend should contain:

- `ui/src/components/CodeBlock.tsx` exporting `CodeBlock`.
- `ui/src/components/ui/breadcrumb.tsx` exporting breadcrumb building blocks compatible with local shadcn-style usage.
- `ui/src/pages/ItemDetailPage.tsx` rendering those breadcrumb components above the page header.

No backend API contracts change in this work.

Revision note: created before implementation to capture the concrete scope, route constraints, and validation targets for the shared code-block, loading-state, and breadcrumb pass.

Revision note: updated after implementation to record the completed shared `CodeBlock`, breadcrumb adoption, scoped loading-state fixes, Biome accessibility adjustment, and the final passing frontend validation commands.
