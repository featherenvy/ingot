import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { render, screen } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router'
import ProjectLayout from '../layouts/ProjectLayout'
import RootLayout from '../layouts/RootLayout'
import { useConnectionStore } from '../stores/connection'

class MockWebSocket {
  static CONNECTING = 0
  static OPEN = 1
  static CLOSING = 2
  static CLOSED = 3

  readyState = MockWebSocket.CONNECTING
  onopen: (() => void) | null = null
  onmessage: ((event: MessageEvent) => void) | null = null
  onclose: (() => void) | null = null
  onerror: (() => void) | null = null

  close() {
    this.readyState = MockWebSocket.CLOSED
    this.onclose?.()
  }
}

function jsonResponse(body: unknown) {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { 'Content-Type': 'application/json' },
  })
}

const PROJECTS = [
  {
    id: 'prj_1',
    name: 'Ingot',
    path: '/tmp/ingot',
    default_branch: 'main',
    color: '#1f2937',
  },
]

function renderWithShell(initialEntries: string[]) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })

  return render(
    <QueryClientProvider client={queryClient}>
      <MemoryRouter initialEntries={initialEntries}>
        <Routes>
          <Route element={<RootLayout />}>
            <Route path="/projects/:projectId" element={<ProjectLayout />}>
              <Route index element={<div>Dashboard content</div>} />
              <Route path="jobs" element={<div>Jobs content</div>} />
              <Route path="board" element={<div>Board content</div>} />
              <Route path="items/:itemId" element={<div>Item content</div>} />
            </Route>
          </Route>
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>,
  )
}

describe('ProjectLayout with topbar nav', () => {
  beforeEach(() => {
    vi.stubGlobal('WebSocket', MockWebSocket)
  })

  afterEach(() => {
    useConnectionStore.setState({
      status: 'disconnected',
      lastSeq: 0,
      ws: null,
    })
    vi.restoreAllMocks()
    vi.unstubAllGlobals()
  })

  it('renders project navigation links in the topbar', async () => {
    vi.spyOn(globalThis, 'fetch').mockImplementation((input) => {
      const url = String(input)
      if (url.endsWith('/api/projects')) return Promise.resolve(jsonResponse(PROJECTS))
      if (url.endsWith('/api/health')) return Promise.resolve(new Response('ok', { status: 200 }))
      throw new Error(`Unexpected fetch: ${url}`)
    })

    renderWithShell(['/projects/prj_1/jobs'])

    expect(await screen.findByText('Jobs content')).toBeInTheDocument()

    const nav = screen.getByRole('navigation', { name: 'Project navigation' })
    expect(nav).toBeInTheDocument()
    expect(nav.querySelector('a[href="/projects/prj_1/jobs"]')).toHaveTextContent('Jobs')
    expect(nav.querySelector('a[href="/projects/prj_1/board"]')).toHaveTextContent('Board')

    const projectSwitcher = screen.getByRole('combobox', { name: 'Switch project' })
    expect(projectSwitcher).toHaveAttribute('aria-controls')
  })

  it('renders page content for item detail route', async () => {
    vi.spyOn(globalThis, 'fetch').mockImplementation((input) => {
      const url = String(input)
      if (url.endsWith('/api/projects')) return Promise.resolve(jsonResponse(PROJECTS))
      if (url.endsWith('/api/health')) return Promise.resolve(new Response('ok', { status: 200 }))
      throw new Error(`Unexpected fetch: ${url}`)
    })

    renderWithShell(['/projects/prj_1/items/itm_1'])

    expect(await screen.findByText('Item content')).toBeInTheDocument()
  })
})
