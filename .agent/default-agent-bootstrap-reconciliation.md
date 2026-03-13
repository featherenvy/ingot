# Move Default Agent Bootstrap into Startup Reconciliation

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows [.agent/PLANS.md](./PLANS.md) and must be maintained in accordance with that file.

## Purpose / Big Picture

After this change, a fresh Ingot install will no longer stall with queued jobs solely because the agent registry is empty. Startup reconciliation will seed one built-in global Codex agent using model `gpt-5.4`, probe its CLI availability, and persist the result before job dispatch continues. The daemon binary will return to being wiring-only, and the HTTP agent-management routes will use the same probe logic as startup bootstrap.

## Progress

- [x] (2026-03-13 21:42Z) Identified the architectural mismatch: durable bootstrap logic lived in `apps/ingot-daemon`, while startup reconciliation already lives in `crates/ingot-agent-runtime`.
- [x] (2026-03-13 21:48Z) Added a shared agent registration helper in `crates/ingot-agent-adapters` for default capabilities, default Codex bootstrap record construction, and CLI probing.
- [x] (2026-03-13 21:51Z) Moved default-agent bootstrap into `crates/ingot-agent-runtime` startup reconciliation and removed the daemon-local bootstrap module.
- [x] (2026-03-13 21:53Z) Updated the HTTP agent routes to use the shared probe helper instead of local duplicated probe code.
- [x] (2026-03-13 22:01Z) Updated `ARCHITECTURE.md` and `SPEC.md` so startup agent bootstrap and shared probe ownership are explicit.
- [x] (2026-03-13 22:01Z) Ran formatting and targeted Rust tests for `ingot-agent-adapters`, `ingot-agent-runtime`, and the HTTP agent route coverage.
- [x] (2026-03-13 22:05Z) Compiled the `ingot-daemon` binary target after removing the daemon-local bootstrap module and cleaned up the transient adapter test artifact.

## Surprises & Discoveries

- Observation: the existing router already owned CLI probe logic for agent create/update/reprobe, so the “one source of truth” issue predated the daemon bootstrap addition.
  Evidence: `crates/ingot-http-api/src/router.rs` had local `probe_agent_cli`, `validate_codex_exec_probe`, and `apply_probe_result` helpers before this refactor began.

- Observation: the adapter test suite can leave a transient `stdin.txt` file in the crate directory during validation.
  Evidence: `git status --short` showed `crates/ingot-agent-adapters/stdin.txt` after running the adapter crate tests, and the file had to be removed before close-out.

## Decision Log

- Decision: place the runtime bootstrap hook in `JobDispatcher::reconcile_startup()` rather than a new daemon-app helper.
  Rationale: startup reconciliation is already the runtime-owned startup mutation path in this repository, and this change restores `apps/ingot-daemon` to wiring-only.
  Date/Author: 2026-03-13 / Codex

- Decision: keep the default bootstrap agent as a built-in product invariant instead of adding `defaults.yml` configuration in this change.
  Rationale: the product requirement is “fresh install should be runnable,” the requested model is fixed at `gpt-5.4`, and there is no existing config surface for bootstrap agent templates.
  Date/Author: 2026-03-13 / Codex

- Decision: centralize probe/default-agent construction in `ingot-agent-adapters`.
  Rationale: the bootstrap record and CLI probe are adapter-facing infrastructure concerns, and both startup reconciliation and HTTP agent management need the same logic.
  Date/Author: 2026-03-13 / Codex

## Outcomes & Retrospective

The runtime now bootstraps the default agent during startup reconciliation, the daemon app is back to wiring only, and the HTTP agent routes reuse the same probe helper instead of carrying a second copy of the probe rules. The main compromise is that `ingot-http-api` now deliberately depends on shared adapter probe helpers, so `ARCHITECTURE.md` was updated to describe that composition explicitly. Validation covered the adapter crate, runtime crate, the two HTTP agent route tests, and a final `ingot-daemon` binary-target compile.

## Context and Orientation

The daemon entrypoint is `apps/ingot-daemon/src/main.rs`. It opens the SQLite database at `~/.ingot/ingot.db`, migrates it, constructs `ingot-agent-runtime::JobDispatcher`, and starts the HTTP API. The runtime dispatcher lives in `crates/ingot-agent-runtime/src/lib.rs`; its `reconcile_startup()` method is the repository’s existing startup reconciliation path.

An “agent” in this repository is a global runtime record stored in the `agents` SQLite table. Jobs select an available compatible agent before they can run. The HTTP API for agent management lives in `crates/ingot-http-api/src/router.rs`. Built-in Codex subprocess launching already lives in `crates/ingot-agent-adapters/src/codex.rs`.

