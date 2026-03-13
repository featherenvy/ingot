import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { render, screen } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router'
import { TooltipProvider } from '../components/ui/tooltip'
import JobsPage from '../pages/JobsPage'
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
  afterEach(() => {
    vi.restoreAllMocks()
  })

  it('renders started timestamps in a readable format', async () => {
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
      throw new Error(`Unexpected fetch: ${url}`)
    })

    renderPage()

    const formattedTime = await screen.findByText('Mar 11, 2026, 12:01 AM UTC')
    expect(formattedTime.tagName).toBe('TIME')
    expect(formattedTime).toHaveAttribute('datetime', '2026-03-11T00:01:00Z')
    expect(formattedTime).not.toHaveAttribute('title')
    expect(screen.queryByText('2026-03-11T00:01:00Z')).not.toBeInTheDocument()
  })
})
