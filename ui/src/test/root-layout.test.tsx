import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { fireEvent, render, screen } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router'
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

function makeQueryClient() {
  return new QueryClient({
    defaultOptions: {
      queries: {
        retry: false,
      },
    },
  })
}

function ExplodingPage(): never {
  throw new Error('Exploded during render')
}

describe('RootLayout error boundary', () => {
  beforeEach(() => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response('ok', {
        status: 200,
      }),
    )
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

  it('shows a fallback instead of crashing when a routed page throws', async () => {
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {})

    render(
      <QueryClientProvider client={makeQueryClient()}>
        <MemoryRouter initialEntries={['/boom']}>
          <Routes>
            <Route element={<RootLayout />}>
              <Route index element={<div>Projects</div>} />
              <Route path="/boom" element={<ExplodingPage />} />
            </Route>
          </Routes>
        </MemoryRouter>
      </QueryClientProvider>,
    )

    expect(await screen.findByRole('alert')).toHaveTextContent('Something went wrong')
    expect(screen.getByText('Exploded during render')).toBeInTheDocument()
    expect(screen.getByText('Ingot')).toBeInTheDocument()
    expect(consoleError).toHaveBeenCalled()
  })

  it('recovers on navigation after rendering the fallback', async () => {
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {})

    render(
      <QueryClientProvider client={makeQueryClient()}>
        <MemoryRouter initialEntries={['/boom']}>
          <Routes>
            <Route element={<RootLayout />}>
              <Route index element={<div>Projects</div>} />
              <Route path="/boom" element={<ExplodingPage />} />
            </Route>
          </Routes>
        </MemoryRouter>
      </QueryClientProvider>,
    )

    expect(await screen.findByRole('alert')).toBeInTheDocument()

    fireEvent.click(screen.getByRole('link', { name: 'Back to projects' }))

    expect(await screen.findByText('Projects')).toBeInTheDocument()
    expect(screen.queryByRole('alert')).not.toBeInTheDocument()
    expect(consoleError).toHaveBeenCalled()
  })
})
