# Simplify workspace state transitions in touched Rust code

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows [.agent/PLANS.md](/Users/aa/Documents/ingot/.agent/PLANS.md) and must be maintained in accordance with that file.

## Purpose / Big Picture

The current workspace-state refactor replaced flat workspace fields with `WorkspaceState`, but the touched code now repeats the same state reconstruction and in-place transition patterns in several crates. After this cleanup pass, the changed code should express intent with small domain helpers instead of rebuilding `WorkspaceState` manually. The visible proof is that the touched crates still compile and the affected tests keep passing while the duplicated transition code is reduced.

## Progress

- [x] (2026-03-16 14:05Z) Reviewed the modified Rust diff and identified the main duplication points in workspace-state construction and mutation.
- [x] (2026-03-16 14:11Z) Read `.agent/PLANS.md` and captured the refactor scope in this ExecPlan.
- [x] (2026-03-16 14:25Z) Added shared workspace-state constructors and in-place transition helpers in `crates/ingot-domain/src/workspace.rs`.
- [x] (2026-03-16 14:28Z) Replaced repeated state-building code in the touched store, workspace, runtime, HTTP, and test-support files.
- [x] (2026-03-16 14:33Z) Ran formatting and targeted verification for the affected crates.

## Surprises & Discoveries

- Observation: The same `(status, commits, current_job_id) -> WorkspaceState` mapping currently exists in both the domain serde bridge and the SQLite mapper, with slightly different error plumbing.
  Evidence: `crates/ingot-domain/src/workspace.rs` and `crates/ingot-store-sqlite/src/store/workspace.rs` both contain near-identical status matches over `WorkspaceStatus`.
- Observation: Once the domain model gained `mark_ready_with_head`, multiple outer-layer call sites collapsed to a single intent-level call without needing any behavioral concessions.
  Evidence: `crates/ingot-agent-runtime/src/lib.rs`, `crates/ingot-http-api/src/router/items.rs`, and `crates/ingot-http-api/src/router/workspaces.rs` all dropped manual `base/head` reconstruction in favor of the helper.

## Decision Log

- Decision: Keep the cleanup centered on the workspace domain model instead of extracting generic helpers in each caller crate.
  Rationale: The duplicated logic is describing domain invariants, so the lowest-friction simplification is to centralize that behavior where `WorkspaceState` is defined.
  Date/Author: 2026-03-16 / Codex

## Outcomes & Retrospective

The workspace-state cleanup stayed contained to the touched files and removed the most repetitive parts of the refactor: manual `WorkspaceState` reconstruction and repeated sentinel `mem::replace` transitions. The domain model now owns more of its own invariants, and the outer layers mostly describe state intent instead of variant plumbing. No further work is required for this pass beyond any broader user-requested review of the remaining modified files.

## Context and Orientation

The workspace domain model lives in `crates/ingot-domain/src/workspace.rs`. It now stores lifecycle information in `WorkspaceState` instead of flat `status`, `current_job_id`, and commit-OID fields. The SQLite persistence layer in `crates/ingot-store-sqlite/src/store/workspace.rs`, the test builder in `crates/ingot-test-support/src/fixtures/workspace.rs`, and several runtime and HTTP routes all reconstruct or mutate that state directly. This pass is limited to the already touched Rust files in the current git diff.

## Plan of Work

First, add small constructors and transition helpers to `WorkspaceCommitState`, `WorkspaceState`, and `Workspace` so callers can create or mutate state without open-coding variant matches or sentinel `mem::replace` patterns. Next, replace duplicated state-construction code in the SQLite mapper, workspace builder, runtime, HTTP routes, and test fixtures with those helpers. Finally, run formatting and targeted cargo checks/tests that exercise the touched crates.

## Concrete Steps

Run from `/Users/aa/Documents/ingot`:

    cargo fmt --all
    cargo test -p ingot-domain
    cargo test -p ingot-store-sqlite
    cargo test -p ingot-workspace
    cargo test -p ingot-usecases
    cargo check -p ingot-agent-runtime
    cargo check -p ingot-http-api
    cargo check -p ingot-test-support

Observed result: all commands above completed successfully after the refactor.

## Validation and Acceptance

Acceptance means the touched crates compile, the workspace-domain tests still validate serde/state transitions, and the affected store/usecase/workspace tests continue to pass. No route or runtime behavior should change; the only intended difference is that state transitions are expressed through shared helpers instead of repeated manual reconstruction.

## Idempotence and Recovery

The refactor is source-only and can be repeated safely. If a helper proves too opinionated, revert just the call-site conversion for that helper while keeping the rest of the domain cleanup intact.

## Artifacts and Notes

Relevant verification summary:

    cargo test -p ingot-domain           # 35 passed
    cargo test -p ingot-store-sqlite     # 9 passed
    cargo test -p ingot-workspace        # 5 passed
    cargo test -p ingot-usecases         # 53 passed
    cargo check -p ingot-agent-runtime   # passed
    cargo check -p ingot-http-api        # passed
    cargo check -p ingot-test-support    # passed

## Interfaces and Dependencies

In `crates/ingot-domain/src/workspace.rs`, keep `WorkspaceState` as the public representation of workspace lifecycle. Add helper APIs there rather than introducing a parallel abstraction elsewhere. The SQLite mapper should continue to return `RepositoryError`, the serde bridge should continue to use the existing wire format, and the runtime/usecase crates should continue to call into the same repository interfaces.

Revision note: Updated after implementation to record the added domain helpers, the converted call sites, and the successful verification commands.
