# Complete 5/5 Cleanup Pass for Adapters, Evaluator, Infra Ports, and Paths

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows the repository requirements in `.agent/PLANS.md`.

## Purpose / Big Picture

After this change, the remaining “4/5” cleanup items called out by the user will each be functionally complete:

- `crates/ingot-agent-adapters/` will keep shared CLI subprocess and schema wiring in shared helpers instead of leaving the last lifecycle plumbing split between `codex.rs` and `claude_code.rs`.
- `crates/ingot-workflow/src/evaluator.rs` will stop being the single large file that mixes top-level evaluation flow, projection helpers, and its test suite.
- Runtime and HTTP mirror refresh code will use one shared project-repo refresh helper instead of duplicating the same state-root and unresolved-finalize logic in separate layers.
- Daemon and HTTP path helper usage will be cleaned up so router code stops carrying misleading wrapper modules for state-root/config/log path concerns.

The observable proof is that focused Rust tests for the touched crates continue to pass, and the remaining call sites referenced by the user move behind the intended seams.

## Progress

- [x] (2026-03-29 08:39Z) Reviewed `.agent/PLANS.md`, checked `bd ready --json`, and mapped the current cleanup hotspots in the affected crates.
- [x] (2026-03-29 08:39Z) Claimed existing bead issues `ingot-fq8` and `ingot-nu7`.
- [x] (2026-03-29 08:39Z) Created and claimed bead issues `ingot-sd1` (adapter plumbing) and `ingot-sq2` (path-helper cleanup).
- [x] (2026-03-29 10:42Z) Refactored shared adapter subprocess/schema plumbing in `crates/ingot-agent-adapters/` by introducing shared command config plus schema/result-file launch helpers and simplifying the Codex/Claude adapter files.
- [x] (2026-03-29 10:45Z) Split workflow evaluator projection logic/tests into focused modules under `crates/ingot-workflow/src/evaluator/` while keeping the public evaluator API stable.
- [x] (2026-03-29 10:48Z) Extracted shared mirror-refresh helpers in `ingot-git::project_repo` and migrated runtime/HTTP callers plus the HTTP repo-path resolver closures.
- [x] (2026-03-29 10:49Z) Removed the leftover misleading router wrapper by replacing `support/project_repo.rs` with `support/sort_key.rs` and moved HTTP state-root access behind `AppState` helpers.
- [x] (2026-03-29 10:56Z) Ran `cargo fmt --all`, focused crate tests, `cargo clippy --all-targets -- -D warnings`, and a full successful `make ci`.
- [ ] Close completed bead issues, commit, rebase/push, and record final results here.

## Surprises & Discoveries

- Observation: `ingot-k94` was already closed, but the user’s “remaining HTTP infra-port” examples now point at route-level calls to `HttpInfraAdapter::refresh_project_mirror` and local repo-path resolver closures rather than raw `ingot_git` calls.
  Evidence: `crates/ingot-http-api/src/router/jobs.rs`, `crates/ingot-http-api/src/router/projects.rs`, and `crates/ingot-http-api/src/router/app.rs`.

- Observation: `crates/ingot-http-api/src/router/support/project_repo.rs` no longer contains project-repo path logic at all; it only wraps `next_sort_key` lookup. The file name is now misleading and is part of the remaining cleanup debt.
  Evidence: `crates/ingot-http-api/src/router/support/project_repo.rs`.

- Observation: the first full `make ci` run failed only on a new Clippy `too_many_arguments` warning in the new adapter helper, not on behavior or test regressions.
  Evidence: `cargo clippy --all-targets -- -D warnings` initially rejected `launch_adapter_with_schema_and_result_files` until the file arguments were grouped into `SchemaResultFiles`.

## Decision Log

- Decision: Use one ExecPlan for all four cleanups instead of separate plan files.
  Rationale: The user requested one bundled “complete to 5/5” pass, and the touched seams overlap in runtime/router ownership and cleanup validation.
  Date/Author: 2026-03-29 / Codex

