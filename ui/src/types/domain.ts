// DTOs mirroring backend JSON responses

export type Classification = 'change' | 'bug'
export type LifecycleState = 'open' | 'done'
export type ParkingState = 'active' | 'deferred'
export type DoneReason = 'completed' | 'dismissed' | 'invalidated'
export type ResolutionSource = 'system_command' | 'approval_command' | 'manual_command'
export type ApprovalState = 'not_required' | 'not_requested' | 'pending' | 'approved'
export type EscalationState = 'none' | 'operator_required'
export type EscalationReason =
  | 'candidate_rework_budget_exhausted'
  | 'integration_rework_budget_exhausted'
  | 'convergence_conflict'
  | 'step_failed'
  | 'protocol_violation'
  | 'manual_decision_required'
  | 'other'
export type Priority = 'critical' | 'major' | 'minor'
export type OriginKind = 'manual' | 'promoted_finding'
export type BoardStatus = 'INBOX' | 'WORKING' | 'APPROVAL' | 'DONE'

export type JobStatus =
  | 'queued'
  | 'assigned'
  | 'running'
  | 'completed'
  | 'failed'
  | 'cancelled'
  | 'expired'
  | 'superseded'
export type OutcomeClass =
  | 'clean'
  | 'findings'
  | 'transient_failure'
  | 'terminal_failure'
  | 'protocol_violation'
  | 'cancelled'
export type PhaseKind = 'author' | 'validate' | 'review' | 'investigate' | 'system'
export type PhaseStatus =
  | 'done'
  | 'running'
  | 'escalated'
  | 'idle'
  | 'deferred'
  | 'pending_approval'
  | 'finalization_ready'
  | 'awaiting_convergence'
  | 'new'
  | 'unknown'

export type WorkspaceKind = 'authoring' | 'review' | 'integration'
export type WorkspaceStatus =
  | 'provisioning'
  | 'ready'
  | 'busy'
  | 'stale'
  | 'retained_for_debug'
  | 'abandoned'
  | 'error'
  | 'removing'

export type ConvergenceStatus = 'queued' | 'running' | 'conflicted' | 'prepared' | 'finalized' | 'failed' | 'cancelled'
export type FindingSubjectKind = 'candidate' | 'integrated'
export type FindingSeverity = 'low' | 'medium' | 'high' | 'critical'
export type FindingTriageState = 'untriaged' | 'promoted' | 'dismissed'
export type ActivityEventType =
  | 'item_created'
  | 'item_revision_created'
  | 'item_updated'
  | 'item_deferred'
  | 'item_resumed'
  | 'item_dismissed'
  | 'item_invalidated'
  | 'item_reopened'
  | 'item_escalated'
  | 'item_escalation_cleared'
  | 'job_dispatched'
  | 'job_completed'
  | 'job_failed'
  | 'job_cancelled'
  | 'finding_promoted'
  | 'finding_dismissed'
  | 'approval_requested'
  | 'approval_approved'
  | 'approval_rejected'
  | 'convergence_started'
  | 'convergence_conflicted'
  | 'convergence_prepared'
  | 'convergence_finalized'
  | 'convergence_failed'
  | 'git_operation_planned'
  | 'git_operation_reconciled'

export type JsonPrimitive = string | number | boolean | null
export type JsonValue = JsonPrimitive | JsonObject | JsonValue[]

export interface JsonObject {
  [key: string]: JsonValue
}

export interface Project {
  id: string
  name: string
  path: string
  default_branch: string
  color: string
}

export interface Activity {
  id: string
  project_id: string
  event_type: ActivityEventType
  entity_type: string
  entity_id: string
  payload: JsonValue
  created_at: string
}

export type AdapterKind = 'claude_code' | 'codex'
export type AgentStatus = 'available' | 'unavailable' | 'probing'
export type AgentCapability = 'read_only_jobs' | 'mutating_jobs' | 'structured_output' | 'streaming_progress'

export interface Agent {
  id: string
  slug: string
  name: string
  adapter_kind: AdapterKind
  provider: string
  model: string
  cli_path: string
  capabilities: AgentCapability[]
  health_check: string | null
  status: AgentStatus
}

