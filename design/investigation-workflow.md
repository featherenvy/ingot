# Design: Investigation Workflow

> Extension to ingot supporting standalone analysis jobs that produce
> structured findings, each promotable to an individual delivery item.

## Problem

Today every Finding is scoped to an existing Item's revision. The
`InvestigateItem` step is auxiliary — it runs alongside the delivery
workflow but never drives it. There is no entry point for a user who
wants to say:

> "Investigate each crate and find helper duplication, then suggest
> refactoring."

and get back a set of individually-addressable work items.

## Design Goals

1. **Standalone investigation entry point** — submit an analysis prompt
   against a project without first creating a delivery item.
2. **Structured promotable findings** — findings carry enough detail
   (title, description, acceptance criteria) to become delivery items
   without human re-authoring.
3. **Batch promotion** — triage and promote findings in bulk, not
   one-by-one.
4. **Board visibility** — investigation items appear on the board with
   their own lifecycle, distinct from delivery items.
5. **Minimal disruption** — delivery:v1 workflow, evaluator, and
   existing finding/triage system remain unchanged.

## Domain Model Extensions

### New Classification Variant

```rust
pub enum Classification {
    Change,
    Bug,
    Investigation,  // NEW
}
```

Investigation items are tracked on the board alongside Change/Bug items
but follow a different workflow.

### New Workflow Version

```rust
pub enum WorkflowVersion {
    DeliveryV1,
    InvestigationV1,  // NEW
}
```

The workflow version is frozen on the ItemRevision at creation time, as
today. Investigation items always use `InvestigationV1`.

### Extended Finding Schema

Existing finding fields remain unchanged. New optional fields enable
direct promotion without human re-authoring:

```rust
pub struct Finding {
    // --- existing fields (unchanged) ---
    pub id: FindingId,
    pub source_item_id: ItemId,
    pub source_item_revision_id: ItemRevisionId,
    pub source_job_id: JobId,
    pub source_step_id: StepId,
    pub source_report_schema_version: String,
    pub source_finding_key: String,
    pub code: String,
    pub severity: FindingSeverity,
    pub summary: String,
    pub paths: Vec<String>,
    pub evidence: serde_json::Value,
    pub triage: FindingTriage,

    // --- new optional promotion fields ---
    pub promotion_title: Option<String>,
    pub promotion_description: Option<String>,
    pub promotion_acceptance_criteria: Option<String>,
    pub suggested_classification: Option<Classification>,
    pub estimated_scope: Option<EstimatedScope>,
    pub group_key: Option<String>,
}
```

```rust
pub enum EstimatedScope {
    Small,   // single function/file
    Medium,  // single crate/module
    Large,   // cross-crate
}
```

The promotion fields are populated by investigation reports and ignored
by existing review/validation reports. `group_key` allows the agent to
indicate that several findings should be addressed together (optional
grouping hint for the operator during triage).

**Backward compatibility**: All new fields are `Option`. Existing
`finding_report:v1` payloads without these fields deserialize cleanly
with `None` values. No migration of existing findings is needed.

### New Report Contract: `investigation_report:v1`

Extends `finding_report:v1` with promotion-ready finding structure:

```json
{
  "outcome": "clean" | "findings",
  "summary": "string — high-level analysis narrative",
  "scope": {
    "description": "string — what was investigated",
    "paths_examined": ["string"],
    "methodology": "string — how the agent structured the analysis"
  },
  "findings": [
    {
      "finding_key": "string — unique within report",
      "code": "string — machine-readable category",
      "severity": "low" | "medium" | "high" | "critical",
      "summary": "string — one-line finding summary",
      "paths": ["string — affected file paths"],
      "evidence": ["string — supporting evidence"],

      "promotion": {
        "title": "string — proposed item title",
        "description": "string — proposed item description",
        "acceptance_criteria": "string — proposed acceptance criteria",
        "classification": "change" | "bug",
        "estimated_scope": "small" | "medium" | "large"
      },

      "group_key": "string | null — optional grouping hint"
    }
  ],
  "extensions": null
}
```

The `promotion` object is **required** in `investigation_report:v1`
(unlike `finding_report:v1` where it would be absent). This makes the
contract explicit: investigation agents must produce promotable findings.

The `scope` block captures methodology for auditability — the operator
can see what the agent actually examined.

