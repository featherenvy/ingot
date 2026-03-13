import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { fireEvent, render, screen } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router'
import DashboardPage from '../pages/DashboardPage'

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
      <MemoryRouter initialEntries={['/projects/prj_1']}>
        <Routes>
          <Route path="/projects/:projectId" element={<DashboardPage />} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>,
  )
}

describe('DashboardPage', () => {
  afterEach(() => {
    vi.restoreAllMocks()
  })

  it('renders a destructive alert when the dashboard query fails', async () => {
    vi.spyOn(globalThis, 'fetch').mockImplementation((input) => {
      const url = String(input)

      if (url.endsWith('/api/projects/prj_1/items')) {
        return Promise.reject(new Error('network down'))
      }

      throw new Error(`Unexpected fetch: ${url}`)
    })

    renderPage()

    expect(await screen.findByText('Dashboard failed to load')).toBeInTheDocument()
    expect(screen.getByText('Error: network down')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Retry' })).toBeInTheDocument()
  })

  it('retries and recovers from an initial dashboard query failure', async () => {
    let shouldFail = true

    vi.spyOn(globalThis, 'fetch').mockImplementation((input) => {
      const url = String(input)

      if (url.endsWith('/api/projects/prj_1/items')) {
        if (shouldFail) {
          return Promise.reject(new Error('network down'))
        }

        return Promise.resolve(
          jsonResponse([
            {
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
              title: 'Ship it',
              evaluation: {
                board_status: 'INBOX',
                attention_badges: [],
                current_step_id: null,
                current_phase_kind: null,
                phase_status: null,
                next_recommended_action: 'dispatch',
                dispatchable_step_id: null,
                auxiliary_dispatchable_step_ids: [],
                allowed_actions: [],
                terminal_readiness: false,
                diagnostics: [],
              },
            },
          ]),
        )
      }

      throw new Error(`Unexpected fetch: ${url}`)
    })

    renderPage()

    expect(await screen.findByText('Dashboard failed to load')).toBeInTheDocument()

    shouldFail = false
    fireEvent.click(screen.getByRole('button', { name: 'Retry' }))

    expect(await screen.findByRole('heading', { name: 'Dashboard' })).toBeInTheDocument()
    expect(screen.getByText('item currently in this lane')).toBeInTheDocument()
  })
})
