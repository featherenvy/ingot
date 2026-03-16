# Ingot

A local code-delivery control plane. Ingot is a long-running daemon that orchestrates supervised AI coding work against real local Git repositories—owning Git truth, enforcing bounded execution, and closing work only after integrated validation plus any required human approval.

Ingot is not a task tracker, not a general workflow engine, and not an "agent runner." It is a narrow execution control layer for one thing: single-item code delivery in a real local Git repository with strong auditability and conservative recovery.

## How It Works

You give Ingot a work item (a title, description, and acceptance criteria). It:

1. **Authors** code changes in an isolated Git worktree using a supervised AI agent
2. **Reviews** the changes—both the incremental diff and the full candidate—via agent-driven structured review
3. **Validates** the candidate by running your project's declared verification commands (build, test, lint)
4. **Repairs** any findings through bounded rework loops with explicit budgets
5. **Converges** the result against your target branch via rebase replay, preserving commit boundaries
6. **Validates the integration** against the rebased result
7. **Finalizes** the target ref only after all gates pass and any required approval is granted

Every step is durable, auditable, and recoverable. If the daemon crashes mid-operation, it reconciles from its SQLite state and Git operation journal on restart—assuming uncertainty, never success.

## Key Design Decisions

- **Items are durable.** A work item survives retries, rework loops, approval rejection, revision changes, defer/resume, and manual terminal decisions.
- **Revisions freeze meaning.** Changing a title, description, or acceptance criteria creates a new revision. In-flight work is never silently rewritten.
- **Jobs are bounded.** Every agent job is a new subprocess with explicit inputs, outputs, and budgets. No hidden conversation reuse.
- **Git truth belongs to the daemon.** Agents edit files. The daemon creates canonical commits with audit trailers, owns scratch refs, and moves the target ref via compare-and-swap.
- **Convergence is explicit and two-stage.** Authoring success does not imply integration success. Prepare replays the commit chain onto the current target, then finalize CAS-updates the ref.
- **Human authority is first-class.** Human commands outrank late or stale agent events. Approval, escalation, defer, dismiss, and rework are explicit state transitions.
- **Conservative recovery.** If there is uncertainty, the system assumes failure. The Git operation journal enables crash recovery without inventing success.

## Architecture

Two processes:

- A **Rust daemon** (`ingotd`) that owns orchestration, persistence, workspaces, Git, recovery, and agent execution
- A **React SPA** that presents live state over REST and WebSocket

```
┌─────────────────────────────────────────────────────────────┐
│                         React UI                            │
│                   (Vite, TypeScript)                         │
│                                                             │
│  Project Switcher                                           │
│  ├─ Dashboard                                               │
│  ├─ Board (items only)                                      │
│  ├─ Item Detail / Revision / Workspace                      │
│  ├─ Jobs                                                    │
│  └─ Config                                                  │
└────────────────┬──────────────────────────┬─────────────────┘
                 │ HTTP (REST)              │ WebSocket
                 │ commands + queries       │ live state push
┌────────────────┴──────────────────────────┴─────────────────┐
│                       Rust Daemon                           │
│                                                             │
│  Workflow Evaluator ── Dispatcher / Job Runner ── Git Mgr   │
│         │                      │                    │       │
│  Item Projection ──── Convergence Manager ── Agent Runtime  │
│         │                      │                    │       │
│     SQLite ──────── Activity / Observability ── CLI Procs   │
└─────────────────────────────────────────────────────────────┘
```

### Rust Workspace (12 crates)

| Crate | Responsibility |
|---|---|
| `ingot-domain` | Pure entities, enums, invariants, repository port traits, domain events |
| `ingot-workflow` | Workflow graph, step contracts, pure evaluator (projects state, never mutates) |
| `ingot-usecases` | Command handlers, transaction boundaries, daemon-only system actions |
| `ingot-store-sqlite` | SQLite repository implementations and migrations |
| `ingot-git` | Git operations via `tokio::process`—commits, refs, convergence replay |
| `ingot-workspace` | Worktree provisioning, reset, reuse, and cleanup |
| `ingot-agent-protocol` | `AgentAdapter` trait, request/response types, result schemas |
| `ingot-agent-adapters` | Claude Code and Codex adapter implementations |
| `ingot-agent-runtime` | Subprocess spawning, supervision, heartbeats, log capture |
| `ingot-config` | YAML config loading with global/project merge |
| `ingot-http-api` | Axum routes, DTOs, WebSocket transport |
| `ingot-daemon` | Binary wiring only—DI, config bootstrap, signal handling |

