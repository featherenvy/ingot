# Ingot Architecture (Strict v1 / Revised v6)

## Overview

Ingot is a local daemon that orchestrates human-supervised AI coding work across isolated Git worktrees. It tracks durable work, provisions revision-scoped workspaces, dispatches bounded agent jobs, records structured results plus daemon-owned commits, prepares integration against a local target ref, validates the integrated result, and closes work only after policy-satisfied approval or explicit manual disposition.

The architectural center is:

* **Item is the durable work object and the only board-rendered entity.**
* **ItemRevision is the meaning-bearing contract.** It freezes the execution-relevant definition of the work for one revision.
* **Job is a bounded execution attempt attached to exactly one item revision and one unique workflow step ID.**
* **Workspace is first-class code reality.** Jobs only make sense relative to a concrete workspace and concrete Git inputs.
* **Convergence is explicit and two-stage.** Ingot prepares an integrated result, validates it there, and finalizes the target ref only after all required gates pass.
* **Git truth belongs to the daemon.** Agents do file-level work only. The daemon owns commits, scratch refs, and target-ref movement.
* **Human authority is explicit.** Human defer, dismiss, invalidate, reopen, approve, reject, and manual revision commands outrank late or stale agent events.
* **Semantics freeze at three layers.**

  * **Per item:** workflow version and graph meaning
  * **Per revision:** work contract, policies, template mapping, and budgets
  * **Per job:** exact prompt, template digest, workspace, and Git inputs

Ingot is not a generic workflow platform in v1. It is a narrow execution control layer for one thing: **single-item code delivery in a real local Git repository**.

The system has two processes:

* a **Rust daemon** that owns orchestration, persistence, workspace management, Git integration, convergence, recovery, and agent execution
* a **React frontend** that provides a live dashboard over HTTP and WebSocket

v1 operates on **local repositories and local refs only**. Remote push, PR creation, hosted CI, MCP exposure, and arbitrary workflow authoring are deferred.

---

## Design Intent

Ingot v1 is optimized for **one-item-at-a-time supervised code delivery**.

Design goals:

1. **Durable work tracking** — the item survives retries, rework loops, defer/resume, approval rejection, revision changes, and manual terminal decisions.
2. **Frozen meaning** — future config or prompt changes must not rewrite the meaning of existing work.
3. **Bounded execution** — jobs are short-lived attempts with explicit inputs, outputs, and budgets.
4. **Git truthfulness** — the daemon, not the agent, owns canonical commit creation and ref movement.
5. **Explicit convergence** — integration is prepared against the target line, validated there, and finalized only after policy is satisfied.
6. **Low operator friction** — the UI recommends the next legal action; the operator does not assemble state machines.
7. **Strong auditability** — revision contracts, step contracts, template digests, job results, commit lineage, convergence attempts, Git side effects, and manual decisions are all traceable.
8. **Conservative recovery** — crashes and process uncertainty must never be “resolved” by inventing success.

Bug work is supported in v1 as **item classification and revision contract content**, not as a separate runtime graph. If reproduction steps or regression expectations matter, they belong in the revision contract.

---

## Scope and Non-Goals

### In scope for v1

* item-centric work tracking
* a single built-in workflow: `delivery:v1`
* item classification for UI/filtering: `change|bug`
* revision-scoped authoring workspaces
* fresh review workspaces
* integration workspaces for convergence
* explicit convergence prepare, integrated validation, and finalization
* operator approval gating
* audit history, projection diagnostics, and conservative recovery

### Explicitly out of scope for v1

* multiple runtime workflows
* parent/child items, dependency graphs, or planning boards
* clone workspaces
* container/Docker workspaces
* prompt templates that control workflow semantics
* in-system manual conflict resolution continuation
* agent-driven conflict resolution
* report-only or discovery-only workflow steps
* MCP server exposure
* remote Git push, PR creation, hosted CI orchestration, or autonomous external closure
* filesystem hot-reload watchers for live config/template changes

This is a smaller v1 than the earlier draft. That is deliberate. The old draft was drifting into control-plane complexity before the hard invariants were solved.

---

## Multi-Project Model

Ingot manages multiple projects from one daemon. Each project is a registered local Git repository with its own items, revisions, jobs, workspaces, convergences, and activity.

### Config hierarchy

```text
~/.ingot/defaults.yml      # Global defaults
  → <repo>/.ingot/config.yml
    → CLI flags
```

Settings merge top-down. The daemon resolves an effective config before use.

### Prompt templates

Prompt templates are **built into the daemon** and may be overridden **per project** on disk:

```text
<repo>/.ingot/templates/*.yml
```

There is **no global on-disk template library** in v1. Built-in templates are the default; project files may override prompt text by slug.

Template changes take effect only after explicit reload via:

* daemon restart, or
* `POST /api/reload`

There is no filesystem watch in v1.

### What lives where

Two storage layers with a hard boundary:

**Filesystem**

* global defaults
* per-project config
* per-project prompt template overrides
* job logs and copied artifacts
* Git worktrees and scratch refs

**SQLite**

* projects
* agents
* items
* item revisions
* revision contexts
* workspaces
* jobs
* convergences
* Git operations
* activity

### Scope rules

* **Agents** are global. Only built-in adapters are supported in v1.
* **Workflow definitions and step contracts** are compiled into the daemon.
* **Prompt templates** are text only. They may not change workspace choice, mutability, artifact kind, retry semantics, or transitions.
* **Items do not read live workflow policy.** At item creation the daemon freezes the workflow version.
* **Revisions do not read live policy/config.** At revision creation the daemon freezes approval policy, budgets, repo-context rules, overflow behavior, and step-to-template mapping.
* **Jobs do not read live templates.** At dispatch the daemon snapshots the full prompt, template slug, and template digest.

This is the only honest split:

* **Item** freezes graph meaning
* **ItemRevision** freezes work contract and execution policy
* **Job** freezes exact execution input

---

## Core Runtime Model

Ingot separates durable work, meaning-bearing revisions, bounded execution attempts, Git workspaces, and convergence attempts.

### Project

A registered Git repository.

Fields: `id`, `name`, `path`, `default_branch`, `color`

All runtime entities are project-scoped.

### Agent

A registered AI runtime. Global across projects.

Fields:

* `id`
* `slug`
* `name`
* `adapter_kind` — `claude_code|codex`
* `provider`
* `model`
* `cli_path`
* `capabilities`
* `health_check`
* `status`

Typical capabilities:

* `read_only_jobs`
* `mutating_jobs`
* `structured_output`
* `streaming_progress`

There is no generic shell adapter contract in v1.

### Prompt Template

A reusable prompt body for one phase kind. Stored as built-in defaults plus optional project overrides.

