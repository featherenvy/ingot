# Ingot Service Specification

Status: Draft v1 (language-agnostic)

Purpose: Define a local service that orchestrates human-supervised AI coding work against real local Git repositories, owns Git truth, and closes work only after integrated validation plus any required approval.

Companion document: [ARCHITECTURE.md](./ARCHITECTURE.md) is non-normative and describes one recommended implementation shape.

Normative language: The key words MUST, MUST NOT, SHOULD, SHOULD NOT, and MAY are to be interpreted as normative requirements.

## 1. Problem Statement

Ingot is a long-running local daemon that manages supervised AI coding work in isolated Git workspaces. It tracks durable work, provisions revision-scoped workspaces, dispatches bounded agent jobs, records structured results plus daemon-owned commits, prepares integration against a local target ref, validates the integrated result, and closes work only after policy-satisfied approval or explicit manual disposition.

Ingot is not a generic workflow platform in v1. It is a narrow execution control layer for one thing: single-item code delivery in a real local Git repository.

Architectural boundary:

* `Item` is the durable work object and the only board-rendered entity.
* `ItemRevision` is the meaning-bearing contract for one revision of that work.
* `Job` is a bounded execution attempt attached to exactly one item revision and one stable workflow step.
* `Workspace` is first-class execution reality; jobs only make sense relative to a concrete workspace and concrete Git inputs.
* `Convergence` is explicit and two-stage: prepare an integrated result, validate it there, then finalize the target ref only after all gates pass.
* Git truth belongs to the daemon. Agents do file-level work only.
* Human authority is explicit. Human commands outrank late or stale agent events.

v1 operates on local repositories and local refs only. Remote push, PR creation, hosted CI, MCP exposure, and arbitrary workflow authoring are out of scope.

## 2. Goals and Non-Goals

### 2.1 Goals

* Durable work tracking across retries, rework loops, defer/resume, approval rejection, revision changes, and manual terminal decisions.
* Frozen meaning so future config or prompt changes do not rewrite existing work.
* Bounded execution where every job has explicit inputs, outputs, and budgets.
* Git truthfulness so the daemon, not the agent, owns canonical commit creation and ref movement.
* Explicit convergence against the target line before closure.
* Low operator friction through derived next actions instead of manual state machine assembly.
* Strong auditability across revisions, jobs, convergences, Git side effects, and manual decisions.
* Conservative recovery that never invents success after uncertainty.

### 2.2 Non-Goals

* Multiple runtime workflows in v1.
* Parent/child items, dependency graphs, or planning boards.
* Clone or container-based workspaces.
* Prompt templates that alter workflow semantics.
* In-system manual conflict continuation.
* Agent-driven conflict resolution.
* Arbitrary user-authored report-only workflow graphs.
* Remote Git push, PR creation, or hosted CI orchestration.
* Filesystem hot-reload watchers for live config/template changes.

## 3. System Overview

### 3.1 Main Components

An implementation MUST provide these logical components:

* `Project Registry` for registered local Git repositories.
* `Config and Template Loader` for layered config plus project template overrides.
* `Workflow and State Evaluator` that projects canonical item state from durable records.
* `Dispatcher and Job Runner` that launches bounded agent jobs.
* `Workspace Manager` that provisions, resets, reuses, and cleans workspaces.
* `Git Manager` that owns scratch refs, canonical commits, convergence prepare, and target-ref finalization.
* `Convergence Manager` that prepares, validates, finalizes, and reconciles integration attempts.
* `Persistence Layer` backed by SQLite for durable runtime truth.
* `HTTP API` for commands and queries.
* `WebSocket Event Stream` with monotonic sequence numbers for live updates.

The operator UI is not required for conformance, but the built-in transport is.

### 3.2 Runtime Boundary

The service has two processes in the reference architecture:

* a daemon that owns orchestration, persistence, workspaces, Git, recovery, and agent execution
* a frontend that presents live state over HTTP and WebSocket

The frontend is replaceable. The daemon contract is normative.

### 3.3 Storage Model

Ingot has two storage layers with a hard boundary.

Filesystem:

* global defaults
* per-project config
* per-project prompt template overrides
* job logs and copied artifacts
* Git worktrees and scratch refs

SQLite:

* projects
* agents
* items
* item revisions
* revision contexts
* workspaces
* jobs
* findings
* convergences
* git operations
* activity

### 3.4 Configuration and Template Sources

Configuration precedence:

```text
~/.ingot/defaults.yml
  -> <repo>/.ingot/config.yml
    -> CLI flags
```

Prompt templates are built into the daemon and MAY be overridden per project at:

```text
<repo>/.ingot/templates/*.yml
```

There is no global on-disk template library in v1.

Template and config changes take effect only after explicit reload by daemon restart or `POST /api/reload`. Filesystem watch is not required in v1.

### 3.5 Scope Rules

* Agents are global across projects.
* Workflow definitions and step contracts are compiled into the daemon.
* Prompt templates control prompt text only. They MUST NOT control workspace choice, mutability, artifact kind, retry semantics, or transitions.
* Items do not read live workflow policy after creation.
* Revisions do not read live policy or config after creation.
* Jobs do not read live templates after dispatch.

This yields the required freeze points:

* Item freezes graph meaning.
* ItemRevision freezes work contract and execution policy.
* Job freezes exact execution input.

## 4. Core Domain Model

### 4.1 Project

Required fields:

* `id`
* `name`
* `path`
* `default_branch`
* `color`

All runtime entities are project-scoped.

### 4.2 Agent

Required fields:

* `id`
* `slug`
* `name`
* `adapter_kind` with v1 values `claude_code|codex`
* `provider`
* `model`
* `cli_path`
* `capabilities`
* `health_check`
* `status`

Typical capabilities include `read_only_jobs`, `mutating_jobs`, `structured_output`, and `streaming_progress`.

There is no generic shell adapter contract in v1. Only named adapters (`claude_code`, `codex`) are supported.

### 4.3 PromptTemplate

Required fields:

* `slug`
* `phase_kind` with values `author|validate|review|investigate`
* `prompt`
* `enabled`

Semantics:

* templates are reusable prompt bodies keyed by slug
* existing revisions keep a frozen `step_id -> template_slug` mapping
* existing jobs keep the full prompt snapshot plus template digest

### 4.4 WorkflowDefinition

v1 ships with exactly one runtime workflow:

* `workflow_version = delivery:v1`

A workflow definition specifies:

* unique stable `step_id` values
* step contracts
* allowed transitions, including auxiliary report-only dispatch rules when present
* semantic loop budgets
* default step-to-template mapping
* default repo-context policies
* default overflow strategies
* convergence requirement

Older workflow versions MUST remain available in code until no open item uses them.

### 4.5 Workspace

Required fields:

* `id`
* `project_id`
* `kind` with values `authoring|review|integration`
* `strategy` with v1 value `worktree`
* `path`
* `created_for_revision_id`
* `parent_workspace_id`
* `target_ref`
* `workspace_ref`
* `base_commit_oid`
* `head_commit_oid`
* `retention_policy` with values `ephemeral|retain_until_debug|persistent`
* `status` with values `provisioning|ready|busy|stale|retained_for_debug|abandoned|error|removing`
* `current_job_id`
* `created_at`
* `updated_at`

Semantics:

* `workspace_ref` is daemon-owned scratch state, never agent-owned
* authoring and integration workspaces are Git-tracked mutable contexts owned by the daemon
* review workspaces are fresh read-only worktrees over a specific review subject
* mutability is not a workspace property; it is a job execution property

Lifecycle rules:

* authoring workspace: one per revision, seeded from `seed_commit_oid`, reused within the revision
* review workspace: fresh per review or investigation job, ephemeral by default
* integration workspace: one per convergence attempt, provisioned from current `target_ref` head

Field nullability and conditional requirements:

* `created_for_revision_id` is required for authoring and integration workspaces and null for review workspaces.
* `parent_workspace_id` is nullable and used only when lineage matters.
* `target_ref` is required for integration workspaces, optional for authoring workspaces, and null for review workspaces.
* `workspace_ref` is required for authoring and integration workspaces and null for review workspaces.
* `base_commit_oid` is required for review workspaces once provisioned, required for integration workspaces once provisioned, and optional for authoring workspaces.
* `head_commit_oid` is required once the workspace becomes `ready`.
* `current_job_id` is null unless the workspace is actively attached to a running job.

### 4.6 Item

Required fields:

* `id`
* `project_id`
* `classification` with values `change|bug`
* `workflow_version`
* `lifecycle_state` with values `open|done`
* `parking_state` with values `active|deferred`
* `done_reason` with values `completed|dismissed|invalidated`
* `resolution_source` with values `system_command|approval_command|manual_command`
* `approval_state` with values `not_required|not_requested|pending|approved`
* `escalation_state` with values `none|operator_required`
* `escalation_reason` with values `candidate_rework_budget_exhausted|integration_rework_budget_exhausted|convergence_conflict|step_failed|protocol_violation|manual_decision_required|other`
* `current_revision_id`
* `origin_kind` with values `manual|promoted_finding`
* `origin_finding_id`
* `priority` with values `critical|major|minor`
* `labels`
* `operator_notes`
* `created_at`
* `updated_at`
* `closed_at`

Semantics:

* `current_step_id` is derived, not canonical
* `next_recommended_action` is derived, not canonical
* `classification` affects UI only, not workflow semantics
* `origin_kind` captures how the item was created and remains stable across later revisions
* the item survives retries, rework, approval rejection, revision changes, defer/resume, and manual terminal decisions

Field nullability and conditional requirements:

* `done_reason`, `resolution_source`, and `closed_at` are null while `lifecycle_state=open` and required when `lifecycle_state=done`.
* `escalation_reason` is null when `escalation_state=none` and required when `escalation_state=operator_required`.
* `origin_finding_id` is null when `origin_kind=manual` and required when `origin_kind=promoted_finding`.

### 4.7 ItemRevision

Required fields:

