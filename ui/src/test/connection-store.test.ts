import { QueryClient } from '@tanstack/react-query'
import { waitFor } from '@testing-library/react'
import { queryKeys } from '../api/queries'
import { useConnectionStore } from '../stores/connection'
import { useProjectsStore } from '../stores/projects'

class MockWebSocket {
  static CONNECTING = 0
  static OPEN = 1
  static CLOSING = 2
  static CLOSED = 3
  static instances: MockWebSocket[] = []

  readyState = MockWebSocket.CONNECTING
  onopen: (() => void) | null = null
  onmessage: ((event: MessageEvent) => void) | null = null
  onclose: (() => void) | null = null
  onerror: (() => void) | null = null

  constructor(_url: string) {
    MockWebSocket.instances.push(this)
  }

  close() {
    this.readyState = MockWebSocket.CLOSED
    this.onclose?.()
  }
}

describe('connection store', () => {
  beforeEach(() => {
    MockWebSocket.instances = []
    vi.stubGlobal('WebSocket', MockWebSocket)
    useConnectionStore.setState({
      status: 'disconnected',
      lastSeq: 0,
      ws: null,
      jobLogSyncState: 'live',
      recentLogChunkAtByJobId: {},
    })
    useProjectsStore.setState({
      activeProjectId: 'prj_1',
    })
  })

  afterEach(() => {
    useConnectionStore.setState({
      status: 'disconnected',
      lastSeq: 0,
      ws: null,
      jobLogSyncState: 'live',
      recentLogChunkAtByJobId: {},
    })
    useProjectsStore.setState({
      activeProjectId: null,
    })
    vi.restoreAllMocks()
    vi.unstubAllGlobals()
  })

  it('invalidates cached item details when the active project changes', async () => {
    const queryClient = new QueryClient({
      defaultOptions: {
        queries: {
          retry: false,
        },
      },
    })
    queryClient.setQueryData(queryKeys.item('prj_1', 'itm_1'), {
      item: { id: 'itm_1' },
      execution_mode: 'manual',
    })

    useConnectionStore.getState().connect(queryClient)
    const ws = MockWebSocket.instances[0]

    ws.onmessage?.(
      new MessageEvent('message', {
        data: JSON.stringify({
          seq: 1,
          event: 'project_updated',
          project_id: 'prj_1',
          entity_type: 'project',
          entity_id: 'prj_1',
          payload: {},
        }),
      }),
    )

    await waitFor(() => {
      expect(queryClient.getQueryState(queryKeys.item('prj_1', 'itm_1'))?.isInvalidated).toBe(true)
    })
  })

  it('appends streamed job log chunks into the cached job logs entry', async () => {
    const queryClient = new QueryClient({
      defaultOptions: {
        queries: {
          retry: false,
        },
      },
    })

    useConnectionStore.getState().connect(queryClient)
    const ws = MockWebSocket.instances[0]

    ws.onmessage?.(
      new MessageEvent('message', {
        data: JSON.stringify({
          seq: 1,
          event: 'job_log_chunk',
          project_id: 'prj_1',
          entity_type: 'job',
          entity_id: 'job_1',
          payload: {
            stream: 'stdout',
            chunk: 'hello\n',
          },
        }),
      }),
    )

    ws.onmessage?.(
      new MessageEvent('message', {
        data: JSON.stringify({
          seq: 2,
          event: 'job_log_chunk',
          project_id: 'prj_1',
          entity_type: 'job',
          entity_id: 'job_1',
          payload: {
            stream: 'stderr',
            chunk: 'warn\n',
          },
        }),
      }),
    )

    await waitFor(() => {
      expect(queryClient.getQueryData(queryKeys.jobLogs('job_1'))).toEqual({
        prompt: null,
        stdout: 'hello\n',
        stderr: 'warn\n',
        result: null,
      })
    })

    expect(useConnectionStore.getState().jobLogSyncState).toBe('live')
    expect(useConnectionStore.getState().recentLogChunkAtByJobId.job_1).toBeTypeOf('number')
  })

  it('marks the log stream as resyncing on sequence gaps and recovered on the next chunk', async () => {
    const queryClient = new QueryClient({
      defaultOptions: {
        queries: {
          retry: false,
        },
      },
    })
    queryClient.setQueryData(queryKeys.jobLogs('job_1'), {
      prompt: null,
      stdout: 'before\n',
      stderr: null,
      result: null,
    })

    useConnectionStore.getState().connect(queryClient)
    const ws = MockWebSocket.instances[0]

    ws.onmessage?.(
      new MessageEvent('message', {
        data: JSON.stringify({
          seq: 1,
          event: 'job_log_chunk',
          project_id: 'prj_1',
          entity_type: 'job',
          entity_id: 'job_1',
          payload: {
            stream: 'stdout',
            chunk: 'hello\n',
          },
        }),
      }),
    )

    ws.onmessage?.(
      new MessageEvent('message', {
        data: JSON.stringify({
          seq: 3,
          event: 'item_updated',
          project_id: 'prj_1',
          entity_type: 'item',
          entity_id: 'itm_1',
          payload: {},
        }),
      }),
    )

    await waitFor(() => {
      expect(useConnectionStore.getState().jobLogSyncState).toBe('resyncing')
      expect(queryClient.getQueryState(queryKeys.jobLogs('job_1'))?.isInvalidated).toBe(true)
    })

    ws.onmessage?.(
      new MessageEvent('message', {
        data: JSON.stringify({
          seq: 4,
          event: 'job_log_chunk',
          project_id: 'prj_1',
          entity_type: 'job',
          entity_id: 'job_1',
          payload: {
            stream: 'stderr',
            chunk: 'warn\n',
          },
        }),
      }),
    )

    await waitFor(() => {
      expect(useConnectionStore.getState().jobLogSyncState).toBe('recovered')
    })
  })
})