Fields:

* `slug`
* `phase_kind` — `author|validate|review`
* `prompt`
* `enabled`

Important semantics:

* Templates control **prompt text only**
* Templates do **not** control mutability, workspace class, artifact kind, transitions, or budgets
* Existing revisions keep a frozen step→template mapping
* Existing jobs keep the full dispatched prompt and template digest

### Workflow Definition

A built-in versioned state machine compiled into the daemon.

v1 ships with exactly one runtime workflow:

* `workflow_version = delivery:v1`

A workflow definition specifies:

* unique stable `step_id` values
* step contracts
* allowed transitions
* semantic loop budgets
* default step→template mapping
* default repo-context policies
* default overflow strategies
* convergence requirement

Older workflow versions must remain available in code until no open item uses them.

### Workspace

A first-class execution context tied to one project and usually one item revision.

| Field                     | Type               | Notes                                                        |              |                                |                    |           |       |           |           |
| ------------------------- | ------------------ | ------------------------------------------------------------ | ------------ | ------------------------------ | ------------------ | --------- | ----- | --------- | --------- |
| `id`                      | UUID               | PK                                                           |              |                                |                    |           |       |           |           |
| `project_id`              | UUID               | FK                                                           |              |                                |                    |           |       |           |           |
| `kind`                    | enum `authoring    | review                                                       | integration` | Purpose and expected lifecycle |                    |           |       |           |           |
| `strategy`                | enum `worktree`    | v1 only                                                      |              |                                |                    |           |       |           |           |
| `path`                    | text               | Filesystem path                                              |              |                                |                    |           |       |           |           |
| `created_for_revision_id` | UUID nullable      | Required for authoring/integration                           |              |                                |                    |           |       |           |           |
| `parent_workspace_id`     | UUID nullable      | Lineage when relevant                                        |              |                                |                    |           |       |           |           |
| `target_ref`              | text nullable      | Intended integration target                                  |              |                                |                    |           |       |           |           |
| `workspace_ref`           | text nullable      | Daemon-owned scratch ref; required for authoring/integration |              |                                |                    |           |       |           |           |
| `base_commit_oid`         | text nullable      | Initial base relevant to this workspace                      |              |                                |                    |           |       |           |           |
| `head_commit_oid`         | text nullable      | Current observed head                                        |              |                                |                    |           |       |           |           |
| `retention_policy`        | enum `ephemeral    | retain_until_debug                                           | persistent`  | Cleanup behavior               |                    |           |       |           |           |
| `status`                  | enum `provisioning | ready                                                        | busy         | stale                          | retained_for_debug | abandoned | error | removing` | Lifecycle |
| `current_job_id`          | UUID nullable      | Running job if any                                           |              |                                |                    |           |       |           |           |
| `created_at`              | timestamp          | Required                                                     |              |                                |                    |           |       |           |           |
| `updated_at`              | timestamp          | Required                                                     |              |                                |                    |           |       |           |           |

Important semantics:

* `workspace_ref` is a daemon-owned scratch ref, never agent-owned
* authoring and integration workspaces are Git-tracked mutable contexts owned by the daemon
* review workspaces are fresh read-only worktrees over a specific review subject
* mutability is **not** a workspace property anymore; it is a **job execution permission**

#### Workspace lifecycle

**Authoring workspace**

* one per item revision
* seeded from `ItemRevision.seed_commit_oid`
* reused across authoring and non-mutating authoring steps in that revision
* retained until the revision is superseded, the item is closed, or cleanup runs

**Review workspace**

* fresh per review job
* provisioned against the exact base/head review subject
* ephemeral by default

**Integration workspace**

* one per convergence attempt
* provisioned from the current `target_ref` head
* retained while convergence is `running`, `conflicted`, or `prepared`
* removed after finalization, cancellation, failure, or debug cleanup

### Item

The durable work object and the only board-rendered entity.

| Field                 | Type                                    | Notes                                         |                                         |                   |                                     |                          |                 |                        |
| --------------------- | --------------------------------------- | --------------------------------------------- | --------------------------------------- | ----------------- | ----------------------------------- | ------------------------ | --------------- | ---------------------- |
| `id`                  | UUID                                    | PK                                            |                                         |                   |                                     |                          |                 |                        |
| `project_id`          | UUID                                    | FK                                            |                                         |                   |                                     |                          |                 |                        |
| `classification`      | enum `change                            | bug`                                          | Display/filter only; no workflow effect |                   |                                     |                          |                 |                        |
| `workflow_version`    | text                                    | Built-in workflow version, e.g. `delivery:v1` |                                         |                   |                                     |                          |                 |                        |
| `lifecycle_state`     | enum `open                              | done`                                         | Canonical lifecycle                     |                   |                                     |                          |                 |                        |
| `parking_state`       | enum `active                            | deferred`                                     | Human intent modifier while open        |                   |                                     |                          |                 |                        |
| `done_reason`         | enum `completed                         | dismissed                                     | invalidated` nullable                   | Required iff done |                                     |                          |                 |                        |
| `resolution_source`   | enum `evaluator                         | approval_command                              | manual_command` nullable                | Required iff done |                                     |                          |                 |                        |
| `approval_state`      | enum `not_required                      | not_requested                                 | pending                                 | approved`         | Current revision approval lifecycle |                          |                 |                        |
| `escalation_state`    | enum `none                              | operator_required`                            | Canonical machine-stop state            |                   |                                     |                          |                 |                        |
| `escalation_reason`   | enum `candidate_rework_budget_exhausted | integration_rework_budget_exhausted           | convergence_conflict                    | step_failed       | protocol_violation                  | manual_decision_required | other` nullable | Required iff escalated |
| `current_revision_id` | UUID                                    | FK to latest revision                         |                                         |                   |                                     |                          |                 |                        |
| `priority`            | enum `critical                          | major                                         | minor`                                  | Required          |                                     |                          |                 |                        |
| `labels`              | JSON nullable                           | Metadata only                                 |                                         |                   |                                     |                          |                 |                        |
| `operator_notes`      | text nullable                           | Metadata only                                 |                                         |                   |                                     |                          |                 |                        |
| `created_at`          | timestamp                               | Required                                      |                                         |                   |                                     |                          |                 |                        |
| `updated_at`          | timestamp                               | Required                                      |                                         |                   |                                     |                          |                 |                        |
| `closed_at`           | timestamp nullable                      | Required iff done                             |                                         |                   |                                     |                          |                 |                        |

Important semantics:

* `current_step_id` is derived, not canonical
* `next_recommended_action` is derived, not stored
* `classification` is not workflow truth
* the item survives retries, rework, approval rejection, revision changes, defer/resume, and manual terminal decisions