* `id`
* `item_id`
* `revision_no`
* `title`
* `description`
* `acceptance_criteria`
* `target_ref`
* `approval_policy` with values `required|not_required`
* `policy_snapshot`
* `template_map_snapshot`
* `seed_commit_oid`
* `seed_target_commit_oid`
* `supersedes_revision_id`
* `created_at`

Semantics:

* revisions are immutable once created
* any change to title, description, acceptance criteria, target ref, approval policy, `seed_commit_oid`, or `seed_target_commit_oid` creates a new revision
* old revisions remain for audit
* future jobs for a revision read only that revision's frozen snapshots
* `seed_commit_oid` is the source baseline from which authoring for the revision proceeds
* `seed_target_commit_oid` is the target baseline captured at revision creation for audit, target-baseline history across superseding revisions, and downstream promotion defaults
* `seed_target_commit_oid` does not by itself change the current candidate diff subject, which continues to derive from `seed_commit_oid` and the current authoring head
* `seed_commit_oid` and `seed_target_commit_oid` MUST remain reachable local commits for as long as the revision is current and may still dispatch jobs
* explicit or derived seed OIDs MUST be validated as reachable local commits in the project repository at revision-creation time
* when a default seed depends on the current `target_ref` head, the daemon MUST capture that head atomically with revision creation so later ref movement cannot rewrite the revision baseline
* `policy_snapshot` freezes execution policy, including the default repo-context policy and any step-specific repo-context overrides
* every repo-context policy object stored in `policy_snapshot` MUST conform to `repo_context_policy:v1`

Field nullability and conditional requirements:

* `supersedes_revision_id` is null for the initial revision and required for later revisions that replace a prior revision.

### 4.8 RevisionContext

Required fields:

* `item_revision_id`
* `schema_version`
* `payload`
* `updated_from_job_id`
* `updated_at`

`RevisionContext` is a deterministic structured projection, not a hidden model session. It is derived from:

* current authoring head commit
* changed path manifest
* latest validation result summary
* latest review findings summary
* execution-relevant operator notes
* prior accepted structured results

`schema_version` MUST be `revision_context:v1` in v1.

`payload` MUST use the canonical core schema plus optional `extensions` object. Required core fields are:

* `authoring_head_commit_oid`
* `changed_paths` as an ordered list of repo-relative paths
* `latest_validation` as either null or an object with `job_id`, `schema_version`, `outcome`, and `summary`
* `latest_review` as either null or an object with `job_id`, `schema_version`, `outcome`, and `summary`
* `accepted_result_refs` as an ordered list of objects with `job_id`, `step_id`, `schema_version`, `outcome`, and `summary`
* `operator_notes_excerpt`

Steps with `context_policy=resume_context` receive the current snapshot.

### 4.9 Job

Required fields:

* `id`
* `project_id`
* `item_id`
* `item_revision_id`
* `step_id`
* `semantic_attempt_no`
* `retry_no`
* `supersedes_job_id`
* `status` with values `queued|assigned|running|completed|failed|cancelled|expired|superseded`
* `outcome_class` with values `clean|findings|transient_failure|terminal_failure|protocol_violation|cancelled`
* `phase_kind` with values `author|validate|review|investigate`
* `workspace_id`
* `workspace_kind`
* `execution_permission` with values `may_mutate|must_not_mutate`
* `context_policy`
* `phase_template_slug`
* `phase_template_digest`
* `prompt_snapshot`
* `input_base_commit_oid`
* `input_head_commit_oid`
* `output_artifact_kind` with values `commit|review_report|validation_report|finding_report|none`
* `output_commit_oid`
* `result_schema_version`
* `result_payload`
* `agent_id`
* `process_pid`
* `lease_owner_id`
* `heartbeat_at`
* `lease_expires_at`
* `error_code`
* `error_message`
* `created_at`
* `started_at`
* `ended_at`

Semantics:

* every job is a new subprocess
* there is no provider-native hidden conversation reuse
* `validate` jobs perform objective verification over a concrete workspace subject and emit `validation_report`
* `review` jobs perform agent judgment over an explicit diff subject and emit `review_report`
* `semantic_attempt_no` increments only when the workflow semantically re-enters the same step
* redispatch of the same semantic attempt keeps `semantic_attempt_no` and increments `retry_no`
* successful mutating jobs always end in exactly one daemon-created canonical commit
* when a job produces a structured terminal result with `output_artifact_kind=validation_report`, `result_schema_version` MUST be `validation_report:v1`
* when a job produces a structured terminal result with `output_artifact_kind=review_report`, `result_schema_version` MUST be `review_report:v1`
* when a job produces a structured terminal result with `output_artifact_kind=finding_report`, `result_schema_version` MUST be `finding_report:v1`
* `result_payload` MUST conform to the canonical core schema named by `result_schema_version`; non-core provider, project, or adapter data MAY appear only under `extensions`
* workflow evaluation, prompt assembly, UI, and conformance tests MUST rely only on canonical core fields, never on `extensions`

Field nullability and conditional requirements:

* `supersedes_job_id` is null on first dispatch of a semantic attempt and required on retries or manual redispatch lineage.
* `outcome_class` is null until the job reaches a terminal state.
* `workspace_id` is null while queued and required once the job is assigned.
* `input_base_commit_oid` is required for review jobs, investigation jobs, `validate_integrated`, and other jobs that evaluate a diff subject; otherwise it may be null.
* `input_head_commit_oid` is required once execution begins.
* `output_commit_oid` is required for successful mutating jobs and null otherwise.
* `result_schema_version` and `result_payload` are required when the job produces a structured terminal result and null otherwise.
* `agent_id` is null until the job is assigned to a concrete agent runtime.
* `process_pid`, `lease_owner_id`, `heartbeat_at`, and `lease_expires_at` are null until the job is running.
* `error_code` and `error_message` are null unless the job terminates with failure, cancellation, expiry, or another operator-visible error condition.

### 4.10 Convergence

Required fields:

* `id`
* `project_id`
* `item_id`
* `item_revision_id`
* `source_workspace_id`
* `integration_workspace_id`
* `source_head_commit_oid`
* `target_ref`
* `strategy` with fixed v1 value `rebase_then_fast_forward`
* `status` with values `queued|running|conflicted|prepared|finalized|failed|cancelled`
* `input_target_commit_oid`
* `prepared_commit_oid`
* `final_target_commit_oid`
* `conflict_summary`
* `created_at`
* `completed_at`

Semantics:

* convergence is item-revision-scoped
* `strategy=rebase_then_fast_forward` means prepare replays the full current-revision authoring commit chain onto `input_target_commit_oid` while preserving commit boundaries
* `source_head_commit_oid` is the original authoring tip before replay
* `prepared_commit_oid` is the tip of the rewritten prepared chain, not a squash or synthetic merge commit
* `prepared` means the full authoring chain was replayed cleanly and target ref has not moved
* `finalized` means target ref was compare-and-swapped from `input_target_commit_oid` to the prepared commit
* if target ref moves after preparation but before finalization, convergence fails and approval is cleared

Field nullability and conditional requirements:

* `integration_workspace_id` is required once the attempt is provisioned.
* `prepared_commit_oid` is null until a clean prepare succeeds and required for `prepared` and `finalized` convergences.
* `final_target_commit_oid` is required iff `status=finalized`.
* `conflict_summary` is required iff `status=conflicted`.
* `completed_at` is null while the convergence is active and required for terminal states.

### 4.11 GitOperation

Required fields:

* `id`
* `project_id`
* `operation_kind` with values `create_job_commit|prepare_convergence_commit|finalize_target_ref|reset_workspace|remove_workspace_ref`
* `entity_type` with values `job|convergence|workspace|item_revision`
* `entity_id`
* `workspace_id`
* `ref_name`
* `expected_old_oid`
* `new_oid`
* `commit_oid`
* `status` with values `planned|applied|reconciled|failed`
* `metadata`
* `created_at`
* `completed_at`

Semantics:

* a `GitOperation` row MUST be written before a daemon-owned Git side effect
* `prepare_convergence_commit` is one logical replay operation and MAY create multiple daemon-owned commits
* daemon-created commits MUST include trailers:
  * `Ingot-Operation: <git_operation_id>`
  * `Ingot-Item: <item_id>`
  * `Ingot-Revision: <revision_no>`
  * `Ingot-Job: <job_id>` or `Ingot-Convergence: <convergence_id>`
* replayed prepared commits created by `prepare_convergence_commit` MUST additionally include:
  * `Ingot-Source-Commit: <source_commit_oid>`
* for `prepare_convergence_commit`, `metadata` MUST record `source_commit_oids` ordered oldest-first before replay begins; it SHOULD record any successfully replayed `prepared_commit_oids` prefix when known; after a clean prepare it MUST record `prepared_commit_oids` ordered oldest-first with the same cardinality and positional correspondence
* for `prepare_convergence_commit`, `commit_oid` and `new_oid` denote the last successfully replayed prepared commit when one exists; on a clean prepare this is the rewritten prepared tip
* startup reconciliation uses the journal plus actual Git state to adopt or fail incomplete work safely

Field nullability and conditional requirements:

* `workspace_id` is nullable and present only when the side effect is tied to a concrete workspace.
* `ref_name` is required when the operation targets a ref and null for purely commit-recording operations.
* `expected_old_oid` is required when compare-and-swap semantics apply.
* `new_oid` is required when a ref or workspace head is expected to move.
* `commit_oid` is required for commit-creating operations once the commit exists.
* `completed_at` is null while the operation is unresolved and required for terminal journal states.

### 4.12 Finding

Required fields:

* `id`
* `project_id`
* `source_item_id`
* `source_item_revision_id`
* `source_job_id`
* `source_step_id`
* `source_report_schema_version`
* `source_finding_key`
* `source_subject_kind` with values `candidate|integrated`
* `source_subject_base_commit_oid`
* `source_subject_head_commit_oid`
* `code`
* `severity` with values `low|medium|high|critical`
* `summary`
* `paths`
* `evidence`
* `triage_state` with values `untriaged|promoted|dismissed`
* `promoted_item_id`
* `dismissal_reason`
* `created_at`
* `triaged_at`

Semantics:

