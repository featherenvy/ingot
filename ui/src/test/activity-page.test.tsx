import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { fireEvent, render, screen } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router'
import { TooltipProvider } from '../components/ui/tooltip'
import ActivityPage from '../pages/ActivityPage'
import type { Activity } from '../types/domain'

function jsonResponse(body: unknown) {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: {
      'Content-Type': 'application/json',
    },
  })
}

function makeActivity(count: number, startAt = 0): Activity[] {
  return Array.from({ length: count }, (_, index) => {
    const n = startAt + index
    return {
      id: `act_${n}`,
      project_id: 'prj_1',
      event_type: 'item_updated',
      entity_type: 'item',
      entity_id: `entity-${n}`,
      payload: {
        index: n,
      },
      created_at: `2026-03-11T00:${String(n).padStart(2, '0')}:00Z`,
    }
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
        <MemoryRouter initialEntries={['/projects/prj_1/activity']}>
          <Routes>
            <Route path="/projects/:projectId/activity" element={<ActivityPage />} />
          </Routes>
        </MemoryRouter>
      </TooltipProvider>
    </QueryClientProvider>,
  )
}

describe('ActivityPage', () => {
  afterEach(() => {
    vi.restoreAllMocks()
  })

  it('requests activity in bounded pages and paginates older results', async () => {
    const fetchSpy = vi.spyOn(globalThis, 'fetch').mockImplementation((input) => {
      const url = new URL(String(input), 'http://localhost')

      if (
        url.pathname === '/api/projects/prj_1/activity' &&
        url.searchParams.get('limit') === '50' &&
        url.searchParams.get('offset') === '0'
      ) {
        return Promise.resolve(jsonResponse(makeActivity(50)))
      }

      if (
        url.pathname === '/api/projects/prj_1/activity' &&
        url.searchParams.get('limit') === '50' &&
        url.searchParams.get('offset') === '50'
      ) {
        return Promise.resolve(jsonResponse(makeActivity(5, 50)))
      }

      throw new Error(`Unexpected fetch: ${url.pathname}${url.search}`)
    })

    renderPage()

    expect(await screen.findByText('entity-0')).toBeInTheDocument()
    expect(screen.getByText('Mar 11, 2026, 12:00 AM UTC')).toBeInTheDocument()
    expect(screen.queryByText('2026-03-11T00:00:00Z')).not.toBeInTheDocument()
    expect(screen.getByText('entity-49')).toBeInTheDocument()
    expect(screen.queryByText('entity-50')).not.toBeInTheDocument()
    expect(
      screen.getByText('Showing events 1-50. Use pagination to inspect older activity without rendering the full log.'),
    ).toBeInTheDocument()

    fireEvent.click(screen.getByRole('button', { name: 'Older' }))

    expect(await screen.findByText('entity-50')).toBeInTheDocument()
    expect(screen.getByText('entity-54')).toBeInTheDocument()
    expect(screen.queryByText('entity-0')).not.toBeInTheDocument()
    expect(
      screen.getByText(
        'Showing events 51-55. Use pagination to inspect older activity without rendering the full log.',
      ),
    ).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Older' })).toBeDisabled()
    expect(fetchSpy).toHaveBeenCalledTimes(2)
  })

  it('uses collapsible disclosure semantics for long payloads', async () => {
    vi.spyOn(globalThis, 'fetch').mockImplementation((input) => {
      const url = new URL(String(input), 'http://localhost')

      if (url.pathname === '/api/projects/prj_1/activity') {
        return Promise.resolve(
          jsonResponse([
            {
              id: 'act_long',
              project_id: 'prj_1',
              event_type: 'item_updated',
              entity_type: 'item',
              entity_id: 'entity-long',
              payload: {
                summary: 'line one',
                detail: 'line two',
                extra: 'line three',
                tail: 'line four',
              },
              created_at: '2026-03-11T00:00:00Z',
            },
          ]),
        )
      }

      throw new Error(`Unexpected fetch: ${url.pathname}${url.search}`)
    })

    renderPage()

    expect(await screen.findByRole('button', { name: 'Copy payload' })).toBeInTheDocument()
    const toggle = await screen.findByRole('button', { name: 'Show more' })
    expect(toggle).toHaveAttribute('aria-expanded', 'false')

    fireEvent.click(toggle)

    expect(await screen.findByRole('button', { name: 'Show less' })).toHaveAttribute('aria-expanded', 'true')
  })

  it('renders a destructive alert when the activity query fails', async () => {
    vi.spyOn(globalThis, 'fetch').mockImplementation((input) => {
      const url = new URL(String(input), 'http://localhost')

      if (url.pathname === '/api/projects/prj_1/activity') {
        return Promise.reject(new Error('network down'))
      }

      throw new Error(`Unexpected fetch: ${url.pathname}${url.search}`)
    })

    renderPage()

    expect(await screen.findByText('Activity failed to load')).toBeInTheDocument()
    expect(screen.getByText('Error: network down')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Retry' })).toBeInTheDocument()
  })
})