### ItemRevision

The meaning-bearing work contract.

| Field                    | Type           | Notes                                                                            |                          |
| ------------------------ | -------------- | -------------------------------------------------------------------------------- | ------------------------ |
| `id`                     | UUID           | PK                                                                               |                          |
| `item_id`                | UUID           | FK                                                                               |                          |
| `revision_no`            | int >= 1       | Unique per item                                                                  |                          |
| `title`                  | text           | Required                                                                         |                          |
| `description`            | text nullable  | Operator-written problem statement or context                                    |                          |
| `acceptance_criteria`    | text nullable  | Closure criteria                                                                 |                          |
| `target_ref`             | text           | Local branch/ref to converge into                                                |                          |
| `approval_policy`        | enum `required | not_required`                                                                    | Frozen for this revision |
| `policy_snapshot`        | JSON           | Frozen budgets, repo-context policies, transport retry caps, overflow strategies |                          |
| `template_map_snapshot`  | JSON           | Frozen `step_id -> template_slug` mapping                                        |                          |
| `seed_commit_oid`        | text           | Commit used to seed the authoring workspace                                      |                          |
| `seed_target_commit_oid` | text           | Target ref head when the revision was created                                    |                          |
| `supersedes_revision_id` | UUID nullable  | Prior revision if this one replaces another                                      |                          |
| `created_at`             | timestamp      | Required                                                                         |                          |

Important semantics:

* revisions are immutable once created
* any change to title, description, acceptance criteria, target ref, approval policy, or seed commit creates a new revision
* old revisions remain for audit
* future jobs for this revision read only the frozen policy snapshot and template map snapshot

### RevisionContext

A canonical deterministic resume context for one revision.

Fields:

* `item_revision_id`
* `schema_version`
* `payload` JSON
* `updated_from_job_id`
* `updated_at`

`RevisionContext` is not a hidden model session and not a free-form AI summary. It is a structured projection built deterministically from:

* current authoring head commit
* changed path manifest
* latest validation result summary
* latest review findings summary
* operator notes relevant to execution
* prior accepted structured results

Steps whose contract says `resume_context` receive the current `RevisionContext` snapshot.

### Job

A bounded execution attempt attached to exactly one item revision and exactly one stable step ID.

| Field                   | Type               | Notes                                                                  |                               |                               |                               |                     |                            |             |           |
| ----------------------- | ------------------ | ---------------------------------------------------------------------- | ----------------------------- | ----------------------------- | ----------------------------- | ------------------- | -------------------------- | ----------- | --------- |
| `id`                    | UUID               | PK                                                                     |                               |                               |                               |                     |                            |             |           |
| `project_id`            | UUID               | FK                                                                     |                               |                               |                               |                     |                            |             |           |
| `item_id`               | UUID               | FK                                                                     |                               |                               |                               |                     |                            |             |           |
| `item_revision_id`      | UUID               | FK                                                                     |                               |                               |                               |                     |                            |             |           |
| `step_id`               | text               | Stable workflow node within the frozen workflow version                |                               |                               |                               |                     |                            |             |           |
| `semantic_attempt_no`   | int >= 1           | Increments only when the workflow semantically re-enters the same step |                               |                               |                               |                     |                            |             |           |
| `retry_no`              | int >= 0           | Redispatch count for the same semantic attempt                         |                               |                               |                               |                     |                            |             |           |
| `supersedes_job_id`     | UUID nullable      | Retry lineage                                                          |                               |                               |                               |                     |                            |             |           |
| `status`                | enum `queued       | assigned                                                               | running                       | completed                     | failed                        | cancelled           | expired                    | superseded` | Lifecycle |
| `outcome_class`         | enum `clean        | findings                                                               | transient_failure             | terminal_failure              | protocol_violation            | cancelled` nullable | Normalized terminal result |             |           |
| `phase_kind`            | enum `author       | validate                                                               | review`                       | Execution intent              |                               |                     |                            |             |           |
| `workspace_id`          | UUID nullable      | Required once assigned                                                 |                               |                               |                               |                     |                            |             |           |
| `workspace_kind`        | enum `authoring    | review                                                                 | integration`                  | Frozen step contract snapshot |                               |                     |                            |             |           |
| `execution_permission`  | enum `may_mutate   | must_not_mutate`                                                       | Frozen step contract snapshot |                               |                               |                     |                            |             |           |
| `context_policy`        | enum `fresh        | resume_context`                                                        | Frozen step contract snapshot |                               |                               |                     |                            |             |           |
| `phase_template_slug`   | text               | Frozen template selection                                              |                               |                               |                               |                     |                            |             |           |
| `phase_template_digest` | text               | Frozen template content digest                                         |                               |                               |                               |                     |                            |             |           |
| `prompt_snapshot`       | text               | Full prompt written before execution                                   |                               |                               |                               |                     |                            |             |           |
| `input_base_commit_oid` | text nullable      | Review/convergence diff base when relevant                             |                               |                               |                               |                     |                            |             |           |
| `input_head_commit_oid` | text nullable      | Workspace head seen by this job at start                               |                               |                               |                               |                     |                            |             |           |
| `output_artifact_kind`  | enum `commit       | review_report                                                          | validation_report             | none`                         | Frozen step contract snapshot |                     |                            |             |           |
| `output_commit_oid`     | text nullable      | Required for successful mutating jobs                                  |                               |                               |                               |                     |                            |             |           |
| `result_schema_version` | text nullable      | Canonical structured result schema version                             |                               |                               |                               |                     |                            |             |           |
| `result_payload`        | JSON nullable      | Canonical structured result                                            |                               |                               |                               |                     |                            |             |           |
| `agent_id`              | UUID nullable      | Assigned agent                                                         |                               |                               |                               |                     |                            |             |           |
| `process_pid`           | int nullable       | Local subprocess PID when available                                    |                               |                               |                               |                     |                            |             |           |
| `lease_owner_id`        | UUID nullable      | Worker/session lease owner                                             |                               |                               |                               |                     |                            |             |           |
| `heartbeat_at`          | timestamp nullable | Last heartbeat                                                         |                               |                               |                               |                     |                            |             |           |
| `lease_expires_at`      | timestamp nullable | Lease timeout                                                          |                               |                               |                               |                     |                            |             |           |
| `error_code`            | text nullable      | Failure metadata                                                       |                               |                               |                               |                     |                            |             |           |
| `error_message`         | text nullable      | Failure metadata                                                       |                               |                               |                               |                     |                            |             |           |
| `created_at`            | timestamp          | Required                                                               |                               |                               |                               |                     |                            |             |           |
| `started_at`            | timestamp nullable | Optional                                                               |                               |                               |                               |                     |                            |             |           |
| `ended_at`              | timestamp nullable | Any terminal state                                                     |                               |                               |                               |                     |                            |             |           |

