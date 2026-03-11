# Ingot Architecture

This document is non-normative. The product contract lives in [SPEC.md](./SPEC.md).

Use this document for:

* implementation shape
* module boundaries
* design rationale
* operator-surface goals
* operational footprint

Use [SPEC.md](./SPEC.md) for:

* runtime behavior
* entity fields and invariants
* command semantics
* error handling
* recovery rules
* conformance tests

## Design Position

Ingot is a local code-delivery control plane, not a tracker poller and not a general workflow engine.

The core position is:

* items are durable work objects
* revisions freeze execution meaning
* jobs are bounded subprocess attempts
* workspaces are first-class execution reality
* Git truth belongs to the daemon
* convergence is explicit and two-stage
* human commands are first-class state transitions

v1 is intentionally narrow. The product is optimized for supervised code delivery into a real local Git target with strong auditability and conservative recovery.

## Why The Design Looks This Way

### Workflow Semantics Are Code-Owned

Ingot does not let prompt templates or project config redefine the workflow graph. That is deliberate.

Reasons:

* runtime semantics must be stable and testable
* transitions need compile-time review, not ad hoc prompt edits
* independent teams should be able to reason about a fixed state machine
* operator-visible behavior must be reconstructable from durable state alone

Config and templates remain useful, but they stay below the semantic boundary.

### Revisions Freeze Meaning

The critical freeze points are:

* item creation freezes workflow version
* revision creation freezes approval policy, budgets, and step-to-template mapping
* job dispatch freezes the exact prompt and template digest

This prevents live edits from rewriting the meaning of work already in flight.

### Git Is A First-Class Subsystem

Ingot treats Git as part of the runtime truth, not as an implementation detail hidden behind shell hooks.

That yields a few hard consequences:

* agents edit files only
* the daemon creates canonical commits
* the daemon owns scratch refs and target-ref movement
* every daemon-owned Git side effect is journaled before execution

This is the main difference between “agent runner” systems and “code delivery” systems.

### Convergence Is Separate From Authoring

A candidate change is not treated as done when the authoring workspace looks good. Ingot prepares an integrated result against the current target line, validates there, and only then finalizes the target ref.

That separation is the backbone of the product:

* authoring success does not imply integration success
* approval applies to a prepared integrated result, not just a candidate commit
* target-ref drift is detected explicitly instead of being hand-waved

### Conservative Recovery Wins Over Optimism

If a daemon dies between SQLite writes and Git side effects, Ingot assumes uncertainty, not success.

The architecture is therefore built around:

* a durable SQLite state model
* a GitOperation journal
* startup reconciliation
* stale-event rejection

## System Shape

The reference system has two processes:

* a daemon that owns orchestration, persistence, workspaces, Git, recovery, and agent execution
* a frontend that presents live state over HTTP and WebSocket

```text
┌───────────────────────────────────────────────────────────────┐
│                           React UI                            │
│                     (Vite, TypeScript)                        │
│                                                               │
│  Project Switcher                                             │
│  ├─ Dashboard                                                 │
│  ├─ Board (items only)                                        │
│  ├─ Item Detail / Revision / Workspace                        │
│  ├─ Jobs                                                      │
│  └─ Config                                                    │
└──────────────────┬────────────────────────────┬───────────────┘
                   │ HTTP (REST)                │ WebSocket
                   │ commands + queries         │ live state push
┌──────────────────┴────────────────────────────┴───────────────┐
│                         Rust Daemon                           │
│                                                               │
│  ┌──────────────────┐  ┌──────────────────┐  ┌──────────────┐ │
│  │ Workflow / State │  │ Dispatcher /     │  │ Workspace /  │ │
│  │ Evaluator        │──│ Job Runner       │──│ Git Manager  │ │
│  └────────┬─────────┘  └────────┬─────────┘  └──────┬───────┘ │
│           │                     │                   │         │
│  ┌────────▼─────────┐  ┌────────▼─────────┐  ┌──────▼───────┐ │
│  │ Item Projection  │  │ Convergence      │  │ Agent Runtime│ │
│  │ + Diagnostics    │  │ Manager          │  │ + Adapters   │ │
│  └────────┬─────────┘  └────────┬─────────┘  └──────┬───────┘ │
│           │                     │                   │         │
│  ┌────────▼─────────┐  ┌────────▼─────────┐  ┌──────▼───────┐ │
│  │ SQLite           │  │ Activity /       │  │ CLI process  │ │
│  │ runtime state    │  │ observability    │  │ supervision  │ │
│  └──────────────────┘  └──────────────────┘  └──────────────┘ │
└───────────────────────────────────────────────────────────────┘
```

