# Adopt missing shadcn primitives for core UI flows

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows the repository guidance in `.agent/PLANS.md` and must be maintained in accordance with that file.

## Purpose / Big Picture

After this change, the React UI will use the missing shadcn primitives that materially improve navigation, loading, and operator workflows. Users will open create forms in proper dialogs and sheets instead of pushing the page layout around, project navigation will expose real tab semantics, long payloads and logs will scroll through shared containers, and transient feedback will arrive through a single toast system instead of inconsistent inline status text.

## Progress

- [x] (2026-03-13 13:15Z) Inspected the current UI routes, shared components, tests, and existing shadcn setup to map each requested primitive to the actual pages that need it.
- [x] (2026-03-13 13:21Z) Scaffolded the missing shadcn primitives, added Sonner support, mounted the global toaster, and wrapped the app in `TooltipProvider`.
- [x] (2026-03-13 13:43Z) Refactored the affected pages and shared components to use dialogs, a sheet, tabs, tooltips, scroll areas, collapsibles, dropdown menus, skeletons, and transient toast feedback without changing API contracts or route structure.
- [x] (2026-03-13 13:59Z) Updated existing UI tests and added new route tests for the board sheet, project tabs, and workspace dropdown confirm flow.
- [x] (2026-03-13 14:02Z) Ran `make ui-test`, `make ui-build`, and `bun run lint` in `ui/`; frontend validation passed.
- [x] (2026-03-13 14:03Z) Ran `make lint` and confirmed the remaining failure is unrelated Rust clippy debt in `crates/ingot-http-api/src/router.rs`.

## Surprises & Discoveries

- Observation: The current UI already uses the shadcn-generated `radix-ui` aggregator package rather than separate `@radix-ui/*` packages.
  Evidence: `ui/src/components/ui/button.tsx` imports `Slot` from `radix-ui`, and the installed module exports `Dialog`, `DropdownMenu`, `Tooltip`, `Tabs`, `Select`, `ScrollArea`, and `Collapsible`.

- Observation: Several files under `ui/` are already dirty in the working tree, including the exact pages this pass needs to modify.
  Evidence: `git status --short` shows existing modifications in `ui/src/main.tsx`, `ui/src/layouts/ProjectLayout.tsx`, `ui/src/pages/BoardPage.tsx`, `ui/src/pages/ConfigPage.tsx`, `ui/src/pages/ProjectsPage.tsx`, and others, so the implementation must avoid overwriting unrelated edits.

- Observation: Radix tooltip and Sonner primitives need additional browser API shims in the Vitest environment.
  Evidence: the first `make ui-test` run failed because page tests rendered `Tooltip` without `TooltipProvider`, and Sonner attempted to call `window.matchMedia`; adding shared test wrappers plus `matchMedia`, `scrollIntoView`, and pointer-capture shims in `ui/src/test/setup.ts` resolved the failures.

- Observation: The generated Sonner template pulled in `next-themes`, but the app does not provide a theme context.
  Evidence: the generated `ui/src/components/ui/sonner.tsx` imported `useTheme` from `next-themes`; the final implementation removed that dependency and uses Sonner’s `theme="system"` directly.

## Decision Log

- Decision: Use the shadcn CLI for primitive scaffolding, then patch the generated files manually for this app’s route-driven and testable behaviors.
  Rationale: The repo already uses shadcn-generated source files, so the CLI keeps the primitives consistent while still allowing project-specific integration logic.
  Date/Author: 2026-03-13 / Codex

- Decision: Keep toast usage limited to transient success and informational feedback, while retaining inline alerts for actionable failures and queue blockers.
  Rationale: This aligns with the product direction chosen during planning and avoids hiding errors that need persistent context.
  Date/Author: 2026-03-13 / Codex

- Decision: Use a small `TooltipValue` helper and shared skeleton helpers instead of repeating tooltip wiring and loading markup across pages.
  Rationale: The same shortened-value tooltip pattern and page-shaped loading shells now appear in multiple pages, so a local helper keeps the route components readable without introducing a heavy abstraction layer.
  Date/Author: 2026-03-13 / Codex

- Decision: Keep `WorkspacesPage` confirmation state in the row and prevent dropdown menu closure on the first action selection.
  Rationale: This preserves the existing confirm-before-act behavior while still collapsing the three visible action buttons into one menu trigger.
  Date/Author: 2026-03-13 / Codex

## Outcomes & Retrospective

The UI now has the missing shadcn primitives wired into the highest-friction routes. Project navigation renders as actual tabs, the board item form opens in a sheet, the projects and config create flows open in dialogs, the config provider is constrained through a select, long logs and JSON payloads scroll through shared containers, and transient success states are delivered through Sonner toasts instead of scattered inline green text.

The frontend validation target was met. `make ui-test`, `make ui-build`, and `bun run lint` in `ui/` all pass. The repo-wide `make lint` target still fails, but the failure is outside this UI work: clippy reports an existing `collapsible_if` warning promoted to error in `crates/ingot-http-api/src/router.rs`.

## Context and Orientation

The Vite React app lives under `ui/`. Shared shadcn components live in `ui/src/components/ui/`. The top-level app provider tree is in `ui/src/main.tsx`. Route shells live in `ui/src/layouts/`, with `ProjectLayout.tsx` currently simulating tabs by styling `NavLink` buttons. The pages that need the largest behavior changes are `ui/src/pages/BoardPage.tsx`, `ui/src/pages/ProjectsPage.tsx`, `ui/src/pages/ConfigPage.tsx`, `ui/src/pages/WorkspacesPage.tsx`, `ui/src/pages/JobsPage.tsx`, and `ui/src/pages/ActivityPage.tsx`. Shared display components that need primitive adoption include `ui/src/components/Timestamp.tsx`, `ui/src/components/LogBlock.tsx`, `ui/src/components/item-detail/JobsTable.tsx`, `ui/src/components/item-detail/ConvergencesTable.tsx`, `ui/src/components/item-detail/JobActions.tsx`, and `ui/src/components/item-detail/OperatorActions.tsx`.

