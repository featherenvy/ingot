import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router'
import { queryKeys } from '../api/queries'
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

  return {
    queryClient,
    ...render(
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
    ),
  }
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
    expect(await screen.findByRole('button', { name: 'Copy project defaults' })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Register Codex agent' })).toBeInTheDocument()
    expect(await screen.findByRole('button', { name: 'Reprobe' })).toBeInTheDocument()
    expect(screen.getByText('available')).toBeInTheDocument()

    fireEvent.click(screen.getByRole('button', { name: 'Register Codex agent' }))

    expect(await screen.findByRole('dialog')).toBeInTheDocument()
    expect(screen.getByLabelText('Agent name')).toHaveValue('Codex CLI')
    expect(screen.getByRole('combobox', { name: 'Provider' })).toHaveTextContent('openai')

    fireEvent.click(screen.getByRole('combobox', { name: 'Provider' }))

    const providerList = await screen.findByRole('listbox')
    expect(within(providerList).getByText('openai')).toBeInTheDocument()
    expect(within(providerList).getByText('anthropic')).toBeInTheDocument()
  })

  it('allows selecting a custom model value from the searchable combobox', async () => {
    vi.spyOn(globalThis, 'fetch').mockImplementation((input) => {
      const url = String(input)
      if (url.endsWith('/api/agents')) {
        return Promise.resolve(jsonResponse([]))
      }
      if (url.endsWith('/api/projects/prj_1/config')) {
        return Promise.resolve(jsonResponse({ branch: 'main', sandbox: 'enabled' }))
      }
      throw new Error(`Unexpected fetch: ${url}`)
    })

    renderPage()

    fireEvent.click(await screen.findByRole('button', { name: 'Register Codex agent' }))
    fireEvent.click(screen.getByRole('combobox', { name: 'Model' }))

    const searchInput = screen.getByPlaceholderText('Filter models...')
    fireEvent.change(searchInput, { target: { value: 'custom-model' } })

    fireEvent.click(await screen.findByText('Use "custom-model"'))

    expect(screen.getByRole('combobox', { name: 'Model' })).toHaveTextContent('custom-model')
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

  it('renders reprobe failures in a toast', async () => {
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

    expect(await screen.findByText('Reprobe failed.')).toBeInTheDocument()
    expect(screen.getByText('Probe command failed.')).toBeInTheDocument()
  })

  it('renders a destructive alert when a page-critical config query fails', async () => {
    vi.spyOn(globalThis, 'fetch').mockImplementation((input) => {
      const url = String(input)

      if (url.endsWith('/api/agents')) {
        return Promise.resolve(jsonResponse([]))
      }

      if (url.endsWith('/api/projects/prj_1/config')) {
        return Promise.reject(new Error('network down'))
      }

      throw new Error(`Unexpected fetch: ${url}`)
    })

    renderPage()

    expect(await screen.findByText('Config failed to load')).toBeInTheDocument()
    expect(screen.getByText('Error: network down')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Retry' })).toBeInTheDocument()
  })

  it('renders agent routing selects and sends correct payload on change', async () => {
    let capturedBody: unknown = null
    const projects = [
      {
        id: 'prj_1',
        name: 'Ingot',
        path: '/tmp/ingot',
        default_branch: 'main',
        color: '#1f2937',
        execution_mode: 'manual',
        agent_routing: { author: 'claude-code', review: null, investigate: null },
      },
    ]

    vi.spyOn(globalThis, 'fetch').mockImplementation((input, init) => {
      const url = String(input)
      const method = init?.method ?? 'GET'

      if (method === 'GET' && url.endsWith('/api/agents')) {
        return Promise.resolve(
          jsonResponse([
            {
              id: 'agt_1',
              slug: 'claude-code',
              name: 'Claude Code',
              adapter_kind: 'claude_code',
              provider: 'anthropic',
              model: 'claude-sonnet-4-20250514',
              cli_path: 'claude',
              capabilities: [],
              health_check: 'ok',
              status: 'available',
            },
            {
              id: 'agt_2',
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
      if (method === 'GET' && url.endsWith('/api/projects/prj_1/config')) {
        return Promise.resolve(jsonResponse({ branch: 'main' }))
      }
      if (method === 'GET' && url.endsWith('/api/projects')) {
        return Promise.resolve(jsonResponse(projects))
      }
      if (method === 'PUT' && url.endsWith('/api/projects/prj_1')) {
        capturedBody = JSON.parse(init?.body as string)
        return Promise.resolve(jsonResponse(projects[0]))
      }

      throw new Error(`Unexpected fetch: ${method} ${url}`)
    })

    renderPage()

    expect(await screen.findByText('Agent Routing')).toBeInTheDocument()

    await waitFor(() => {
      expect(screen.getByRole('combobox', { name: 'Author agent' })).toHaveTextContent('Claude Code (claude-code)')
    })

    const reviewSelect = screen.getByRole('combobox', { name: 'Review agent' })
    expect(reviewSelect).toHaveTextContent('Default (auto)')

    fireEvent.click(reviewSelect)
    const codexOption = await screen.findByText('Codex CLI (codex)')
    fireEvent.click(codexOption)

    await waitFor(() => {
      expect(capturedBody).toEqual({
        agent_routing: { author: 'claude-code', review: 'codex', investigate: null },
      })
    })

    expect(await screen.findByText('Agent routing updated.')).toBeInTheDocument()
  })

  it('renders auto-triage policy card and sends update on toggle', async () => {
    let capturedBody: unknown = null
    const projects = [
      {
        id: 'prj_1',
        name: 'Ingot',
        path: '/tmp/ingot',
        default_branch: 'main',
        color: '#1f2937',
        execution_mode: 'autopilot',
        agent_routing: null,
        auto_triage_policy: null,
      },
    ]

    vi.spyOn(globalThis, 'fetch').mockImplementation((input, init) => {
      const url = String(input)
      const method = init?.method ?? 'GET'

      if (method === 'GET' && url.endsWith('/api/agents')) {
        return Promise.resolve(jsonResponse([]))
      }
      if (method === 'GET' && url.endsWith('/api/projects/prj_1/config')) {
        return Promise.resolve(jsonResponse({ branch: 'main' }))
      }
      if (method === 'GET' && url.endsWith('/api/projects')) {
        return Promise.resolve(jsonResponse(projects))
      }
      if (method === 'PUT' && url.endsWith('/api/projects/prj_1')) {
        capturedBody = JSON.parse(init?.body as string)
        return Promise.resolve(jsonResponse({ ...projects[0], auto_triage_policy: capturedBody }))
      }

      throw new Error(`Unexpected fetch: ${method} ${url}`)
    })

    renderPage()

    expect(await screen.findByText('Auto-Triage Policy')).toBeInTheDocument()

    await waitFor(() => {
      expect(screen.getByRole('button', { name: 'Disabled' })).toBeInTheDocument()
    })

    fireEvent.click(screen.getByRole('button', { name: 'Disabled' }))

    await waitFor(() => {
      expect(capturedBody).toEqual({
        auto_triage_policy: {
          critical: 'fix_now',
          high: 'fix_now',
          medium: 'fix_now',
          low: 'backlog',
        },
      })
    })

    expect(await screen.findByText('Auto-triage policy updated.')).toBeInTheDocument()
  })

  it('invalidates cached item details after changing the execution mode', async () => {
    let projects = [
      {
        id: 'prj_1',
        name: 'Ingot',
        path: '/tmp/ingot',
        default_branch: 'main',
        color: '#1f2937',
        execution_mode: 'manual',
        agent_routing: null,
      },
    ]

    vi.spyOn(globalThis, 'fetch').mockImplementation((input, init) => {
      const url = String(input)
      const method = init?.method ?? 'GET'

      if (method === 'GET' && url.endsWith('/api/agents')) {
        return Promise.resolve(jsonResponse([]))
      }
      if (method === 'GET' && url.endsWith('/api/projects/prj_1/config')) {
        return Promise.resolve(jsonResponse({ branch: 'main', sandbox: 'enabled' }))
      }
      if (method === 'GET' && url.endsWith('/api/projects')) {
        return Promise.resolve(jsonResponse(projects))
      }
      if (method === 'PUT' && url.endsWith('/api/projects/prj_1')) {
        projects = [{ ...projects[0], execution_mode: 'autopilot' }]
        return Promise.resolve(jsonResponse(projects[0]))
      }

      throw new Error(`Unexpected fetch: ${method} ${url}`)
    })

    const { queryClient } = renderPage()
    queryClient.setQueryData(queryKeys.item('prj_1', 'itm_1'), {
      item: { id: 'itm_1' },
      execution_mode: 'manual',
    })

    fireEvent.click(await screen.findByRole('button', { name: 'Autopilot' }))

    expect(await screen.findByText('Execution mode updated.')).toBeInTheDocument()
    await waitFor(() => {
      expect(queryClient.getQueryState(queryKeys.item('prj_1', 'itm_1'))?.isInvalidated).toBe(true)
    })
  })
})