Important semantics:

* every job is a new subprocess
* there is no provider-native hidden conversation reuse
* `semantic_attempt_no` increments only when the workflow takes a new semantic edge into the step
* redispatches of the same step without progression keep the same `semantic_attempt_no` and increment `retry_no`
* non-mutating jobs may run in an authoring workspace, but they must leave no file changes behind
* successful mutating jobs always end in exactly one daemon-created canonical commit

### Convergence

A first-class record of preparing and finalizing integration of one item revision into its target line.

| Field                      | Type                            | Notes                                             |            |          |           |        |            |           |
| -------------------------- | ------------------------------- | ------------------------------------------------- | ---------- | -------- | --------- | ------ | ---------- | --------- |
| `id`                       | UUID                            | PK                                                |            |          |           |        |            |           |
| `project_id`               | UUID                            | FK                                                |            |          |           |        |            |           |
| `item_id`                  | UUID                            | FK                                                |            |          |           |        |            |           |
| `item_revision_id`         | UUID                            | FK                                                |            |          |           |        |            |           |
| `source_workspace_id`      | UUID                            | Authoring workspace for this revision             |            |          |           |        |            |           |
| `integration_workspace_id` | UUID                            | Integration worktree for this attempt             |            |          |           |        |            |           |
| `source_head_commit_oid`   | text                            | Authoring head being integrated                   |            |          |           |        |            |           |
| `target_ref`               | text                            | Target branch/ref                                 |            |          |           |        |            |           |
| `strategy`                 | enum `rebase_then_fast_forward` | Fixed in v1                                       |            |          |           |        |            |           |
| `status`                   | enum `queued                    | running                                           | conflicted | prepared | finalized | failed | cancelled` | Lifecycle |
| `input_target_commit_oid`  | text                            | Target head at prepare start                      |            |          |           |        |            |           |
| `prepared_commit_oid`      | text nullable                   | Integrated result commit in integration workspace |            |          |           |        |            |           |
| `final_target_commit_oid`  | text nullable                   | Target ref after successful finalization          |            |          |           |        |            |           |
| `conflict_summary`         | text nullable                   | Human-readable conflict summary                   |            |          |           |        |            |           |
| `created_at`               | timestamp                       | Required                                          |            |          |           |        |            |           |
| `completed_at`             | timestamp nullable              | Optional                                          |            |          |           |        |            |           |

Important semantics:

* convergence is item-revision-scoped
* `prepared` means the integrated result exists and the target ref has **not** moved
* `finalized` means the target ref was compare-and-swapped from `input_target_commit_oid` to the prepared commit
* if the target ref moves after preparation but before finalization, the convergence fails and approval is cleared

### GitOperation

A journal entry for every daemon-owned Git side effect.

| Field              | Type                    | Notes                          |                     |                 |                       |                      |
| ------------------ | ----------------------- | ------------------------------ | ------------------- | --------------- | --------------------- | -------------------- |
| `id`               | UUID                    | PK                             |                     |                 |                       |                      |
| `project_id`       | UUID                    | FK                             |                     |                 |                       |                      |
| `operation_kind`   | enum `create_job_commit | prepare_convergence_commit     | finalize_target_ref | reset_workspace | remove_workspace_ref` | Git side effect kind |
| `entity_type`      | enum `job               | convergence                    | workspace           | item_revision`  | Owning entity         |                      |
| `entity_id`        | UUID                    | Owning entity ID               |                     |                 |                       |                      |
| `workspace_id`     | UUID nullable           | Related workspace              |                     |                 |                       |                      |
| `ref_name`         | text nullable           | Target ref or workspace ref    |                     |                 |                       |                      |
| `expected_old_oid` | text nullable           | CAS precondition when relevant |                     |                 |                       |                      |
| `new_oid`          | text nullable           | Intended new ref/commit        |                     |                 |                       |                      |
| `commit_oid`       | text nullable           | Created commit when relevant   |                     |                 |                       |                      |
| `status`           | enum `planned           | applied                        | reconciled          | failed`         | Journal lifecycle     |                      |
| `metadata`         | JSON                    | Structured details             |                     |                 |                       |                      |
| `created_at`       | timestamp               | Required                       |                     |                 |                       |                      |
| `completed_at`     | timestamp nullable      | Optional                       |                     |                 |                       |                      |

Important semantics:

* a `GitOperation` row is written **before** a daemon-owned Git side effect
* daemon-created commits must include trailers carrying at least:

  * `Ingot-Operation: <git_operation_id>`
  * `Ingot-Item: <item_id>`
  * `Ingot-Revision: <revision_no>`
  * `Ingot-Job: <job_id>` or `Ingot-Convergence: <convergence_id>`
* startup reconciliation uses this journal plus actual Git state to adopt or fail incomplete work safely

### Activity

An append-only structured event log.

Typical event types:

* `item_created`
* `item_revision_created`
* `item_updated`
* `item_deferred`
* `item_resumed`
* `item_dismissed`
* `item_invalidated`
* `item_reopened`
* `item_escalated`
* `item_escalation_cleared`
* `job_dispatched`
* `job_completed`
* `job_failed`
* `job_cancelled`
* `approval_requested`
* `approval_approved`
* `approval_rejected`
* `convergence_started`
* `convergence_conflicted`
* `convergence_prepared`
* `convergence_finalized`
* `convergence_failed`
* `git_operation_planned`
* `git_operation_reconciled`

Activity is audit history, not source of truth.

---

## Built-In Workflow Definition

v1 ships with one built-in runtime workflow:

* `delivery:v1`

All items use this workflow. A `bug` item is still executed through `delivery:v1`; the classification affects UI and operator framing only.

### Step contracts

Step contracts are code-owned and versioned with the workflow. They are not editable in YAML.