## Storage Split

The runtime is intentionally split between filesystem state and SQLite state.

Filesystem:

* worktrees
* scratch refs
* prompt snapshots
* stdout and stderr logs
* copied result artifacts
* per-project config and template overrides

SQLite:

* canonical item state
* immutable revisions
* jobs and retry lineage
* workspaces and convergence records
* Git operation journal
* activity history

This split keeps Git and large artifacts where they belong while preserving a queryable source of truth for orchestration.

## Recommended Rust Workspace

The reference implementation is a Cargo workspace. The daemon binary is wiring only.

```text
ingot/
├── Cargo.toml
├── Cargo.lock
├── apps/
│   └── ingot-daemon/
├── crates/
│   ├── ingot-domain/
│   ├── ingot-workflow/
│   ├── ingot-usecases/
│   ├── ingot-config/
│   ├── ingot-store-sqlite/
│   ├── ingot-git/
│   ├── ingot-workspace/
│   ├── ingot-agent-protocol/
│   ├── ingot-agent-adapters/
│   ├── ingot-agent-runtime/
│   └── ingot-http-api/
├── ui/
├── SPEC.md
├── ARCHITECTURE.md
└── README.md
```

### Crate Responsibilities

| Crate                  | Responsibility                                                                                           | Must not depend on                               |
| ---------------------- | -------------------------------------------------------------------------------------------------------- | ------------------------------------------------ |
| `ingot-domain`         | Pure entities, enums, invariants, value objects, repository ports, event types                           | `sqlx`, `axum`, `tokio::process`                 |
| `ingot-workflow`       | Built-in workflow definitions, step contracts, evaluator, transition tables                              | `sqlx`, `axum`, adapter code                     |
| `ingot-usecases`    | Command handlers, transaction boundaries, use-case orchestration, port composition                       | `axum`, `sqlx` concrete types, CLI-specific code |
| `ingot-config`         | YAML loading, merge logic, config schema validation, template override loading                           | `axum`, `sqlx`                                   |
| `ingot-store-sqlite`   | sqlx models, migrations, repository implementations, transaction adapters                                | `axum`, adapter crates                           |
| `ingot-git`            | Safe Git wrappers, diff generation, ref validation, commit trailers, convergence helpers, target-ref CAS | `axum`, workflow logic                           |
| `ingot-workspace`      | Worktree provisioning, reset, reuse, and cleanup using `ingot-git`                                       | `axum`, `sqlx`                                   |
| `ingot-agent-protocol` | Adapter traits, request and response types, result schemas, progress events                              | `sqlx`, `axum`                                   |
| `ingot-agent-adapters` | Built-in Claude and Codex adapter implementations                                                        | `sqlx`, `axum`, workflow crates                  |
| `ingot-agent-runtime`  | Subprocess spawning, cancellation, heartbeats, log writing, adapter supervision                          | `axum`, workflow crates                          |
| `ingot-http-api`       | Axum routes, DTOs, auth middleware, WebSocket transport                                                  | `sqlx` direct queries, adapter code              |

### Dependency Direction

