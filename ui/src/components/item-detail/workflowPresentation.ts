import type { Item, PhaseKind } from '../../types/domain'

export type WorkflowVersion = Item['workflow_version']

export type WorkflowStepDef = { id: string; label: string; phase: PhaseKind }
export type WorkflowPhaseDef = { id: string; label: string; steps: WorkflowStepDef[] }

export const WORKFLOW_PHASES_BY_VERSION: Record<WorkflowVersion, WorkflowPhaseDef[]> = {
  'delivery:v1': [
    {
      id: 'candidate',
      label: 'Candidate',
      steps: [
        { id: 'author_initial', label: 'Author', phase: 'author' },
        { id: 'review_incremental_initial', label: 'Incr. Review', phase: 'review' },
        { id: 'review_candidate_initial', label: 'Cand. Review', phase: 'review' },
        { id: 'validate_candidate_initial', label: 'Validate', phase: 'validate' },
        { id: 'repair_candidate', label: 'Repair', phase: 'author' },
        { id: 'review_incremental_repair', label: 'Re-review', phase: 'review' },
        { id: 'review_candidate_repair', label: 'Cand. Re-review', phase: 'review' },
        { id: 'validate_candidate_repair', label: 'Re-validate', phase: 'validate' },
        { id: 'investigate_item', label: 'Investigate', phase: 'investigate' },
      ],
    },
    {
      id: 'converge',
      label: 'Converge',
      steps: [{ id: 'prepare_convergence', label: 'Prepare', phase: 'system' }],
    },
    {
      id: 'integration',
      label: 'Integration',
      steps: [
        { id: 'validate_integrated', label: 'Validate', phase: 'validate' },
        { id: 'repair_after_integration', label: 'Repair', phase: 'author' },
        { id: 'review_incremental_after_integration_repair', label: 'Incr. Review', phase: 'review' },
        { id: 'review_after_integration_repair', label: 'Cand. Review', phase: 'review' },
        { id: 'validate_after_integration_repair', label: 'Re-validate', phase: 'validate' },
      ],
    },
  ],
  'investigation:v1': [
    {
      id: 'investigation',
      label: 'Investigation',
      steps: [
        { id: 'investigate_project', label: 'Investigate', phase: 'investigate' },
        { id: 'reinvestigate_project', label: 'Reinvestigate', phase: 'investigate' },
      ],
    },
  ],
}

export type WorkflowFindingsCopy = {
  agentScopeTitle: string
  currentSectionTitle: string
  currentSectionHint: string
  previousSectionTitle: string
  previousSectionSummaryNoun: string
  triageWarning: string
}

export const WORKFLOW_FINDINGS_COPY: Record<WorkflowVersion, WorkflowFindingsCopy> = {
  'delivery:v1': {
    agentScopeTitle: 'Agent scope for next repair job',
    currentSectionTitle: 'Current Review',
    currentSectionHint: 'agent acts on these findings only',
    previousSectionTitle: 'Previous Reviews',
    previousSectionSummaryNoun: 'earlier job',
    triageWarning: 'Triage all findings before the agent can proceed.',
  },
  'investigation:v1': {
    agentScopeTitle: 'Current investigation findings',
    currentSectionTitle: 'Current Investigation',
    currentSectionHint: 'triage or promote from this run',
    previousSectionTitle: 'Previous Investigation Runs',
    previousSectionSummaryNoun: 'earlier investigation run',
    triageWarning: 'Triage all findings before the investigation can close.',
  },
}
