# Harness runtime hardening

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows `.agent/PLANS.md` and must be maintained in accordance with that file.

## Purpose / Big Picture

After this change, malformed `.ingot/harness.toml` files will no longer silently disable harness behavior. Daemon validation will fail cleanly, agent-job prompt assembly will stop before launch, timed-out harness commands will be terminated instead of orphaned, validation will re-sync its worktree before running commands, and repo-local skill files will actually be inlined into prompts. The change is observable through focused runtime and HTTP API tests that fail before the patch and pass after it.

## Progress

- [x] (2026-03-16 15:40Z) Re-read `.agent/PLANS.md`, inspected the current dirty worktree, and confirmed the in-progress harness runtime and HTTP API files are already present in this branch.
- [x] (2026-03-16 16:35Z) Implemented strict runtime harness loading, prep-time failure handling, daemon validation workspace re-sync, timeout cleanup, and prompt skill resolution in `crates/ingot-agent-runtime/src/lib.rs`.
- [x] (2026-03-16 16:42Z) Updated the harness HTTP `GET` route and `SPEC.md` to distinguish missing from malformed harness files and to document runtime-wide malformed-harness failures.
- [x] (2026-03-16 16:58Z) Added focused runtime and HTTP API regressions for invalid harness config, timed-out command cleanup, workspace drift repair, repo-local skill prompt injection, and the new harness route semantics.
- [x] (2026-03-16 17:10Z) Ran focused tests and `make test`; both passed. `make lint` still fails on pre-existing `clippy::needless_borrow` findings in `crates/ingot-http-api/tests/item_routes.rs`, which is outside this harness patch.

## Surprises & Discoveries

- Observation: the worktree already contains uncommitted harness-related changes, including new `harness.rs` files in `ingot-domain` and `ingot-http-api`.
  Evidence: `git status --short` shows modified runtime/spec files and untracked harness files in those crates.

- Observation: `provision_authoring_workspace()` previously updated the anchor ref but only verified the worktree head, so a manually drifted detached worktree failed validation instead of being repaired.
  Evidence: the first authoring drift regression failed with `WorkspaceHeadMismatch { expected: ..., actual: ... }` until the helper was changed to `git reset --hard` and `git clean -fd` existing managed worktrees.

- Observation: queued daemon-only integration validation cannot rely on `job.state.workspace_id()` because queued jobs do not retain assignments, and the auto-dispatch path was preassigning `validate_integrated` jobs only to have reconciliation drop that state.
  Evidence: the first integration drift regression failed with `InvalidState("integration jobs require a provisioned integration workspace")` until the runtime resolved the integration workspace from the prepared convergence instead.

## Decision Log

- Decision: make malformed harness configuration fatal anywhere the daemon or harness API reads it, while keeping a missing harness file as the only empty-profile fallback.
  Rationale: this is the most comprehensive interpretation of the review feedback and prevents silent loss of validation or prompt capabilities.
  Date/Author: 2026-03-16 / Codex

- Decision: record this work in a new ExecPlan instead of relying on the conversational plan alone.
  Rationale: the repository requires ExecPlans for significant cross-crate runtime changes, and this patch spans runtime behavior, HTTP API semantics, spec text, and tests.
  Date/Author: 2026-03-16 / Codex

- Decision: repair stale managed authoring/integration worktrees in `ingot-workspace` instead of trying to special-case drift only inside the runtime.
  Rationale: the runtime already routes through these helpers for workspace provisioning, so fixing them once hardens both daemon validation and any other path that reuses the same managed worktree semantics.
  Date/Author: 2026-03-16 / Codex

- Decision: resolve queued daemon-only integration validation workspaces from the prepared convergence instead of relying on `JobState` assignment data.
  Rationale: queued jobs do not persist `workspace_id`, so convergence state is the only durable source of the integration workspace for `validate_integrated`.
  Date/Author: 2026-03-16 / Codex

## Outcomes & Retrospective

Implemented the full harness hardening scope: malformed harness files now fail runtime consumers instead of silently degrading, daemon validation re-syncs managed worktrees before running commands, timed-out harness commands are killed before returning, and repo-local skill files are inlined into prompt artifacts. The harness HTTP `GET` route and `SPEC.md` now reflect the same malformed-versus-missing distinction.

Focused validation passed with:

    cargo test -p ingot-agent-runtime --test auto_dispatch
    cargo test -p ingot-http-api harness

