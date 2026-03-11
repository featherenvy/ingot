// Domain types mirroring ingot-domain entities

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
export type PhaseKind = 'author' | 'validate' | 'review'

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

export interface Project {
  id: string
  name: string
  path: string
  default_branch: string
  color: string
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
  next_recommended_action: string
  dispatchable_step_id: string | null
  allowed_actions: string[]
  terminal_readiness: boolean
  diagnostics: string[]
}

export interface Job {
  id: string
  item_id: string
  item_revision_id: string
  step_id: string
  status: JobStatus
  outcome_class: OutcomeClass | null
  phase_kind: PhaseKind
  workspace_id: string | null
  created_at: string
  started_at: string | null
  ended_at: string | null
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
  target_head_valid: boolean
}

export interface ItemDetail {
  item: Item
  current_revision: ItemRevision
  evaluation: Evaluation
  revision_history: ItemRevision[]
  jobs: Job[]
  workspaces: Workspace[]
  convergences: Convergence[]
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