* a `Finding` is durable runtime state extracted from a canonical structured report
* findings never touch Git and are stored only in SQLite
* a finding captures the exact candidate or integrated subject that produced it
* promotion creates a new item in the same project and links it bidirectionally through `promoted_item_id` and the promoted item's `origin_finding_id`
* dismissal never deletes the finding; it only records triage state and reason
* the daemon MUST preserve reachability of `source_subject_head_commit_oid` until the finding is triaged
* when `source_subject_kind=integrated`, the daemon MUST also preserve reachability of `source_subject_base_commit_oid` until the finding is triaged

Field nullability and conditional requirements:

* `source_subject_head_commit_oid` is required for every finding.
* `source_subject_base_commit_oid` is required when `source_subject_kind=integrated` and optional otherwise.
* `promoted_item_id` is required iff `triage_state=promoted`.
* `dismissal_reason` is required iff `triage_state=dismissed`.
* `triaged_at` is null while `triage_state=untriaged` and required otherwise.

### 4.13 Activity

Activity is an append-only structured event log. Typical event types:

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
* `finding_recorded`
* `finding_promoted`
* `finding_dismissed`
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

## 5. Workflow Specification

### 5.1 Built-In Workflow

v1 ships with exactly one runtime workflow:

* `delivery:v1`

All items use this workflow. A `bug` item still executes through `delivery:v1`; classification affects operator framing, not runtime graph. If reproduction steps or regression expectations matter for a bug, they belong in the revision contract (description and acceptance criteria), not in a separate runtime construct.

`delivery:v1` also includes built-in auxiliary report-only steps. They are part of the built-in workflow surface, but they do not advance or rewind closure state.

### 5.2 Step Contracts

| `step_id`                           | `phase_kind`  | `workspace_kind` | `execution_permission` | `context_policy` | `output_artifact_kind` | `closure_relevance` | Default template      |
| ----------------------------------- | ------------- | ---------------- | ---------------------- | ---------------- | ---------------------- | ------------------- | --------------------- |
| `author_initial`                    | `author`      | `authoring`      | `may_mutate`           | `fresh`          | `commit`               | `closure_relevant`  | `author-initial`      |
| `review_incremental_initial`        | `review`      | `review`         | `must_not_mutate`      | `fresh`          | `review_report`        | `closure_relevant`  | `review-incremental`  |
| `review_candidate_initial`          | `review`      | `review`         | `must_not_mutate`      | `fresh`          | `review_report`        | `closure_relevant`  | `review-candidate`    |
| `validate_candidate_initial`        | `validate`    | `authoring`      | `must_not_mutate`      | `resume_context` | `validation_report`    | `closure_relevant`  | `validate-candidate`  |
| `repair_candidate`                  | `author`      | `authoring`      | `may_mutate`           | `resume_context` | `commit`               | `closure_relevant`  | `repair-candidate`    |
| `review_incremental_repair`         | `review`      | `review`         | `must_not_mutate`      | `fresh`          | `review_report`        | `closure_relevant`  | `review-incremental`  |
| `review_candidate_repair`           | `review`      | `review`         | `must_not_mutate`      | `fresh`          | `review_report`        | `closure_relevant`  | `review-candidate`    |
| `validate_candidate_repair`         | `validate`    | `authoring`      | `must_not_mutate`      | `resume_context` | `validation_report`    | `closure_relevant`  | `validate-candidate`  |
| `investigate_item`                  | `investigate` | `review`         | `must_not_mutate`      | `fresh`          | `finding_report`       | `report_only`       | `investigate-item`    |
| `prepare_convergence`               | `system`      | `integration`    | `daemon_only`          | `none`           | `none`                 | `closure_relevant`  | —                     |
| `validate_integrated`               | `validate`    | `integration`    | `must_not_mutate`      | `resume_context` | `validation_report`    | `closure_relevant`  | `validate-integrated` |
| `repair_after_integration`          | `author`      | `authoring`      | `may_mutate`           | `resume_context` | `commit`               | `closure_relevant`  | `repair-integrated`   |
| `review_incremental_after_integration_repair` | `review` | `review` | `must_not_mutate` | `fresh` | `review_report` | `closure_relevant` | `review-incremental` |
| `review_after_integration_repair`   | `review`      | `review`         | `must_not_mutate`      | `fresh`          | `review_report`        | `closure_relevant`  | `review-candidate`    |
| `validate_after_integration_repair` | `validate`    | `authoring`      | `must_not_mutate`      | `resume_context` | `validation_report`    | `closure_relevant`  | `validate-candidate`  |

Important semantics:

* `step_id` values are workflow truth
* repeated `phase_kind` does not imply repeated step identity
* `prepare_convergence` is a system step, not a job
* `closure_relevant` steps advance or rewind delivery closure state
* every successful mutating authoring step is followed by a mandatory incremental review of just the newly produced commit range
* whole-candidate review and final candidate validation are mandatory closure-relevant gates before convergence prepare
* `report_only` steps are auxiliary, never consume candidate or integration rework budget, and never change the closure graph position

### 5.3 Workflow Graph

```text
author_initial
  -> review_incremental_initial

review_incremental_initial(clean)
  -> review_candidate_initial
review_incremental_initial(findings)
  -> repair_candidate

review_candidate_initial(clean)
  -> validate_candidate_initial
review_candidate_initial(findings)
  -> repair_candidate

validate_candidate_initial(clean)
  -> prepare_convergence
validate_candidate_initial(findings)
  -> repair_candidate

repair_candidate
  -> review_incremental_repair

review_incremental_repair(clean)
  -> review_candidate_repair
review_incremental_repair(findings)
  -> repair_candidate

review_candidate_repair(clean)
  -> validate_candidate_repair
review_candidate_repair(findings)
  -> repair_candidate

validate_candidate_repair(clean)
  -> prepare_convergence
validate_candidate_repair(findings)
  -> repair_candidate

prepare_convergence(prepared)
  -> validate_integrated
prepare_convergence(conflicted)
  -> escalated: convergence_conflict

validate_integrated(clean, approval_policy=required)
  -> pending approval
validate_integrated(clean, approval_policy=not_required)
  -> daemon-only: finalize_prepared_convergence
validate_integrated(findings)
  -> repair_after_integration

repair_after_integration
  -> review_incremental_after_integration_repair

review_incremental_after_integration_repair(clean)
  -> review_after_integration_repair
review_incremental_after_integration_repair(findings)
  -> repair_after_integration

review_after_integration_repair(clean)
  -> validate_after_integration_repair
review_after_integration_repair(findings)
  -> repair_after_integration

validate_after_integration_repair(clean)
  -> prepare_convergence
validate_after_integration_repair(findings)
  -> repair_after_integration
```

Successful `validate_integrated` does not create another job step. It either enters the approval gate or projects the daemon-only `finalize_prepared_convergence` action according to the revision's `approval_policy`.

If a prepared convergence later becomes stale because `target_ref` moved, the daemon MUST execute the daemon-only `invalidate_prepared_convergence` action before approval or finalization can proceed.

`investigate_item` is an explicit auxiliary report-only step. It MAY be dispatched when an item is open, idle, not pending approval, and the evaluator does not currently project a daemon-only next action. Its completion records `Finding` rows but does not change `current_step_id`, `dispatchable_step_id`, approval state, or closure progress.

Auxiliary report-only transition rule:

* from any open, idle, non-pending item state that is not currently projected toward a daemon-only next action, `investigate_item` MAY be dispatched alongside the current closure position; after `clean`, `findings`, `transient_failure`, `terminal_failure`, `protocol_violation`, or `cancelled`, the item returns to the same closure position

### 5.4 Default Budgets

Defaults are resolved into the revision snapshot at revision creation:

* `candidate_rework_budget = 2`
* `integration_rework_budget = 2`
* `transport_retry_cap = configurable per step class`
* `approval_policy = required`
* `overflow_strategy = truncate`

### 5.5 Workflow Freezing

At item creation the daemon freezes:

* `workflow_version`

At revision creation the daemon freezes:

* approval policy
* candidate rework budget
* integration rework budget
* transport retry caps
* repo-context policies
* overflow strategies
* step-to-template mapping

Future config or template changes MUST NOT rewrite active revisions.

## 6. State Evaluation Model

### 6.1 Canonical Current Item State

Canonical item state consists of:

* `lifecycle_state`
* `parking_state`
* `approval_state`
* `escalation_state`
* `escalation_reason`
* `current_revision_id`
* current revision jobs
* current revision convergence attempts

### 6.2 Derived Projections

The evaluator computes but does not canonically store:

* `current_step_id`
* `current_phase_kind`
* `phase_status`
* `next_recommended_action`
* `dispatchable_step_id`
* `auxiliary_dispatchable_step_ids`
* `allowed_actions`
* `board_status`
* `attention_badges`
* `terminal_readiness`

The evaluator is pure read-side logic. It MUST NOT mutate durable state, create `GitOperation` rows, move refs, or update item, job, workspace, or convergence rows.

Operational terms used elsewhere:

* `idle item` means `lifecycle_state=open` and zero active jobs plus zero active convergence for the current revision. It does not by itself imply that approval is not pending; individual commands may impose that extra requirement.
* `next_recommended_action` may point to a job dispatch, a daemon-only operation, or a human command.
* daemon-only `next_recommended_action` values used in v1 are `prepare_convergence`, `finalize_prepared_convergence`, and `invalidate_prepared_convergence`.
* `dispatchable_step_id` is the legal job `step_id` to dispatch next, or null when the next recommended action is a human or daemon-only action such as approval or convergence prepare.
* `auxiliary_dispatchable_step_ids` is an ordered list of zero or more legal built-in report-only `step_id` values that MAY be dispatched without changing the current closure position
* report-only steps never change closure workflow position; while a report-only job is running, `current_step_id` continues to reflect the closure-relevant step
* `board_status` MUST be one of `INBOX|WORKING|APPROVAL|DONE`. `DONE` applies iff `lifecycle_state=done`. `APPROVAL` applies iff `lifecycle_state=open`, `approval_state=pending`, and `next_recommended_action!=invalidate_prepared_convergence`. `INBOX` applies to remaining open items only when there is no active job, no active convergence, and the current revision has no non-superseded terminal closure-relevant jobs yet. `WORKING` applies to all other remaining open items.
* while a report-only job is running, `current_phase_kind` and `phase_status` MUST reflect the active report-only job, even though `current_step_id` continues to reflect the closure-relevant step
* `attention_badges` MUST be an ordered list containing zero or more of `escalated` and `deferred`. Include `escalated` when `escalation_state=operator_required`. Include `deferred` when `parking_state=deferred`. If both apply, the recommended order is `["escalated", "deferred"]`. `Blocked` is not a canonical value in v1.