### New Step IDs

```rust
pub enum StepId {
    // --- existing delivery:v1 steps (unchanged) ---
    AuthorInitial,
    ReviewIncrementalInitial,
    // ... etc ...

    // --- investigation:v1 steps ---
    InvestigateProject,     // primary investigation
    ReinvestigateProject,   // re-run after triage feedback
}
```

## Investigation Workflow: `investigation:v1`

### Step Graph

```
                    ┌──────────────────┐
                    │ InvestigateProject│
                    │                  │
                    │ phase: Investigate│
                    │ workspace: Review │
                    │ output: Finding   │
                    │   Report         │
                    └────────┬─────────┘
                             │
                    ┌────────▼─────────┐
                    │  Triage Findings  │  (operator-driven, not a step)
                    │                  │
                    │  promote / dismiss│
                    │  / defer / group  │
                    └────────┬─────────┘
                             │
              ┌──────────────┼──────────────┐
              │              │              │
         findings       all triaged    re-investigate
         promoted       (no fix_now)    requested
              │              │              │
              ▼              ▼              ▼
         ┌────────┐    ┌─────────┐   ┌──────────────────┐
         │  DONE  │    │  DONE   │   │ReinvestigateProject│
         │(spawned│    │(clean)  │   │                    │
         │ items) │    │         │   │(loops back to      │
         └────────┘    └─────────┘   │ triage)            │
                                     └────────────────────┘
```

### Step Contracts

```rust
// Primary investigation
StepContract {
    step_id: StepId::InvestigateProject,
    phase_kind: PhaseKind::Investigate,
    workspace_kind: WorkspaceKind::Review,
    execution_permission: ExecutionPermission::MustNotMutate,
    context_policy: ContextPolicy::Fresh,
    output_artifact_kind: OutputArtifactKind::FindingReport,
    closure_relevance: ClosureRelevance::ClosureRelevant,
    default_template_slug: Some("investigate-project"),
}

// Re-investigation (after triage feedback)
StepContract {
    step_id: StepId::ReinvestigateProject,
    phase_kind: PhaseKind::Investigate,
    workspace_kind: WorkspaceKind::Review,
    execution_permission: ExecutionPermission::MustNotMutate,
    context_policy: ContextPolicy::Fresh,
    output_artifact_kind: OutputArtifactKind::FindingReport,
    closure_relevance: ClosureRelevance::ClosureRelevant,
    default_template_slug: Some("reinvestigate-project"),
}
```

Key differences from delivery:v1 `InvestigateItem`:
- `ClosureRelevance::ClosureRelevant` (drives lifecycle, not auxiliary)
- Distinct step IDs in the investigation graph (not shared with delivery)
- Different default template slugs (investigation-specific prompts)

### Evaluator Behavior

The evaluator dispatches based on `WorkflowVersion`:

```rust
match revision.workflow_version {
    WorkflowVersion::DeliveryV1 => evaluate_delivery_v1(ctx),
    WorkflowVersion::InvestigationV1 => evaluate_investigation_v1(ctx),
}
```

Investigation evaluator logic:

| State | board_status | phase_status | next_action |
|-------|-------------|-------------|-------------|
| No jobs yet | INBOX | New | Dispatch InvestigateProject |
| Investigation running | WORKING | Running | Wait |
| Investigation complete (findings) | WORKING | Triaging | Triage findings |
| Investigation complete (clean) | DONE | Done | Close item |
| All findings triaged (none fix_now) | DONE | Done | Close item |
| Reinvestigation requested | WORKING | Idle | Dispatch ReinvestigateProject |
| Findings promoted | DONE | Done | Close item (spawned items track) |

**No convergence, no approval gate, no authoring steps.**
Investigation items never produce commits and never touch target_ref.

### Job Input for Investigation Steps

Investigation jobs receive the full repository at a pinned commit, not a
diff range:

```rust
// New JobInput variant
pub enum JobInput {
    None,
    AuthoringHead { head_commit_oid: CommitOid },
    CandidateSubject { base_commit_oid: CommitOid, head_commit_oid: CommitOid },
    IntegratedSubject { base_commit_oid: CommitOid, head_commit_oid: CommitOid },
    ProjectSnapshot { head_commit_oid: CommitOid },  // NEW
}
```

