# Harden implicit authoring-base binding and demo bootstrap regressions

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with `.agent/PLANS.md`.

## Purpose / Big Picture

After this change, queued work that relies on implicit authoring-base binding will start from the actual current `target_ref` head when the first mutating authoring step really begins, not from a stale commit captured earlier while the item sat in the queue. This prevents later items from replaying stale “add the scaffold” commits onto a `main` branch that already contains the scaffold, which is the direct cause of the convergence conflicts observed in `~/.ingot/logs` for the demo finance-tracker project.

The second user-visible outcome is that demo projects become a supported regression scenario rather than an untested bootstrap shortcut. After implementation, a developer will be able to create a demo project, prove that its initial revisions are still implicit revisions, then exercise a stale queued `author_initial` job against that demo project and observe that runtime execution rebinds the job to the then-current `main` head before the first authoring workspace is created.

## Progress

- [x] (2026-03-22 10:15Z) Investigated the failing finance-tracker demo project in `~/.ingot/logs` and `~/.ingot/ingot.db`; confirmed that convergence conflicts were `AA` add/add conflicts caused by later items replaying scaffold commits onto `main` after item `001` had already converged.
- [x] (2026-03-22 10:29Z) Verified that the normal `POST /items` path already derives `seed_target_commit_oid` from the live target head and leaves `seed_commit_oid` null for implicit seeds.
- [x] (2026-03-22 10:38Z) Identified the primary runtime bug: `crates/ingot-agent-runtime/src/lib.rs` snapshots `author_initial` head input at autopilot queue time, and `crates/ingot-workspace/src/lib.rs` later binds the implicit authoring base from that stale queued head when the workspace is provisioned.
- [x] (2026-03-22 10:44Z) Identified the demo-specific divergence: `crates/ingot-http-api/src/demo/mod.rs` creates the full template backlog against one eagerly resolved target snapshot and currently has no route or end-to-end regression coverage.
- [x] (2026-03-22 10:53Z) Drafted this ExecPlan covering the runtime fix, demo bootstrap alignment, and extensive regression coverage.
- [x] (2026-03-22 10:39Z) Deep-read the plan’s referenced files plus adjacent queueing, execution, persistence, manual-dispatch, and reconciliation paths. Corrected the plan to cover the real invariant carriers (`seed_target_commit_oid`, queued `jobs.input_head_commit_oid`, and authoring `workspaces.base_commit_oid`) and the actual alternate paths that preserve or reuse them.
- [x] (2026-03-22 10:47Z) Re-audited adjacent runtime cleanup tests and HTTP test support. Tightened the plan to reuse the existing `cleanup_supervised_task` unit-test seam and to specify an in-binary `HOME` guard based on standard-library synchronization, since this repo has no existing env-mutation helper or `serial_test` dependency.
- [x] (2026-03-22 11:16Z) Implemented `prepare_run(...)` rebinding in `crates/ingot-agent-runtime/src/lib.rs`, persisting refreshed queued `JobInput::authoring_head(...)` rows before authoring workspace provisioning and skipping explicit seeds plus revisions that already have an authoring workspace.
- [x] (2026-03-22 11:16Z) Narrowed the autopilot usecase contract in `crates/ingot-usecases/src/dispatch.rs` so implicit `author_initial` dispatch now requires a caller-supplied live head instead of silently falling back to `revision.seed.seed_target_commit_oid()`.
- [x] (2026-03-22 11:16Z) Aligned `crates/ingot-http-api/src/demo/mod.rs` with per-item implicit seed resolution and added dedicated demo-route coverage in `crates/ingot-http-api/tests/demo_project_routes.rs`.
- [x] (2026-03-22 11:16Z) Added focused regressions proving execution-time rebinding, cleanup-path persistence, stricter autopilot dispatch semantics, demo-route `HOME` isolation, and demo-created stale-job runtime correction.
- [x] (2026-03-22 11:16Z) Ran the focused binaries plus `make test` and `make ci`; all commands passed.

## Surprises & Discoveries

- Observation: the original “seed commit bug” fix is present in the normal item creation route, but it does not protect queued autopilot jobs that were already given a concrete `author_initial` head.
  Evidence: `crates/ingot-http-api/tests/item_routes.rs` already asserts that initial revisions have `seed_commit_oid = null`, while the failing database rows show much later `author_initial` jobs still queued with `input_head_commit_oid = 057bd6f...`.

- Observation: the convergence failures in the demo project were not generic content conflicts; they were immediate add/add collisions on scaffold files such as `.gitignore`, `client/index.html`, `client/src/App.jsx`, `server/db.js`, and `server/index.js`.
  Evidence: the conflicted integration worktrees under `~/.ingot/worktrees/prj_019d122182547bd18ff9e1301be192b6/` show `AA` entries in `git status --short` and only stage-2/stage-3 entries in `git ls-files -u`.

