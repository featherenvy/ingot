# Repository Guidelines

## Project Structure & Module Organization
`apps/ingot-daemon/` contains the `ingotd` binary and wiring only. Core Rust code lives under `crates/`: keep domain types in `ingot-domain`, workflow evaluation in `ingot-workflow`, application logic in `ingot-usecases`, and infrastructure concerns in crates such as `ingot-store-sqlite`, `ingot-git`, and `ingot-http-api`. The frontend lives in `ui/`, with routes in `ui/src/pages`, shared API code in `ui/src/api`, local state in `ui/src/stores`, and Vitest files in `ui/src/test`. SQLite migrations live in `crates/ingot-store-sqlite/migrations/`. Read `SPEC.md` for behavior and `ARCHITECTURE.md` for module boundaries.

## Build, Test, and Development Commands
Use the Makefile as the main entry point:

- `make check` runs `cargo check` for the full Rust workspace.
- `make test` runs Rust tests; `make ui-test` runs Vitest in `ui/`.
- `make lint` runs `clippy`, Biome, and `cargo fmt --check`.
- `make build` builds Rust; `make ui-build` type-checks and builds the UI.
- `make ci` runs the combined backend/frontend type-check, lint, and test gate.
- `make dev` starts the daemon on `127.0.0.1:4190` and the Vite UI together.
- `make ui-install` installs UI dependencies with Bun.

Run `make ci` before opening a PR; it is the closest thing to a local CI gate.

## Coding Style & Naming Conventions
Rust uses `rustfmt` defaults: 4-space indentation, `snake_case` modules/functions, and `PascalCase` types and enums. Keep `ingot-domain` and `ingot-workflow` free of HTTP, database, and process-layer dependencies. TypeScript uses Biome with 2-space indentation, single quotes, no semicolons, and a 120-column line width. Name React pages/components in `PascalCase` (`DashboardPage.tsx`) and hooks/helpers in `camelCase`.

## Testing Guidelines
Add tests with every behavior change. Rust tests should run under `cargo test`, preferably close to the crate they cover with `#[cfg(test)]` modules. UI tests use Vitest and Testing Library; follow the existing `*.test.ts` pattern in `ui/src/test/`. There is no enforced coverage threshold yet, so favor focused tests for workflow transitions, API query logic, and board/status rendering.

## Commit & Pull Request Guidelines
Recent commits use short, imperative, sentence-case subjects, for example `Bootstrap Ingot workspace: Rust crates, SQLite schema, and React UI scaffolding`. Keep commits scoped to one change. PRs should include a concise summary, linked issue or spec section when relevant, the commands you ran (`make lint`, `make test`, `make ui-test`), and screenshots for visible UI changes.

# ExecPlans

When writing complex features or significant refactors, use an ExecPlan (as described in .agent/PLANS.md) from design to implementation.