| `step_id`                           | `phase_kind` | `workspace_kind` | `execution_permission` | `context_policy` | `output_artifact_kind` | Default template      |
| ----------------------------------- | ------------ | ---------------- | ---------------------- | ---------------- | ---------------------- | --------------------- |
| `author_initial`                    | `author`     | `authoring`      | `may_mutate`           | `fresh`          | `commit`               | `author-initial`      |
| `validate_candidate_initial`        | `validate`   | `authoring`      | `must_not_mutate`      | `resume_context` | `validation_report`    | `validate-candidate`  |
| `review_candidate_initial`          | `review`     | `review`         | `must_not_mutate`      | `fresh`          | `review_report`        | `review-candidate`    |
| `repair_candidate`                  | `author`     | `authoring`      | `may_mutate`           | `resume_context` | `commit`               | `repair-candidate`    |
| `validate_candidate_repair`         | `validate`   | `authoring`      | `must_not_mutate`      | `resume_context` | `validation_report`    | `validate-candidate`  |
| `review_candidate_repair`           | `review`     | `review`         | `must_not_mutate`      | `fresh`          | `review_report`        | `review-candidate`    |
| `prepare_convergence`               | `system`     | `integration`    | `daemon_only`          | —                | `none`                 | —                     |
| `validate_integrated`               | `validate`   | `integration`    | `must_not_mutate`      | `resume_context` | `validation_report`    | `validate-integrated` |
| `repair_after_integration`          | `author`     | `authoring`      | `may_mutate`           | `resume_context` | `commit`               | `repair-integrated`   |
| `validate_after_integration_repair` | `validate`   | `authoring`      | `must_not_mutate`      | `resume_context` | `validation_report`    | `validate-candidate`  |
| `review_after_integration_repair`   | `review`     | `review`         | `must_not_mutate`      | `fresh`          | `review_report`        | `review-candidate`    |

Important semantics:

* unique `step_id` values are the workflow truth
* repeated `phase_kind` does not imply repeated step identity
* `prepare_convergence` is a system step, not a job
* every runtime step in v1 is closure-relevant; there are no report-only side paths

### Workflow graph

```text
author_initial
  -> validate_candidate_initial

validate_candidate_initial(clean)
  -> review_candidate_initial
validate_candidate_initial(findings)
  -> repair_candidate

review_candidate_initial(clean)
  -> prepare_convergence
review_candidate_initial(findings)
  -> repair_candidate

repair_candidate
  -> validate_candidate_repair

validate_candidate_repair(clean)
  -> review_candidate_repair
validate_candidate_repair(findings)
  -> repair_candidate   [consumes candidate rework budget]

review_candidate_repair(clean)
  -> prepare_convergence
review_candidate_repair(findings)
  -> repair_candidate   [consumes candidate rework budget]

prepare_convergence(prepared)
  -> validate_integrated
prepare_convergence(conflicted)
  -> escalated: convergence_conflict

validate_integrated(clean)
  -> approval_or_auto_finalize
validate_integrated(findings)
  -> repair_after_integration

repair_after_integration
  -> validate_after_integration_repair

validate_after_integration_repair(clean)
  -> review_after_integration_repair
validate_after_integration_repair(findings)
  -> repair_after_integration   [consumes integration rework budget]

review_after_integration_repair(clean)
  -> prepare_convergence
review_after_integration_repair(findings)
  -> repair_after_integration   [consumes integration rework budget]
```

### Default budgets

Defaults are resolved into the revision policy snapshot at revision creation.

* `candidate_rework_budget`: 2
* `integration_rework_budget`: 2
* `transport_retry_cap`: configurable per step class
* `approval_policy`: `required` by default
* `overflow_strategy`: `truncate` by default

### Workflow freezing

At item creation the daemon freezes:

* `workflow_version`

At revision creation the daemon freezes:

* approval policy
* candidate rework budget
* integration rework budget
* transport retry caps
* repo-context policies
* overflow strategies
* step→template mapping

Future config or template changes do not rewrite active revisions.

---

## State Evaluation Model

Ingot v1 has one authoritative evaluator. There is no secondary reducer allowed to disagree with workflow truth.

### Canonical current item state

The canonical current state is:

* `lifecycle_state`
* `parking_state`
* `approval_state`
* `escalation_state`
* `escalation_reason`
* `current_revision_id`
* current revision jobs
* current revision convergence attempts

### Derived projections

The evaluator computes but does not canonically store:

* `current_step_id`
* `current_phase_kind`
* `phase_status`
* `next_recommended_action`
* `allowed_actions`
* `board_status`
* `attention_state`
* `terminal_readiness`

A cache may exist, but it is not canonical.

### Normalized outcomes

Job progression keys off a small vocabulary:

* `clean`
* `findings`
* `transient_failure`
* `terminal_failure`
* `protocol_violation`
* `cancelled`

`conflicted` is a **convergence state**, not a job outcome.

Any step/outcome pairing that is not legal for the step contract is treated as `protocol_violation`.

### Outcome handling

* `clean` — follow the success edge
* `findings` — follow the findings edge only for validate/review steps
* `transient_failure` — redispatch the same step while transport retry budget remains; otherwise escalate with `step_failed`
* `terminal_failure` — escalate with `step_failed`
* `protocol_violation` — escalate with `protocol_violation`
* `cancelled` — remain on the same step with no automatic redispatch

There is no `report_only` in v1 runtime steps. There is no `no_repro` because there is no separate bug workflow.

### Evaluation algorithm

For one item:

1. If `lifecycle_state=done`, projection is terminal.
2. If `parking_state=deferred`, projection is parked and no auto-dispatch occurs.
3. If there is an active job or active convergence for the current revision, project the current step as running.
4. Otherwise inspect the latest non-superseded terminal job for the current step and apply the frozen transition rules from the current revision’s workflow and policy snapshot.
5. If candidate or integration rework budget is exhausted, set `escalation_state=operator_required` with the matching reason.
6. If the current convergence is `conflicted`, set `escalation_state=operator_required` with reason `convergence_conflict`.
7. If the workflow reaches the approval gate and the revision’s `approval_policy=required`, set `approval_state=pending`.
8. If the workflow reaches the approval gate and `approval_policy=not_required`, attempt finalization automatically.
9. If the target ref moved after a convergence was prepared but before finalization, fail that convergence, clear pending approval, and project `prepare_convergence` again.

### Terminal readiness

An item is terminally ready only when all of the following are true for the current revision:

* no active jobs
* no active convergence
* no escalation
* all required workflow steps completed successfully
* a prepared convergence exists and is still valid for the current target head
* integrated validation completed cleanly
* if approval is required, approval has been granted and finalization succeeded

For `approval_policy=not_required`, the daemon finalizes convergence and then closes the item with:

* `lifecycle_state=done`
* `done_reason=completed`
* `resolution_source=evaluator`
* `closed_at=txn_timestamp`

### Approval commands

`POST /items/:id/approval/approve` must:

1. verify `approval_state=pending`
2. verify there are no active jobs or active convergence operations
3. verify the prepared convergence still matches the current `target_ref` head
4. create a `GitOperation` for target-ref finalization
5. compare-and-swap `target_ref` from `input_target_commit_oid` to `prepared_commit_oid`
6. mark the convergence `finalized`
7. set `approval_state=approved`
8. set `lifecycle_state=done`
9. set `done_reason=completed`
10. set `resolution_source=approval_command`
11. set `closed_at`