The key problem is that fresh installs had an empty `agents` table. The UI surfaced that correctly, but the prior fix added durable bootstrap behavior in the daemon app layer. This ExecPlan moves that behavior into startup reconciliation, keeps the daemon app thin, and documents the behavior as explicit product behavior.

## Plan of Work

Add a shared adapter-facing helper in `crates/ingot-agent-adapters/src/registry.rs`. It must expose the built-in default Codex agent template, default capabilities for supported adapters, and probe logic that mutates an `Agent` record’s `status` and `health_check` according to CLI probe results.

In `crates/ingot-agent-runtime/src/bootstrap.rs`, add a startup helper that checks whether the agent registry is empty and, if so, creates exactly one built-in Codex agent after probing it. Call that helper at the top of `JobDispatcher::reconcile_startup()` in `crates/ingot-agent-runtime/src/lib.rs`.

In `crates/ingot-http-api/src/router.rs`, remove the local probe/default-capability helpers and replace them with the shared helper so `POST /api/agents`, `PUT /api/agents/:id`, and `POST /api/agents/:id/reprobe` all use the same behavior as startup bootstrap.

In `ARCHITECTURE.md`, update the startup reconciliation and crate-responsibility language so startup agent bootstrap is explicit and the daemon app remains wiring only. In `SPEC.md`, make the default bootstrap behavior normative: the daemon seeds one built-in global Codex agent on startup when the agent registry is empty.

## Concrete Steps

Work from the repository root at `/Users/aa/Documents/ingot`.

Run formatting:

    cargo fmt --all

Run targeted tests:

    cargo test -p ingot-agent-adapters
    cargo test -p ingot-agent-runtime
    cargo test -p ingot-http-api create_agent_route_probes_cli_and_lists_agents
    cargo test -p ingot-http-api update_reprobe_and_delete_agent_routes_mutate_bootstrap_state

Expected outcomes:

    The adapter crate tests pass, including new bootstrap/probe tests.
    The runtime crate tests pass, including new startup bootstrap tests.
    The HTTP API tests still show available status on a good Codex probe and unavailable status after reprobe with a missing CLI path.

## Validation and Acceptance

The change is complete when these conditions are true:

Fresh install behavior:
The daemon starts with an empty `agents` table, runs startup reconciliation, and persists exactly one global Codex agent with model `gpt-5.4`. If the `codex` CLI is available and supports the required flags, the agent becomes `available`; otherwise it becomes `unavailable` with a health-check message.

App architecture behavior:
`apps/ingot-daemon/src/main.rs` contains only startup wiring and does not contain agent bootstrap mutation logic.

Shared probe behavior:
The startup bootstrap path and the HTTP agent create/update/reprobe paths use the same helper for capability defaults and probe status handling.

Docs behavior:
`ARCHITECTURE.md` and `SPEC.md` explicitly describe startup agent bootstrap as part of startup reconciliation and define the built-in default agent shape.

## Idempotence and Recovery

Bootstrap must be safe to run on every startup. It should do nothing if any agent record already exists, whether that record came from bootstrap or an operator action. If a probe fails, the agent must still be persisted as `unavailable` so the operator sees the failure instead of another silent empty-registry stall.

If implementation work fails halfway, rerun formatting and the targeted tests after fixing compile errors. The bootstrap logic is additive and should not require database migrations or destructive rollback.

## Artifacts and Notes

The most important proof points for this change are:

    A test that starts from an empty temporary database and verifies one `gpt-5.4` Codex agent appears after startup bootstrap.

    A test that calls the HTTP create-agent route with a fake Codex CLI and observes `status=available` plus `health_check` containing `codex exec help ok`.

    A test that calls the HTTP reprobe route after forcing an invalid `cli_path` and observes `status=unavailable`.

## Interfaces and Dependencies

In `crates/ingot-agent-adapters/src/registry.rs`, define public helpers for:

    pub fn default_agent_capabilities(adapter_kind: AdapterKind) -> Vec<AgentCapability>
    pub fn bootstrap_codex_agent() -> Agent
    pub fn bootstrap_codex_agent_with(cli_path: impl Into<String>, model: impl Into<String>) -> Agent
    pub async fn probe_and_apply(agent: &mut Agent)

In `crates/ingot-agent-runtime/src/bootstrap.rs`, define:

    pub async fn ensure_default_agent(db: &Database) -> Result<(), RuntimeError>

That function should be called from `JobDispatcher::reconcile_startup()`.

Revision note: created this ExecPlan during implementation because the task crossed crate boundaries, changed startup behavior, and required architecture/spec updates.

Revision note: updated this ExecPlan after implementation to record the final crate ownership, documentation changes, and validation results.
