import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router'
import { TooltipProvider } from '../components/ui/tooltip'
import ItemDetailPage from '../pages/ItemDetailPage'
import type { ItemDetail } from '../types/domain'

function makeItemDetail(): ItemDetail {
  return {
    item: {
      id: 'itm_1',
      project_id: 'prj_1',
      classification: 'change',
      workflow_version: 'delivery:v1',
      lifecycle_state: 'open',
      parking_state: 'active',
      done_reason: null,
      resolution_source: null,
      approval_state: 'not_requested',
      escalation_state: 'none',
      escalation_reason: null,
      current_revision_id: 'rev_1',
      origin_kind: 'manual',
      origin_finding_id: null,
      priority: 'major',
      labels: [],
      operator_notes: null,
      created_at: '2026-03-11T00:00:00Z',
      updated_at: '2026-03-11T00:00:00Z',
      closed_at: null,
    },
    current_revision: {
      id: 'rev_1',
      item_id: 'itm_1',
      revision_no: 1,
      title: 'Ship the feature',
      description: 'Complete the work',
      acceptance_criteria: 'All checks pass',
      target_ref: 'main',
      approval_policy: 'required',
      seed_commit_oid: '0123456789abcdef',
      supersedes_revision_id: null,
      created_at: '2026-03-11T00:00:00Z',
    },
    evaluation: {
      board_status: 'WORKING',
      attention_badges: [],
      current_step_id: 'author_initial',
      current_phase_kind: 'author',
      phase_status: 'running',
      next_recommended_action: 'dispatch',
      dispatchable_step_id: 'author_initial',
      auxiliary_dispatchable_step_ids: [],
      allowed_actions: ['dispatch'],
      terminal_readiness: false,
      diagnostics: [],
    },
    queue: {
      state: null,
      position: null,
      lane_owner_item_id: null,
      lane_target_ref: null,
      checkout_sync_blocked: false,
      checkout_sync_message: null,
    },
    revision_history: [],
    jobs: [],
    findings: [],
    workspaces: [],
    convergences: [],
    revision_context_summary: null,
    diagnostics: [],
  }
}

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
        <MemoryRouter initialEntries={['/projects/prj_1/items/itm_1']}>
          <Routes>
            <Route path="/projects/:projectId/items/:itemId" element={<ItemDetailPage />} />
          </Routes>
        </MemoryRouter>
      </TooltipProvider>
    </QueryClientProvider>,
  )
}

