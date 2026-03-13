# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What This Is

Ingot is a local code-delivery control plane — a long-running daemon that orchestrates supervised AI coding work against real local Git repositories. It is NOT a tracker poller or general workflow engine. The normative spec is SPEC.md; the implementation shape is ARCHITECTURE.md.

## Commands

Run `make help` for the full list. Key commands:

```
make check          # Type-check Rust workspace
make test           # Run Rust tests
make lint           # All linters: clippy + biome + fmt check
make build          # Build Rust workspace
make all            # check + test + lint + build (CI gate)

make ui-build       # Typecheck + vite build
make ui-test        # Vitest
make ui-lint        # Biome check

make dev            # Run daemon (:4190) and UI dev server (:4191) in parallel
make dev-daemon     # Daemon only
make dev-ui         # UI only
```

Run a single Rust test: `cargo test -p ingot-workflow test_name`

Run a single UI test: `cd ui && bunx vitest run src/test/board.test.ts`

## Architecture

Two processes: a Rust daemon (owns orchestration, persistence, Git, agent execution) and a React SPA (presents live state).

### Rust Workspace

12 crates in `apps/` and `crates/` with strict dependency direction:

- **ingot-domain** — Pure types, no infrastructure. Entities (Item, Job, Workspace, Convergence, etc.), enums, repository port traits (`ports.rs`), domain events. Everything else depends on this.
- **ingot-workflow** — Pure evaluator and workflow graph. The `delivery:v1` workflow with 11 step contracts, transition edges, and the `Evaluator` that projects board status / next action from canonical state. Must NOT mutate durable state.
- **ingot-usecases** — Command handlers and transaction boundaries. Owns daemon-only system actions (finalize, invalidate). Depends on ports, not concrete infrastructure.
- **ingot-store-sqlite** — Repository implementations, migrations in `migrations/`. SQLite with WAL mode.
- **ingot-git** — Git operations via `tokio::process`. Commit creation, ref management, convergence replay.
- **ingot-workspace** — Worktree provisioning using ingot-git.
- **ingot-agent-protocol** — `AgentAdapter` trait, request/response types.
- **ingot-agent-adapters** — Claude Code and Codex adapter implementations.
- **ingot-agent-runtime** — Subprocess spawning and supervision.
- **ingot-config** — YAML config loading with global/project merge.
- **ingot-http-api** — Axum routes, error mapping from UseCaseError to HTTP status codes.
- **ingot-daemon** (`apps/`) — Binary `ingotd`, wiring only. Owns DI, config bootstrap, signal handling.

Hard dependency rules: `ingot-domain` and `ingot-workflow` must never depend on sqlx, axum, or tokio::process. `ingot-usecases` depends on ports, not infrastructure. See the "Must not depend on" column in ARCHITECTURE.md.

### UI (`ui/`)

React + TypeScript SPA built with Vite and Bun.

- **State**: Zustand for client-only state (active project, WS connection). TanStack Query for all server data (fetch, cache, background refetch).
- **Data flow**: REST loads populate TanStack Query cache. WebSocket events invalidate relevant query keys (in `stores/connection.ts`). Sequence gaps trigger full project resync.
- **Routing**: React Router with project-scoped routes: `/projects/:projectId/board`, `/projects/:projectId/items/:itemId`, etc.
- **API layer**: `api/client.ts` (raw fetch), `api/queries.ts` (query key factory + options), `api/mutations.ts` (mutation hooks with cache invalidation).
- **Lint/format**: Biome (replaces ESLint+Prettier). Single quotes, no semicolons, 2-space indent, 120 line width.

### Key Domain Concepts

- **Item** — Durable work object, only entity on the board. Survives retries, rework, revision changes.
- **ItemRevision** — Immutable snapshot of work contract. Any change to title/description/criteria creates a new revision.
- **Job** — Bounded subprocess attempt. Always a new process, no hidden conversation reuse.
- **Workspace** — First-class execution context. Authoring (one per revision, reused), Review (fresh per job, ephemeral), Integration (one per convergence attempt).
- **Convergence** — Two-stage: prepare (rebase replay preserving commit boundaries) then finalize (CAS on target ref). Stale convergences are invalidated, never silently reused.
- **GitOperation** — Journal entry written BEFORE any daemon-owned Git side effect. Enables crash recovery.
- **Evaluator** — Pure read-side projection. Derives board_status, current_step_id, next_recommended_action from canonical rows. Never mutates state.

### Critical Invariants

- At most one active job per item revision (enforced by partial unique index).
- At most one active convergence per item revision (enforced by partial unique index).
- Successful mutating jobs produce exactly one daemon-owned commit with required trailers.
- `approval_state=approved` implies `lifecycle_state=done`.
- Completed items cannot be reopened.
- HTTP/WebSocket reads must never trigger daemon-only system actions.
