import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { fireEvent, render, screen, within } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router'
import ProjectsPage from '../pages/ProjectsPage'

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
      <MemoryRouter initialEntries={['/']}>
        <Routes>
          <Route path="/" element={<ProjectsPage />} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>,
  )
}

describe('ProjectsPage', () => {
  afterEach(() => {
    vi.restoreAllMocks()
  })

  it('opens the registration dialog and renders the linked project list', async () => {
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

    expect(await screen.findByRole('button', { name: 'Register project' })).toBeInTheDocument()
    expect(await screen.findByRole('link', { name: /Ingot/i })).toHaveAttribute('href', '/projects/prj_1')
    expect(screen.getByText('main')).toBeInTheDocument()

    fireEvent.click(screen.getByRole('button', { name: 'Register project' }))

    expect(await screen.findByRole('dialog')).toBeInTheDocument()
    expect(screen.getByText('Register Project')).toBeInTheDocument()
    expect(screen.getByLabelText('Repository path')).toBeInTheDocument()
  })

  it('shows a required-field message when the repository path is missing', async () => {
    vi.spyOn(globalThis, 'fetch').mockImplementation((input) => {
      const url = String(input)
      if (url.endsWith('/api/projects')) {
        return Promise.resolve(jsonResponse([]))
      }
      throw new Error(`Unexpected fetch: ${url}`)
    })

    renderPage()

    fireEvent.click(await screen.findByRole('button', { name: 'Register project' }))
    const dialog = await screen.findByRole('dialog')
    fireEvent.click(within(dialog).getByRole('button', { name: 'Register project' }))

    expect(await screen.findByText('Repository path is required.')).toBeInTheDocument()
  })

  it('renders a destructive alert when the projects query fails', async () => {
    vi.spyOn(globalThis, 'fetch').mockImplementation((input) => {
      const url = String(input)
      if (url.endsWith('/api/projects')) {
        return Promise.reject(new Error('network down'))
      }
      throw new Error(`Unexpected fetch: ${url}`)
    })

    renderPage()

    expect(await screen.findByText('Projects failed to load')).toBeInTheDocument()
    expect(screen.getByText('Error: network down')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Retry' })).toBeInTheDocument()
  })
})