### 6.3 Normalized Outcomes

Job progression uses this vocabulary:

* `clean`
* `findings`
* `transient_failure`
* `terminal_failure`
* `protocol_violation`
* `cancelled`

`conflicted` is a convergence state, not a job outcome.

### 6.4 Outcome Handling

* `clean` follows the success edge for closure-relevant steps; for `report_only` steps it records the structured result and leaves closure state unchanged
* `findings` follows the findings edge only for closure-relevant validate and review steps; for `report_only` steps it records findings and leaves closure state unchanged
* `transient_failure` redispatches the same step while transport retry budget remains for closure-relevant steps; for `report_only` steps it MAY redispatch while retry budget remains and otherwise leaves closure state unchanged without escalation
* `terminal_failure` escalates with `step_failed` for closure-relevant steps; for `report_only` steps it leaves closure state unchanged and preserves explicit redispatch availability
* `protocol_violation` escalates with `protocol_violation` for closure-relevant steps; for `report_only` steps it leaves closure state unchanged and preserves explicit redispatch availability
* `cancelled` remains on the same step with no automatic redispatch

### 6.5 Evaluation Algorithm

For one item:

1. If `lifecycle_state=done`, the item is terminal.
2. If `parking_state=deferred`, no auto-dispatch occurs.
3. If there is an active closure-relevant job or active convergence for the current revision, project the current step as running. If the only active job is report-only, keep the closure-relevant step projection and mark the item as working.
4. Otherwise determine workflow position from canonical rows for the current revision using only closure-relevant terminal jobs, plus current convergence state and canonical approval state. Terminal report-only jobs never advance or rewind the closure graph.
5. If candidate or integration rework budget is exhausted, project operator-required attention and a human next action. The command or system action that exhausts the budget MUST materialize the corresponding escalation state canonically.
6. If the current convergence is `conflicted`, project operator-required attention and a human next action. The convergence handler MUST materialize `escalation_state=operator_required` with reason `convergence_conflict` canonically.
7. If the workflow is at the approval gate and `approval_policy=required`, project approval actions from canonical `approval_state`. Clean completion of `validate_integrated` MUST materialize `approval_state=pending` as part of job-completion handling.
8. If the workflow is at the approval gate and `approval_policy=not_required`, project `next_recommended_action=finalize_prepared_convergence` and `dispatchable_step_id=null`. The daemon MUST finalize through the daemon-only action, not inside the evaluator.
9. If a prepared convergence exists but the current `target_ref` head no longer matches `input_target_commit_oid`, project `next_recommended_action=invalidate_prepared_convergence`, remove approval and finalization commands from `allowed_actions`, and require the daemon-only invalidation action before projecting `prepare_convergence` again.

### 6.6 Terminal Readiness

An item is terminally ready only when all of the following are true for the current revision:

* no active jobs
* no active convergence
* no escalation
* all required workflow steps completed successfully
* a prepared convergence exists and is still valid for the current target head
* integrated validation completed cleanly
* if approval is required, approval has been granted and finalization succeeded

### 6.7 Daemon-Only System Actions

The following daemon-only actions have no public HTTP endpoint in v1:

* `finalize_prepared_convergence`
* `invalidate_prepared_convergence`

`finalize_prepared_convergence` MUST:

1. verify the current revision has `approval_policy=not_required`
2. verify there are no active jobs or active convergence operations
3. verify the prepared convergence still matches current `target_ref` head
4. create a `GitOperation` for target-ref finalization
5. compare-and-swap `target_ref` from `input_target_commit_oid` to `prepared_commit_oid`
6. mark the convergence `finalized`
7. set `lifecycle_state=done`
8. set `done_reason=completed`
9. set `resolution_source=system_command`
10. set `closed_at`

`invalidate_prepared_convergence` MUST:

1. verify a prepared convergence exists for the current revision
2. verify the current `target_ref` head no longer matches `input_target_commit_oid`
3. mark the convergence `failed` with error `target_ref_moved`
4. set `approval_state` to `not_requested` or `not_required` according to the revision's `approval_policy`
5. keep `lifecycle_state=open` and `parking_state=active`

HTTP queries and WebSocket delivery MUST NOT execute daemon-only system actions synchronously as a side effect of read evaluation.

### 6.8 Approval Commands

`POST /items/:id/approval/approve` MUST:

1. verify `approval_state=pending`
2. verify there are no active jobs or active convergence operations
3. verify the prepared convergence still matches current `target_ref` head
4. create a `GitOperation` for target-ref finalization
5. compare-and-swap `target_ref` from `input_target_commit_oid` to `prepared_commit_oid`
6. mark the convergence `finalized`
7. set `approval_state=approved`
8. set `lifecycle_state=done`
9. set `done_reason=completed`
10. set `resolution_source=approval_command`
11. set `closed_at`

`POST /items/:id/approval/reject` MUST:

Request body:

* MAY include the same optional revision-contract overrides and optional seed fields as `POST /items/:id/revise`

1. verify `approval_state=pending`
2. verify there are no active jobs or active convergence operations
3. cancel the prepared convergence
4. create a new revision that supersedes the current one with the same title, description, acceptance criteria, target ref, and approval policy by default
5. set the new revision's `seed_commit_oid` from explicit input when provided; otherwise derive it from the prior revision's current authoring head, or fall back to the prior revision's `seed_commit_oid` when no authoring workspace exists
6. set the new revision's `seed_target_commit_oid` from explicit input when provided; otherwise derive it from the current head of the new revision's `target_ref`
7. capture any default derived from `target_ref` atomically with revision creation
8. note that rebinding `seed_target_commit_oid` records a new target baseline but does not itself rebase carried-forward work
9. set `approval_state` for the new revision to `not_requested` or `not_required` per the revision's approval policy
10. keep `lifecycle_state=open` and `parking_state=active`

Approval is not durable if finalization fails. If target ref moved before approval finalization, the approval command MUST fail safely by applying the same state transition as `invalidate_prepared_convergence`, then require a new prepare attempt.

### 6.9 Illegal Combinations

The following combinations are invalid and MUST be prevented:

* `lifecycle_state=done` with `parking_state=deferred`
* `approval_state=pending` with `parking_state=deferred`
* `approval_state=approved` while `lifecycle_state=open`
* `escalation_state=operator_required` while `lifecycle_state=done`
* `approval_state=pending` when no prepared convergence exists for the current revision
* `lifecycle_state=done` with active jobs or active convergence
* reopen of a `completed` item

## 7. Workspace, Git, and Commit Truth

### 7.1 Workspace Strategy

v1 supports one workspace strategy only:

* `worktree`

### 7.2 Refs

Ingot distinguishes:

* `target_ref`: the local durable branch/ref the current revision will eventually finalize into
* `workspace_ref`: the daemon-owned scratch ref used inside authoring or integration workspaces

Agents MAY edit files but MUST NOT create commits, rewrite refs, rebase, or move HEAD to unrelated refs.

### 7.3 Execution Permission

Mutability is a job property, not a workspace property:

* `may_mutate`
* `must_not_mutate`

### 7.4 Mutating Job Protocol

For a mutating job the daemon MUST:

1. provision or reuse the authoring workspace for the current revision
2. verify the workspace starts at the expected `workspace_ref` and `input_head_commit_oid`
3. run the agent with explicit instructions not to commit or alter refs
4. on successful agent exit, verify no unexpected commits or ref movements occurred
5. inspect the working tree
6. fail the job as `terminal_failure` if no valid change set exists
7. create a `GitOperation` row for `create_job_commit`
8. stage changes and create exactly one daemon-owned canonical commit
9. attach required trailers
10. record that commit as `output_commit_oid`
11. advance workspace head and workspace ref to that commit

### 7.5 Non-Mutating Job Protocol

For a non-mutating job the daemon MUST:

1. provision the required workspace
2. record `input_head_commit_oid`
3. verify the workspace is clean before execution
4. run the job
5. verify the workspace is still clean after execution
6. fail the job as `protocol_violation` if the workspace was dirtied
7. reset or abandon the workspace according to policy

### 7.6 Review Subjects

Review and investigation jobs MUST record both:

* `input_base_commit_oid`
* `input_head_commit_oid`

A review or investigation result MUST be attributable to a specific diff subject.

Closure-relevant review steps in `delivery:v1` use these diff subjects:

* `review_incremental_initial` MUST review only the newly produced initial authoring commit range, with `input_base_commit_oid=seed_commit_oid` and `input_head_commit_oid` equal to the current authoring workspace head
* `review_incremental_repair` and `review_incremental_after_integration_repair` MUST review only the newly produced repair commit range, with `input_base_commit_oid` equal to the authoring workspace head before the repair job and `input_head_commit_oid` equal to the current authoring workspace head after the repair job
* `review_candidate_initial`, `review_candidate_repair`, and `review_after_integration_repair` MUST review the full current candidate subject for the revision, with `input_base_commit_oid=seed_commit_oid` and `input_head_commit_oid` equal to the current authoring workspace head, or `seed_commit_oid` when no authoring workspace exists yet

`investigate_item` uses a review workspace and MUST also record a specific diff subject:

* on the candidate side, `input_base_commit_oid` defaults to `seed_commit_oid` and `input_head_commit_oid` defaults to the current authoring workspace head, or `seed_commit_oid` when no authoring workspace exists yet
* when a valid prepared convergence is the current integrated subject, `input_base_commit_oid` MUST be that convergence's `input_target_commit_oid` and `input_head_commit_oid` MUST be its `prepared_commit_oid`