`POST /items/:id/approval/reject` must:

1. verify `approval_state=pending`
2. verify there are no active jobs or active convergence operations
3. cancel the prepared convergence
4. create a new revision that supersedes the current one

   * same title/description/acceptance criteria/target ref by default
   * same approval policy by default
   * `seed_commit_oid` defaults to the prior revision’s authoring head
5. set `approval_state` for the new revision to `not_requested` or `not_required`
6. keep `lifecycle_state=open`
7. keep `parking_state=active`

Approval is not durable if finalization fails. If the target ref moved before approval finalization, the approval command fails safely, clears pending approval, and requires a new convergence.

### Legal combinations

The following are invalid and must be prevented:

* `lifecycle_state=done` with `parking_state=deferred`
* `approval_state=pending` with `parking_state=deferred`
* `approval_state=approved` while `lifecycle_state=open`
* `escalation_state=operator_required` while `lifecycle_state=done`
* `approval_state=pending` when no valid prepared convergence exists for the current revision
* `lifecycle_state=done` with active jobs or active convergence
* reopen of a `completed` item

A completed item is not reopened in v1. New work after completion is a new item.

---

## Workspace, Git, and Commit Truth

Git is a first-class subsystem. The daemon owns the truth.

### Workspace strategy

v1 supports one workspace strategy only:

* `worktree`

`clone` is deferred.

### Refs

Ingot distinguishes:

* **`target_ref`** — the local durable branch/ref the current revision will eventually finalize into
* **`workspace_ref`** — the daemon-owned scratch ref used inside authoring or integration workspaces

Agents may edit files. They may not create commits, rewrite refs, rebase, or move HEAD to unrelated refs. Any unexpected Git write is a protocol violation.

### Execution permission

Mutability is a **job property**, not a workspace property:

* `may_mutate`
* `must_not_mutate`

This solves the earlier contradiction. Non-mutating jobs are allowed in an authoring workspace, but they must leave the filesystem and Git state unchanged.

### Mutating job protocol

For a mutating job, the daemon must:

1. provision or reuse the authoring workspace for the current revision
2. verify the workspace starts at the expected `workspace_ref` and `input_head_commit_oid`
3. run the agent with explicit instructions not to commit or alter refs
4. on successful agent exit, verify that no unexpected commits or ref movements occurred
5. inspect the working tree
6. if no valid change set exists, fail the job as `terminal_failure`
7. create a `GitOperation` row for `create_job_commit`
8. stage changes and create **exactly one daemon-owned canonical commit**
9. attach operation/item/revision/job trailers to the commit
10. record that commit as `output_commit_oid`
11. advance the workspace head and workspace ref to that commit

Important semantics:

* agents never create canonical commits
* a successful mutating job always ends in one daemon-created commit
* an empty mutating result is not success
* unexpected Git writes by the agent are `protocol_violation`

### Non-mutating job protocol

For a non-mutating job, the daemon must:

1. provision the required workspace
2. record `input_head_commit_oid`
3. verify the workspace is clean before execution
4. run the job
5. verify the workspace is still clean after execution
6. if the workspace was dirtied, fail the job as `protocol_violation`
7. reset or abandon the workspace according to policy

### Review subjects

Review jobs must record both:

* `input_base_commit_oid`
* `input_head_commit_oid`

A review result must be attributable to a specific diff subject, not just a single head commit.

### Convergence lifecycle

Convergence is a two-stage operation.

#### 1. Prepare convergence

* create an integration workspace from the latest `target_ref` head
* record `input_target_commit_oid`
* apply the current authoring head using the fixed strategy `rebase_then_fast_forward`
* if conflicts occur:

  * mark convergence `conflicted`
  * retain the integration workspace for inspection
  * escalate the item
* if the integration is clean:

  * create a daemon-owned prepared commit in the integration workspace
  * journal it with a `GitOperation`
  * mark convergence `prepared`
  * do **not** move `target_ref`

#### 2. Validate and finalize

* run `validate_integrated` against the prepared integrated result
* if validation finds issues, return to the post-integration repair loop
* if approval is required, wait for explicit approval
* before finalization, verify `target_ref` is still at `input_target_commit_oid`
* if still valid, create a `GitOperation` for `finalize_target_ref` and move the ref
* if the target moved, fail the convergence, clear pending approval, and require a new prepare attempt

### Conflict handling

In-system manual conflict continuation is **out of scope** in v1.

When convergence becomes `conflicted`:

* the item escalates
* the integration workspace may be retained for inspection
* no agent jobs run against that retained conflict workspace
* the operator may resolve the issue **outside Ingot** and then create a new revision, optionally supplying an explicit `seed_commit_oid`

There is no “manual resolve in integration workspace then continue the same convergence” path in v1. That path breaks lineage unless the resolved result becomes a new revision seed, and that is exactly the complexity this v1 refuses to hide.

### Reset and cleanup

* authoring workspaces are retained through the active revision and cleaned up after revision supersession or item closure unless retained for debug
* review workspaces are removed after completion unless retained for debug
* integration workspaces are retained while convergence is `running`, `conflicted`, or `prepared`, then removed after finalization, failure, or explicit cleanup

### Git operation journal and crash recovery

Git and SQLite are not atomic together. The journal is what makes recovery honest.

Rule: **every daemon-owned Git side effect must have a `GitOperation` row written before the side effect happens.**

Recovery rules:

* if a planned commit operation exists and a commit with matching trailers is present, reconcile it and adopt the commit
* if a planned finalization operation exists and the target ref is already at the expected new OID, reconcile it and adopt the move
* if no evidence of the side effect exists, mark the operation failed
* uncertain process death must never be interpreted as successful completion

---

## Prompt Assembly and Context Budgets

Prompt assembly is deterministic and budgeted.

### Prompt assembly order

1. current revision contract

   * title
   * description
   * acceptance criteria
   * target ref
2. workflow step header from the built-in step contract
3. prompt template snapshot
4. current `RevisionContext` snapshot when `context_policy=resume_context`
5. repository context according to the revision’s frozen policy snapshot
6. convergence metadata when relevant
7. structured output instructions and schema hints

The fully assembled prompt is written to disk before execution.

### Budget rules

Budgets are resolved into the revision policy snapshot.

Each step has frozen values for:

* `max_prompt_tokens`
* `max_repo_context_tokens`
* `overflow_strategy`

Budget priority is:

1. revision contract
2. step header
3. template prompt
4. revision context
5. repository context

### Overflow strategies

v1 supports only deterministic strategies:

* `truncate`
* `manifest_only`
* `fail`