describe('ItemDetailPage', () => {
  afterEach(() => {
    vi.restoreAllMocks()
  })

  it('renders through the loading-to-loaded transition without a hook order error', async () => {
    vi.spyOn(globalThis, 'fetch').mockImplementation((input) => {
      const url = String(input)
      if (url.endsWith('/api/agents')) {
        return Promise.resolve(jsonResponse([]))
      }
      if (url.endsWith('/api/projects/prj_1/items/itm_1')) {
        return Promise.resolve(jsonResponse(makeItemDetail()))
      }
      throw new Error(`Unexpected fetch: ${url}`)
    })

    renderPage()

    expect(document.querySelector('[data-slot="skeleton"]')).toBeInTheDocument()
    expect(await screen.findByRole('heading', { name: 'Ship the feature' })).toBeInTheDocument()
    const breadcrumb = screen.getByRole('navigation', { name: 'Breadcrumb' })
    expect(breadcrumb).toBeInTheDocument()
    expect(screen.getByRole('link', { name: 'Board' })).toHaveAttribute('href', '/projects/prj_1/board')
    expect(within(breadcrumb).getByText('Ship the feature')).toHaveAttribute('aria-current', 'page')
    expect(screen.getByRole('button', { name: 'Dispatch author_initial' })).toBeInTheDocument()
  })

  it('renders query failures in a destructive alert', async () => {
    vi.spyOn(globalThis, 'fetch').mockImplementation((input) => {
      const url = String(input)
      if (url.endsWith('/api/agents')) {
        return Promise.resolve(jsonResponse([]))
      }
      if (url.endsWith('/api/projects/prj_1/items/itm_1')) {
        return Promise.reject(new Error('network down'))
      }
      throw new Error(`Unexpected fetch: ${url}`)
    })

    renderPage()

    expect(await screen.findByText('Item detail failed to load')).toBeInTheDocument()
    expect(screen.getByText('Error: network down')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Retry' })).toBeInTheDocument()
  })

  it('explains when a queued job is blocked by missing agents', async () => {
    const detail = makeItemDetail()
    detail.jobs = [
      {
        id: 'job_1',
        project_id: 'prj_1',
        item_id: 'itm_1',
        item_revision_id: 'rev_1',
        step_id: 'author_initial',
        status: 'queued',
        outcome_class: null,
        phase_kind: 'author',
        workspace_id: 'wrk_1',
        job_input: { kind: 'authoring_head', head_commit_oid: '0123456789abcdef' },
        created_at: '2026-03-11T00:00:00Z',
        started_at: null,
        ended_at: null,
      },
    ]

    vi.spyOn(globalThis, 'fetch').mockImplementation((input) => {
      const url = String(input)
      if (url.endsWith('/api/agents')) {
        return Promise.resolve(jsonResponse([]))
      }
      if (url.endsWith('/api/projects/prj_1/items/itm_1')) {
        return Promise.resolve(jsonResponse(detail))
      }
      throw new Error(`Unexpected fetch: ${url}`)
    })

    renderPage()

    expect(await screen.findByText(/Queued jobs are waiting because no agents are configured/i)).toBeInTheDocument()
    expect(screen.getByRole('link', { name: 'Open Config' })).toHaveAttribute('href', '/projects/prj_1/config')
  })

  it('shows agent availability loading while queued-job blocker context is still resolving', async () => {
    const detail = makeItemDetail()
    detail.jobs = [
      {
        id: 'job_1',
        project_id: 'prj_1',
        item_id: 'itm_1',
        item_revision_id: 'rev_1',
        step_id: 'author_initial',
        status: 'queued',
        outcome_class: null,
        phase_kind: 'author',
        workspace_id: 'wrk_1',
        job_input: { kind: 'authoring_head', head_commit_oid: '0123456789abcdef' },
        created_at: '2026-03-11T00:00:00Z',
        started_at: null,
        ended_at: null,
      },
    ]

    let resolveAgents: (value: Response) => void = () => {
      throw new Error('Expected agents request to stay pending')
    }

    vi.spyOn(globalThis, 'fetch').mockImplementation((input) => {
      const url = String(input)
      if (url.endsWith('/api/agents')) {
        return new Promise<Response>((resolve) => {
          resolveAgents = resolve
        })
      }
      if (url.endsWith('/api/projects/prj_1/items/itm_1')) {
        return Promise.resolve(jsonResponse(detail))
      }
      throw new Error(`Unexpected fetch: ${url}`)
    })

    renderPage()

    expect(await screen.findByRole('heading', { name: 'Ship the feature' })).toBeInTheDocument()
    expect(screen.getByText('Checking agent availability…')).toBeInTheDocument()

    resolveAgents(jsonResponse([]))

    await waitFor(() => {
      expect(screen.queryByText('Checking agent availability…')).not.toBeInTheDocument()
    })
  })

  it('opens a confirmation dialog before rejecting approval', async () => {
    const detail = makeItemDetail()
    detail.evaluation.allowed_actions = ['approval_reject']
    detail.evaluation.dispatchable_step_id = null
    detail.evaluation.next_recommended_action = 'awaiting_operator'

    const fetchSpy = vi.spyOn(globalThis, 'fetch').mockImplementation((input, init) => {
      const url = String(input)
      const method = init?.method ?? 'GET'

      if (method === 'GET' && url.endsWith('/api/agents')) {
        return Promise.resolve(jsonResponse([]))
      }
      if (method === 'GET' && url.endsWith('/api/projects/prj_1/items/itm_1')) {
        return Promise.resolve(jsonResponse(detail))
      }
      if (method === 'POST' && url.endsWith('/api/projects/prj_1/items/itm_1/approval/reject')) {
        return Promise.resolve(jsonResponse(detail))
      }
      throw new Error(`Unexpected fetch: ${method} ${url}`)
    })

    renderPage()

    fireEvent.click(await screen.findByRole('button', { name: 'Reject approval' }))

    const dialog = await screen.findByRole('alertdialog', { name: 'Reject approval?' })
    expect(dialog).toHaveTextContent('sends the item back for rework')
    expect(fetchSpy).toHaveBeenCalledTimes(2)

    fireEvent.click(within(dialog).getByRole('button', { name: 'Reject approval' }))

    await waitFor(() => {
      expect(fetchSpy).toHaveBeenCalledWith(
        '/api/projects/prj_1/items/itm_1/approval/reject',
        expect.objectContaining({ method: 'POST' }),
      )
    })
  })

  it('opens a confirmation dialog before cancelling an active job', async () => {
    const detail = makeItemDetail()
    detail.jobs = [
      {
        id: 'job_1',
        project_id: 'prj_1',
        item_id: 'itm_1',
        item_revision_id: 'rev_1',
        step_id: 'author_initial',
        status: 'running',
        outcome_class: null,
        phase_kind: 'author',
        workspace_id: 'wrk_1',
        job_input: { kind: 'authoring_head', head_commit_oid: '0123456789abcdef' },
        created_at: '2026-03-11T00:00:00Z',
        started_at: '2026-03-11T00:01:00Z',
        ended_at: null,
      },
    ]

    const fetchSpy = vi.spyOn(globalThis, 'fetch').mockImplementation((input, init) => {
      const url = String(input)
      const method = init?.method ?? 'GET'

      if (method === 'GET' && url.endsWith('/api/agents')) {
        return Promise.resolve(jsonResponse([]))
      }
      if (method === 'GET' && url.endsWith('/api/projects/prj_1/items/itm_1')) {
        return Promise.resolve(jsonResponse(detail))
      }
      if (method === 'POST' && url.endsWith('/api/projects/prj_1/items/itm_1/jobs/job_1/cancel')) {
        return Promise.resolve(jsonResponse({}))
      }
      throw new Error(`Unexpected fetch: ${method} ${url}`)
    })

    renderPage()

    fireEvent.click(await screen.findByRole('button', { name: 'Cancel' }))

    const dialog = await screen.findByRole('alertdialog', { name: 'Cancel job?' })
    expect(dialog).toHaveTextContent('job_1')
    expect(dialog).toHaveTextContent('itm_1')
    expect(fetchSpy).toHaveBeenCalledTimes(2)

    fireEvent.click(within(dialog).getByRole('button', { name: 'Cancel job' }))

    await waitFor(() => {
      expect(fetchSpy).toHaveBeenCalledWith(
        '/api/projects/prj_1/items/itm_1/jobs/job_1/cancel',
        expect.objectContaining({ method: 'POST' }),
      )
    })
  })

  it('renders the extracted item detail sections when data is present', async () => {
    const detail = makeItemDetail()
    detail.findings = [
      {
        id: 'fnd_1',
        project_id: 'prj_1',
        source_item_id: 'itm_1',
        source_item_revision_id: 'rev_1',
        source_job_id: 'job_1',
        source_step_id: 'review_initial',
        source_report_schema_version: 'finding:v1',
        source_finding_key: 'fnd-key',
        source_subject_kind: 'candidate',
        source_subject_base_commit_oid: 'fedcba9876543210',
        source_subject_head_commit_oid: '0123456789abcdef',
        code: 'missing-test',
        severity: 'high',
        summary: 'Missing regression coverage',
        paths: ['src/lib.rs'],
        evidence: null,
        triage_state: 'untriaged',
        linked_item_id: null,
        triage_note: null,
        created_at: '2026-03-11T00:00:00Z',
        triaged_at: null,
      },
    ]
    detail.jobs = [
      {
        id: 'job_1',
        project_id: 'prj_1',
        item_id: 'itm_1',
        item_revision_id: 'rev_1',
        step_id: 'review_initial',
        status: 'failed',
        outcome_class: 'terminal_failure',
        phase_kind: 'review',
        workspace_id: 'wrk_1',
        job_input: {
          kind: 'candidate_subject',
          base_commit_oid: '0123456789abcdef',
          head_commit_oid: 'fedcba9876543210',
        },
        created_at: '2026-03-11T00:00:00Z',
        started_at: '2026-03-11T00:01:00Z',
        ended_at: '2026-03-11T00:02:00Z',
      },
    ]
    detail.convergences = [
      {
        id: 'cnv_1',
        status: 'prepared',
        input_target_commit_oid: 'aaaaaaaa11111111',
        prepared_commit_oid: 'bbbbbbbb22222222',
        final_target_commit_oid: 'cccccccc33333333',
        target_head_valid: true,
      },
    ]
    detail.revision_context_summary = {
      updated_at: '2026-03-12T00:00:00Z',
      changed_paths: ['src/lib.rs', 'src/main.rs'],
      latest_validation: {
        job_id: 'job_2',
        schema_version: 'validation:v1',
        outcome: 'clean',
        summary: 'Validation passed',
      },
      latest_review: {
        job_id: 'job_3',
        schema_version: 'review:v1',
        outcome: 'findings',
        summary: 'Review found one issue',
      },
      accepted_result_refs: [
        {
          job_id: 'job_2',
          step_id: 'validate_initial',
          schema_version: 'validation:v1',
          outcome: 'clean',
          summary: 'Validation passed',
        },
      ],
      operator_notes_excerpt: 'Watch retry budget',
    }
    detail.diagnostics = ['detail diagnostic']

    vi.spyOn(globalThis, 'fetch').mockImplementation((input) => {
      const url = String(input)
      if (url.endsWith('/api/agents')) {
        return Promise.resolve(jsonResponse([]))
      }
      if (url.endsWith('/api/projects/prj_1/items/itm_1')) {
        return Promise.resolve(jsonResponse(detail))
      }
      throw new Error(`Unexpected fetch: ${url}`)
    })

    renderPage()

    expect(await screen.findByRole('heading', { name: 'Ship the feature' })).toBeInTheDocument()
    // Section nav buttons
    expect(screen.getByRole('button', { name: /Jobs\s*1/ })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /Findings\s*1/ })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /Convergences\s*1/ })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /Revision Context\s*1/ })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /Diagnostics\s*1/ })).toBeInTheDocument()
    expect(screen.getByText('Mar 12, 2026, 12:00 AM UTC')).toBeInTheDocument()
    expect(screen.getByText('Missing regression coverage')).toBeInTheDocument()
    expect(screen.getByText('validate_initial:clean')).toBeInTheDocument()
    expect(screen.getByText('detail diagnostic')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Copy acceptance criteria' })).toBeInTheDocument()
  })

  it('keeps triage controls visible for already-triaged findings so operators can revise them', async () => {
    const detail = makeItemDetail()
    detail.findings = [
      {
        id: 'fnd_1',
        project_id: 'prj_1',
        source_item_id: 'itm_1',
        source_item_revision_id: 'rev_1',
        source_job_id: 'job_1',
        source_step_id: 'review_candidate_initial',
        source_report_schema_version: 'review_report:v1',
        source_finding_key: 'finding-1',
        source_subject_kind: 'candidate',
        source_subject_base_commit_oid: 'base',
        source_subject_head_commit_oid: 'head',
        code: 'BUG001',
        severity: 'high',
        summary: 'Need a decision',
        paths: ['src/lib.rs'],
        evidence: null,
        triage_state: 'wont_fix',
        linked_item_id: null,
        triage_note: 'accepted',
        created_at: '2026-03-11T00:00:00Z',
        triaged_at: '2026-03-11T00:01:00Z',
      },
    ]

    vi.spyOn(globalThis, 'fetch').mockImplementation((input) => {
      const url = String(input)
      if (url.endsWith('/api/agents')) {
        return Promise.resolve(jsonResponse([]))
      }
      if (url.endsWith('/api/projects/prj_1/items/itm_1')) {
        return Promise.resolve(jsonResponse(detail))
      }
      throw new Error(`Unexpected fetch: ${url}`)
    })

    renderPage()

    expect(await screen.findByDisplayValue('wont_fix')).toBeInTheDocument()
    expect(screen.getByDisplayValue('accepted')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Apply' })).toBeInTheDocument()
  })

  it('throws before fetching when a required route param is missing', () => {
    const fetchSpy = vi.spyOn(globalThis, 'fetch')
    vi.spyOn(console, 'error').mockImplementation(() => {})
    const queryClient = new QueryClient({
      defaultOptions: {
        queries: {
          retry: false,
        },
      },
    })

    expect(() =>
      render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={['/projects/prj_1']}>
            <Routes>
              <Route path="/projects/:projectId" element={<ItemDetailPage />} />
            </Routes>
          </MemoryRouter>
        </QueryClientProvider>,
      ),
    ).toThrow('Missing required route param: itemId')

    expect(fetchSpy).not.toHaveBeenCalled()
  })
})