- Decision: Treat `ingot-fq8` as the tracker for the remaining runtime/HTTP mirror-refresh consolidation even though the earlier router-only extraction issue `ingot-k94` is already closed.
  Rationale: The remaining problem is the shared service boundary between runtime and HTTP, not the already-finished direct `ingot_git` route-call cleanup tracked by `ingot-k94`.
  Date/Author: 2026-03-29 / Codex

## Outcomes & Retrospective

The four cleanup tracks are now structurally complete. The adapter crate has one shared place for the remaining CLI command configuration and schema/result-file launch plumbing. The workflow evaluator now keeps its public API in `evaluator.rs` while the projection logic and tests live in dedicated submodules. Runtime and HTTP both consume the same project-repo path/mirror-refresh helpers from `ingot-git`, and the HTTP router now exposes state-root-derived paths and infra access through `AppState` instead of scattering that wiring through route modules. The old `support/project_repo.rs` wrapper is gone, replaced by a correctly named `support/sort_key.rs`.

The only implementation surprise was the strict Clippy threshold on helper argument count. Packaging the result-file metadata into `SchemaResultFiles` kept the shared helper without suppressing the lint. Validation stayed green after that adjustment, including the full `make ci` gate.

## Context and Orientation

The adapter cleanup lives in `crates/ingot-agent-adapters/src/`. Shared CLI process lifecycle code already exists in `subprocess.rs`, while `lib.rs` owns schema selection and textual result fallback. `codex.rs` and `claude_code.rs` still own adapter-specific flags, but they also still carry some shared launch/lifecycle structure that can move into shared helpers.

The workflow evaluator lives in `crates/ingot-workflow/src/evaluator.rs`. It exports `Evaluator`, `Evaluation`, `PhaseStatus`, `AllowedAction`, `AttentionBadge`, and `BoardStatus`. The file also contains the idle projection helpers and a large inline test module. `crates/ingot-workflow/src/recommended_action.rs` now centralizes named recommended actions, so the remaining cleanup is structural: move projection helpers and tests out of the large file without changing behavior.

The runtime/HTTP shared mirror-refresh seam spans `crates/ingot-agent-runtime/src/lib.rs`, `crates/ingot-http-api/src/router/infra_ports.rs`, and `crates/ingot-git/src/project_repo.rs`. Both runtime and HTTP currently call into `ingot_git::project_repo::refresh_project_mirror`, and both still locally own pieces of state-root/project-path wiring. The goal is to expose one shared helper for “project repo refresh from state root + project metadata” and reuse it consistently.

The shared path helper work lives in `crates/ingot-config/src/paths.rs` and the remaining router wrappers in `crates/ingot-http-api/src/router/support/`. `support/config.rs` and `support/project_repo.rs` are the obvious remaining cleanup spots because they retain local wrapper logic around already-centralized helpers or now-misnamed responsibilities.

## Plan of Work

First, update `crates/ingot-agent-adapters/src/subprocess.rs` with any missing helper(s) needed to own shared schema/file lifecycle or adapter-launch plumbing. Then simplify `crates/ingot-agent-adapters/src/codex.rs` and `crates/ingot-agent-adapters/src/claude_code.rs` so they mainly declare adapter-specific arguments and result parsing.

Second, convert `crates/ingot-workflow/src/evaluator.rs` into `crates/ingot-workflow/src/evaluator/` with a small `mod.rs` for public types and top-level `Evaluator::evaluate`, plus focused modules for idle/projection helper logic and tests. Keep all public exports stable through `crates/ingot-workflow/src/lib.rs`.

Third, add a shared refresh helper to the non-HTTP/non-runtime layer that computes project repo paths and refreshes mirrors from `state_root` plus `Project`, then switch `crates/ingot-agent-runtime/src/lib.rs` and `crates/ingot-http-api/src/router/infra_ports.rs` to use it. While doing this, clean up the remaining route-level refresh helper usage in `jobs.rs`, `projects.rs`, and the repo-path resolver closure in `app.rs`.

Fourth, finish the path-helper cleanup by removing or relocating misleading router support wrappers such as `support/project_repo.rs`, tightening config path loading around `ingot_config::paths`, and leaving route modules with clearer imports.