Hard dependency rules: `ingot-domain` and `ingot-workflow` must never depend on sqlx, axum, or tokio::process. `ingot-usecases` depends on ports, not infrastructure.

### UI

React + TypeScript SPA. Zustand for client state, TanStack Query for server data with WebSocket-driven cache invalidation. Biome for linting/formatting. Tailwind CSS.

## Prerequisites

- Rust stable (1.85+)
- [Bun](https://bun.sh) (for UI package management and scripts)
- SQLite 3
- Git
- At least one supported AI agent CLI installed:
  - [Claude Code](https://docs.anthropic.com/en/docs/claude-code) (`claude`)
  - [Codex](https://github.com/openai/codex) (`codex`)

## Getting Started

```sh
# Clone
git clone https://github.com/AustinDizworksAI/ingot.git
cd ingot

# Install UI dependencies
make ui-install

# Run both daemon and UI dev server
make dev
```

The daemon serves on `:4190` and the UI dev server on `:4191`.

### Register a project

Once the daemon is running, register a local Git repository through the API:

```sh
curl -X POST http://localhost:4190/api/projects \
  -H "Content-Type: application/json" \
  -d '{"name": "my-project", "path": "/path/to/repo", "default_branch": "main"}'
```

### Configure verification (optional)

Add a harness profile to your repository to enable automated build/test/lint validation:

```toml
# <repo>/.ingot/harness.toml

[commands.build]
run = "make build"
timeout = "5m"

[commands.test]
run = "make test"
timeout = "10m"

[commands.lint]
run = "make lint"
timeout = "2m"

[skills]
paths = [".ingot/skills/*.md"]
```

## Development

```sh
make help             # Show all available targets

make check            # Type-check Rust workspace
make test             # Run Rust tests
make lint             # All linters: clippy + biome + fmt check
make build            # Build Rust workspace
make all              # check + test + lint + build

make ui-build         # Typecheck + vite build
make ui-test          # Vitest
make ui-lint          # Biome check

make dev              # Daemon (:4190) + UI dev server (:4191)
make dev-daemon       # Daemon only
make dev-ui           # UI only
```

Run a single Rust test:

```sh
cargo test -p ingot-workflow test_name
```

Run a single UI test:

```sh
cd ui && bunx vitest run src/test/board.test.ts
```

## Configuration

Configuration is layered with increasing precedence:

```
~/.ingot/defaults.yml          # Global defaults
  → <repo>/.ingot/config.yml   # Per-project overrides
    → CLI flags                # Runtime overrides
```

Prompt templates are built into the daemon and may be overridden per project at `<repo>/.ingot/templates/*.yml`.

## Operational Footprint

```
~/.ingot/
├── ingot.db          # SQLite runtime state
├── auth_token        # Bearer token for API auth
├── daemon.lock
├── daemon.pid
├── log/daemon.log
├── backups/
├── defaults.yml
└── logs/<job_id>/
    ├── prompt.txt
    ├── stdout.log
    ├── stderr.log
    └── result.json

<repo>/.ingot/
├── config.yml
├── harness.toml
└── templates/*.yml
```

## Formal Verification

The `formal/` directory contains TLA+ specifications for critical control properties. Run model checking with:

```sh
make tla-check
```

## Documentation

- [SPEC.md](./SPEC.md) — Normative service specification (runtime behavior, entity invariants, command semantics, recovery rules)
- [ARCHITECTURE.md](./ARCHITECTURE.md) — Non-normative implementation shape (module boundaries, design rationale, tech stack)

## License

MIT