export interface Item {
  id: string
  project_id: string
  classification: Classification
  workflow_version: string
  lifecycle_state: LifecycleState
  parking_state: ParkingState
  done_reason: DoneReason | null
  resolution_source: ResolutionSource | null
  approval_state: ApprovalState
  escalation_state: EscalationState
  escalation_reason: EscalationReason | null
  current_revision_id: string
  origin_kind: OriginKind
  origin_finding_id: string | null
  priority: Priority
  labels: string[]
  operator_notes: string | null
  created_at: string
  updated_at: string
  closed_at: string | null
}

export interface ItemRevision {
  id: string
  item_id: string
  revision_no: number
  title: string
  description: string
  acceptance_criteria: string
  target_ref: string
  approval_policy: 'required' | 'not_required'
  seed_commit_oid: string
  supersedes_revision_id: string | null
  created_at: string
}

export interface Evaluation {
  board_status: BoardStatus
  attention_badges: string[]
  current_step_id: string | null
  current_phase_kind: PhaseKind | null
  phase_status: PhaseStatus | null
  next_recommended_action: string
  dispatchable_step_id: string | null
  auxiliary_dispatchable_step_ids: string[]
  allowed_actions: string[]
  terminal_readiness: boolean
  diagnostics: string[]
}

export interface ItemSummary {
  item: Item
  title: string
  evaluation: Evaluation
}

export interface Job {
  project_id: string
  id: string
  item_id: string
  item_revision_id: string
  step_id: string
  status: JobStatus
  outcome_class: OutcomeClass | null
  phase_kind: PhaseKind
  workspace_id: string | null
  lease_owner_id?: string | null
  heartbeat_at?: string | null
  lease_expires_at?: string | null
  error_code?: string | null
  error_message?: string | null
  created_at: string
  started_at: string | null
  ended_at: string | null
}

export interface JobLogs {
  prompt: string | null
  stdout: string | null
  stderr: string | null
  result: JsonValue | null
}

export interface Workspace {
  id: string
  kind: WorkspaceKind
  status: WorkspaceStatus
  target_ref: string | null
  workspace_ref: string | null
  base_commit_oid: string | null
  head_commit_oid: string | null
}

export interface Convergence {
  id: string
  status: ConvergenceStatus
  input_target_commit_oid: string | null
  prepared_commit_oid: string | null
  final_target_commit_oid: string | null
  target_head_valid: boolean
}

export interface Finding {
  id: string
  project_id: string
  source_item_id: string
  source_item_revision_id: string
  source_job_id: string
  source_step_id: string
  source_report_schema_version: string
  source_finding_key: string
  source_subject_kind: FindingSubjectKind
  source_subject_base_commit_oid: string | null
  source_subject_head_commit_oid: string
  code: string
  severity: FindingSeverity
  summary: string
  paths: string[]
  evidence: JsonValue
  triage_state: FindingTriageState
  promoted_item_id: string | null
  dismissal_reason: string | null
  created_at: string
  triaged_at: string | null
}

export interface RevisionContextResultSummary {
  job_id: string
  schema_version: string
  outcome: string
  summary: string
}

export interface RevisionContextAcceptedResultRef {
  job_id: string
  step_id: string
  schema_version: string
  outcome: string
  summary: string
}

export interface RevisionContextSummary {
  updated_at: string
  changed_paths: string[]
  latest_validation: RevisionContextResultSummary | null
  latest_review: RevisionContextResultSummary | null
  accepted_result_refs: RevisionContextAcceptedResultRef[]
  operator_notes_excerpt: string | null
}

export interface ItemDetail {
  item: Item
  current_revision: ItemRevision
  evaluation: Evaluation
  revision_history: ItemRevision[]
  jobs: Job[]
  findings: Finding[]
  workspaces: Workspace[]
  convergences: Convergence[]
  revision_context_summary: RevisionContextSummary | null
  diagnostics: string[]
}

export interface WsEvent {
  seq: number
  event: string
  project_id: string
  entity_type: string
  entity_id: string
  payload: Record<string, unknown>
}