- Observation: demo projects currently have no direct route or end-to-end regression tests even though they are a high-risk bootstrap path that pre-creates many backlog items at once.
  Evidence: `rg -n "demo-project|create_demo_project|DemoProject" crates/ingot-http-api/tests crates/ingot-agent-runtime/tests -S` returned no matches.

- Observation: the spec already chose the correct semantic model for implicit seeds.
  Evidence: `SPEC.md:1036` says the first mutating authoring dispatch must atomically resolve the current `target_ref` head, bind the authoring workspace base to that commit, and record the same commit in `job_input.head_commit_oid`.

- Observation: the manual HTTP dispatch path already performs the late bind correctly; the stale-state gap is specific to queued runtime execution.
  Evidence: `crates/ingot-http-api/src/router/dispatch.rs::bind_dispatch_subjects_if_needed` resolves `revision.target_ref`, writes `JobInput::authoring_head(resolved_head)`, and immediately calls `ensure_authoring_workspace(...)`. `crates/ingot-http-api/tests/dispatch_routes.rs::dispatch_item_job_route_binds_implicit_author_initial_from_target_head` already covers that path.

- Observation: `claim_queued_agent_job_execution(...)` cannot repair a stale queued `job_input` because it only writes assignment, prompt, lease, and running-state fields.
  Evidence: `crates/ingot-store-sqlite/src/store/job.rs::claim_queued_agent_job_execution` does not update `job_input_kind`, `input_base_commit_oid`, or `input_head_commit_oid`. Any refreshed `author_initial` head must be persisted earlier with `update_job(...)` while the row is still queued.

- Observation: demo-route tests will write into the real home directory unless they override `HOME`, and those tests will race each other if they mutate `HOME` concurrently.
  Evidence: `crates/ingot-http-api/src/demo/mod.rs::create_demo_project` derives `project_dir` from `std::env::var("HOME")`, prefers `$HOME/Documents`, and rejects existing paths.

- Observation: the repository does not currently provide a reusable helper crate or test dependency for serializing process-global environment mutation.
  Evidence: `rg -n "serial_test|temp_env|set_var\\(|remove_var\\(" crates/ingot-http-api/tests crates/ingot-agent-runtime/tests crates -S` found no existing `serial_test` or env-helper usage in these test trees.

- Observation: on this toolchain, `std::env::set_var` and `std::env::remove_var` are `unsafe`, so the demo-route tests needed both synchronization and explicit `unsafe` blocks.
  Evidence: the first `cargo test -p ingot-http-api --test demo_project_routes` compile failed with `error[E0133]: call to unsafe function 'set_var' is unsafe and requires unsafe block`.

- Observation: the new demo integration test needed a direct dev-dependency on `ingot-agent-protocol` because `AgentRunner`’s trait signature exposes protocol request and response types.
  Evidence: the first `cargo test -p ingot-http-api --test demo_project_routes` compile failed with `error[E0433]: use of unresolved module or unlinked crate 'ingot_agent_protocol'`.

## Decision Log

- Decision: fix the primary bug at runtime execution time, not only at queue time.
  Rationale: the observed failure happens because a queued `author_initial` job can sit behind other work while `main` advances. Recomputing only when the job is queued is not strong enough; the binding must be refreshed immediately before the first authoring workspace for an implicit revision is created or reused.
  Date/Author: 2026-03-22 / Codex

- Decision: keep the semantic source of truth for implicit binding in the existing authoring workspace `base_commit_oid`, as the spec already intends.
  Rationale: the workspace record is the durable proof of when a revision’s implicit base became fixed. The implementation should refresh the incoming head for the first binding, then preserve that bound base for every later review and convergence step.
  Date/Author: 2026-03-22 / Codex

- Decision: treat the demo project route as a first-class regression surface and add dedicated tests rather than relying only on generic item-route and runtime tests.
  Rationale: the failing project was created through `POST /api/demo-project`, and that path currently bypasses any dedicated assertions about implicit seeding, queue behavior, or multi-item bootstrap correctness.
  Date/Author: 2026-03-22 / Codex

- Decision: align demo bootstrap with shared item-creation semantics instead of keeping an independent seed-resolution path.
  Rationale: the normal item route already encodes the intended initial revision shape. Reusing or mirroring that logic reduces the chance that demo bootstrap drifts from production behavior again.
  Date/Author: 2026-03-22 / Codex

- Decision: do not change the already-correct manual dispatch path while fixing the runtime queue/execution path.
  Rationale: `crates/ingot-http-api/src/router/dispatch.rs` already late-binds implicit authoring work against the live target head and has route coverage. Changing it would add risk without addressing the stale queued-row invariant that caused the real failure.
  Date/Author: 2026-03-22 / Codex

