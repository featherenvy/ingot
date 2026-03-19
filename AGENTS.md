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


<!-- BEGIN BEADS INTEGRATION v:1 profile:full hash:d4f96305 -->
## Issue Tracking with bd (beads)

**IMPORTANT**: This project uses **bd (beads)** for ALL issue tracking. Do NOT use markdown TODOs, task lists, or other tracking methods.

### Why bd?

- Dependency-aware: Track blockers and relationships between issues
- Git-friendly: Dolt-powered version control with native sync
- Agent-optimized: JSON output, ready work detection, discovered-from links
- Prevents duplicate tracking systems and confusion

### Quick Start

**Check for ready work:**

```bash
bd ready --json
```

**Create new issues:**

```bash
bd create "Issue title" --description="Detailed context" -t bug|feature|task -p 0-4 --json
bd create "Issue title" --description="What this issue is about" -p 1 --deps discovered-from:bd-123 --json
```

**Claim and update:**

```bash
bd update <id> --claim --json
bd update bd-42 --priority 1 --json
```

**Complete work:**

```bash
bd close bd-42 --reason "Completed" --json
```

### Issue Types

- `bug` - Something broken
- `feature` - New functionality
- `task` - Work item (tests, docs, refactoring)
- `epic` - Large feature with subtasks
- `chore` - Maintenance (dependencies, tooling)

### Priorities

- `0` - Critical (security, data loss, broken builds)
- `1` - High (major features, important bugs)
- `2` - Medium (default, nice-to-have)
- `3` - Low (polish, optimization)
- `4` - Backlog (future ideas)

### Workflow for AI Agents

1. **Check ready work**: `bd ready` shows unblocked issues
2. **Claim your task atomically**: `bd update <id> --claim`
3. **Work on it**: Implement, test, document
4. **Discover new work?** Create linked issue:
   - `bd create "Found bug" --description="Details about what was found" -p 1 --deps discovered-from:<parent-id>`
5. **Complete**: `bd close <id> --reason "Done"`

### Auto-Sync

bd automatically syncs via Dolt:

- Each write auto-commits to Dolt history
- Use `bd dolt push`/`bd dolt pull` for remote sync
- No manual export/import needed!

### Important Rules

- ✅ Use bd for ALL task tracking
- ✅ Always use `--json` flag for programmatic use
- ✅ Link discovered work with `discovered-from` dependencies
- ✅ Check `bd ready` before asking "what should I work on?"
- ❌ Do NOT create markdown TODO lists
- ❌ Do NOT use external issue trackers
- ❌ Do NOT duplicate tracking systems

For more details, see README.md and docs/QUICKSTART.md.

## Landing the Plane (Session Completion)

**When ending a work session**, you MUST complete ALL steps below. Work is NOT complete until `git push` succeeds.

**MANDATORY WORKFLOW:**

1. **File issues for remaining work** - Create issues for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds
3. **Update issue status** - Close finished work, update in-progress items
4. **PUSH TO REMOTE** - This is MANDATORY:
   ```bash
   git pull --rebase
   bd dolt push
   git push
   git status  # MUST show "up to date with origin"
   ```
5. **Clean up** - Clear stashes, prune remote branches
6. **Verify** - All changes committed AND pushed
7. **Hand off** - Provide context for next session

**CRITICAL RULES:**
- Work is NOT complete until `git push` succeeds
- NEVER stop before pushing - that leaves work stranded locally
- NEVER say "ready to push when you are" - YOU must push
- If push fails, resolve and retry until it succeeds

<!-- END BEADS INTEGRATION -->