`ProjectSnapshot` tells the runtime to check out the repository at a
specific commit and give the agent read-only access to the entire tree.
Unlike `CandidateSubject` (which implies a base..head range),
`ProjectSnapshot` implies whole-tree analysis.

The workspace is provisioned at `target_ref` HEAD at dispatch time.

## Agent Protocol

### Investigation Prompt Assembly

Investigation prompts differ from delivery prompts:

1. **No item revision context** — no "here is the change you need to
   make" framing.
2. **Project-scoped** — "you are investigating project X".
3. **Structured output contract** — agent must return
   `investigation_report:v1` with promotable findings.
4. **Methodology guidance** — template instructs agent to describe its
   investigation approach in the `scope` block.

Template structure (`investigate-project`):

```
You are investigating a codebase to find issues and suggest improvements.

## Project
- Repository: {project.repo_path}
- Branch: {revision.target_ref}
- Commit: {job_input.head_commit_oid}

## Investigation Brief
{revision.description}

## Output Contract
Return a JSON object matching `investigation_report:v1`.

Each finding MUST include a `promotion` block with:
- title: Concise item title (under 80 chars)
- description: Full description of the issue and suggested fix
- acceptance_criteria: Measurable criteria for resolution
- classification: "change" (refactoring) or "bug" (defect)
- estimated_scope: "small", "medium", or "large"

Use `group_key` to indicate findings that should be addressed together.

## Methodology
Describe your investigation approach in the `scope` block. List which
paths you examined and what strategy you used.
```

The `reinvestigate-project` template additionally includes:
- Previous investigation findings (with triage decisions)
- Operator feedback on what to investigate further
- Instruction to avoid re-reporting dismissed findings

### Output Schema

Passed to agent via `AgentRequest.output_schema`:

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "required": ["outcome", "summary", "scope", "findings"],
  "properties": {
    "outcome": { "enum": ["clean", "findings"] },
    "summary": { "type": "string", "minLength": 1 },
    "scope": {
      "type": "object",
      "required": ["description", "paths_examined", "methodology"],
      "properties": {
        "description": { "type": "string" },
        "paths_examined": { "type": "array", "items": { "type": "string" } },
        "methodology": { "type": "string" }
      }
    },
    "findings": {
      "type": "array",
      "items": {
        "type": "object",
        "required": ["finding_key", "code", "severity", "summary",
                     "paths", "evidence", "promotion"],
        "properties": {
          "finding_key": { "type": "string" },
          "code": { "type": "string" },
          "severity": { "enum": ["low", "medium", "high", "critical"] },
          "summary": { "type": "string" },
          "paths": { "type": "array", "items": { "type": "string" } },
          "evidence": { "type": "array", "items": { "type": "string" } },
          "promotion": {
            "type": "object",
            "required": ["title", "description", "acceptance_criteria",
                        "classification", "estimated_scope"],
            "properties": {
              "title": { "type": "string", "maxLength": 120 },
              "description": { "type": "string" },
              "acceptance_criteria": { "type": "string" },
              "classification": { "enum": ["change", "bug"] },
              "estimated_scope": { "enum": ["small", "medium", "large"] }
            }
          },
          "group_key": { "type": ["string", "null"] }
        }
      }
    },
    "extensions": {}
  }
}
```

## Usecase Extensions

### Create Investigation Item

New command handler alongside `create_manual_item()`:

```rust
pub struct CreateInvestigationInput {
    pub title: String,              // investigation title
    pub description: String,        // investigation prompt / brief
    pub target_ref: GitRef,         // branch to investigate
    pub priority: Priority,
    pub labels: Vec<String>,
    pub operator_notes: Option<String>,
}
```

Creates:
- `Item` with `classification=Investigation`,
  `workflow_version=InvestigationV1`
- `ItemRevision` with:
  - `acceptance_criteria` = "Produce structured findings for triage"
  - `approval_policy` = `NotRequired` (investigations don't need
    approval)
  - `candidate_rework_budget` = 0 (no authoring)
  - `integration_rework_budget` = 0 (no convergence)
  - `seed` = `Implicit` (investigate current branch HEAD)
  - `template_map_snapshot` maps investigation step IDs to templates

### Investigation Job Dispatch

Dispatch logic recognizes `InvestigationV1` and delegates to the
investigation evaluator:

```rust
fn job_input_for_investigation_step(
    step_id: &StepId,
    target_ref_head: &CommitOid,
) -> JobInput {
    match step_id {
        StepId::InvestigateProject | StepId::ReinvestigateProject => {
            JobInput::ProjectSnapshot {
                head_commit_oid: target_ref_head.clone(),
            }
        }
        _ => unreachable!("investigation workflow has no other steps"),
    }
}
```

### Finding Extraction

`extract_findings()` is extended to handle `investigation_report:v1`:

```rust
"investigation_report:v1" => {
    let report: InvestigationReport = serde_json::from_value(payload)?;
    validate_investigation_report(&report)?;
    report.findings.into_iter().map(|f| Finding {
        // existing fields from finding
        source_finding_key: f.finding_key,
        code: f.code,
        severity: f.severity.into(),
        summary: f.summary,
        paths: f.paths,
        evidence: json!(f.evidence),
        // new promotion fields
        promotion_title: Some(f.promotion.title),
        promotion_description: Some(f.promotion.description),
        promotion_acceptance_criteria: Some(f.promotion.acceptance_criteria),
        suggested_classification: Some(f.promotion.classification.into()),
        estimated_scope: Some(f.promotion.estimated_scope.into()),
        group_key: f.group_key,
        triage: FindingTriage::Untriaged,
        ..
    })
}
```

### Batch Promotion

New use case for promoting multiple findings at once:

```rust
pub struct BatchPromoteInput {
    pub finding_ids: Vec<FindingId>,
    pub target_ref: Option<GitRef>,       // override, else inherit
    pub approval_policy: Option<ApprovalPolicy>,  // override
}