`summarize` is deferred. It is too easy to make “deterministic summary” a hand-wave.

### On-disk job artifacts

```text
~/.ingot/logs/<job_id>/
├── prompt.txt
├── stdout.log
├── stderr.log
└── result.json
```

`result_payload` in SQLite is canonical. `result.json` is a copied inspection artifact.

---

## Concurrency, Invariants, and Recovery

### Hard invariants

1. Every job belongs to exactly one item and exactly one item revision.
2. Every item has exactly one current revision.
3. At most one active job may exist per item revision.
4. At most one active convergence may exist per item revision.
5. `lifecycle_state=done` implies zero active jobs and zero active convergence.
6. `parking_state=deferred` implies the item is open, idle, and `approval_state!=pending`.
7. `approval_state=pending` implies the item is open, parking is active, there are no active jobs or active convergence operations, and a valid prepared convergence exists for the current revision.
8. `approval_state=approved` implies `lifecycle_state=done`, `done_reason=completed`, `resolution_source=approval_command`, and a finalized convergence exists for the current revision.
9. `escalation_state=operator_required` implies the item is open and escalation metadata is consistent.
10. Job side effects may be adopted only if `job.item_revision_id == item.current_revision_id` at the point of state application.
11. Successful mutating jobs require `workspace_id`, `input_head_commit_oid`, and `output_commit_oid`.
12. Every daemon-owned Git side effect requires a corresponding `GitOperation`.
13. Existing item semantics do not change when live config or templates change.
14. A completed item cannot be reopened.

### Database enforcement

Use row-local constraints where possible and transactional compare-and-swap where cross-row checks are required.

Database-level enforcement should cover at least:

* one active job per item revision (partial unique index)
* one active convergence per item revision (partial unique index)
* one current revision per item
* one authoring workspace per item revision
* item done-field coupling
* unique `revision_no` per item
* stable `step_id + semantic_attempt_no + retry_no` uniqueness per item revision
* same-project relationships across item/revision/job/workspace/convergence/GitOperation

Cross-row conditions such as “approval pending requires a valid prepared convergence for the current revision” are enforced transactionally.

### Idempotency and stale events

* commands accept idempotency keys
* redispatch of the same step creates a new job row and supersedes the prior one
* late callbacks no-op when item revision, job ID, or lease owner do not match
* human terminal decisions outrank late callbacks

### Leases and heartbeats

Each running job records:

* child PID when available
* lease owner/session ID
* `heartbeat_at`
* `lease_expires_at`

A sweeper expires jobs only after lease timeout or confirmed process death.

### Startup reconciliation

Startup reconciliation is conservative:

1. reconcile `GitOperation` rows in `planned` or `applied`
2. inspect jobs in `assigned` or `running`
3. if child PID is gone or lease clearly expired, mark the job `expired`
4. if process state is uncertain, mark the workspace `stale` and wait for the sweeper
5. inspect active convergences and integration workspaces
6. if an integration workspace contains unresolved conflicts, mark convergence `conflicted`
7. if a prepared commit exists and is journaled but not fully recorded, reconcile it
8. if finalization already happened, reconcile and mark convergence `finalized`
9. rerun state evaluation for affected items

The daemon never auto-marks an uncertain job `completed`.

---

## UI Model

### Primary views

* project dashboard
* item board
* item detail / revision / workspace view
* execution queue / jobs
* workspace management
* config

### Board columns

* `INBOX`
* `WORKING`
* `APPROVAL`
* `DONE`

Only items appear on the board.

### Attention badges

* `Escalated`
* `Deferred`

`Blocked` is not a canonical state in v1.

### Item card shows

* current revision title
* classification
* current step
* active job chip, if any
* approval badge when pending
* attention badge when escalated or deferred
* revision number
* priority

### Item detail must answer four questions fast

1. what is this item?
2. what happened already?
3. what is blocking closure?
4. what should happen next?

The detail pane includes:

* workflow version
* current revision contract
* projected current step
* recommended next action and legal alternatives
* revision history
* full job timeline
* latest revision context summary
* workspace panel showing target ref, workspace ref, base/head, and diff manifest
* convergence panel showing prepare/finalize state and target-head validity
* diagnostics explaining the current projection

---

## Item Update and Command Semantics

### Metadata patch

`PATCH /items/:id` may update fields that do **not** change execution meaning:

* `classification`
* `priority`
* `labels`
* `operator_notes`

These updates do not create a new revision.

### Revision-changing updates

`POST /items/:id/revise` is required for any change to:

* `title`
* `description`
* `acceptance_criteria`
* `target_ref`
* `approval_policy`
* `seed_commit_oid`

Strict v1 rule: **revision-changing commands require the item to be idle**.

The daemon does not silently cancel active work for meaning changes. The operator cancels first, then revises.

`revise`:

1. verifies the item is open and idle
2. creates a new immutable revision
3. freezes a new policy snapshot and template map snapshot
4. seeds the new revision from:

   * explicit `seed_commit_oid` if provided
   * otherwise the prior revision’s authoring head
5. clears escalation
6. resets approval state based on the new revision’s approval policy
7. leaves prior jobs/workspaces/convergences as historical lineage

### Defer and resume

`POST /items/:id/defer`:

* allowed only when the item is open, idle, and not pending approval
* sets `parking_state=deferred`

`POST /items/:id/resume`:

* sets `parking_state=active`

### Dismiss and invalidate

`POST /items/:id/dismiss` and `POST /items/:id/invalidate`:

* allowed only when the item is open and idle
* close the item with:

  * `lifecycle_state=done`
  * `done_reason=dismissed|invalidated`
  * `resolution_source=manual_command`
  * `closed_at`

### Reopen

`POST /items/:id/reopen`:

* allowed only for `dismissed` or `invalidated` items
* not allowed for `completed` items
* creates a new revision cloned from the last revision by default
* resets the item to:

  * `lifecycle_state=open`
  * `parking_state=active`
  * fresh approval state for the new revision
  * cleared escalation

A completed item is never reopened. That is not reopening. That is new work.

---

## API Surface

All project-scoped endpoints are prefixed with `/api/projects/:project_id/`.

### Projects

* `GET /api/projects`
* `POST /api/projects`
* `PUT /api/projects/:id`
* `DELETE /api/projects/:id`

### Agents

* `GET /api/agents`
* `POST /api/agents`
* `PUT /api/agents/:id`
* `DELETE /api/agents/:id`
* `POST /api/agents/:id/reprobe`

### Config and definitions

* `GET /api/config`
* `GET /api/projects/:project_id/config`
* `GET /api/phase-templates`
* `GET /api/projects/:project_id/phase-templates`
* `GET /api/workflows`
* `POST /api/reload`

