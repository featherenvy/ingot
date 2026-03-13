import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { fireEvent, render, screen } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router'
import BoardPage from '../pages/BoardPage'

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
      <MemoryRouter initialEntries={['/projects/prj_1/board']}>
        <Routes>
          <Route path="/projects/:projectId/board" element={<BoardPage />} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>,
  )
}

describe('BoardPage', () => {
  afterEach(() => {
    vi.restoreAllMocks()
  })

  it('opens the new item form in a sheet instead of rendering it inline by default', async () => {
    vi.spyOn(globalThis, 'fetch').mockImplementation((input) => {
      const url = String(input)
      if (url.endsWith('/api/projects/prj_1/items')) {
        return Promise.resolve(jsonResponse([]))
      }
      throw new Error(`Unexpected fetch: ${url}`)
    })

    renderPage()

    expect(await screen.findByText('Board')).toBeInTheDocument()
    expect(screen.queryByLabelText('Title')).not.toBeInTheDocument()

    fireEvent.click(screen.getByRole('button', { name: 'New item' }))

    expect(await screen.findByRole('dialog')).toBeInTheDocument()
    expect(screen.getByText('Create Item')).toBeInTheDocument()
    expect(screen.getByLabelText('Title')).toBeInTheDocument()
  })

  it('shows field-level validation messages when submitting an empty item form', async () => {
    vi.spyOn(globalThis, 'fetch').mockImplementation((input) => {
      const url = String(input)
      if (url.endsWith('/api/projects/prj_1/items')) {
        return Promise.resolve(jsonResponse([]))
      }
      throw new Error(`Unexpected fetch: ${url}`)
    })

    renderPage()

    fireEvent.click(await screen.findByRole('button', { name: 'New item' }))
    fireEvent.click(screen.getByRole('button', { name: 'Create item' }))

    expect(await screen.findByText('Title is required.')).toBeInTheDocument()
    expect(screen.getByText('Description is required.')).toBeInTheDocument()
    expect(screen.getByText('Acceptance criteria are required.')).toBeInTheDocument()
  })
})