- Decision: the execution-time refresh must update the queued job row before assignment, workspace attachment, or spawn.
  Rationale: cleanup and recovery helpers such as `cleanup_unclaimed_prepared_workspace`, `reconcile_assigned_job`, and `reconcile_inert_assigned_dispatch_job` preserve `job_input` as-is. Persisting the refreshed head before those paths can run makes the invariant survive retries without any separate recovery migration.
  Date/Author: 2026-03-22 / Codex

- Decision: keep `seed_target_commit_oid` as an audit baseline and do not mutate it when rebinding stale queued jobs.
  Rationale: the spec and current domain model treat `seed_target_commit_oid` as revision-creation history, not the mutable bound authoring base. The runtime fix must update `jobs.input_head_commit_oid` and `workspaces.base_commit_oid`, not rewrite revision rows.
  Date/Author: 2026-03-22 / Codex

- Decision: implement demo-test `HOME` isolation with a test-local standard-library guard instead of adding a new test dependency.
  Rationale: this repository already uses shared `common/mod.rs` helpers plus standard-library synchronization, and the deep-read found no `serial_test` or env-helper dependency to reuse. A `static` guard such as `OnceLock<std::sync::Mutex<()>>` inside `demo_project_routes.rs` is enough to serialize `HOME` mutation without widening the dependency surface.
  Date/Author: 2026-03-22 / Codex

- Decision: leave `crates/ingot-workspace/src/lib.rs` unchanged and fix the stale-head invariant entirely in the runtime plus usecase caller contract.
  Rationale: `ensure_authoring_workspace_state(...)` already binds implicit bases from `job.job_input.head_commit_oid()` as intended. Changing workspace provisioning would duplicate repository and persistence responsibilities that `prepare_run(...)` already owns.
  Date/Author: 2026-03-22 / Codex

- Decision: resolve the demo route’s implicit target head inside the per-item creation loop instead of extracting a broader shared helper.
  Rationale: the demo path only needs the implicit-seed subset of normal item creation semantics. Per-item resolution keeps the code local, matches the plan’s “minimal helper only if needed” guidance, and is locked by the new route tests.
  Date/Author: 2026-03-22 / Codex

## Outcomes & Retrospective

The implementation landed in the runtime, the autopilot dispatch usecase, the demo route, and focused regression tests. Queued implicit `author_initial` jobs are now rebound from the live `target_ref` head at `prepare_run(...)` time before the first authoring workspace is created, and that refreshed head is persisted back into the queued `jobs` row so cleanup and recovery keep the corrected state.

The second outcome is durable demo-route coverage. `POST /api/demo-project` now has its own integration-test binary, the tests isolate `HOME` with a process-local guard, and the route-plus-runtime regression proves that a stale queued job attached to a real demo-created revision is corrected to the advanced branch head before the first workspace base is bound.

The main lesson from the investigation held up during implementation: fixing revision creation alone was insufficient. The real invariant lives across queued job persistence, workspace binding, and retry/recovery paths, so the runtime had to own the first-bind refresh while the usecase contract had to stop silently reintroducing the stale audit snapshot.

## Context and Orientation

An “implicit authoring base” in this repository means a revision where `seed_commit_oid` is null. The revision still stores `seed_target_commit_oid`, which is an audit snapshot of where the target branch pointed when the revision was created, but the real authoring base is not supposed to become fixed until the first mutating authoring step creates the authoring workspace. The durable place where that binding lives is the authoring workspace’s `base_commit_oid`.

The invariant-bearing fields for this change are concrete:

- `item_revisions.seed_commit_oid`: null means the revision still needs a late authoring-base bind.
- `item_revisions.seed_target_commit_oid`: immutable audit baseline captured at revision creation.
- `jobs.job_input_kind` plus `jobs.input_head_commit_oid`: queued and running step input persisted in SQLite. This is what stale queued `author_initial` jobs carried in the failing project.
- `workspaces.base_commit_oid`: the canonical bound authoring base after the first authoring workspace exists.
- `workspaces.head_commit_oid`: the current workspace tip used by later authoring, review, validation, and convergence logic.

The full lifecycle matters because multiple crates touch these fields.

Revision creation happens in three places. `crates/ingot-http-api/src/router/items.rs::create_item` creates normal items and already resolves `seed_target_commit_oid` from the current target head. `crates/ingot-http-api/src/router/items.rs::build_superseding_revision` does the same for revise and reopen flows. `crates/ingot-http-api/src/demo/mod.rs::create_demo_project` creates template items directly and currently inlines its own implicit seed construction.