In this repository, “shadcn primitive” means a local component source file checked into `ui/src/components/ui/`, not a remote package import. “Toast” refers to Sonner’s transient notification UI mounted globally through a single top-level `<Toaster />`.

## Plan of Work

First, add the missing primitives and Sonner support. This includes generating `dialog`, `sheet`, `select`, `tabs`, `tooltip`, `skeleton`, `dropdown-menu`, `scroll-area`, `collapsible`, and `sonner`, then mounting the toaster in `ui/src/main.tsx`.

Next, refactor navigation and creation flows. `ProjectLayout.tsx` will become route-driven tabs, `BoardPage.tsx` will move item creation into a sheet, and `ProjectsPage.tsx` plus `ConfigPage.tsx` will move their create forms into dialogs. `ConfigPage.tsx` will also constrain the provider field through a `Select`.

Then, migrate the supporting display and action patterns. The current `title` attributes will be replaced with tooltips for truncated or full-value disclosure. `LogBlock.tsx` and long JSON blocks will use `ScrollArea`. `ActivityPage.tsx` will replace its hand-rolled payload disclosure with shadcn `Collapsible`. `WorkspacesPage.tsx` will collapse row actions into a `DropdownMenu` while preserving the current confirm-before-act interaction.

Finally, replace spinner-only loading states with page-shaped and section-shaped skeletons, move transient success feedback to toast, update the UI tests, and run build and test validation.

## Concrete Steps

From the repository root `/Users/aa/Documents/ingot`, run:

    cd /Users/aa/Documents/ingot/ui && bunx shadcn@latest add dialog sheet select tabs tooltip skeleton dropdown-menu scroll-area collapsible sonner -y
    cd /Users/aa/Documents/ingot/ui && bun add sonner

Then patch the affected source files under `ui/src/` to wire those primitives into the current routes and components. After the code changes, run:

    cd /Users/aa/Documents/ingot && make ui-test
    cd /Users/aa/Documents/ingot && make ui-build
    cd /Users/aa/Documents/ingot && make lint

Expected results: the UI tests pass, the UI production build succeeds, and lint remains green for the touched UI files. Any unrelated pre-existing failures must be documented explicitly.

Actual commands run during implementation:

    cd /Users/aa/Documents/ingot/ui && bunx shadcn@latest add dialog sheet select tabs tooltip skeleton dropdown-menu scroll-area collapsible sonner -y
    cd /Users/aa/Documents/ingot/ui && bun remove next-themes
    cd /Users/aa/Documents/ingot/ui && bunx @biomejs/biome check --write src
    cd /Users/aa/Documents/ingot && make ui-test
    cd /Users/aa/Documents/ingot && make ui-build
    cd /Users/aa/Documents/ingot/ui && bun run lint
    cd /Users/aa/Documents/ingot && make lint

## Validation and Acceptance

Acceptance is behavioral. After implementation:

- The board route opens “New item” in a side sheet instead of inserting a full card above the columns.
- The projects and config routes open their create forms in dialogs.
- The config provider field only offers `openai` and `anthropic`.
- Project navigation exposes tabs with tab semantics and keeps route navigation intact.
- Long log and JSON panels scroll inside shared containers.
- Activity payloads expand and collapse through proper disclosure semantics.
- Workspace actions are reachable from a row menu and still require confirmation before mutating.
- Transient success states show toasts instead of scattered inline green text.
- Full page loads render skeleton placeholders instead of bare spinners.

Observed result: all UI behaviors above are implemented and covered by the updated Vitest suite. The frontend build succeeds. The only remaining validation blocker is the unrelated clippy warning in `crates/ingot-http-api/src/router.rs:2473`.

## Idempotence and Recovery

The shadcn CLI commands are safe to re-run if a primitive needs to be regenerated. The implementation is confined to `ui/` and `.agent/`, so recovery is by ordinary source inspection and targeted patching rather than destructive resets. If a generated component is unsuitable, patch the local source file rather than re-running broad generation steps.

## Artifacts and Notes

Key discovery commands used before implementation:

    git status --short
    find ui/src/components -maxdepth 3 -type f | sort
    sed -n '1,300p' ui/src/pages/ConfigPage.tsx
    sed -n '1,240p' ui/src/layouts/ProjectLayout.tsx
    node -e "const r=require('./ui/node_modules/radix-ui'); console.log(Object.keys(r).filter(k=>/Dialog|DropdownMenu|Tooltip|Tabs|Select|ScrollArea|Collapsible/.test(k)).sort().join('\n'))"

## Interfaces and Dependencies

The resulting UI contains local primitives for `dialog`, `sheet`, `select`, `tabs`, `tooltip`, `skeleton`, `dropdown-menu`, `scroll-area`, `collapsible`, and `sonner` under `ui/src/components/ui/`. `ui/src/main.tsx` renders a global toaster and wraps the app in `TooltipProvider`. No backend or domain API contract changes are part of this work.

Revision note: created before implementation to capture the execution path, current repo constraints, and the required validation targets for this cross-cutting UI pass.

Revision note: updated after implementation to record the completed frontend work, the added UI test coverage, the successful frontend validation commands, and the unrelated Rust clippy failure surfaced by `make lint`.
