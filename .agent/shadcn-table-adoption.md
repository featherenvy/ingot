# Adopt shadcn table primitives for the UI tables

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows the repository guidance in `.agent/PLANS.md` and must be maintained in accordance with that file.

## Purpose / Big Picture

After this change, the React UI no longer repeats inline `thStyle` and `tdStyle` definitions across each table page. The application will have the `shadcn/ui` Table component installed in the existing Vite app, and the table-heavy pages will render through shared table primitives instead of copied inline styles. A contributor should be able to open the affected routes and see the same table data, but with a single reusable implementation for headers, cells, and table wrappers.

## Progress

- [x] (2026-03-13 10:00Z) Inspected the current UI stack and confirmed the Vite app has no Tailwind or CSS pipeline yet, so shadcn adoption requires setup work before the table refactor.
- [x] (2026-03-13 10:00Z) Verified the official shadcn Vite and Table documentation as the source for the setup direction.
- [x] (2026-03-13 10:06Z) Added Tailwind v4 and the Vite plugin, created a global stylesheet import, configured the `@/` alias, and completed `shadcn init` plus `shadcn add table`.
- [x] (2026-03-13 10:08Z) Migrated the affected table views to shadcn `Table*` primitives and moved OID truncation into `ui/src/lib/git.ts`.
- [x] (2026-03-13 10:09Z) Enabled Tailwind directive parsing in `ui/biome.json` so the repository formatter can handle shadcn’s generated CSS.
- [x] (2026-03-13 10:08Z) Fixed the unrelated `ExplodingPage` JSX typing issue in `ui/src/test/root-layout.test.tsx` so broader validation reflects the UI changes instead of a pre-existing test annotation problem.
- [x] (2026-03-13 10:08Z) Ran `bunx vitest run src/test/item-detail-page.test.tsx src/test/root-layout.test.tsx` and confirmed all five focused tests pass.
- [x] (2026-03-13 10:08Z) Ran `bun run build` in `ui/` and confirmed the production build succeeds.
- [x] (2026-03-13 10:29Z) Added the shadcn `card`, `input`, `textarea`, `badge`, `alert`, and `separator` primitives and migrated the app shell plus the form-heavy pages onto those components.
- [x] (2026-03-13 10:29Z) Reworked the jobs, workspaces, activity, and item detail panels to remove the remaining raw buttons and old inline surface styling in favor of shadcn cards, alerts, and buttons.
- [x] (2026-03-13 10:29Z) Added smoke tests for `ProjectsPage` and `ConfigPage`, and re-ran focused UI tests plus `bun run build` with all checks passing.

## Surprises & Discoveries

- Observation: The current UI relies almost entirely on inline styles and imports no CSS file from `src/main.tsx`.
  Evidence: `ui/src/main.tsx` imports only `App` and query providers, and `find ui/src -maxdepth 2 -name '*.css'` returns no application stylesheet.

- Observation: Adopting shadcn here is a foundational UI change because shadcn components require Tailwind-generated utility classes.
  Evidence: The `ui/package.json` dependencies currently include React, React Query, React Router, and Zustand, but not Tailwind, `class-variance-authority`, or related shadcn support packages.

- Observation: The current shadcn CLI still prompts for a preset even when invoked with non-interactive flags, so the setup required an explicit interactive selection.
  Evidence: `bunx shadcn@latest init -t vite -b radix -y` paused at `Which preset would you like to use?`; choosing `Nova` allowed initialization to continue.

- Observation: Biome does not parse Tailwind-specific CSS directives by default, which broke formatting after shadcn updated `src/styles/globals.css`.
  Evidence: `bunx @biomejs/biome format --write ... src/styles/globals.css` failed on `@custom-variant`, `@theme`, and `@apply` until `css.parser.tailwindDirectives` was enabled in `ui/biome.json`.

- Observation: The generated shadcn `CardTitle` renders a `div`, not a semantic heading element, so tests that assumed heading roles had to be updated after the card migration.
  Evidence: `src/test/config-page.test.tsx`, `src/test/projects-page.test.tsx`, and `src/test/item-detail-page.test.tsx` initially failed on `findByRole('heading', ...)` until the assertions targeted rendered text instead.

## Decision Log

- Decision: Use the official shadcn CLI to initialize the Vite app and scaffold the `table` component instead of hand-copying component files.
  Rationale: The CLI is the supported path for the current shadcn release and will set up the expected files and dependencies consistently with the upstream component definitions.
  Date/Author: 2026-03-13 / Codex