Queued autopilot authoring work is created in `crates/ingot-agent-runtime/src/lib.rs::auto_dispatch_autopilot_locked`, which calls `crates/ingot-usecases/src/dispatch.rs::auto_dispatch_autopilot`. That usecase creates the `Job` and persists `JobInput::authoring_head(...)` through `crates/ingot-store-sqlite/src/store/job.rs::create_job`. In the current code, the production caller is only the runtime. The unit-test-only fallback inside `bind_autopilot_authoring_head_if_needed` still uses `revision.seed.seed_target_commit_oid()` when the caller passes no fresh head, but the production runtime already passes a live head at queue time.

The stale queued-row failure appears later, at execution time. `crates/ingot-agent-runtime/src/lib.rs::prepare_run` reloads the queued job, refreshes the project mirror, and then calls `prepare_workspace`. For authoring work, `prepare_workspace` delegates to `crates/ingot-workspace/src/lib.rs::ensure_authoring_workspace_state`, which uses `job.job_input.head_commit_oid()` when `revision.seed.seed_commit_oid()` is null and there is no existing authoring workspace. That is where an old queued `input_head_commit_oid` becomes a wrong `workspaces.base_commit_oid`.

Persistence details are important. `crates/ingot-store-sqlite/src/store/job.rs::claim_queued_agent_job_execution` does not rewrite `job_input`; it only claims a queued job into running state. `crates/ingot-agent-runtime/src/lib.rs::cleanup_unclaimed_prepared_workspace`, `reconcile_assigned_job`, and `reconcile_inert_assigned_dispatch_job` also preserve `job_input` unchanged. That means the refreshed execution-time head must be written into the queued `jobs` row before assignment if we want retries and recovery to stay correct.

The manual HTTP dispatch path is already correct and should remain a control sample. `crates/ingot-http-api/src/router/dispatch.rs::bind_dispatch_subjects_if_needed` resolves the current target head on demand, writes `JobInput::authoring_head(resolved_head)`, and immediately calls `crates/ingot-http-api/src/router/items.rs::ensure_authoring_workspace`. `crates/ingot-http-api/tests/dispatch_routes.rs` already proves that path binds implicit authoring work from the live target head.

Demo-route tests need one more piece of orientation. `crates/ingot-http-api/src/demo/mod.rs::create_demo_project` writes a real git repo under `$HOME/Documents` or `$HOME`. New tests for this route must set `HOME` to a temp directory and serialize any environment mutation inside that test binary, otherwise the suite will touch the real home directory or race concurrent tests. There is no existing env-mutation helper in the repo, so `crates/ingot-http-api/tests/demo_project_routes.rs` should define its own small guard, for example a `static OnceLock<std::sync::Mutex<()>>`, and keep the `HOME` override scoped to each test.

## Plan of Work

### Milestone 1: refresh implicit `author_initial` heads at `prepare_run` and persist the refresh before assignment

At the end of this milestone, a queued implicit `author_initial` job can sit in SQLite with an old `input_head_commit_oid`, but the first call to `prepare_run` will overwrite that stale queued input with the live `target_ref` head before any workspace is created or reused. The first authoring workspace will then bind `base_commit_oid` to that refreshed head, and every later step will continue to derive from that bound workspace state.

Implement this in `crates/ingot-agent-runtime/src/lib.rs`, inside or immediately before `prepare_run`, not inside `ensure_authoring_workspace_state`. The runtime has the project lock, mirror path, loaded `Job`, loaded `ItemRevision`, and `Database` handle needed to both resolve the current head and persist the corrected queued row. `crates/ingot-workspace/src/lib.rs` intentionally does not have repository or database context to do that safely.

Add a small runtime helper in `lib.rs` with logic equivalent to: if `job.step_id == step::AUTHOR_INITIAL`, `job.workspace_kind == WorkspaceKind::Authoring`, `job.execution_permission == ExecutionPermission::MayMutate`, `revision.seed.seed_commit_oid().is_none()`, and `self.db.find_authoring_workspace_for_revision(revision.id).await?` returns `None`, then resolve `revision.target_ref` against the refreshed mirror, replace `job.job_input` with `JobInput::authoring_head(resolved_head)`, and persist the queued row with `self.db.update_job(&job).await?`. If any of those conditions are false, leave the job unchanged.

Keep the rebind narrow. Do not rewrite explicit seeds. Do not rewrite jobs once an authoring workspace already exists, because `workspaces.base_commit_oid` is then the source of truth. Do not mutate `revision.seed.seed_target_commit_oid()`. Do not touch manual dispatch code in `crates/ingot-http-api/src/router/dispatch.rs`, because that path already late-binds correctly and is separately tested.