### 7.7 Convergence Lifecycle

Prepare:

1. create an integration workspace from the latest `target_ref` head
2. record `input_target_commit_oid`
3. compute the current revision source range as the commits in `seed_commit_oid..source_head_commit_oid`, ordered oldest-first
4. create a `GitOperation` for `prepare_convergence_commit` with the ordered `source_commit_oids` before replay begins
5. replay the source range onto `input_target_commit_oid` oldest-first, preserving commit boundaries and creating one daemon-owned prepared commit per source commit
6. if conflicts occur, mark the `GitOperation` failed, mark convergence `conflicted`, retain the integration workspace, and escalate the item
7. if replay is clean, persist the ordered `prepared_commit_oids` in the `GitOperation` metadata, set `prepared_commit_oid` to the rewritten tip, and mark convergence `prepared`

Validate and finalize:

1. run `validate_integrated` against the prepared result and record `input_base_commit_oid=input_target_commit_oid` plus `input_head_commit_oid=prepared_commit_oid`
2. if validation finds issues, return to the post-integration repair loop
3. if approval is required, the clean completion handler for `validate_integrated` MUST set `approval_state=pending` and wait for explicit approval
4. if approval is not required, the daemon MUST project and execute `finalize_prepared_convergence`
5. before any approval-command or daemon-only finalization, verify `target_ref` is still at `input_target_commit_oid`
6. if still valid, finalization MUST create a `GitOperation` for `finalize_target_ref` and move the ref
7. if target moved, the daemon MUST execute `invalidate_prepared_convergence`, which fails the prepared convergence, clears pending approval when present, and requires a new prepare attempt

### 7.8 Conflict Handling

In-system manual conflict continuation is out of scope in v1.

When convergence becomes `conflicted`:

* the item escalates
* the integration workspace MAY be retained for inspection
* no agent jobs run against that retained conflict workspace
* the operator MAY resolve the issue outside Ingot and create a new revision seeded from the resolved result

### 7.9 Reset and Cleanup

* authoring workspaces are retained through the active revision and cleaned up after revision supersession or item closure unless retained for debug
* review workspaces are removed after completion unless retained for debug
* integration workspaces are retained while convergence is `running`, `conflicted`, or `prepared`, then removed after finalization, failure, or explicit cleanup
* any authoring or integration workspace, scratch ref, or equivalent daemon-owned anchor that is the only remaining support for a current revision's `seed_commit_oid` or `seed_target_commit_oid` MUST be retained until that revision is superseded or the item is closed
* however, any authoring workspace or equivalent daemon-owned ref that is the only remaining anchor for an untriaged candidate finding subject MUST be retained until all such findings are triaged
* likewise, any integration workspace or equivalent daemon-owned ref that is the only remaining anchor for an untriaged integrated finding subject MUST be retained until all such findings are triaged

### 7.10 Journal and Crash Recovery

Git and SQLite are not atomic together. The journal makes recovery honest.

Recovery rules:

* if a planned commit operation exists and a commit with matching trailers is present, reconcile it and adopt the commit
* if a planned or applied `prepare_convergence_commit` exists and the full rewritten commit chain with matching `Ingot-Operation` and `Ingot-Source-Commit` trailers is present in the integration workspace, reconcile it and adopt the prepared state using the rewritten tip
* if only a prefix of a planned `prepare_convergence_commit` replay exists and the integration workspace is not conflicted, mark the operation failed, mark the workspace `stale`, and require a new prepare attempt
* if a planned finalization operation exists and the target ref is already at the expected new OID, reconcile it and adopt the move
* if a planned `reset_workspace` operation exists and the workspace ref and head already match the expected clean state, reconcile it and adopt the reset
* if a planned `remove_workspace_ref` operation exists and the scratch ref is already absent, reconcile it and adopt the removal
* if no evidence of the side effect exists, mark the operation failed
* if workspace cleanup happened only partially, mark the workspace `stale` or `error` and require explicit repair
* uncertain process death MUST NEVER be interpreted as success

## 8. Command and HTTP API Specification

### 8.1 General Rules

* Project-scoped endpoints are prefixed with `/api/projects/:project_id/`.
* Authentication uses a local bearer token generated by the daemon.
* Commands SHOULD accept an `Idempotency-Key` header.
* Errors SHOULD use a JSON envelope:

```json
{
  "error": {
    "code": "item_not_idle",
    "message": "Revision-changing commands require the item to be idle."
  }
}
```

* `400` is appropriate for malformed input.
* `404` is appropriate for missing entities.
* `409` is appropriate for command precondition failures.
* `422` is appropriate for structurally valid but semantically illegal state transitions.
* `500` is appropriate for unexpected daemon faults.

### 8.2 Project and Agent Registry Endpoints

* `GET /api/projects`
* `POST /api/projects`
* `PUT /api/projects/:id`
* `DELETE /api/projects/:id`
* `GET /api/agents`
* `POST /api/agents`
* `PUT /api/agents/:id`
* `DELETE /api/agents/:id`
* `POST /api/agents/:id/reprobe`

### 8.3 Config and Definition Endpoints

* `GET /api/config`
* `GET /api/projects/:project_id/config`
* `GET /api/phase-templates`
* `GET /api/projects/:project_id/phase-templates`
* `GET /api/workflows`
* `POST /api/reload`

There is no workflow CRUD in v1.

### 8.4 Item Endpoints

* `POST .../items`
* `GET .../items` with derived `board_status`, `attention_badges`, `current_step_id`, and `next_recommended_action`
* `GET .../items/:item_id` with current revision contract, revision history, jobs, workspaces, convergences, findings, revision-context summary, and diagnostics
* `GET .../items/:item_id/evaluation`
* `PATCH .../items/:item_id`
* `POST .../items/:item_id/revise`
* `POST .../items/:item_id/defer`
* `POST .../items/:item_id/resume`
* `POST .../items/:item_id/dismiss`
* `POST .../items/:item_id/invalidate`
* `POST .../items/:item_id/reopen`
* `POST .../items/:item_id/approval/approve`
* `POST .../items/:item_id/approval/reject`

Item command semantics:

* revision-creating commands accept JSON bodies containing the applicable revision contract fields. `seed_commit_oid` and `seed_target_commit_oid` are optional independent fields on `POST /items`, `POST /items/:id/revise`, `POST /items/:id/reopen`, and `POST /items/:id/approval/reject`.
* when provided explicitly, each seed field MUST be a reachable local commit in the project repository; otherwise the command MUST fail with `revision_seed_unreachable`
* when a seed field is omitted, that field MUST be derived independently by the command's default rules
* when command-specific defaults require resolving the current `target_ref` head and that ref does not resolve to a local commit in the project repository, the command MUST fail with `target_ref_unresolved`
* `POST /items` creates a manual item with `origin_kind=manual` and `origin_finding_id=null`. It MUST also create the initial revision. If `seed_commit_oid` is omitted, the daemon MUST resolve `target_ref`, read its current head, and use that head. If `seed_target_commit_oid` is omitted, the daemon MUST use that same resolved head.
* `PATCH /items/:id` MAY update only `classification`, `priority`, `labels`, and `operator_notes`
* `POST /items/:id/revise` is required for changes to title, description, acceptance criteria, target ref, approval policy, `seed_commit_oid`, or `seed_target_commit_oid`. The revise procedure MUST:
  1. verify the item is open and idle
  2. create a new immutable revision
  3. freeze a new policy snapshot and template map snapshot for the new revision
  4. set `seed_commit_oid` from explicit input when provided; otherwise derive it from the prior revision's current authoring head, or fall back to the prior revision's `seed_commit_oid` when no authoring workspace exists
  5. set `seed_target_commit_oid` from explicit input when provided; otherwise derive it from the current head of the new revision's `target_ref`
  6. capture any default derived from `target_ref` atomically with revision creation
  7. note that rebinding `seed_target_commit_oid` records a new target baseline but does not itself rebase carried-forward work
  8. clear escalation
  9. reset approval state based on the new revision's approval policy
  10. leave prior jobs, workspaces, and convergences as historical lineage
* `POST /items/:id/defer` requires the item to be open, idle, and not pending approval; sets `parking_state=deferred`
* `POST /items/:id/resume` requires `parking_state=deferred`; sets `parking_state=active`
* `POST /items/:id/dismiss` and `POST /items/:id/invalidate` require the item to be open and idle
* `POST /items/:id/reopen` is allowed only for dismissed or invalidated items, never completed items. Its request body MAY include the same optional revision-contract overrides and optional seed fields as `POST /items/:id/revise`. The reopen procedure MUST create a new revision cloned from the last revision by default, derive `seed_commit_oid` and `seed_target_commit_oid` using the same default rules as `POST /items/:id/revise`, set `lifecycle_state=open`, set `parking_state=active`, reset approval state for the new revision, and clear escalation

### 8.5 Job Endpoints

* `POST .../items/:item_id/jobs`
* `POST .../items/:item_id/jobs/:job_id/retry`
* `POST .../items/:item_id/jobs/:job_id/cancel`
* `GET .../jobs`
* `GET .../jobs/:job_id/logs`

Internal worker lifecycle endpoints MAY exist:

* `POST /api/jobs/:job_id/assign`
* `POST /api/jobs/:job_id/start`
* `POST /api/jobs/:job_id/heartbeat`
* `POST /api/jobs/:job_id/complete`
* `POST /api/jobs/:job_id/fail`
* `POST /api/jobs/:job_id/expire`

Daemon-only system actions such as `finalize_prepared_convergence` and `invalidate_prepared_convergence` are internal runtime behavior, not public HTTP endpoints in v1.

Job command semantics:

