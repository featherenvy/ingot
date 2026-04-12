import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { fireEvent, render, screen } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router'
import { TooltipProvider } from '../components/ui/tooltip'
import JobsPage from '../pages/JobsPage'
import { useConnectionStore } from '../stores/connection'
import type { Agent, Job } from '../types/domain'

function jsonResponse(body: unknown) {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: {
      'Content-Type': 'application/json',
    },
  })
}

function renderPage() {
  const queryClient = new QueryClient({
    defaultOptions: {
      queries: {
        retry: false,
      },
    },
  })

  return render(
    <QueryClientProvider client={queryClient}>
      <TooltipProvider>
        <MemoryRouter initialEntries={['/projects/prj_1/jobs']}>
          <Routes>
            <Route path="/projects/:projectId/jobs" element={<JobsPage />} />
          </Routes>
        </MemoryRouter>
      </TooltipProvider>
    </QueryClientProvider>,
  )
}

describe('JobsPage', () => {
  beforeEach(() => {
    useConnectionStore.setState({
      status: 'connected',
      lastSeq: 0,
      ws: null,
      jobLogSyncState: 'live',
      recentLogChunkAtByJobId: {},
    })
  })

  afterEach(() => {
    vi.restoreAllMocks()
    useConnectionStore.setState({
      status: 'disconnected',
      lastSeq: 0,
      ws: null,
      jobLogSyncState: 'live',
      recentLogChunkAtByJobId: {},
    })
  })

  it('renders job step labels and duration', async () => {
    const agents: Agent[] = []
    const jobs: Job[] = [
      {
        id: 'job_1',
        project_id: 'prj_1',
        item_id: 'itm_1',
        item_revision_id: 'rev_1',
        step_id: 'author_initial',
        status: 'completed',
        outcome_class: 'clean',
        phase_kind: 'author',
        workspace_id: 'wrk_1',
        job_input: { kind: 'authoring_head', head_commit_oid: '0123456789abcdef' },
        created_at: '2026-03-11T00:00:00Z',
        started_at: '2026-03-11T00:01:00Z',
        ended_at: '2026-03-11T00:02:00Z',
      },
    ]

    vi.spyOn(globalThis, 'fetch').mockImplementation((input) => {
      const url = String(input)
      if (url.endsWith('/api/agents')) {
        return Promise.resolve(jsonResponse(agents))
      }
      if (url.endsWith('/api/projects/prj_1/jobs')) {
        return Promise.resolve(jsonResponse(jobs))
      }
      if (url.endsWith('/api/projects/prj_1/items')) {
        return Promise.resolve(jsonResponse([]))
      }
      throw new Error(`Unexpected fetch: ${url}`)
    })

    renderPage()

    // Step label is formatted from step_id
    expect(await screen.findByText('Author Initial')).toBeInTheDocument()
    // Duration is rendered (1 minute between start and end)
    expect(screen.getByText('1m 0s')).toBeInTheDocument()
    // Phase kind is shown
    expect(screen.getByText('author')).toBeInTheDocument()
  })

  it('renders a destructive alert when the jobs query fails', async () => {
    vi.spyOn(globalThis, 'fetch').mockImplementation((input) => {
      const url = String(input)
      if (url.endsWith('/api/agents')) {
        return Promise.resolve(jsonResponse([]))
      }
      if (url.endsWith('/api/projects/prj_1/jobs')) {
        return Promise.reject(new Error('network down'))
      }
      if (url.endsWith('/api/projects/prj_1/items')) {
        return Promise.resolve(jsonResponse([]))
      }
      throw new Error(`Unexpected fetch: ${url}`)
    })

    renderPage()

    expect(await screen.findByText('Jobs failed to load')).toBeInTheDocument()
    expect(screen.getByText('Error: network down')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Retry' })).toBeInTheDocument()
  })

  it('shows live log tabs and waiting state for a running job', async () => {
    const jobs: Job[] = [
      {
        id: 'job_1',
        project_id: 'prj_1',
        item_id: 'itm_1',
        item_revision_id: 'rev_1',
        step_id: 'review_candidate_initial',
        status: 'running',
        outcome_class: null,
        phase_kind: 'review',
        workspace_id: 'wrk_1',
        job_input: {
          kind: 'candidate_subject',
          base_commit_oid: '0123456789abcdef',
          head_commit_oid: 'fedcba9876543210',
        },
        created_at: '2026-03-11T00:00:00Z',
        started_at: '2026-03-11T00:01:00Z',
        ended_at: null,
      },
    ]

    vi.spyOn(globalThis, 'fetch').mockImplementation((input) => {
      const url = String(input)
      if (url.endsWith('/api/agents')) {
        return Promise.resolve(jsonResponse([]))
      }
      if (url.endsWith('/api/projects/prj_1/jobs')) {
        return Promise.resolve(jsonResponse(jobs))
      }
      if (url.endsWith('/api/projects/prj_1/items')) {
        return Promise.resolve(jsonResponse([]))
      }
      if (url.endsWith('/api/jobs/job_1/logs')) {
        return Promise.resolve(
          jsonResponse({
            prompt: 'Review the candidate diff',
            output: {
              schema_version: 'agent_output:v1',
              segments: [],
            },
            result: null,
          }),
        )
      }
      throw new Error(`Unexpected fetch: ${url}`)
    })

    renderPage()

    fireEvent.click(await screen.findByRole('button', { name: /Review Candidate Initial/i }))

    expect(await screen.findByText('Live')).toBeInTheDocument()
    expect(screen.getByText('waiting')).toBeInTheDocument()
    expect(await screen.findByText('Waiting for normalized agent output...')).toBeInTheDocument()
    expect(screen.getAllByText('Output')).not.toHaveLength(0)
    expect(screen.getByRole('tab', { name: /Prompt/i })).toBeInTheDocument()
    expect(screen.getByRole('tab', { name: /Result/i })).toBeInTheDocument()
  })

  it('shows streaming and recovered sync cues when recent chunks and recovery state exist', async () => {
    const jobs: Job[] = [
      {
        id: 'job_1',
        project_id: 'prj_1',
        item_id: 'itm_1',
        item_revision_id: 'rev_1',
        step_id: 'review_candidate_initial',
        status: 'running',
        outcome_class: null,
        phase_kind: 'review',
        workspace_id: 'wrk_1',
        job_input: {
          kind: 'candidate_subject',
          base_commit_oid: '0123456789abcdef',
          head_commit_oid: 'fedcba9876543210',
        },
        created_at: '2026-03-11T00:00:00Z',
        started_at: '2026-03-11T00:01:00Z',
        ended_at: null,
      },
    ]

    useConnectionStore.setState({
      status: 'connected',
      lastSeq: 4,
      ws: null,
      jobLogSyncState: 'recovered',
      recentLogChunkAtByJobId: {
        job_1: Date.now(),
      },
    })

    vi.spyOn(globalThis, 'fetch').mockImplementation((input) => {
      const url = String(input)
      if (url.endsWith('/api/agents')) {
        return Promise.resolve(jsonResponse([]))
      }
      if (url.endsWith('/api/projects/prj_1/jobs')) {
        return Promise.resolve(jsonResponse(jobs))
      }
      if (url.endsWith('/api/projects/prj_1/items')) {
        return Promise.resolve(jsonResponse([]))
      }
      if (url.endsWith('/api/jobs/job_1/logs')) {
        return Promise.resolve(
          jsonResponse({
            prompt: 'Review the candidate diff',
            output: {
              schema_version: 'agent_output:v1',
              segments: [
                {
                  sequence: 1,
                  channel: 'primary',
                  kind: 'text',
                  status: null,
                  title: null,
                  text: 'streaming now',
                  data: null,
                },
                {
                  sequence: 2,
                  channel: 'diagnostic',
                  kind: 'text',
                  status: null,
                  title: 'stderr',
                  text: 'warn',
                  data: null,
                },
              ],
            },
            result: null,
          }),
        )
      }
      throw new Error(`Unexpected fetch: ${url}`)
    })

    renderPage()

    fireEvent.click(await screen.findByRole('button', { name: /Review Candidate Initial/i }))

    expect(await screen.findByText('streaming')).toBeInTheDocument()
    expect(await screen.findByText('Log stream recovered')).toBeInTheDocument()
    expect(screen.getByText(/Current output now reflects the persisted log plus new chunks/i)).toBeInTheDocument()
  })
})