pub struct BatchPromoteOutput {
    pub promoted: Vec<(FindingId, ItemId)>,
    pub skipped: Vec<(FindingId, String)>,  // reason
}
```

Each promoted finding:
1. Uses `promotion_title` / `promotion_description` /
   `promotion_acceptance_criteria` from the finding (falls back to
   existing `backlog_finding()` synthesis if absent).
2. Uses `suggested_classification` (defaults to `Change`).
3. Creates item with `Origin::PromotedFinding { finding_id }`.
4. Sets finding triage to `Backlog { linked_item_id }`.

Findings with the same `group_key` can optionally be merged into a
single item (operator chooses during triage).

### Investigation Completion

When all findings on an investigation item are triaged and none are
`fix_now` / `untriaged` / `needs_investigation`, the evaluator signals
the item can be closed. The operator (or autopilot) closes the
investigation item.

The investigation item's `done_reason` captures the outcome:
- `Completed` — findings produced and triaged
- `Dismissed` — investigation found nothing actionable
- `Invalidated` — investigation was cancelled

## HTTP API Extensions

### New Endpoints

```
POST   /projects/:id/investigations          Create investigation item
GET    /projects/:id/investigations           List investigation items
GET    /projects/:id/items/:id/findings       (existing) List findings
POST   /projects/:id/findings/batch-promote   Batch promote findings
POST   /projects/:id/findings/batch-triage    Batch triage findings
```

### Investigation Creation Request

```json
{
  "title": "Find helper duplication across crates",
  "description": "Investigate each crate using subagent and find util/helper non-test methods. Then find duplication and suggest refactoring to reduce helper duplication.",
  "target_ref": "refs/heads/main",
  "priority": "minor",
  "labels": ["refactoring", "tech-debt"]
}
```

### Batch Promote Request

```json
{
  "finding_ids": ["finding-1", "finding-2", "finding-3"],
  "group_by_key": true,
  "target_ref": "refs/heads/main"
}
```

When `group_by_key=true`, findings sharing a `group_key` are merged into
a single item with a combined description.

## UI Impact

### Board Changes

Investigation items appear on the board with:
- A distinctive classification badge (`Investigation` vs `Change`/`Bug`)
- Finding count indicator (e.g., "5 findings, 2 untriaged")
- No convergence progress (not applicable)

### Investigation Detail View

- Shows the investigation brief (description)
- Shows the agent's scope narrative (methodology, paths examined)
- Lists findings with triage controls
- Batch actions: "Promote All", "Promote Selected", "Dismiss All"
- Shows spawned items with links

### Finding Card (Enhanced)

When a finding has promotion fields, the triage UI shows:
- Proposed item title
- Proposed description (collapsible)
- Proposed acceptance criteria
- Estimated scope badge
- Group key indicator (if grouped with other findings)
- "Promote" button pre-fills from promotion fields

## SQLite Schema

### New Columns on `findings` Table

```sql
ALTER TABLE findings ADD COLUMN promotion_title TEXT;
ALTER TABLE findings ADD COLUMN promotion_description TEXT;
ALTER TABLE findings ADD COLUMN promotion_acceptance_criteria TEXT;
ALTER TABLE findings ADD COLUMN suggested_classification TEXT;
ALTER TABLE findings ADD COLUMN estimated_scope TEXT;
ALTER TABLE findings ADD COLUMN group_key TEXT;
```

All nullable. Existing findings unaffected.

### No New Tables

Investigation items are `items` rows with
`classification='investigation'` and
`workflow_version='investigation:v1'`. No separate table needed.

## Invariants

1. Investigation items never produce commits. `output_artifact_kind` is
   always `FindingReport`, never `Commit`.
2. Investigation items never enter convergence. No `Convergence` rows
   are created.
3. Investigation items never enter the approval gate.
   `approval_state=NotRequired` always.
4. The `InvestigateProject` step is `ClosureRelevant` in the
   investigation workflow (unlike `InvestigateItem` which is
   `ReportOnly` in delivery:v1).
5. Promotion fields on findings are only populated by
   `investigation_report:v1`. Existing report types leave them `None`.
6. Investigation items can be closed only when all findings are triaged
   (none blocking).

## Affected Crates

| Crate | Changes |
|-------|---------|
| **ingot-domain** | `Classification::Investigation`, `WorkflowVersion::InvestigationV1`, `EstimatedScope`, `JobInput::ProjectSnapshot`, new `StepId` variants, `Finding` promotion fields |
| **ingot-workflow** | `investigation:v1` step contracts, graph, evaluator branch |
| **ingot-agent-protocol** | `investigation_report:v1` schema, validation, prompt suffix |
| **ingot-usecases** | `create_investigation_item()`, investigation dispatch, `extract_findings()` extension, `batch_promote_findings()`, `batch_triage_findings()` |
| **ingot-store-sqlite** | Migration for finding columns, investigation item queries |
| **ingot-agent-runtime** | `ProjectSnapshot` workspace provisioning |
| **ingot-http-api** | Investigation endpoints, batch promote/triage endpoints |
| **ingot-daemon** | Wire new routes |
| **ui/** | Investigation board column, finding triage UI, batch promote |

**Unchanged:** ingot-git, ingot-workspace (workspace provisioning
already supports review/read-only), ingot-config,
ingot-agent-adapters (adapters are prompt-agnostic).

## Alternatives Considered

### A. Investigation as a Separate Entity (Not an Item)

A new `Investigation` table alongside `items`, with its own lifecycle.

**Rejected because:** Fragments the board concept. Items are the unit of
board visibility. Adding a parallel entity means duplicating board
status, evaluation, activity tracking, and UI components. Using `Item`
with a different classification and workflow is cheaper and more
consistent.

### B. Overloading delivery:v1 with a "skip authoring" Flag

Add a flag to skip authoring steps, making the investigation step
primary.

**Rejected because:** Overcomplicates the delivery evaluator with
conditional paths. A clean separate workflow graph is easier to reason
about and test independently. Delivery:v1 has 15 steps with specific
transition edges — adding conditionals risks subtle bugs.

### C. Making InvestigateItem the Only Step (No New Workflow)

Reuse `InvestigateItem` but make it closure-relevant for investigation
items.

**Rejected because:** `InvestigateItem` has delivery:v1 semantics baked
in (it's auxiliary, uses delivery-scoped job input). Investigation needs
its own step IDs to avoid overloading the meaning of existing steps.
However, the template slug system means investigation steps can share
prompt templates if desired.

### D. Richer Finding Types Instead of Promotion Fields

Define `InvestigationFinding` as a separate type from `Finding`.

**Rejected because:** Creates parallel finding systems. The triage
workflow, UI components, and database queries all operate on `Finding`.
Adding optional promotion fields to the existing type is simpler and
backward-compatible. The `investigation_report:v1` contract makes the
promotion block required, ensuring investigation findings are always
promotable.