* `POST .../items/:item_id/jobs` dispatches either the current `dispatchable_step_id`, one of the current `auxiliary_dispatchable_step_ids`, or an explicit equivalent legal current job step. If none is available and no explicit legal current job step is provided, the command MUST fail without mutating item state.
* workflow-projected mandatory stages such as incremental review, whole-candidate review, and final candidate validation are automatic only in the sense that the evaluator projects them as the sole closure-relevant `dispatchable_step_id`; the daemon is not required to launch them without a dispatch command
* explicit legal current job steps MAY include built-in report-only steps such as `investigate_item`. Dispatching a report-only step requires the item to be open, idle, not pending approval, and not currently projected toward a daemon-only next action, and MUST be reflected in `auxiliary_dispatchable_step_ids`; it MUST NOT change closure position, approval state, or rework budgets.
* `POST .../items/:item_id/jobs/:job_id/retry` is allowed only when the referenced job is terminal and non-success, the item is open and idle, the job belongs to the current revision, and either the same `step_id` is still currently dispatchable or it remains a legal explicit report-only step for the current item state. It creates a new job row, preserves `semantic_attempt_no`, increments `retry_no`, sets `supersedes_job_id`, and leaves the prior job as historical lineage.
* `POST .../items/:item_id/jobs/:job_id/cancel` is allowed only when the referenced job is `queued`, `assigned`, or `running`. It terminates any subprocess when present, marks the job `cancelled`, clears active workspace attachment, and leaves the item on the same step with no automatic redispatch.

### 8.6 Workspace and Convergence Endpoints

* `GET .../workspaces`
* `GET .../workspaces/:workspace_id`
* `POST .../workspaces/:workspace_id/reset`
* `POST .../workspaces/:workspace_id/abandon`
* `POST .../workspaces/:workspace_id/remove`
* `POST .../items/:item_id/convergence/prepare`
* `GET .../convergences/:convergence_id`
* `POST .../convergences/:convergence_id/abort`

There is no `retry_convergence` command in v1. A new prepare attempt creates a new convergence record.

Workspace and convergence command semantics:

* `POST .../workspaces/:workspace_id/reset` is allowed only when the workspace is not busy. For authoring and integration workspaces it returns the worktree to the recorded daemon-owned clean state at `head_commit_oid` and `workspace_ref`. For review workspaces it recreates the clean review subject. Any daemon-owned ref movement or hard reset MUST be journaled with the appropriate `GitOperation`.
* `POST .../workspaces/:workspace_id/abandon` is allowed only when the workspace is not busy. It marks the workspace `abandoned`, detaches it from future scheduling, and retains on-disk contents for debugging until explicit removal or cleanup.
* `POST .../workspaces/:workspace_id/remove` is allowed only when the workspace is not busy. It removes the on-disk worktree, removes any daemon-owned scratch ref associated with that workspace, journals any daemon-owned Git side effects, marks the workspace `removing` during cleanup, and leaves the row `abandoned` after successful removal with historical metadata intact even if the filesystem path no longer exists.
* `POST .../items/:item_id/convergence/prepare` is allowed only when the evaluator projects `next_recommended_action=prepare_convergence` and there is no active convergence for the current revision.
* `POST .../convergences/:convergence_id/abort` is allowed only when the convergence is `queued`, `running`, or `prepared` and not finalized. It cancels active convergence work, clears pending approval if this convergence was the prepared current convergence, marks the convergence `cancelled`, and then removes or retains the integration workspace according to retention policy.

### 8.7 Finding Endpoints

* `GET .../items/:item_id/findings`
* `GET .../findings/:finding_id`
* `POST .../findings/:finding_id/promote`
* `POST .../findings/:finding_id/dismiss`

Finding command semantics:

* when a job completes successfully with `validation_report:v1`, `review_report:v1`, or `finding_report:v1`, the daemon MUST extract each canonical `finding:v1` object into a durable `Finding` row keyed by `source_job_id + source_finding_key`
* finding extraction MUST determine `source_subject_kind` canonically: `validate_integrated` findings are always `integrated`; review or investigation findings are `integrated` iff their `input_base_commit_oid` and `input_head_commit_oid` match the prepared or finalized integrated subject for the same revision; all other findings are `candidate`
* finding extraction MUST persist `source_subject_head_commit_oid=input_head_commit_oid`; it MUST also persist `source_subject_base_commit_oid=input_base_commit_oid` whenever present
* `POST .../findings/:finding_id/promote` is allowed only when `triage_state=untriaged`. It MUST verify that `source_subject_head_commit_oid` remains reachable and, when `source_subject_kind=integrated`, that `source_subject_base_commit_oid` remains reachable; otherwise it MUST fail with `finding_subject_unreachable`. It creates a new item in the same project with `origin_kind=promoted_finding` and `origin_finding_id=<finding_id>`, defaults `classification=bug`, defaults title and description from the finding summary and evidence, defaults `acceptance_criteria` to resolving the promoted finding and validating that it no longer reproduces, defaults `target_ref` and `approval_policy` from the source item revision unless overridden, defaults `seed_commit_oid` to `source_subject_head_commit_oid` for both candidate and integrated findings, defaults `seed_target_commit_oid` to `source_subject_base_commit_oid` when `source_subject_kind=integrated`, and otherwise defaults `seed_target_commit_oid` to the source item revision's `seed_target_commit_oid`. It records the new item's initial revision and sets `triage_state=promoted` with `promoted_item_id`
* `POST .../findings/:finding_id/dismiss` is allowed only when `triage_state=untriaged`. It records `triage_state=dismissed`, persists `dismissal_reason`, and keeps the finding attached to the source job and item for audit

### 8.8 Activity and Stats Endpoints

* `GET .../activity`
* `GET /api/activity`
* `GET /api/stats`

### 8.9 Example Payloads

Create item request:

```json
{
  "classification": "change",
  "priority": "major",
  "title": "Fix race in revision evaluator",
  "description": "The evaluator can project approval pending with a stale prepared convergence.",
  "acceptance_criteria": "Approval pending must require a valid prepared convergence for the current revision.",
  "target_ref": "refs/heads/main",
  "approval_policy": "required"
}
```

Approval success response:

```json
{
  "item_id": "itm_123",
  "lifecycle_state": "done",
  "done_reason": "completed",
  "resolution_source": "approval_command",
  "approval_state": "approved",
  "convergence": {
    "id": "conv_456",
    "status": "finalized"
  }
}
```

## 9. Prompt Assembly and Budgets

### 9.1 Prompt Assembly Order

The fully assembled prompt MUST be deterministic and ordered as:

1. current revision contract
2. workflow step header from built-in step contract
3. prompt template snapshot
4. current `RevisionContext` when `context_policy=resume_context`
5. repository context according to the revision's frozen policy snapshot
6. convergence metadata when relevant
7. structured output instructions and schema hints

For validate, review, and report-only investigation steps, structured output instructions and schema hints MUST target the canonical core schema for that step and MUST instruct adapters to place any non-core data under `extensions`.

The fully assembled prompt MUST be written to disk before execution.

### 9.2 Budget Rules

Each step has frozen values for:

* `max_prompt_tokens`
* `max_repo_context_tokens`
* `overflow_strategy`

Budget priority order:

1. revision contract
2. step header
3. template prompt
4. revision context
5. repository context

### 9.3 Overflow Strategies

v1 supports:

* `truncate`
* `manifest_only`
* `fail`

`summarize` is deferred.

### 9.4 On-Disk Job Artifacts

```text
~/.ingot/logs/<job_id>/
├── prompt.txt
├── stdout.log
├── stderr.log
└── result.json
```

`result_payload` in SQLite is canonical. `result.json` is a copied inspection artifact.

### 9.5 Canonical Structured Contracts

General rules:

* v1 defines canonical core schemas for `finding:v1`, `validation_report:v1`, `review_report:v1`, `finding_report:v1`, `revision_context:v1`, and `repo_context_policy:v1`
* each structured object consists of required core fields plus an optional `extensions` object
* producers MUST populate all required core fields and MUST place non-core data only under `extensions`
* consumers MUST ignore unknown `extensions` keys
* evaluator logic, prompt assembly, UI projections, and conformance tests MUST rely only on core fields

#### 9.5.1 `finding:v1`

Required core fields:

* `finding_key`
* `code`
* `severity` with values `low|medium|high|critical`
* `summary`
* `paths`
* `evidence` as an ordered list of strings

Semantics:

* `finding_key` MUST be stable within the source report and unique within `source_job_id`
* `paths` entries MUST be repo-relative paths

#### 9.5.2 `validation_report:v1`

Required core fields:

* `outcome` with values `clean|findings`
* `summary`
* `checks` as an ordered list of objects with `name`, `status` (`pass|fail|skip`), and `summary`
* `findings` as an ordered list of `finding:v1` objects

Semantics:

* validation reports represent objective checks over the job's current workspace subject
* `outcome=clean` requires `findings=[]`
* `outcome=findings` requires at least one failed check or one finding

#### 9.5.3 `review_report:v1`

Required core fields:

* `outcome` with values `clean|findings`
* `summary`
* `review_subject` as an object with `base_commit_oid` and `head_commit_oid`
* `overall_risk` with values `low|medium|high`
* `findings` as an ordered list of `finding:v1` objects

Semantics:

* `review_subject.base_commit_oid` and `review_subject.head_commit_oid` MUST match the job's `input_base_commit_oid` and `input_head_commit_oid`
* `outcome=clean` requires `findings=[]`

#### 9.5.4 `finding_report:v1`

Required core fields:

* `outcome` with values `clean|findings`
* `summary`
* `findings` as an ordered list of `finding:v1` objects

Semantics:

* `outcome=clean` requires `findings=[]`
* `outcome=findings` requires at least one finding

#### 9.5.5 `revision_context:v1`

Required core fields:

* `authoring_head_commit_oid`
* `changed_paths` as an ordered list of repo-relative paths
* `latest_validation` as either null or an object with `job_id`, `schema_version`, `outcome`, and `summary`
* `latest_review` as either null or an object with `job_id`, `schema_version`, `outcome`, and `summary`
* `accepted_result_refs` as an ordered list of objects with `job_id`, `step_id`, `schema_version`, `outcome`, and `summary`
* `operator_notes_excerpt`

Semantics:

* `latest_validation` and `latest_review` summaries MUST be derived from the canonical core fields of the latest non-superseded structured results for the current revision
* `accepted_result_refs` MAY reference only jobs from the current revision

#### 9.5.6 `repo_context_policy:v1`

