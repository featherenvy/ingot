import { render, screen } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { FindingsTable } from '../components/item-detail/FindingsTable'
import { TooltipProvider } from '../components/ui/tooltip'
import type { Finding, Job, LinkedFindingItemSummary } from '../types/domain'

function renderFindingsTable(props: {
  findings: Finding[]
  jobs: Job[]
  linkedFindingItems?: LinkedFindingItemSummary[]
  workflowVersion: 'delivery:v1' | 'investigation:v1'
}) {
  return render(
    <MemoryRouter>
      <TooltipProvider>
        <FindingsTable
          {...props}
          linkedFindingItems={props.linkedFindingItems ?? []}
          onPromote={() => {}}
          onTriage={() => {}}
          pendingFindingId={null}
        />
      </TooltipProvider>
    </MemoryRouter>,
  )
}

function makeJob(params: { id: string; stepId: string; endedAt: string; phaseKind?: Job['phase_kind'] }): Job {
  return {
    id: params.id,
    project_id: 'prj_1',
    item_id: 'itm_1',
    item_revision_id: 'rev_1',
    step_id: params.stepId,
    status: 'completed',
    outcome_class: 'findings',
    phase_kind: params.phaseKind ?? 'review',
    workspace_id: null,
    job_input: {
      kind: 'candidate_subject',
      base_commit_oid: 'base',
      head_commit_oid: 'head',
    },
    created_at: '2026-03-11T00:00:00Z',
    started_at: '2026-03-11T00:01:00Z',
    ended_at: params.endedAt,
  }
}

function makeFinding(params: {
  id: string
  sourceJobId: string
  sourceStepId: string
  createdAt: string
  triageState: Finding['triage_state']
}): Finding {
  return {
    id: params.id,
    project_id: 'prj_1',
    source_item_id: 'itm_1',
    source_item_revision_id: 'rev_1',
    source_job_id: params.sourceJobId,
    source_step_id: params.sourceStepId,
    source_report_schema_version: 'review_report:v1',
    source_finding_key: `${params.id}-key`,
    source_subject_kind: 'candidate',
    source_subject_base_commit_oid: 'base',
    source_subject_head_commit_oid: 'head',
    code: 'BUG001',
    severity: 'medium',
    summary: `Summary for ${params.id}`,
    paths: ['src/lib.rs'],
    evidence: [],
    investigation: null,
    triage_state: params.triageState,
    linked_item_id: null,
    triage_note: null,
    created_at: params.createdAt,
    triaged_at: null,
  }
}