This milestone must also account for the recovery and retry paths that reuse the same rows. Because `claim_queued_agent_job_execution(...)` cannot update `job_input`, the new helper must run before assignment. Because `cleanup_unclaimed_prepared_workspace`, `reconcile_assigned_job`, and `reconcile_inert_assigned_dispatch_job` only change status and workspace attachment, persisting the refreshed head before assignment is enough to make those paths inherit the fix without additional production changes.

Acceptance for this milestone is observable in SQLite and in the authoring workspace: the queued `jobs.input_head_commit_oid` changes from the stale commit to the live `target_ref` head during `prepare_run`, and the newly created authoring workspace records that same commit as `base_commit_oid`.

### Milestone 2: lock the alternate-path invariants with focused runtime tests

At the end of this milestone, the repository will have tests that cover not just the happy path of `prepare_run`, but also the cleanup and recovery paths that preserve or reuse the refreshed queued `author_initial` head.

Put the primary execution-time regression near the code it exercises. `crates/ingot-agent-runtime/src/lib.rs` already contains unit tests that call the private `prepare_run(...)` helper directly, including cleanup coverage in the same neighborhood. Add a new unit test there that creates an implicit revision, inserts a queued `author_initial` job with a stale `JobInput::authoring_head(old_head)`, advances `main`, calls `prepare_run(...)`, and then reloads both the job row and the workspace row. The assertions must prove three things: `jobs.input_head_commit_oid` was rewritten to the advanced head, `workspaces.base_commit_oid` equals the advanced head, and `prepared.original_head_commit_oid` also reflects the advanced head for later cleanup paths.

Cover the two alternate paths separately, because the repository already splits them across test locations. In `crates/ingot-agent-runtime/src/lib.rs`, extend the existing cleanup-oriented unit-test seam around `cleanup_supervised_task_releases_workspace_for_unclaimed_prepared_agent_job` so the refreshed implicit head is asserted before and after cleanup. In `crates/ingot-agent-runtime/tests/reconciliation.rs`, add a startup-recovery or assigned-reconciliation regression that begins with a prepared or assigned implicit `author_initial` job whose queued head was already refreshed, then confirms that `reconcile_assigned_job(...)` or `reconcile_inert_assigned_dispatch_job(...)` returns the row to `Queued` without reverting `job_input.head_commit_oid`.

Keep the existing queue-time coverage in `crates/ingot-agent-runtime/tests/auto_dispatch.rs`. That file already has a test proving `auto_dispatch_autopilot_locked(...)` writes the current target head into a queued implicit `author_initial` job at dispatch time. After the runtime fix lands, that test remains the queue-time guard, while the new `prepare_run(...)` test becomes the execution-time guard.

If implementation touches `crates/ingot-usecases/src/dispatch.rs` at all, limit that change to behavior the production runtime actually needs and update `crates/ingot-usecases/src/dispatch.rs` tests accordingly. The current deep-read showed only one production caller of `auto_dispatch_autopilot(...)`, so a usecase code change is optional rather than required.

Acceptance for this milestone is met when the new unit test fails before the runtime change and passes after it, and when the reconciliation regression proves that cleanup or startup recovery preserves the refreshed queued head.

### Milestone 3: harden the demo route and add demo-project-specific regressions

At the end of this milestone, `POST /api/demo-project` will be covered as its own contract, and there will be at least one regression that starts from a real demo project rather than hand-built fixture rows.

Add a new integration-test binary at `crates/ingot-http-api/tests/demo_project_routes.rs`. Reuse `crates/ingot-http-api/tests/common/mod.rs` and build the router with `build_router_with_project_locks_and_state_root(...)` so the test can share the same temp `state_root` with a runtime `JobDispatcher` when needed. Inside that test file, add a small helper that serializes `HOME` mutation and points `HOME` at a unique temp directory for each test. Because the repo has no reusable env-mutation helper, make this helper local to the test binary with a `static OnceLock<std::sync::Mutex<()>>`, restore the previous `HOME` at the end of each test, and keep every demo project name unique. Without that helper, the demo route writes into the real home directory and concurrent tests will race on the same environment variable.

The first demo test should be route-only and should lock the initial revision contract. Call `POST /api/demo-project` for a known template such as `finance-tracker`, then assert that the response count matches the number of template items, the created project points at a temp directory under the overridden home, and every inserted initial revision has `seed_commit_oid = null`, `seed_target_commit_oid = <repo HEAD at creation>`, and `target_ref = refs/heads/main`. This test is the demo-path counterpart to `crates/ingot-http-api/tests/item_routes.rs::create_item_route_derives_initial_revision_with_null_seed_commit`.