- Decision: Keep the migration narrow by adopting shadcn for table structure first while leaving the existing page layout and non-table styling largely intact.
  Rationale: The user request is about duplicated table styles, not a full design-system rewrite. This contains the surface area while still solving the duplication with supported components.
  Date/Author: 2026-03-13 / Codex

- Decision: Use a shared `ui/src/lib/git.ts` helper for `shortOid` rather than leaving OID truncation embedded in table modules.
  Rationale: The helper is used in both the item detail convergence table and the workspaces table, and it belongs with shared formatting utilities rather than page-specific code.
  Date/Author: 2026-03-13 / Codex

- Decision: Update `ui/biome.json` to support Tailwind directives instead of excluding the generated CSS from formatting.
  Rationale: Once shadcn is part of the UI stack, the formatter and linter need to understand Tailwind syntax as part of the normal developer workflow.
  Date/Author: 2026-03-13 / Codex

- Decision: Expand the migration beyond tables to cover the shell, form pages, and operator surfaces using shadcn primitives rather than building a custom helper layer.
  Rationale: The UI still read as the old inline-style application after the table migration alone; a cohesive shadcn feel required moving the app shell and core forms onto `Card`, `Input`, `Textarea`, `Button`, `Alert`, `Badge`, and `Separator`.
  Date/Author: 2026-03-13 / Codex

## Outcomes & Retrospective

The migration achieved the intended outcome. The UI now has a supported shadcn/Tailwind foundation, the shadcn table primitives live under `ui/src/components/ui/table.tsx`, and the duplicated inline table cell styles are gone from the affected pages. The OID truncation helper is centralized in `ui/src/lib/git.ts`.

The migration now goes beyond tables: the app shell no longer overrides shadcn typography, the key forms and operational panels use shadcn cards and controls, and the raw buttons cited in the review were replaced on the migrated surfaces. The validation result is stronger than the previous refactor pass because the broader UI build succeeds and the focused smoke-test coverage now includes `ProjectsPage` and `ConfigPage` in addition to the item detail and root layout routes.

## Context and Orientation

The UI application lives under `ui/` and is a Vite React app. The pages with duplicated table styling are `ui/src/pages/ItemDetailPage.tsx`, `ui/src/pages/ConfigPage.tsx`, `ui/src/pages/JobsPage.tsx`, and `ui/src/pages/ActivityPage.tsx`. `ui/src/pages/WorkspacesPage.tsx` duplicates the `shortOid` helper used in the item detail UI. The recent refactor also moved item detail table sections into `ui/src/components/item-detail/ItemDetailSections.tsx`, so the table migration must include that new component module rather than only the page file name mentioned in the original review comment.

In this repository, `shadcn/ui` means a local copy of component source files generated into the app, not an npm package that is imported wholesale. The "Table component" is a set of React wrappers such as `Table`, `TableHeader`, `TableRow`, `TableHead`, and `TableCell` that render semantic HTML table elements with shared utility-class styling.

## Plan of Work

First, initialize shadcn in the existing Vite app and add the `table` component. This should create the global styling and helper files required by shadcn, such as the CSS entrypoint, `components.json`, utility helpers, and the table primitive source file.

Next, create a small shared utility module for `shortOid` and migrate any remaining inline table markup in the affected pages to use the shadcn table primitives. Where a row needs custom behavior, such as the selectable jobs list or the workspace actions column, keep that behavior in place but render the table structure through the shared `Table*` components.

Finally, run focused tests and build commands from `ui/`. If unrelated pre-existing failures remain, document them explicitly instead of hiding them.

## Concrete Steps

From the repository root `/Users/aa/Documents/ingot`, perform these commands:

    cd /Users/aa/Documents/ingot/ui && bunx shadcn@latest init -t vite -b radix -y
    cd /Users/aa/Documents/ingot/ui && bunx shadcn@latest add table -y

Then edit the affected UI files to replace inline table styles and duplicated OID truncation helpers with shared modules. After the refactor, run:

    cd /Users/aa/Documents/ingot/ui && bun run format
    cd /Users/aa/Documents/ingot/ui && bunx vitest run src/test/item-detail-page.test.tsx
    cd /Users/aa/Documents/ingot/ui && bun run build

