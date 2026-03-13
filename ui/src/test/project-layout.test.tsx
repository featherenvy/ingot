import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { render, screen } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router'
import ProjectLayout from '../layouts/ProjectLayout'

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
      <MemoryRouter initialEntries={['/projects/prj_1/jobs']}>
        <Routes>
          <Route path="/projects/:projectId" element={<ProjectLayout />}>
            <Route path="jobs" element={<div>Jobs content</div>} />
          </Route>
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>,
  )
}

describe('ProjectLayout', () => {
  afterEach(() => {
    vi.restoreAllMocks()
  })

  it('renders route-driven tabs with the current route marked active', async () => {
    vi.spyOn(globalThis, 'fetch').mockImplementation((input) => {
      const url = String(input)
      if (url.endsWith('/api/projects')) {
        return Promise.resolve(
          jsonResponse([
            {
              id: 'prj_1',
              name: 'Ingot',
              path: '/tmp/ingot',
              default_branch: 'main',
              color: '#1f2937',
            },
          ]),
        )
      }
      throw new Error(`Unexpected fetch: ${url}`)
    })

    renderPage()

    expect(await screen.findByText('Ingot')).toBeInTheDocument()
    expect(screen.getByRole('tablist')).toBeInTheDocument()
    expect(screen.getByRole('tab', { name: 'Jobs' })).toHaveAttribute('aria-selected', 'true')
    expect(screen.getByRole('tab', { name: 'Board' })).toHaveAttribute('aria-selected', 'false')
    expect(screen.getByText('Jobs content')).toBeInTheDocument()
  })
})