The second demo test should be route plus runtime, but it should model the actual stale-state risk that survives deploys instead of trying to recreate the old queue-time bug with the current code. Create the demo project through the real route, pick one later template item revision, manually insert a queued stale `author_initial` job whose `job_input.head_commit_oid` equals the demo revision’s original `seed_target_commit_oid`, advance the demo project’s `main` branch by creating a new commit in the real repo, and then run `JobDispatcher::prepare_run(...)` or `tick()` against that job using the same database and `state_root`. Assert that execution-time rebinding rewrites the queued job head and binds the authoring workspace base to the advanced `main` head. This is the concrete cross-deploy scenario that the runtime fix is supposed to harden.

If the implementation chooses to deduplicate route-local seed construction, extract only the minimal shared helper that the code actually needs. `crates/ingot-http-api/src/router/items.rs::resolve_seed_target_commit_oid(...)` is `pub(super)` inside the router module and currently validates optional caller-supplied commits against a repo path. `crates/ingot-http-api/src/demo/mod.rs` only creates implicit seeds and does not accept user-supplied seed commits. Do not force both routes through a larger shared helper if that would drag mirror-specific validation into the demo path. It is acceptable to keep `demo/mod.rs` local and rely on the new tests to lock parity with `create_item`.

Acceptance for this milestone is met when the new demo route tests pass, when they no longer touch the real home directory, and when the route-plus-runtime regression proves that a stale queued `author_initial` job on a real demo-created revision is rebound to the advanced target head before the first authoring workspace is created.

### Milestone 4: validate the targeted binaries first, then the repository gates

At the end of this milestone, the focused tests that exercise queueing, execution, recovery, and demo creation all pass, and the broader repository gates have been attempted with the results recorded here.

Run the narrow binaries first because the change crosses runtime, workspace, SQLite persistence, and HTTP setup. Once they pass, run the broader repository tests. Record the exact commands used and the final pass/fail lines in `Artifacts and Notes`.

## Concrete Steps

Work from `/Users/aa/Documents/ingot`.

Read the current implementation before editing:

    sed -n '2447,2565p' crates/ingot-agent-runtime/src/lib.rs
    sed -n '2596,2665p' crates/ingot-agent-runtime/src/lib.rs
    sed -n '233,318p' crates/ingot-workspace/src/lib.rs
    sed -n '1541,1765p' crates/ingot-agent-runtime/src/lib.rs
    sed -n '4946,4995p' crates/ingot-agent-runtime/src/lib.rs
    sed -n '137,248p' crates/ingot-store-sqlite/src/store/job.rs
    sed -n '122,250p' crates/ingot-http-api/src/demo/mod.rs
    sed -n '136,188p' crates/ingot-http-api/src/router/dispatch.rs
    sed -n '92,118p' crates/ingot-usecases/src/dispatch.rs

Implement Milestone 1 by editing:

    crates/ingot-agent-runtime/src/lib.rs

Only edit `crates/ingot-workspace/src/lib.rs` if the runtime change truly requires a smaller API or assertion tweak there. The preferred implementation leaves workspace provisioning behavior intact and feeds it a corrected `job.job_input.head_commit_oid`.

Implement Milestone 2 by editing tests in:

    crates/ingot-agent-runtime/src/lib.rs
    crates/ingot-agent-runtime/tests/reconciliation.rs
    crates/ingot-agent-runtime/tests/auto_dispatch.rs

Implement Milestone 3 by editing:

    crates/ingot-http-api/src/demo/mod.rs
    crates/ingot-http-api/tests/demo_project_routes.rs

If the implementation extracts a minimal shared helper for initial revision seed construction, place it somewhere both the item route and demo route can legally reach, such as `crates/ingot-http-api/src/router/support.rs`, and update imports accordingly. Do not assume `pub(super)` helpers inside `router/items.rs` are reachable from `demo/mod.rs`.

Run formatting and focused tests after each milestone:

    cargo fmt --all
    cargo test -p ingot-agent-runtime --lib prepare_run_rebinds_implicit_author_initial_head_after_target_advances
    cargo test -p ingot-agent-runtime --lib cleanup_supervised_task_releases_workspace_for_unclaimed_prepared_agent_job
    cargo test -p ingot-agent-runtime --test auto_dispatch
    cargo test -p ingot-agent-runtime --test reconciliation
    cargo test -p ingot-http-api --test item_routes
    cargo test -p ingot-http-api --test dispatch_routes
    cargo test -p ingot-http-api --test demo_project_routes
    cargo test -p ingot-usecases autopilot_dispatch_

If the final implementation adds a demo-specific convergence regression to an existing binary, run that binary explicitly with `cargo test -p ingot-http-api --test convergence_routes` or `cargo test -p ingot-agent-runtime --test convergence`.

Once focused tests pass, run the broader gates:

    make test
    make ci

