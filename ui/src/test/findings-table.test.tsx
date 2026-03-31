import { render, screen } from '@testing-library/react'
import { FindingsTable } from '../components/item-detail/FindingsTable'
import { TooltipProvider } from '../components/ui/tooltip'
import type { Finding, Job } from '../types/domain'

function renderFindingsTable(props: {
  findings: Finding[]
  jobs: Job[]
  workflowVersion: 'delivery:v1' | 'investigation:v1'
}) {
  return render(
    <TooltipProvider>
      <FindingsTable {...props} onTriage={() => {}} pendingFindingId={null} />
    </TooltipProvider>,
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

    expect(screen.getByText('Agent scope for next investigation run')).toBeInTheDocument()
    expect(screen.getByText('Current Investigation')).toBeInTheDocument()
    expect(screen.getByText('Previous Investigation Runs')).toBeInTheDocument()
    expect(
      screen.getByText('Triage all findings before the next investigation run can be dispatched.'),
    ).toBeInTheDocument()
    expect(screen.getByText(/1 earlier investigation run/)).toBeInTheDocument()
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

    expect(screen.getByText('Agent scope for next investigation run')).toBeInTheDocument()
    expect(screen.getByText('Current Investigation')).toBeInTheDocument()
    expect(
      screen.getByText('Triage all findings before the next investigation run can be dispatched.'),
    ).toBeInTheDocument()
    expect(screen.queryByText('Agent scope for next repair job')).not.toBeInTheDocument()
    expect(screen.queryByText('Current Review')).not.toBeInTheDocument()
  })
})
