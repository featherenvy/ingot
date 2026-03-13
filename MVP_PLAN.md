# Ingot MVP Plan

## MVP Target

The MVP is a usable local daemon that can take a registered local Git repository and a configured `codex` agent, create an item, run the full `delivery:v1` workflow, and close the item after integrated validation plus approval or auto-finalization.

For MVP, "workflow fully implemented" means:

- The operator can create an item and dispatch the next legal step.
- Authoring, incremental review, whole-candidate review, candidate validation, convergence prepare, integrated validation, and approval/finalization all work end to end.
- Repair loops work on both the candidate and integrated sides.
- The daemon owns commits, scratch refs, convergence replay, and target-ref movement.
- Agents only edit files and return structured results.
- A minimal operator surface exists via HTTP, with enough UI support to drive the flow.

Non-goals for MVP:

- Multiple agent providers behaving equally well on day one. `codex` is the priority path.
- Full product polish, advanced board UX, or complete spec-hardening around every recovery edge.
- Hosted CI, remote Git push, or PR creation.

## Current Status

### Done

- [x] `delivery:v1` now reflects the intended workflow shape:
  - mutating authoring commit
  - incremental review
  - whole-candidate review
  - final candidate validation
  - convergence prepare
  - integrated validation
  - repair loops as needed
- [x] `SPEC.md` documents the revised workflow, review subjects, and `review` vs `validate` semantics.
- [x] Read-side item evaluation works and exposes `dispatchable_step_id`, auxiliary dispatchable steps, approval state, and convergence state.
- [x] Job completion, failure, and expiration handling work for already-created jobs.
- [x] Finding extraction, dismissal, and promotion work.
- [x] `POST /api/projects/:project_id/items` creates a manual item and initial revision with frozen policy/template snapshots and derived seeds.
- [x] `POST /api/projects/:project_id/items/:item_id/jobs` creates a queued job for the current legal step and derives its commit subject inputs.
- [x] HTTP bootstrap now supports:
  - `POST/PUT/DELETE /api/projects`
  - `GET /api/projects/:project_id/config`
  - `GET/POST/PUT/DELETE /api/agents`
  - `POST /api/agents/:agent_id/reprobe`
- [x] Project registration validates the local Git repo, derives the default branch from HEAD when omitted, and stores the canonical repo path.
- [x] Agent bootstrap now probes the configured CLI path and persists `available|unavailable` health state for operator inspection.
- [x] Item creation now loads per-project config defaults from `<repo>/.ingot/config.yml` and freezes those values into the initial revision policy snapshot.
- [x] Dispatching an authoring-scoped job now provisions or reuses one daemon-owned authoring workspace for the revision, creates a `workspace_ref`, and links the queued job to that workspace.
- [x] SQLite persistence now supports transactional item+revision creation and queued job insertion.
- [x] Rust and UI tests are green after the workflow and route updates.

### Partially Done

- [x] The daemon can queue, start, heartbeat, run, retry, and cancel jobs across the MVP workflow.
- [x] The write-side orchestration for workspaces, convergences, approvals, and Git operations exists for the MVP workflow.
- [x] The UI can drive the core MVP operator flow.

## Remaining Work

### 1. Project And Agent Bootstrap

- [x] Implement project CRUD routes and persistence so a repo can be registered without manual DB seeding.
- [x] Implement agent CRUD and reprobe routes/persistence.
- [x] Decide and document the MVP adapter policy:
  - `codex` is the only adapter path targeted for runnable MVP workflow execution.
  - `claude_code` can be registered and probed, but runtime execution remains stubbed until later phases.
- [x] Add a minimal config-loading path for project defaults where item creation and dispatch need it.
  - `GET /api/config` and `GET /api/projects/:project_id/config` expose the current effective defaults.
  - item creation now freezes approval policy and rework budgets from project config when the request does not override them.

### 2. Job Dispatch Lifecycle

- [x] Add `retry` and `cancel` commands for jobs.
- [x] Add internal or public lifecycle endpoints/usecases for:
  - assign
  - start
  - heartbeat
  - cancel
- [x] Add a dispatcher loop that scans queued jobs and moves one job at a time into execution.
- [x] Ensure item/project mutation locking is used consistently for dispatch and terminal job transitions.

### 3. Workspace Manager

- [x] Implement authoring workspace provisioning from `seed_commit_oid`.
- [x] Implement review workspace provisioning from explicit diff subjects.
- [x] Implement integration workspace provisioning from current `target_ref` head.
- [x] Implement workspace reset, abandon, and remove behavior.
- [x] Create and maintain daemon-owned `workspace_ref` values for authoring workspaces.
- [x] Enforce workspace status transitions: `provisioning -> ready -> busy -> ...`.

### 4. Agent Runtime And Adapters

- [x] Implement `ingot-agent-runtime` subprocess supervision.
- [x] Implement the `codex` adapter.
- [x] Freeze prompt snapshot and template digest at dispatch/start time.
- [x] Treat agent-written commits, rebases, or ref movement as protocol violations.

### 5. Git Manager For Mutating Jobs

- [x] Implement daemon-owned commit creation for successful mutating jobs.
- [x] Stop relying on externally provided `output_commit_oid` as the mechanism of record; the daemon should create the canonical commit itself.
- [x] Record `GitOperation` rows for:
  - `create_job_commit`
  - `prepare_convergence_commit`
  - `finalize_target_ref`
  - workspace reset/remove operations