Expected observations after Milestone 1:

    before the fix, the new prepare_run unit test leaves the queued job row at the old input_head_commit_oid and binds the authoring workspace base to the old head
    after the fix, the same test shows the advanced target head in the reloaded job row and in the workspace base/head fields

Expected observations after Milestone 2:

    cleanup or startup-recovery tests requeue the job without reverting its refreshed authoring head
    the existing auto_dispatch queue-time test still shows the current target head at dispatch time

Expected observations after Milestone 3:

    the demo route test creates the template items under a temp HOME, not the real home directory
    every demo-created initial revision remains implicit
    the route-plus-runtime demo regression proves that a stale queued authoring job on a demo-created revision is rebound to the advanced head before first workspace creation

## Validation and Acceptance

Acceptance for the primary issue is met when all of the following are true:

1. A queued implicit `author_initial` job that waits while `main` advances has its persisted `jobs.input_head_commit_oid` rewritten to the then-current `target_ref` head when execution begins.
2. The first authoring workspace for that revision records the same commit as `base_commit_oid`.
3. Cleanup and recovery paths that requeue or release the job keep the refreshed queued head rather than reintroducing the old audit baseline.
4. Explicitly seeded revisions and revisions that already have an authoring workspace do not change behavior.

Acceptance for the secondary demo issue is met when all of the following are true:

1. `POST /api/demo-project` is covered by explicit tests in its own integration-test binary.
2. Demo-created initial revisions remain implicit (`seed_commit_oid` null) and keep the same initial-revision contract as normal item creation.
3. The demo tests isolate `HOME` so they do not write into the real home directory or race each other on environment mutation.
4. A stale queued `author_initial` job attached to a real demo-created revision is corrected by the runtime before the first authoring workspace is created.

Acceptance for the full change is met when the following commands pass from the repository root:

    cargo test -p ingot-agent-runtime --lib prepare_run_rebinds_implicit_author_initial_head_after_target_advances
    cargo test -p ingot-agent-runtime --lib cleanup_supervised_task_releases_workspace_for_unclaimed_prepared_agent_job
    cargo test -p ingot-agent-runtime --test auto_dispatch
    cargo test -p ingot-agent-runtime --test reconciliation
    cargo test -p ingot-http-api --test item_routes
    cargo test -p ingot-http-api --test dispatch_routes
    cargo test -p ingot-http-api --test demo_project_routes
    make test
    make ci

If `make ci` is too slow or exposes unrelated pre-existing failures, record that fact in this document and preserve the narrower passing evidence.

## Idempotence and Recovery

No database migration or backfill is required. Old queued jobs that still carry a stale `input_head_commit_oid` are repaired lazily the first time the new runtime reaches `prepare_run(...)` for an implicit `author_initial` with no existing authoring workspace. That makes the rollout safe for persisted jobs that outlive deployment.

The runtime-side rebind must be idempotent. Running `prepare_run(...)` twice before a job starts should resolve the same current head and write the same `JobInput::authoring_head(...)` value. Once an authoring workspace exists, the runtime must stop rebinding and let `workspaces.base_commit_oid` remain authoritative.

The rollback story is also simple. If the code is reverted after some queued jobs have already been refreshed, those rows still contain valid reachable commits in `input_head_commit_oid`; no incompatibility is introduced by the new code writing the fresh head earlier.

Demo-route tests need explicit cleanup rules. Each test should set `HOME` to its own temp directory, use unique demo project names, and restore the previous `HOME` value afterward. If a test fails after creating a temp demo repo, deleting that temp home directory is sufficient cleanup; do not touch the real `~/.ingot` state used for manual investigation.

## Artifacts and Notes

The investigation that motivated this plan produced the following evidence and should be preserved as the baseline for reproduction:

    convergences table:
      conv_019d12335cab7cd1bfe5397fab0c1637 -> conflicted while applying item 003 onto e68b8ca...
      conv_019d12420a4e730390daa0cf94efabeb -> conflicted while applying item 002 onto e68b8ca...
      conv_019d126a9fdf7b12aa3165e04a917fa9 -> conflicted while applying item 009 onto e68b8ca...

    later author_initial jobs:
      job_019d1263da7e77b3b91567981cf8c634 created at 2026-03-21T21:53:47Z still had input_head_commit_oid = 057bd6f...
      item 001 had already finalized main to e68b8ca at 2026-03-21T21:00:12Z

    conflicted worktree state:
      AA .gitignore
      AA client/index.html
      AA client/src/App.jsx
      AA server/db.js
      AA server/index.js

