import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { act, cleanup, render, screen } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router'
import { queryKeys } from '../api/queries'
import ProjectLayout from '../layouts/ProjectLayout'
import RootLayout from '../layouts/RootLayout'
import { useConnectionStore } from '../stores/connection'
import { useProjectsStore } from '../stores/projects'

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

const PROJECTS = [
  {
    id: 'prj_1',
    name: 'Ingot',
    path: '/tmp/ingot',
    default_branch: 'main',
    color: '#1f2937',
  },
]

async function renderWithShell(initialEntries: string[]) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  queryClient.setQueryData(queryKeys.projects(), PROJECTS)
  queryClient.setQueryData(queryKeys.health(), 'ok')

  let rendered: ReturnType<typeof render> | undefined
  await act(async () => {
    rendered = render(
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
    await Promise.resolve()
  })

  return rendered!
}

describe('ProjectLayout with topbar nav', () => {
  const originalConnect = useConnectionStore.getState().connect
  const originalSetActive = useProjectsStore.getState().setActive
  let connectSpy: (queryClient: QueryClient) => void
  let setActiveSpy: (id: string | null) => void
  let consoleErrorSpy: ReturnType<typeof vi.spyOn>

  beforeEach(() => {
    vi.stubGlobal('WebSocket', MockWebSocket)
    consoleErrorSpy = vi.spyOn(console, 'error').mockImplementation(() => {})
    connectSpy = vi.fn<(queryClient: QueryClient) => void>()
    setActiveSpy = vi.fn<(id: string | null) => void>()
    useConnectionStore.setState({
      status: 'disconnected',
      lastSeq: 0,
      ws: null,
      connect: connectSpy,
    })
    useProjectsStore.setState({
      activeProjectId: null,
      setActive: setActiveSpy,
    })
  })

  afterEach(async () => {
    await act(async () => {
      cleanup()
      useConnectionStore.setState({
        status: 'disconnected',
        lastSeq: 0,
        ws: null,
        connect: originalConnect,
      })
      useProjectsStore.setState({ activeProjectId: null, setActive: originalSetActive })
    })
    expect(consoleErrorSpy).not.toHaveBeenCalled()
    consoleErrorSpy.mockRestore()
    vi.restoreAllMocks()
    vi.unstubAllGlobals()
  })

  it('renders project navigation links in the topbar', async () => {
    await renderWithShell(['/projects/prj_1/jobs'])

    expect(await screen.findByText('Jobs content')).toBeInTheDocument()

    const nav = screen.getByRole('navigation', { name: 'Project navigation' })
    expect(nav).toBeInTheDocument()
    expect(nav.querySelector('a[href="/projects/prj_1/jobs"]')).toHaveTextContent('Jobs')
    expect(nav.querySelector('a[href="/projects/prj_1/board"]')).toHaveTextContent('Board')

    const projectSwitcher = screen.getByRole('combobox', { name: 'Switch project' })
    expect(projectSwitcher).toHaveAttribute('aria-controls')
    expect(connectSpy).toHaveBeenCalledTimes(1)
    expect(setActiveSpy).toHaveBeenCalledWith('prj_1')
  })

  it('renders page content for item detail route', async () => {
    await renderWithShell(['/projects/prj_1/items/itm_1'])

    expect(await screen.findByText('Item content')).toBeInTheDocument()
    expect(connectSpy).toHaveBeenCalledTimes(1)
    expect(setActiveSpy).toHaveBeenCalledWith('prj_1')
  })
})