Required core fields:

* `profile` with values `none|manifest_only|changed_files|changed_snippets`
* `max_repo_context_tokens`
* `max_files`
* `max_snippet_lines_per_file`
* `include_diff_hunks`
* `include_symbol_summaries`

Semantics:

* `policy_snapshot` MUST store one default `repo_context_policy:v1` object and MAY store step-specific overrides keyed by `step_id`
* `profile=none` yields no repository context
* `profile=manifest_only` yields changed path manifest only
* `profile=changed_files` yields changed path manifest plus selected full contents of changed files subject to caps
* `profile=changed_snippets` yields changed path manifest plus selected snippets or diff hunks from changed files subject to caps
* `include_diff_hunks` and `include_symbol_summaries` further constrain what is included; they MUST NOT expand selection beyond the chosen `profile`

## 10. Failure Model and Error Taxonomy

### 10.1 Error Classes

Configuration and template errors:

* `project_not_registered`
* `config_invalid`
* `template_override_invalid`
* `workflow_version_unknown`

Command precondition errors:

* `item_not_open`
* `item_not_idle`
* `approval_not_pending`
* `illegal_step_dispatch`
* `active_job_exists`
* `active_convergence_exists`
* `finding_not_untriaged`
* `finding_subject_unreachable`
* `revision_seed_unreachable`
* `target_ref_unresolved`
* `completed_item_cannot_reopen`
* `prepared_convergence_missing`

Workspace and Git errors:

* `workspace_provision_failed`
* `workspace_ref_mismatch`
* `workspace_dirty`
* `unexpected_git_write`
* `empty_mutating_result`
* `git_operation_failed`

Execution errors:

* `agent_launch_failed`
* `transport_timeout`
* `heartbeat_expired`
* `terminal_agent_failure`
* `protocol_violation`

Convergence and approval errors:

* `convergence_conflict`
* `target_ref_moved`
* `prepared_convergence_stale`
* `finalization_cas_failed`

Recovery errors:

* `journal_inconsistent`
* `recovery_ambiguous`

### 10.2 Handling Rules

* `transient_failure` MAY redispatch while the step's transport retry budget remains for closure-relevant steps. For report-only steps it MAY redispatch while retry budget remains and otherwise leaves closure state unchanged without escalation.
* `terminal_failure` escalates the item with reason `step_failed` for closure-relevant steps. For report-only steps it leaves closure state unchanged and MAY be retried explicitly.
* `protocol_violation` escalates immediately and MUST NOT be silently retried for closure-relevant steps. For report-only steps it leaves closure state unchanged and MAY be retried explicitly.
* `convergence_conflict` retains the integration workspace if configured and escalates the item.
* `target_ref_moved` MUST be applied through `invalidate_prepared_convergence`, which fails the prepared convergence, clears pending approval when present, and requires a new prepare attempt.
* command precondition failures MUST NOT partially mutate state.
* recovery ambiguity MUST leave the system in a safe, non-completed state.

## 11. Concurrency, Invariants, and Recovery

### 11.1 Hard Invariants

1. Every job belongs to exactly one item and exactly one item revision.
2. Every item has exactly one current revision.
3. At most one active job may exist per item revision.
4. At most one active convergence may exist per item revision.
5. `lifecycle_state=done` implies zero active jobs and zero active convergence.
6. `parking_state=deferred` implies the item is open, idle, and `approval_state!=pending`.
7. `approval_state=pending` implies the item is open, there are no active jobs or active convergence operations, and a prepared convergence exists for the current revision. Approval commands are legal only while that prepared convergence still matches the current `target_ref` head.
8. `approval_state=approved` implies `lifecycle_state=done`, `done_reason=completed`, `resolution_source=approval_command`, and a finalized convergence exists for the current revision.
9. `escalation_state=operator_required` implies the item is open and escalation metadata is consistent.
10. Job side effects may be adopted only if `job.item_revision_id == item.current_revision_id` at state-application time.
11. Successful mutating jobs require `workspace_id`, `input_head_commit_oid`, and `output_commit_oid`.
12. Every daemon-owned Git side effect requires a corresponding `GitOperation`.
13. Existing item semantics do not change when live config or templates change.
14. A completed item cannot be reopened.
15. `item.origin_kind=promoted_finding` implies `item.origin_finding_id` is present and the referenced finding has `triage_state=promoted` with `promoted_item_id=item.id`.
16. `finding.triage_state=promoted` implies `finding.promoted_item_id` is present and the referenced item has `origin_kind=promoted_finding` with `origin_finding_id=finding.id`.
17. Every finding belongs to exactly one project, one source item, one source item revision, and one source job, and those source relationships must be mutually consistent.

### 11.2 Database Enforcement

An implementation SHOULD enforce at least:

* one active job per item revision via partial unique index
* one active convergence per item revision via partial unique index
* one current revision per item
* one authoring workspace per revision
* item done-field coupling
* unique `revision_no` per item
* stable `step_id + semantic_attempt_no + retry_no` uniqueness per item revision
* same-project relationships across item, revision, job, workspace, convergence, GitOperation, and Finding
* unique `source_job_id + source_finding_key` per finding
* at most one promoted item per finding
* backlink consistency between `finding.promoted_item_id` and `item.origin_finding_id`

Cross-row conditions such as approval pending requiring a prepared convergence for the current revision, approval or finalization requiring that convergence to still match the current `target_ref` head, finding source relationships remaining mutually consistent, and bidirectional finding-promotion links remaining consistent, MUST be enforced transactionally.

### 11.3 Idempotency and Stale Events

* commands SHOULD accept idempotency keys
* redispatch of the same step creates a new job row and supersedes the prior one
* late callbacks MUST no-op when item revision, job ID, or lease owner do not match
* human terminal decisions outrank late callbacks

### 11.4 Leases and Heartbeats

Each running job records:

* child PID when available
* lease owner or session ID
* heartbeat timestamp
* lease expiration time

An `uncertain job` is a reconciliation condition, not a stored `Job.status`. It means the daemon cannot prove whether in-flight work completed successfully. Expired leases MUST classify the job as uncertain and transition it to `expired` or another explicit non-success terminal state, never `completed`.

### 11.5 Startup Reconciliation

At startup the daemon MUST:

1. reconcile `GitOperation` rows in `planned` or `applied`
2. inspect active jobs for stale leases or dead subprocesses
3. if an active job's process or filesystem state is uncertain, mark the associated workspace `stale` and exclude it from scheduling until explicit `reset`, `abandon`, `remove`, or equivalent cleanup action
4. fail or expire uncertain jobs conservatively
5. inspect active convergences and integration workspaces
6. if an integration workspace contains unresolved conflicts, mark convergence `conflicted`
7. if a full prepared replay chain exists and the journaled side effect is present, reconcile it and adopt the prepared state using the rewritten tip
8. if only a replay prefix exists without unresolved conflicts, mark the prepare operation failed and the integration workspace `stale`
9. inspect untriaged findings and verify that each candidate subject head remains reachable from a retained daemon-owned authoring anchor or other durable local commit reference, and that each integrated subject remains reachable either from a finalized durable ref or from a retained daemon-owned integration anchor; if not, emit operator-visible diagnostics and require repair before promotion
10. if finalization already happened, reconcile and mark convergence `finalized`
11. rebuild derived projections from canonical rows

## 12. Reference Algorithms

### 12.1 Startup and Reload

```text
start_daemon():
  load_global_defaults()
  load_registered_projects()
  load_built_in_workflows()
  for each registered project:
    load_project_config(project)
    resolve_effective_project_config(project)
    load_project_template_overrides(project)
    validate_project_config(project)
  reconcile_startup_state()
  rebuild_projections()
  begin_http_and_ws_services()
```

```text
reload():
  reread_defaults()
  for each registered project:
    reread_project_config(project)
    resolve_effective_project_config(project)
    reread_project_template_overrides(project)
    validate_project_reload(project)
  apply_only_to_future_revisions_and_jobs()
```

### 12.2 Dispatch Recommended Step

```text
dispatch_item(item_id):
  item = load_item_with_current_revision(item_id)
  assert item is open and not deferred
  assert no active job and no active convergence
  evaluation = evaluate(item)
  step = evaluation.dispatchable_step_id
  assert step is not null
  assert step is a legal job step
  create_job_for_step(step)
  assign_workspace()
  launch_agent_subprocess()
```

### 12.3 Complete Mutating Job

```text
complete_mutating_job(job_id, result):
  job = load_running_job(job_id)
  verify workspace_ref and head match expected inputs
  verify agent did not create commits or move refs
  if working_tree_has_no_valid_changes:
    fail job as terminal_failure
    return
  write GitOperation(planned=create_job_commit)
  create single canonical commit with required trailers
  update workspace head and workspace_ref
  persist result_payload and output_commit_oid
  complete job with outcome clean or findings
  rebuild revision context
```

### 12.4 Prepare Convergence

```text
prepare_convergence(item_id):
  item = load_item_with_current_revision(item_id)
  assert evaluate(item).next_recommended_action == prepare_convergence
  create convergence row
  provision integration workspace from target_ref head
  source_commits = list_commits(item.current_revision.seed_commit_oid..source_head, oldest_first)
  write GitOperation(
    planned=prepare_convergence_commit,
    metadata={source_commit_oids=source_commits}
  )
  replay_commits_oldest_first(
    source_commits,
    onto=target_ref_head,
    preserve_commit_boundaries=true,
    extra_trailer=Ingot-Source-Commit
  )
  if conflicts:
    mark prepare GitOperation failed
    mark convergence conflicted
    escalate item
    retain integration workspace
    return
  persist prepared_commit_oids mapping and rewritten tip
  mark convergence prepared
```

### 12.5 Finalize Prepared Convergence

```text
finalize_prepared_convergence(item_id):
  item = load_item_with_current_revision(item_id)
  assert item.current_revision.approval_policy == not_required
  assert evaluate(item).next_recommended_action == finalize_prepared_convergence
  conv = load_prepared_convergence(item.current_revision_id)
  assert target_ref_head == conv.input_target_commit_oid
  write GitOperation(planned=finalize_target_ref)
  compare_and_swap(target_ref, old=conv.input_target_commit_oid, new=conv.prepared_commit_oid)
  mark convergence finalized
  close item as completed with resolution_source=system_command
```

