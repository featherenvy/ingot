import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { fireEvent, render, screen } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router'
import { Toaster } from '../components/ui/sonner'
import { TooltipProvider } from '../components/ui/tooltip'
import ConfigPage from '../pages/ConfigPage'

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
        <MemoryRouter initialEntries={['/projects/prj_1/config']}>
          <Routes>
            <Route path="/projects/:projectId/config" element={<ConfigPage />} />
          </Routes>
        </MemoryRouter>
        <Toaster />
      </TooltipProvider>
    </QueryClientProvider>,
  )
}

describe('ConfigPage', () => {
  afterEach(() => {
    vi.restoreAllMocks()
  })

  it('opens the registration dialog with the provider select and renders the agents table', async () => {
    vi.spyOn(globalThis, 'fetch').mockImplementation((input) => {
      const url = String(input)
      if (url.endsWith('/api/agents')) {
        return Promise.resolve(
          jsonResponse([
            {
              id: 'agt_1',
              slug: 'codex',
              name: 'Codex CLI',
              adapter_kind: 'codex',
              provider: 'openai',
              model: 'gpt-5-codex',
              cli_path: 'codex',
              capabilities: [],
              health_check: 'ok',
              status: 'available',
            },
          ]),
        )
      }
      if (url.endsWith('/api/projects/prj_1/config')) {
        return Promise.resolve(jsonResponse({ branch: 'main', sandbox: 'enabled' }))
      }
      throw new Error(`Unexpected fetch: ${url}`)
    })

    renderPage()

    expect(await screen.findByText('Project Defaults')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Register Codex agent' })).toBeInTheDocument()
    expect(await screen.findByRole('button', { name: 'Reprobe' })).toBeInTheDocument()
    expect(screen.getByText('available')).toBeInTheDocument()

    fireEvent.click(screen.getByRole('button', { name: 'Register Codex agent' }))

    expect(await screen.findByRole('dialog')).toBeInTheDocument()
    expect(screen.getByLabelText('Agent name')).toHaveValue('Codex CLI')
    expect(screen.getByRole('combobox', { name: 'Provider' })).toHaveTextContent('openai')
    expect(screen.getByText('openai', { selector: 'option' })).toBeInTheDocument()
    expect(screen.getByText('anthropic', { selector: 'option' })).toBeInTheDocument()
  })

  it('only disables the clicked agent row while reprobe is pending', async () => {
    let resolveReprobe: (value: Response) => void = () => {
      throw new Error('Expected reprobe request to be pending')
    }

    vi.spyOn(globalThis, 'fetch').mockImplementation((input, init) => {
      const url = String(input)
      const method = init?.method ?? 'GET'

      if (method === 'GET' && url.endsWith('/api/agents')) {
        return Promise.resolve(
          jsonResponse([
            {
              id: 'agt_1',
              slug: 'codex-a',
              name: 'Codex A',
              adapter_kind: 'codex',
              provider: 'openai',
              model: 'gpt-5-codex',
              cli_path: 'codex',
              capabilities: [],
              health_check: 'ok',
              status: 'available',
            },
            {
              id: 'agt_2',
              slug: 'codex-b',
              name: 'Codex B',
              adapter_kind: 'codex',
              provider: 'openai',
              model: 'gpt-5-codex',
              cli_path: 'codex',
              capabilities: [],
              health_check: 'ok',
              status: 'available',
            },
          ]),
        )
      }

      if (method === 'GET' && url.endsWith('/api/projects/prj_1/config')) {
        return Promise.resolve(jsonResponse({ branch: 'main', sandbox: 'enabled' }))
      }

      if (method === 'POST' && url.endsWith('/api/agents/agt_1/reprobe')) {
        return new Promise<Response>((resolve) => {
          resolveReprobe = resolve
        })
      }

      throw new Error(`Unexpected fetch: ${method} ${url}`)
    })

    renderPage()

    const reprobeButtons = await screen.findAllByRole('button', { name: 'Reprobe' })

    fireEvent.click(reprobeButtons[0])

    expect(await screen.findByRole('button', { name: 'Reprobing…' })).toBeDisabled()
    expect(screen.getByRole('button', { name: 'Reprobe' })).toBeEnabled()

    resolveReprobe(
      jsonResponse({
        id: 'agt_1',
        slug: 'codex-a',
        name: 'Codex A',
        adapter_kind: 'codex',
        provider: 'openai',
        model: 'gpt-5-codex',
        cli_path: 'codex',
        capabilities: [],
        health_check: 'ok',
        status: 'available',
      }),
    )

    expect(await screen.findByText('Reprobe complete for Codex A.')).toBeInTheDocument()
  })

  it('renders reprobe failures in an alert', async () => {
    vi.spyOn(globalThis, 'fetch').mockImplementation((input, init) => {
      const url = String(input)
      const method = init?.method ?? 'GET'

      if (method === 'GET' && url.endsWith('/api/agents')) {
        return Promise.resolve(
          jsonResponse([
            {
              id: 'agt_1',
              slug: 'codex-a',
              name: 'Codex A',
              adapter_kind: 'codex',
              provider: 'openai',
              model: 'gpt-5-codex',
              cli_path: 'codex',
              capabilities: [],
              health_check: 'ok',
              status: 'available',
            },
          ]),
        )
      }

      if (method === 'GET' && url.endsWith('/api/projects/prj_1/config')) {
        return Promise.resolve(jsonResponse({ branch: 'main', sandbox: 'enabled' }))
      }

      if (method === 'POST' && url.endsWith('/api/agents/agt_1/reprobe')) {
        return Promise.resolve(
          new Response(JSON.stringify({ error: { code: 'probe_failed', message: 'Probe command failed.' } }), {
            status: 500,
            headers: {
              'Content-Type': 'application/json',
            },
          }),
        )
      }

      throw new Error(`Unexpected fetch: ${method} ${url}`)
    })

    renderPage()

    fireEvent.click(await screen.findByRole('button', { name: 'Reprobe' }))

    expect(await screen.findByText('Reprobe failed')).toBeInTheDocument()
    expect(screen.getByText('Probe command failed.')).toBeInTheDocument()
  })
})