The broader Rust test gate also passed with:

    make test

`make lint` remains red due to pre-existing `clippy::needless_borrow` findings in `crates/ingot-http-api/tests/item_routes.rs`. Those findings are unrelated to the harness changes and were left out of scope after confirming the harness patch itself compiles, formats, and passes the Rust test suite.

## Context and Orientation

`crates/ingot-agent-runtime/src/lib.rs` owns prompt assembly, job preparation, daemon-only validation execution, and harness command launching. The current branch reads `.ingot/harness.toml` with a helper that defaults malformed files to `HarnessProfile::default()`, which makes invalid config indistinguishable from “no harness configured.” The same file also launches harness commands with `tokio::time::timeout(child.wait_with_output())`, which cancels the wait future without explicitly killing the spawned command tree.

`crates/ingot-http-api/src/router/harness.rs` serves `GET` and `PUT` for the on-disk harness profile. `PUT` already validates malformed TOML, but `GET` currently hides malformed files behind an empty profile. `SPEC.md` documents harness ownership and endpoint semantics, so it must be updated to reflect the new malformed-file behavior.

`crates/ingot-agent-runtime/tests/auto_dispatch.rs` already contains daemon-only validation tests using the shared runtime `TestHarness`. `crates/ingot-http-api/tests` currently has no dedicated harness route test file, so this change will add focused coverage there.

## Plan of Work

Introduce a strict harness loader in `crates/ingot-agent-runtime/src/lib.rs` that returns `Option<HarnessProfile>` on success and typed errors for malformed TOML, invalid duration values, invalid skill globs, or unreadable skill files. Use that loader in two ways: resolve prompt-time harness context before mutating workspace state for agent jobs, and load validation commands before daemon validation runs. Prompt assembly should accept the resolved harness context and render actual skill-file contents, not raw glob patterns.

Add a prep-time job failure helper in the runtime so harness-loading failures do not bubble out of `tick()` and leave the job queued. Rework daemon validation workspace setup to go through the same authoring/integration provisioning path used elsewhere, then persist the synchronized workspace row before executing commands.

Replace the timeout path in `run_harness_command` with an implementation that runs the shell in its own process group, kills the whole group on timeout, waits for exit, and preserves captured stdout/stderr tails. Update the harness HTTP `GET` route to return a validation error when the file exists but cannot be parsed. Finally, update `SPEC.md` and add focused runtime/API regressions.

## Concrete Steps

From `/Users/aa/Documents/ingot`, implement the runtime patch first, then the HTTP/API/spec changes, then the focused tests.

Run these commands during validation:

    cargo test -p ingot-agent-runtime auto_dispatch
    cargo test -p ingot-http-api harness
    make test

## Validation and Acceptance

Acceptance is reached when:

1. A daemon-only validation job with malformed `.ingot/harness.toml` finishes as `Failed` with a harness-config error instead of `Completed/Clean`.
2. A queued authoring or review job with malformed harness config fails before agent launch instead of aborting the dispatcher tick.
3. A timed-out harness command does not leave a background writer alive after the job finishes.
4. A drifted authoring or integration validation worktree is reset to the queued job’s expected head before the harness command runs.
5. Prompt artifacts include the contents of repo-local skill files matched by `skills.paths`.
6. `GET /api/projects/:project_id/harness` returns `422` for malformed on-disk harness files and `200` empty only when the file is absent.

## Idempotence and Recovery

The code changes are additive and safe to re-run. Test repos and databases are isolated temp artifacts created by the existing harness helpers. If a test fails midway, rerun the focused test command after adjusting the code; no manual cleanup should be required beyond the normal temp-directory churn.

## Artifacts and Notes

The final revision of this document should record the exact test commands that passed and any notable evidence snippets for timeout cleanup or malformed-harness failures.

## Interfaces and Dependencies

The runtime patch should keep public Rust types stable except for internal helper signatures. Expected internal additions include a fallible harness-loader result type, a prompt-time harness-context type that includes resolved skill files, and a prep-time failure helper for queued jobs. The timeout fix should use Tokio’s Unix process-group support for spawned shell commands. If direct signal delivery is needed, add only the smallest dependency necessary to kill the spawned process group.

Revision note: created before implementation because this patch spans runtime execution semantics, prompt assembly, the harness HTTP route, spec text, and several focused regressions.