## Concrete Steps

From `/Users/aa/Documents/ingot`:

1. Refactor adapter helpers and adapter files.
2. Split the workflow evaluator into submodules and move tests out of the main file.
3. Extract the shared project mirror refresh helper and update runtime/HTTP callers.
4. Remove leftover router wrapper/path-helper clutter.
5. Run:

    cargo fmt --all
    cargo test -p ingot-agent-adapters
    cargo test -p ingot-workflow
    cargo test -p ingot-http-api
    cargo test -p ingot-agent-runtime
    cargo clippy --all-targets -- -D warnings
    make ci

6. If those pass, run the broader gate that still makes sense for the touched code:

    make ci

7. Close completed bead issues, then:

    git pull --rebase
    bd dolt push
    git push
    git status

Completed results:

    $ cargo test -p ingot-agent-adapters
    test result: ok. 29 passed; 0 failed

    $ cargo test -p ingot-workflow
    test result: ok. 16 passed; 0 failed

    $ cargo test -p ingot-http-api
    test result: ok. all lib/integration tests passed

    $ cargo test -p ingot-agent-runtime
    test result: ok. all lib/integration tests passed

    $ cargo clippy --all-targets -- -D warnings
    Finished `dev` profile ... target(s) in 8.12s

    $ make ci
    ... cargo/ui tests, clippy, biome, rustfmt --check, and ui build all passed ...

## Validation and Acceptance

Acceptance is:

1. `crates/ingot-agent-adapters/src/codex.rs` and `crates/ingot-agent-adapters/src/claude_code.rs` no longer own shared subprocess/schema plumbing that can live in `subprocess.rs` or shared helper code.
2. `crates/ingot-workflow/src/evaluator.rs` is no longer a single monolithic implementation/test file; projection logic and tests are separated while behavior stays unchanged.
3. Runtime and HTTP mirror refresh code use one shared helper/service for state-root + project repo refresh.
4. The remaining router support/path wrappers are cleaned up so there is no misleading `project_repo` helper module left for non-path logic.
5. Focused tests for the touched crates pass.
6. The related bead issues are closed and the branch is pushed.

## Idempotence and Recovery

This work is internal refactoring and should be safe to repeat. If a module split or helper extraction causes broad compile failures, the safe recovery path is to complete one seam at a time and keep behavior-preserving forwarding wrappers until all callers compile, then remove the wrappers in a final cleanup patch.

## Artifacts and Notes

Initial evidence:

    $ rg -n "refresh_project_mirror\(|project_repo_paths\(|job_logs_dir\(|global_config_path\(|default_state_root\(" apps crates -g'*.rs'
    ... crates/ingot-http-api/src/router/jobs.rs ...
    ... crates/ingot-http-api/src/router/projects.rs ...
    ... crates/ingot-http-api/src/router/app.rs ...
    ... crates/ingot-agent-runtime/src/lib.rs ...

    $ wc -l crates/ingot-workflow/src/evaluator.rs
        1243 crates/ingot-workflow/src/evaluator.rs

## Interfaces and Dependencies

At the end of this change:

- `crates/ingot-agent-adapters/src/subprocess.rs` should expose the shared helper(s) needed for adapter launch/schema/file lifecycle.
- `crates/ingot-workflow/src/evaluator/` should keep the public evaluator API stable while splitting projection helpers/tests into internal modules.
- The shared mirror refresh helper should live in a non-HTTP/non-runtime crate and accept at least `(&impl GitOperationRepository, &Path, &Project)` or an equivalent shape that removes duplicated state-root/project field plumbing from runtime and HTTP.
- HTTP router support modules should no longer include a misleading `project_repo` wrapper for unrelated logic.

Revision note: 2026-03-29 / Codex. Created the initial ExecPlan for the bundled 5/5 cleanup pass after claiming the relevant bead issues and mapping the remaining seams.

Revision note: 2026-03-29 / Codex. Updated the plan after the refactors and successful validation runs so the completed seams, Clippy follow-up, and remaining landing steps are accurately recorded.
