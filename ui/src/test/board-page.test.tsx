import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { fireEvent, render, screen } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router'
import BoardPage from '../pages/BoardPage'

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
      <MemoryRouter initialEntries={['/projects/prj_1/board']}>
        <Routes>
          <Route path="/projects/:projectId/board" element={<BoardPage />} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>,
  )
}

describe('BoardPage', () => {
  afterEach(() => {
    vi.restoreAllMocks()
  })

  it('opens the new item form in a sheet instead of rendering it inline by default', async () => {
    vi.spyOn(globalThis, 'fetch').mockImplementation((input) => {
      const url = String(input)
      if (url.endsWith('/api/projects/prj_1/items')) {
        return Promise.resolve(jsonResponse([]))
      }
      throw new Error(`Unexpected fetch: ${url}`)
    })

    renderPage()

    expect(await screen.findByText('Board')).toBeInTheDocument()
    expect(screen.queryByLabelText('Title')).not.toBeInTheDocument()

    fireEvent.click(screen.getByRole('button', { name: 'New item' }))

    expect(await screen.findByRole('dialog')).toBeInTheDocument()
    expect(screen.getByText('Create Item')).toBeInTheDocument()
    expect(screen.getByLabelText('Title')).toBeInTheDocument()
  })

  it('uses the inline empty state when a lane has no items', async () => {
    vi.spyOn(globalThis, 'fetch').mockImplementation((input) => {
      const url = String(input)
      if (url.endsWith('/api/projects/prj_1/items')) {
        return Promise.resolve(jsonResponse([]))
      }
      throw new Error(`Unexpected fetch: ${url}`)
    })

    renderPage()

    expect(await screen.findByText('Board')).toBeInTheDocument()
    expect(screen.getAllByText('No items in this lane.')).toHaveLength(4)
  })

  it('shows field-level validation messages when submitting an empty item form', async () => {
    vi.spyOn(globalThis, 'fetch').mockImplementation((input) => {
      const url = String(input)
      if (url.endsWith('/api/projects/prj_1/items')) {
        return Promise.resolve(jsonResponse([]))
      }
      throw new Error(`Unexpected fetch: ${url}`)
    })

    renderPage()

    fireEvent.click(await screen.findByRole('button', { name: 'New item' }))
    fireEvent.click(screen.getByRole('button', { name: 'Create item' }))

    expect(await screen.findByText('Title is required.')).toBeInTheDocument()
    expect(screen.getByText('Description is required.')).toBeInTheDocument()
    expect(screen.getByText('Acceptance criteria are required.')).toBeInTheDocument()
  })

  it('renders a destructive alert when the board query fails', async () => {
    vi.spyOn(globalThis, 'fetch').mockImplementation((input) => {
      const url = String(input)
      if (url.endsWith('/api/projects/prj_1/items')) {
        return Promise.reject(new Error('network down'))
      }
      throw new Error(`Unexpected fetch: ${url}`)
    })

    renderPage()

    expect(await screen.findByText('Board failed to load')).toBeInTheDocument()
    expect(screen.getByText('Error: network down')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Retry' })).toBeInTheDocument()
  })

  it('renders awaiting checkout sync items in the working lane', async () => {
    vi.spyOn(globalThis, 'fetch').mockImplementation((input) => {
      const url = String(input)
      if (url.endsWith('/api/projects/prj_1/items')) {
        return Promise.resolve(
          jsonResponse([
            {
              title: 'Blocked finalization',
              item: {
                id: 'itm_1',
                sort_key: '2026-03-11T00:00:00Z#itm_1',
                project_id: 'prj_1',
                classification: 'change',
                workflow_version: 'delivery:v1',
                lifecycle_state: 'open',
                parking_state: 'active',
                approval_state: 'not_required',
                escalation_state: 'operator_required',
                escalation_reason: 'checkout_sync_blocked',
                current_revision_id: 'rev_1',
                origin_kind: 'manual',
                priority: 'major',
                labels: [],
                operator_notes: null,
                created_at: '2026-03-11T00:00:00Z',
                updated_at: '2026-03-11T00:10:00Z',
              },
              evaluation: {
                board_status: 'WORKING',
                attention_badges: ['escalated'],
                current_step_id: 'prepare_convergence',
                current_phase_kind: null,
                phase_status: 'awaiting_checkout_sync',
                next_recommended_action: 'resolve_checkout_sync',
                dispatchable_step_id: null,
                auxiliary_dispatchable_step_ids: [],
                allowed_actions: [],
                terminal_readiness: false,
                diagnostics: [],
              },
              finalization: {
                phase: 'target_ref_advanced',
                checkout_adoption_state: 'blocked',
                checkout_adoption_message: 'Registered checkout is blocked',
                final_target_commit_oid: 'abcdef1234567890',
                finalize_operation_unresolved: true,
              },
              queue: {
                state: 'released',
                position: null,
                lane_owner_item_id: null,
                lane_target_ref: 'refs/heads/main',
              },
            },
          ]),
        )
      }
      throw new Error(`Unexpected fetch: ${url}`)
    })

    renderPage()

    expect(await screen.findByText('Blocked finalization')).toBeInTheDocument()
    expect(screen.getByText('Awaiting Checkout Sync')).toBeInTheDocument()
  })
})