### 12.6 Invalidate Prepared Convergence

```text
invalidate_prepared_convergence(item_id):
  item = load_item_with_current_revision(item_id)
  conv = load_prepared_convergence(item.current_revision_id)
  assert target_ref_head != conv.input_target_commit_oid
  mark convergence failed with error target_ref_moved
  reset approval_state according to current revision approval_policy
  leave item open and eligible to project prepare_convergence again
```

### 12.7 Approve Pending Item

```text
approve(item_id):
  item = load_item_with_current_revision(item_id)
  assert approval_state == pending
  conv = load_valid_prepared_convergence(item.current_revision_id)
  assert target_ref_head == conv.input_target_commit_oid
  write GitOperation(planned=finalize_target_ref)
  compare_and_swap(target_ref, old=conv.input_target_commit_oid, new=conv.prepared_commit_oid)
  mark convergence finalized
  mark item approval_state approved
  close item as completed
```

### 12.8 Startup Reconcile Git Operations

```text
reconcile_startup_state():
  for each planned_or_applied GitOperation:
    inspect Git and filesystem state appropriate to operation_kind
    if operation_kind == prepare_convergence_commit and full replay chain matching metadata is present:
      mark operation reconciled
      adopt prepared state using rewritten tip
    else if operation_kind == prepare_convergence_commit and only a replay prefix is present:
      mark operation failed
      mark integration workspace stale unless unresolved conflicts already prove convergence conflicted
    else if side effect definitely happened:
      mark operation reconciled
      adopt resulting state
    else if side effect definitely did not happen:
      mark operation failed
    else:
      leave associated entity non-terminal and require operator-safe recovery
```

## 13. Observability and Transport

### 13.1 Minimum Observability

An implementation MUST provide:

* structured activity history
* per-job prompt, stdout, stderr, and copied result artifacts
* query endpoints for item detail, jobs, workspaces, convergences, and activity
* live WebSocket events with monotonic sequence numbers

### 13.2 WebSocket Event Envelope

Recommended event shape:

```json
{
  "seq": 1842,
  "event": "item.updated",
  "project_id": "prj_1",
  "entity_type": "item",
  "entity_id": "itm_123",
  "payload": {
    "lifecycle_state": "open",
    "approval_state": "pending",
    "current_step_id": "validate_integrated"
  }
}
```

If a client detects a gap in `seq`, it SHOULD resync by refetching state over HTTP.

### 13.3 Item Detail Response

Recommended `GET .../items/:item_id` shape:

```json
{
  "item": {
    "id": "itm_123",
    "classification": "change",
    "workflow_version": "delivery:v1",
    "lifecycle_state": "open",
    "parking_state": "active",
    "approval_state": "pending",
    "current_revision_id": "rev_7",
    "origin_kind": "manual",
    "origin_finding_id": null
  },
  "current_revision": {
    "id": "rev_7",
    "revision_no": 7,
    "title": "Fix race in revision evaluator",
    "description": "The evaluator can project approval pending with a stale prepared convergence.",
    "acceptance_criteria": "Approval pending must require a valid prepared convergence for the current revision.",
    "target_ref": "refs/heads/main",
    "approval_policy": "required"
  },
  "evaluation": {
    "board_status": "APPROVAL",
    "attention_badges": [],
    "current_step_id": "validate_integrated",
    "next_recommended_action": "approval_approve",
    "dispatchable_step_id": null,
    "auxiliary_dispatchable_step_ids": [],
    "allowed_actions": ["approval_approve", "approval_reject"]
  },
  "revision_context_summary": {
    "updated_at": "2026-03-11T11:22:33Z",
    "changed_paths": ["src/evaluator.rs", "tests/evaluator.rs"],
    "validation_summary": "Integrated validation passed",
    "review_summary": "No outstanding review findings"
  },
  "revision_history": [],
  "jobs": [],
  "findings": [],
  "workspaces": [
    {
      "id": "wrk_1",
      "kind": "authoring",
      "target_ref": "refs/heads/main",
      "workspace_ref": "refs/ingot/workspaces/wrk_1",
      "base_commit_oid": "abc123",
      "head_commit_oid": "def456",
      "diff_manifest": ["src/evaluator.rs", "tests/evaluator.rs"]
    }
  ],
  "convergences": [
    {
      "id": "conv_456",
      "status": "prepared",
      "input_target_commit_oid": "fedcba",
      "prepared_commit_oid": "def456",
      "target_head_valid": true
    }
  ],
  "diagnostics": []
}
```

## 14. Conformance Matrix

### 14.1 Config, Templates, and Freezing

* config precedence resolves global defaults, project config, then CLI flags
* template overrides apply per project only
* `POST /api/reload` re-reads config and templates
* reload affects future revisions and jobs only
* workflow version is frozen at item creation
* policy snapshot and template map snapshot are frozen at revision creation
* every revision stores both `seed_commit_oid` and `seed_target_commit_oid` deterministically
* default seed values derived from `target_ref` are captured atomically with revision creation
* `seed_target_commit_oid` records target-baseline history and promotion defaults without changing the current candidate diff subject
* frozen repo-context policy objects conform to `repo_context_policy:v1`
* prompt snapshot and template digest are frozen at job dispatch

### 14.2 State Evaluation

* evaluator derives the current step from canonical rows only and never mutates durable state or Git
* deferred items do not auto-dispatch
* automatic candidate-stage review and validation gates are expressed by projecting exactly one closure-relevant `dispatchable_step_id`, not by implicit daemon execution
* exhausted rework budgets escalate correctly
* pending approval requires a prepared convergence, and approval actions require that it is still valid for the current target head
* target-ref drift projects daemon-only invalidation and requires new prepare
* `RevisionContext.payload` conforms to `revision_context:v1`
* completed items cannot reopen

### 14.3 Workspace and Job Protocols

* one authoring workspace exists per revision
* review workspaces are fresh per review or report-only investigation job
* integration workspaces are one per convergence attempt
* mutating jobs produce exactly one daemon-owned commit
* mutating jobs fail if the result is empty
* non-mutating jobs fail if the workspace becomes dirty
* unexpected agent Git writes are treated as protocol violations
* every successful authoring commit is followed by incremental review of the new commit range before whole-candidate review
* every clean whole-candidate review is followed by final candidate validation before convergence prepare becomes legal
* validation, review, and report-only investigation jobs normalize structured results into canonical core schemas with optional `extensions`
* canonical report findings are extracted into durable `Finding` rows keyed by source job and finding key
* investigation jobs record an explicit diff subject via `input_base_commit_oid` and `input_head_commit_oid`
* report-only steps do not advance or rewind closure state

### 14.4 Convergence and Approval

* prepare creates a convergence record and integration workspace
* conflicts mark convergence `conflicted` and escalate the item
* clean prepare replays the full current-revision authoring chain without squashing, records the source-to-prepared chain mapping, and sets `prepared_commit_oid` to the rewritten tip without moving `target_ref`
* integrated validation gates finalization
* `approval_policy=not_required` finalizes through daemon-only `finalize_prepared_convergence`
* approval approve compare-and-swaps the target ref
* stale prepared convergence is cleared through daemon-only `invalidate_prepared_convergence`
* approval reject supersedes the revision and resets approval state

### 14.5 Recovery and Journaling

* every daemon-owned Git side effect has a corresponding `GitOperation`
* `prepare_convergence_commit` journals the ordered source and prepared commit chains for replayed prepare work
* startup reconciliation adopts already-applied side effects when evidence is clear
* startup reconciliation verifies reachability of untriaged candidate and integrated finding subjects
* partial prepare replay never counts as implicit success
* uncertain process death never becomes implicit success
* stale callbacks no-op when revision, job, or lease owner no longer match

### 14.6 API and Transport

* command precondition failures do not partially mutate state
* revision-creating commands expose optional independent seed fields and either validate explicit reachable seed OIDs or derive omitted fields by the canonical default rules
* commands that must derive a seed from `target_ref` fail with `target_ref_unresolved` when that ref does not resolve locally
* item list includes derived board status, attention badges, current step, and recommended action
* item detail includes current revision contract, revision history, jobs, workspaces, convergences, findings, revision-context summary, and diagnostics
* finding endpoints support listing, promotion, and dismissal without touching Git
* promoted items carry a canonical backlink through `origin_kind=promoted_finding` and `origin_finding_id`
* WebSocket events carry monotonic sequence numbers
* clients can recover from sequence gaps via HTTP resync

## 15. Implementation Checklist

Required for conformance:

* SQLite-backed canonical runtime state
* built-in `delivery:v1` workflow
* immutable revision snapshots
* canonical core schemas for `finding:v1`, `validation_report:v1`, `review_report:v1`, `finding_report:v1`, `revision_context:v1`, and `repo_context_policy:v1`
* daemon-owned canonical commits and ref movement
* explicit convergence prepare and finalize lifecycle
* built-in report-only workflow steps and durable finding triage
* prepare preserves the full current-revision authoring chain during convergence replay
* daemon-only finalization and prepared-convergence invalidation actions
* approval gate with approve and reject commands
* Git operation journal with startup reconciliation
* deterministic prompt assembly and on-disk prompt snapshots
* structured activity history plus per-job logs
* HTTP API and WebSocket stream

Recommended but non-required:

* dedicated frontend
* per-project template editing UX
* richer diagnostics and projection explanations
* debug retention controls for workspaces

## 16. Deferred Features

The following are intentionally deferred and MUST NOT leak into v1 through temporary hooks:

* multiple runtime workflows
* bug-specific reproduce/root-cause/regression-test graph
* parent/child items and dependency edges
* clone workspaces
* Docker workspaces
* arbitrary user-authored report-only workflow graphs
* prompt templates that alter step semantics
* workflow authoring in the UI or API
* in-system manual conflict resolution continuation
* agent-driven conflict resolution
* MCP server exposure
* remote push, PR, or CI integration