Useful files to inspect while implementing:

    .agent/late-bound-authoring-base-spec.md
    SPEC.md
    crates/ingot-agent-runtime/src/lib.rs
    crates/ingot-agent-runtime/tests/auto_dispatch.rs
    crates/ingot-agent-runtime/tests/reconciliation.rs
    crates/ingot-http-api/src/demo/mod.rs
    crates/ingot-http-api/src/router/dispatch.rs
    crates/ingot-http-api/tests/dispatch_routes.rs
    crates/ingot-http-api/tests/item_routes.rs
    crates/ingot-store-sqlite/src/store/job.rs
    crates/ingot-store-sqlite/src/store/revision.rs
    crates/ingot-workspace/src/lib.rs

Expected focused test success looks like:

    test prepare_run_rebinds_implicit_author_initial_head_after_target_advances ... ok
    test cleanup_supervised_task_releases_workspace_for_unclaimed_prepared_agent_job ... ok
    test autopilot_dispatch_rejects_implicit_author_initial_without_live_head ... ok
    test autopilot_dispatch_binds_author_initial_from_implicit_target_head ... ok
    test create_demo_project_route_creates_implicit_initial_revisions_under_temp_home ... ok
    test demo_project_runtime_rebinds_stale_author_initial_job_to_advanced_head ... ok

Validation run captured during implementation:

    cargo test -p ingot-agent-runtime --lib prepare_run_rebinds_implicit_author_initial_head_after_target_advances
    cargo test -p ingot-agent-runtime --lib cleanup_supervised_task_releases_workspace_for_unclaimed_prepared_agent_job
    cargo test -p ingot-agent-runtime --test auto_dispatch
    cargo test -p ingot-agent-runtime --test reconciliation
    cargo test -p ingot-usecases autopilot_dispatch_
    cargo test -p ingot-http-api --test item_routes
    cargo test -p ingot-http-api --test dispatch_routes
    cargo test -p ingot-http-api --test demo_project_routes
    make test
    make ci

Observed terminal summaries:

    test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 21 filtered out; finished in 0.19s
    test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 21 filtered out; finished in 0.20s
    test result: ok. 22 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 5.04s
    test result: ok. 20 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.23s
    test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 65 filtered out; finished in 0.04s
    test result: ok. 15 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.90s
    test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.59s
    test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.45s
    make test -> passed
    make ci -> passed

## Interfaces and Dependencies

The implementation must preserve the current domain model and SQLite schema. No new revision field, job field, or workspace field is needed.

The relevant interfaces after implementation are:

- In `crates/ingot-agent-runtime/src/lib.rs`, `prepare_run(...)` must reload the queued job, decide whether it is an implicit first-bind `author_initial`, and persist a refreshed `JobInput::authoring_head(...)` before calling `prepare_workspace(...)`.
- In `crates/ingot-workspace/src/lib.rs`, `ensure_authoring_workspace_state(...)` must continue to treat `job.job_input.head_commit_oid()` as the first-bind fallback for implicit revisions, but that input should now already be refreshed by the runtime.
- In `crates/ingot-store-sqlite/src/store/job.rs`, `update_job(...)` remains the mechanism that persists the refreshed queued `job_input`, while `claim_queued_agent_job_execution(...)` continues to claim the job without rewriting input fields.
- In `crates/ingot-http-api/src/router/dispatch.rs`, `bind_dispatch_subjects_if_needed(...)` remains the correct manual late-binding path and should stay green as a non-regression control.
- In `crates/ingot-http-api/src/demo/mod.rs`, demo project creation must keep creating implicit seeds and must be covered by route tests that isolate `HOME`.

The new tests should reuse the existing Rust harnesses in `crates/ingot-agent-runtime/tests/common/mod.rs` and `crates/ingot-http-api/tests/common/mod.rs`. Do not invent a second test harness for the demo route.

Revision note: revised on 2026-03-22 after deep-reading the referenced code and adjacent lifecycle paths. This pass corrected the real fix location (`prepare_run(...)` plus persisted queued `job_input`), added recovery-path coverage, corrected test locations and command forms, documented the manual-dispatch non-regression path, and added the concrete `HOME` isolation requirement for demo-route tests.

Revision note: revised again on 2026-03-22 after re-auditing adjacent runtime cleanup tests and HTTP test support. This pass made the cleanup-path verification explicit by naming the existing `cleanup_supervised_task` unit-test seam, added the missing focused lib-test command for that path, and specified a concrete standard-library `HOME` guard pattern for `demo_project_routes.rs` because the repository has no reusable env-mutation helper or `serial_test` dependency.

Revision note: revised again on 2026-03-22 after implementation and validation. This pass marked all plan steps complete, recorded the runtime/usecase/demo-route decisions that landed, captured the toolchain-specific `unsafe` env-mutation discovery plus the added `ingot-agent-protocol` dev-dependency, and replaced expected test outcomes with the actual passing command set and summaries.