- [x] Add required commit trailers and preserve commit lineage.
- [x] Update authoring workspace head/ref after each daemon-created commit.
- [x] Compute changed paths and any diff metadata needed for revision context.

### 6. Candidate-Side Workflow Execution

- [x] Make queued `author_initial` and `repair_*` jobs runnable inside authoring workspaces.
- [x] Make queued review jobs runnable inside review workspaces with correct `input_base_commit_oid` and `input_head_commit_oid`.
- [x] Make candidate validation jobs runnable inside the authoring workspace without mutation.
- [x] Rebuild `revision_context` after every terminal job so `latest_review`, `latest_validation`, changed paths, and accepted refs stay current.
- [x] Verify that repair commits re-enter incremental review before whole-candidate review, and whole-candidate review re-enters candidate validation before convergence.

### 7. Convergence And Integrated Validation

- [x] Implement `POST /items/:item_id/convergence/prepare`.
- [x] Provision the integration workspace from current target head.
- [x] Replay the current authoring commit chain onto the target head while preserving commit boundaries.
- [x] Persist convergence state and source-to-prepared commit mapping.
- [x] Dispatch `validate_integrated` against the prepared result.
- [x] Implement stale prepared convergence invalidation when target ref moves.

### 8. Approval And Finalization

- [x] Implement `approval/approve`.
- [x] Implement `approval/reject`.
- [x] Implement daemon-only finalization for `approval_policy=not_required`.
- [x] Compare-and-swap the target ref at finalize time.
- [x] Mark the item `done` with the correct resolution source after successful finalization.

### 9. Item Lifecycle Commands Outside The Happy Path

- [x] Implement `revise`.
- [x] Implement `defer` and `resume`.
- [x] Implement `dismiss` and `invalidate`.
- [x] Implement `reopen`.

These are not on the shortest happy-path critical path, but they are part of the intended supervised workflow surface and should land before calling the workflow feature-complete.

### 10. Recovery, Journaling, And Activity

- [x] Implement startup reconciliation.
- [x] Reconcile incomplete Git operations safely on daemon restart.
- [x] Detect stale running jobs and stale workspaces.
- [x] Record activity events for item, job, convergence, approval, and finding transitions.
- [x] Retain or clean up workspaces according to retention policy and finding reachability rules.

### 11. Operator Surface

- [x] Add UI actions for:
  - create item
  - dispatch next step
  - retry job
  - cancel job
  - prepare convergence
  - approve / reject
  - board, item-detail, jobs, and workspaces pages now expose these core operator actions
- [x] Add project and agent setup UI or provide a minimal CLI/API bootstrap path.
- [x] Add job log views and clearer step/convergence status displays.
  - project jobs/workspaces pages now list execution artifacts, and jobs expose prompt/stdout/stderr/result logs
  - project activity is now visible in the UI as well

For MVP, HTTP is the hard requirement. UI can remain minimal as long as the flow is operable.

## Recommended Implementation Order

### Phase 1: Start Work

- [x] Create item with initial revision.
- [x] Dispatch the next legal job into a queued state.
- [x] Add project and agent bootstrap.

### Phase 2: Run Authoring Jobs

- [x] Implement authoring workspace provisioning.
- [x] Implement dispatcher + runtime + `codex` adapter.
- [x] Implement mutating job commit creation and revision context updates.

### Phase 3: Run Candidate Review And Validation

- [x] Implement review workspace provisioning.
- [x] Run incremental review, whole-candidate review, and candidate validation end to end.
- [x] Add retry/cancel semantics for operator control.

### Phase 4: Integrate And Close

- [x] Implement convergence prepare and integration workspace provisioning.
- [x] Run integrated validation.
- [x] Implement approve/reject/auto-finalize and item closure.

### Phase 5: Hardening And Full Workflow Surface

- [x] Add revise/defer/resume/dismiss/invalidate/reopen.
- [x] Add journaling/reconciliation/activity.
- [x] Finish the minimal operator UI.

## Acceptance Checklist

The MVP is done when all of the following are true:

- [x] A repo can be registered and a `codex` agent can be configured.
- [x] An operator can create a new item against a local target ref.
- [x] Dispatching the item creates and runs `author_initial`.
- [x] The daemon creates canonical commits for successful mutating jobs.
- [x] The candidate loop can progress through incremental review, whole-candidate review, and candidate validation, including repair loops.
- [x] Convergence can be prepared against current target head.
- [x] Integrated validation can run against the prepared result.
- [x] Approval or auto-finalization can move the target ref and close the item.
- [x] Findings are persisted and visible throughout the flow.
- [x] Restarting the daemon does not silently lose or invent workflow progress for in-flight work.

## Notes

- The current best next step is the execution path, not more read-side work.
- The shortest path to visible progress is:
  1. dispatcher/runtime for queued jobs
  2. daemon-owned commit creation
  3. revision-context rebuilds after terminal jobs
  4. review/integration workspace provisioning
- Bootstrap and initial authoring workspace provisioning are no longer the blockers. The next meaningful milestone is moving a queued authoring job from `queued` into real execution inside the provisioned workspace.
- That milestone is now partially complete: queued authoring commit jobs can auto-run, produce daemon-owned commits, and leave the item ready for the next review dispatch when a compatible `codex` agent is available.
- The remaining work after the MVP is polish and confidence-building rather than missing core capability.