Expected results: shadcn initialization completes, the table component is added, and the focused tests pass. A clean build is preferred, but any unrelated existing failure must be documented if encountered.

Actual implementation notes:

    cd /Users/aa/Documents/ingot/ui && bun add -d tailwindcss @tailwindcss/vite
    cd /Users/aa/Documents/ingot/ui && bunx shadcn@latest init -t vite -b radix
    cd /Users/aa/Documents/ingot/ui && bunx shadcn@latest add table -y
    cd /Users/aa/Documents/ingot/ui && bunx @biomejs/biome format --write biome.json vite.config.ts src/main.tsx src/styles/globals.css src/lib/utils.ts src/lib/git.ts src/components/ui/button.tsx src/components/ui/table.tsx src/components/item-detail/ItemDetailSections.tsx src/pages/ConfigPage.tsx src/pages/JobsPage.tsx src/pages/ActivityPage.tsx src/pages/WorkspacesPage.tsx src/test/root-layout.test.tsx
    cd /Users/aa/Documents/ingot/ui && bunx vitest run src/test/item-detail-page.test.tsx src/test/root-layout.test.tsx
    cd /Users/aa/Documents/ingot/ui && bun run build

Follow-up implementation notes for the shell and form migration:

    cd /Users/aa/Documents/ingot/ui && bunx shadcn@latest add card input textarea badge alert separator -y
    cd /Users/aa/Documents/ingot/ui && bunx @biomejs/biome format --write src/layouts/RootLayout.tsx src/layouts/ProjectLayout.tsx src/pages/DashboardPage.tsx src/pages/ProjectsPage.tsx src/pages/BoardPage.tsx src/pages/ConfigPage.tsx src/pages/JobsPage.tsx src/pages/WorkspacesPage.tsx src/pages/ActivityPage.tsx src/pages/ItemDetailPage.tsx src/components/item-detail/ItemDetailSections.tsx src/components/ui/alert.tsx src/components/ui/badge.tsx src/components/ui/button.tsx src/components/ui/card.tsx src/components/ui/input.tsx src/components/ui/separator.tsx src/components/ui/table.tsx src/components/ui/textarea.tsx src/test/root-layout.test.tsx src/test/item-detail-page.test.tsx src/test/config-page.test.tsx src/test/projects-page.test.tsx
    cd /Users/aa/Documents/ingot/ui && bunx vitest run src/test/root-layout.test.tsx src/test/item-detail-page.test.tsx src/test/config-page.test.tsx src/test/projects-page.test.tsx
    cd /Users/aa/Documents/ingot/ui && bun run build

## Validation and Acceptance

Acceptance is behavioral. After the migration, the following routes should still render their tables correctly with the same content and interactions:

- `/projects/:projectId/items/:itemId` for item detail jobs, findings, and convergences
- `/projects/:projectId/config` for agents
- `/projects/:projectId/jobs` for the selectable jobs list
- `/projects/:projectId/activity` for activity
- `/projects/:projectId/workspaces` for workspaces

Run the focused item-detail test file and verify it still passes. Then run the UI build and confirm either a clean result or an unrelated blocker that does not originate in the migrated table files.

Observed result for this implementation: the four focused UI test files passed and the UI build completed successfully.

## Idempotence and Recovery

The shadcn CLI can be re-run safely with `--reinstall` if component files need to be refreshed. The migration is limited to source files under `ui/`, so recovery is by normal source-control inspection and re-running the CLI or formatter rather than by destructive resets.

## Artifacts and Notes

Key preparation commands used before implementation:

    sed -n '1,240p' ui/package.json
    sed -n '1,240p' ui/vite.config.ts
    sed -n '1,260p' ui/src/pages/ConfigPage.tsx
    sed -n '1,220p' ui/src/pages/JobsPage.tsx

## Interfaces and Dependencies

The resulting UI should include shadcn-generated primitives under `ui/src/components/ui/`, including `table`, `button`, `card`, `input`, `textarea`, `badge`, `alert`, and `separator`, plus a small utility module for OID truncation under `ui/src/lib/`.

The affected page modules should stop declaring local `thStyle`, `tdStyle`, or `shortOid` implementations. They should instead import shared primitives and helpers.

Revision note: created before implementation to capture the required shadcn setup and the migration scope for the duplicated table styles.

Revision note: updated after implementation to record the Tailwind prerequisite setup, the Biome Tailwind parser change, the shared `shortOid` helper, and the successful focused test and build results.