There is no workflow CRUD in v1.

### Items

* `POST .../items` — create item and initial revision
* `GET .../items` — list items with derived board status, attention state, current step, and next recommended action
* `GET .../items/:item_id` — detail with current revision, revision history, jobs, workspaces, convergences, and diagnostics
* `GET .../items/:item_id/evaluation`
* `PATCH .../items/:item_id` — metadata-only update
* `POST .../items/:item_id/revise`
* `POST .../items/:item_id/defer`
* `POST .../items/:item_id/resume`
* `POST .../items/:item_id/dismiss`
* `POST .../items/:item_id/invalidate`
* `POST .../items/:item_id/reopen`
* `POST .../items/:item_id/approval/approve`
* `POST .../items/:item_id/approval/reject`

### Jobs

* `POST .../items/:item_id/jobs` — dispatch recommended next job step or explicit current legal job step
* `POST .../items/:item_id/jobs/:job_id/retry`
* `POST .../items/:item_id/jobs/:job_id/cancel`
* `GET .../jobs`
* `GET .../jobs/:job_id/logs`

Internal worker lifecycle endpoints may exist:

* `POST /api/jobs/:job_id/assign`
* `POST /api/jobs/:job_id/start`
* `POST /api/jobs/:job_id/heartbeat`
* `POST /api/jobs/:job_id/complete`
* `POST /api/jobs/:job_id/fail`
* `POST /api/jobs/:job_id/expire`

### Workspaces and convergence

* `GET .../workspaces`
* `GET .../workspaces/:workspace_id`
* `POST .../workspaces/:workspace_id/reset`
* `POST .../workspaces/:workspace_id/abandon`
* `POST .../workspaces/:workspace_id/remove`
* `POST .../items/:item_id/convergence/prepare`
* `GET .../convergences/:convergence_id`
* `POST .../convergences/:convergence_id/abort`

There is no `retry_convergence` command in v1. If convergence must be attempted again, the item must simply be back at the `prepare_convergence` step and the daemon creates a new convergence attempt.

### Activity and stats

* `GET .../activity`
* `GET /api/activity`
* `GET /api/stats`

---

## Runtime Architecture

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

---

## Rust Workspace and Crate Boundaries

The Rust codebase is a Cargo workspace. The daemon binary is wiring only.

### Recommended workspace layout

```text
ingot/
├── Cargo.toml
├── Cargo.lock
├── apps/
│   └── ingot-daemon/
├── crates/
│   ├── ingot-domain/
│   ├── ingot-workflow/
│   ├── ingot-application/
│   ├── ingot-config/
│   ├── ingot-store-sqlite/
│   ├── ingot-git/
│   ├── ingot-workspace/
│   ├── ingot-agent-protocol/
│   ├── ingot-agent-adapters/
│   ├── ingot-agent-runtime/
│   └── ingot-http-api/
├── ui/
├── ARCHITECTURE.md
└── README.md
```

### Crate responsibilities

| Crate                      | Responsibility                                                                                           | Must not depend on                               |
| -------------------------- | -------------------------------------------------------------------------------------------------------- | ------------------------------------------------ |
| `ingot-domain`             | Pure entities, enums, invariants, value objects, repository ports, event types                           | `sqlx`, `axum`, `tokio::process`                 |
| `ingot-workflow`           | Built-in workflow definitions, step contracts, evaluator, transition tables                              | `sqlx`, `axum`, adapter code                     |
| `ingot-application`        | Command handlers, transaction boundaries, use-case orchestration, port composition                       | `axum`, `sqlx` concrete types, CLI-specific code |
| `ingot-config`             | YAML loading, merge logic, config schema validation, template override loading                           | `axum`, `sqlx`                                   |
| `ingot-store-sqlite`       | sqlx models, migrations, repository implementations, transaction adapters                                | `axum`, adapter crates                           |
| `ingot-git`                | Safe Git wrappers, diff generation, ref validation, commit trailers, convergence helpers, target-ref CAS | `axum`, workflow logic                           |
| `ingot-workspace`          | Worktree provisioning/reset/reuse/cleanup using `ingot-git`                                              | `axum`, `sqlx`                                   |
| `ingot-agent-protocol`     | Adapter traits, request/response types, result schemas, progress events                                  | `sqlx`, `axum`                                   |
| `ingot-agent-adapters`     | Built-in Claude / Codex adapter implementations                                                          | `sqlx`, `axum`, workflow crates                  |
| `ingot-agent-runtime`      | Subprocess spawning, cancellation, heartbeats, log writing, adapter supervision                          | `axum`, workflow crates                          |
| `ingot-http-api`           | Axum routes, DTOs, auth middleware, WebSocket transport                                                  | `sqlx` direct queries, adapter code              |

### Dependency direction

```text
ingot-domain
    ↑
ingot-workflow
    ↑
ingot-application
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
* `ingot-application` depends on ports, not infrastructure implementations
* storage, workspace, git, and agent runtime are infrastructure
* the daemon app owns DI, config bootstrap, background task startup, and signal handling only

---

## Frontend and Transport Notes

* the frontend is a React SPA served by Vite in development and by the daemon in production
* initial state loads via REST
* live updates arrive over WebSocket
* WebSocket messages include monotonic sequence numbers so the frontend can detect gaps and resync
* API auth uses a bearer token generated by the daemon and stored locally with restrictive permissions

---

## File Layout

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

---

## Tech Stack

| Layer         | Technology         | Why                                          |
| ------------- | ------------------ | -------------------------------------------- |
| Runtime       | Tokio              | async process, job, and workspace management |
| HTTP/WS       | Axum               | local API and state push                     |
| Database      | SQLite via sqlx    | runtime state, migrations, checked queries   |
| Serialization | serde + serde_json | REST/WS payloads                             |
| Config        | serde_yml          | YAML settings and prompt templates           |
| Logging       | tracing            | structured logs and diagnostics              |
| Agents        | tokio::process     | spawn and supervise local CLI agents         |
| Git           | tokio::process     | worktree and ref operations                  |
| Frontend      | React + TypeScript | operator UI                                  |
| Bundler       | Vite               | local development                            |

---

## Deferred Features

The following are deliberately deferred and must not leak back into v1 through “temporary” hooks:

* multiple runtime workflows
* bug-specific reproduce/root-cause/regr-test runtime graph
* parent/child items and dependency edges
* clone workspaces
* Docker workspaces
* report-only workflow steps
* prompt templates that alter step semantics
* workflow authoring in the UI or API
* in-system manual conflict resolution continuation
* agent-driven conflict resolution
* MCP server exposure
* remote push / PR / CI integration

---