describe('FindingsTable', () => {
  it('renders investigation-specific copy for investigation items', () => {
    const historicalJob = makeJob({
      id: 'job_1',
      stepId: 'investigate_project',
      endedAt: '2026-03-11T00:02:00Z',
      phaseKind: 'investigate',
    })
    const latestJob = makeJob({
      id: 'job_2',
      stepId: 'reinvestigate_project',
      endedAt: '2026-03-12T00:02:00Z',
      phaseKind: 'investigate',
    })

    renderFindingsTable({
      workflowVersion: 'investigation:v1',
      jobs: [historicalJob, latestJob],
      findings: [
        makeFinding({
          id: 'fnd_1',
          sourceJobId: historicalJob.id,
          sourceStepId: historicalJob.step_id,
          createdAt: '2026-03-11T00:02:00Z',
          triageState: 'wont_fix',
        }),
        makeFinding({
          id: 'fnd_2',
          sourceJobId: latestJob.id,
          sourceStepId: latestJob.step_id,
          createdAt: '2026-03-12T00:02:00Z',
          triageState: 'untriaged',
        }),
      ],
    })

    expect(screen.getByText('Current investigation findings')).toBeInTheDocument()
    expect(screen.getByText('Current Investigation')).toBeInTheDocument()
    expect(screen.getByText('Previous Investigation Runs')).toBeInTheDocument()
    expect(screen.getByText('Triage all findings before the investigation can close.')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Fix now' })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Backlog' })).toBeInTheDocument()
    expect(screen.getByText(/1 earlier investigation run/)).toBeInTheDocument()
  })

  it('renders a fixing badge and linked item for launched investigation findings', () => {
    const latestJob = makeJob({
      id: 'job_1',
      stepId: 'reinvestigate_project',
      endedAt: '2026-03-12T00:02:00Z',
      phaseKind: 'investigate',
    })

    renderFindingsTable({
      workflowVersion: 'investigation:v1',
      jobs: [latestJob],
      findings: [
        makeFinding({
          id: 'fnd_1',
          sourceJobId: latestJob.id,
          sourceStepId: latestJob.step_id,
          createdAt: '2026-03-12T00:02:00Z',
          triageState: 'backlog',
        }),
      ],
      linkedFindingItems: [
        {
          finding_id: 'fnd_1',
          item: {
            id: 'itm_2',
            project_id: 'prj_1',
            classification: 'change',
            workflow_version: 'delivery:v1',
            lifecycle_state: 'open',
            parking_state: 'active',
            approval_state: 'not_requested',
            escalation_state: 'none',
            current_revision_id: 'rev_2',
            origin_kind: 'promoted_finding',
            origin_finding_id: 'fnd_1',
            priority: 'major',
            labels: [],
            operator_notes: null,
            sort_key: '2026-03-12T00:00:00Z#itm_2',
            created_at: '2026-03-12T00:00:00Z',
            updated_at: '2026-03-12T00:00:00Z',
          },
          title: 'Extract shared helper',
          board_status: 'WORKING',
          job_count: 1,
        },
      ],
    })

    expect(screen.getByText('Fixing')).toBeInTheDocument()
    expect(screen.getByRole('link', { name: 'Extract shared helper' })).toHaveAttribute(
      'href',
      '/projects/prj_1/items/itm_2',
    )
  })

  it('keeps delivery copy for delivery items', () => {
    const latestJob = makeJob({
      id: 'job_1',
      stepId: 'review_candidate_initial',
      endedAt: '2026-03-12T00:02:00Z',
    })

    renderFindingsTable({
      workflowVersion: 'delivery:v1',
      jobs: [latestJob],
      findings: [
        makeFinding({
          id: 'fnd_1',
          sourceJobId: latestJob.id,
          sourceStepId: latestJob.step_id,
          createdAt: '2026-03-12T00:02:00Z',
          triageState: 'untriaged',
        }),
      ],
    })

    expect(screen.getByText('Agent scope for next repair job')).toBeInTheDocument()
    expect(screen.getByText('Current Review')).toBeInTheDocument()
    expect(screen.getByText('Triage all findings before the agent can proceed.')).toBeInTheDocument()
  })

  it('uses investigation copy when a delivery item latest findings come from investigation', () => {
    const historicalJob = makeJob({
      id: 'job_1',
      stepId: 'review_candidate_initial',
      endedAt: '2026-03-11T00:02:00Z',
      phaseKind: 'review',
    })
    const latestJob = makeJob({
      id: 'job_2',
      stepId: 'investigate_item',
      endedAt: '2026-03-12T00:02:00Z',
      phaseKind: 'investigate',
    })

    renderFindingsTable({
      workflowVersion: 'delivery:v1',
      jobs: [historicalJob, latestJob],
      findings: [
        makeFinding({
          id: 'fnd_1',
          sourceJobId: historicalJob.id,
          sourceStepId: historicalJob.step_id,
          createdAt: '2026-03-11T00:02:00Z',
          triageState: 'wont_fix',
        }),
        makeFinding({
          id: 'fnd_2',
          sourceJobId: latestJob.id,
          sourceStepId: latestJob.step_id,
          createdAt: '2026-03-12T00:02:00Z',
          triageState: 'untriaged',
        }),
      ],
    })

    expect(screen.getByText('Current investigation findings')).toBeInTheDocument()
    expect(screen.getByText('Current Investigation')).toBeInTheDocument()
    expect(screen.getByText('Triage all findings before the investigation can close.')).toBeInTheDocument()
    expect(screen.queryByText('Agent scope for next repair job')).not.toBeInTheDocument()
    expect(screen.queryByText('Current Review')).not.toBeInTheDocument()
  })
})