```text
ingot-domain
    ↑
ingot-workflow
    ↑
ingot-usecases
   ↑    ↑      ↑        ↑
config store  workspace agent-runtime
         ↑       ↑          ↑
         git   agent-protocol
                  ↑
            agent-adapters

http-api ───────→ application
apps/ingot-daemon wires everything together
```

Rules:

* `ingot-domain` and `ingot-workflow` stay pure and testable
* `ingot-usecases` depends on ports, not infrastructure implementations
* storage, workspace, Git, and agent runtime are infrastructure
* the daemon app owns DI, config bootstrap, background task startup, and signal handling only

## Operator Surface

The operator model is item-first.

Recommended primary views:

* project dashboard
* item board
* item detail
* execution queue and jobs
* workspace management
* config

Recommended board columns:

* `INBOX`
* `WORKING`
* `APPROVAL`
* `DONE`

Only items appear on the board.

Recommended attention badges:

* `Escalated`
* `Deferred`

`Blocked` is not a canonical state in v1.

Recommended item-card contents:

* current revision title
* classification
* current step
* active job chip, if any
* approval badge when pending
* attention badge when escalated or deferred
* revision number
* priority

The item detail view should answer four questions quickly:

1. what is this item?
2. what happened already?
3. what is blocking closure?
4. what should happen next?

That leads directly to the core detail-pane ingredients:

* workflow version
* current revision contract
* projected current step
* recommended next action and legal alternatives
* revision history
* full job timeline
* latest revision context summary
* workspace summary with target ref, workspace ref, base/head, and diff manifest
* convergence summary with prepare/finalize state and target-head validity
* diagnostics explaining the current projection

## Transport Notes

The reference frontend is a React SPA served by Vite in development and by the daemon in production.

Transport expectations:

* initial state loads via REST
* live updates arrive over WebSocket
* WebSocket messages carry monotonic sequence numbers so clients can detect gaps and resync
* API auth uses a bearer token generated by the daemon and stored locally with restrictive permissions

The wire-level contracts themselves are specified in [SPEC.md](./SPEC.md).

## Operational Footprint

```text
~/.ingot/
├── ingot.db
├── auth_token
├── daemon.lock
├── daemon.pid
├── daemon.log
├── backups/
├── defaults.yml
├── logs/
│   └── <job_id>/
│       ├── prompt.txt
│       ├── stdout.log
│       ├── stderr.log
│       └── result.json
└── ...

<repo>/.ingot/
├── config.yml
└── templates/
    └── *.yml
```

There is no `workflows/` directory in v1. Workflow definitions are built into the daemon.

## Recommended Tech Stack

| Layer         | Technology         | Why                                          |
| ------------- | ------------------ | -------------------------------------------- |
| Runtime       | Tokio              | async process, job, and workspace management |
| HTTP/WS       | Axum               | local API and state push                     |
| Database      | SQLite via sqlx    | runtime state, migrations, checked queries   |
| Serialization | serde + serde_json | REST and WebSocket payloads                  |
| Config        | serde_yml          | YAML settings and prompt templates           |
| Logging       | tracing            | structured logs and diagnostics              |
| Agents        | tokio::process     | spawn and supervise local CLI agents         |
| Git           | tokio::process     | worktree and ref operations                  |
| Frontend      | React + TypeScript | operator UI                                  |
| Bundler       | Vite               | local development                            |

## What Stays Out Of v1

The following remain deliberately deferred:

* multiple runtime workflows
* bug-specific reproduce and root-cause flow
* parent and child items with dependency edges
* clone workspaces
* Docker workspaces
* report-only workflow steps
* prompt templates that alter step semantics
* workflow authoring in the UI or API
* in-system manual conflict continuation
* agent-driven conflict resolution
* MCP server exposure
* remote push, PR, or CI integration

The discipline here matters. v1 gets stronger by refusing convenience paths that blur provenance, weaken recovery, or make workflow truth editable in places that are hard to test.
