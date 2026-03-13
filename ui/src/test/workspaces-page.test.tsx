import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { fireEvent, render, screen } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router'
import { Toaster } from '../components/ui/sonner'
import { TooltipProvider } from '../components/ui/tooltip'
import WorkspacesPage from '../pages/WorkspacesPage'

function jsonResponse(body: unknown) {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: {
      'Content-Type': 'application/json',
    },
  })
}

const workspace = {
  id: 'wrk_1',
  project_id: 'prj_1',
  kind: 'author',
  status: 'ready',
  target_ref: 'main',
  base_commit_oid: '0123456789abcdef',
  head_commit_oid: 'fedcba9876543210',
  created_at: '2026-03-11T00:00:00Z',
  updated_at: '2026-03-11T00:00:00Z',
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
        <MemoryRouter initialEntries={['/projects/prj_1/workspaces']}>
          <Routes>
            <Route path="/projects/:projectId/workspaces" element={<WorkspacesPage />} />
          </Routes>
        </MemoryRouter>
        <Toaster />
      </TooltipProvider>
    </QueryClientProvider>,
  )
}

describe('WorkspacesPage', () => {
  afterEach(() => {
    vi.restoreAllMocks()
  })

  it('opens a confirmation dialog before resetting a workspace', async () => {
    vi.spyOn(globalThis, 'fetch').mockImplementation((input, init) => {
      const url = String(input)
      const method = init?.method ?? 'GET'

      if (method === 'GET' && url.endsWith('/api/projects/prj_1/workspaces')) {
        return Promise.resolve(jsonResponse([workspace]))
      }

      if (method === 'GET' && url.endsWith('/api/projects/prj_1/items')) {
        return Promise.resolve(jsonResponse([]))
      }

      if (method === 'POST' && url.endsWith('/api/projects/prj_1/workspaces/wrk_1/reset')) {
        return Promise.resolve(jsonResponse(workspace))
      }

      throw new Error(`Unexpected fetch: ${method} ${url}`)
    })

    renderPage()

    fireEvent.pointerDown(await screen.findByRole('button', { name: 'Actions for workspace wrk_1' }))
    fireEvent.click(await screen.findByRole('menuitem', { name: 'Reset' }))

    const dialog = await screen.findByRole('alertdialog', { name: 'Reset workspace?' })
    expect(dialog).toHaveTextContent('wrk_1')
    expect(dialog).toHaveTextContent('main')

    fireEvent.click(screen.getByRole('button', { name: 'Reset' }))

    expect(await screen.findByText('Workspace reset.')).toBeInTheDocument()
  })
})
