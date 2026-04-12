import type { QueryClient } from '@tanstack/react-query'
import { create } from 'zustand'
import { queryKeys } from '../api/queries'
import type { AgentOutputSegment, JobLogs, WsEvent } from '../types/domain'
import { useProjectsStore } from './projects'

type ConnectionStatus = 'disconnected' | 'connecting' | 'connected'
type JobLogSyncState = 'live' | 'resyncing' | 'recovered'

interface ConnectionState {
  status: ConnectionStatus
  lastSeq: number
  ws: WebSocket | null
  jobLogSyncState: JobLogSyncState
  recentLogChunkAtByJobId: Record<string, number>

  connect: (queryClient: QueryClient) => void
  disconnect: () => void
}

export const useConnectionStore = create<ConnectionState>((set, get) => ({
  status: 'disconnected',
  lastSeq: 0,
  ws: null,
  jobLogSyncState: 'live',
  recentLogChunkAtByJobId: {},

  connect: (queryClient) => {
    const existing = get().ws
    if (existing && existing.readyState <= WebSocket.OPEN) return

    set({ status: 'connecting' })

    const protocol = location.protocol === 'https:' ? 'wss:' : 'ws:'
    const ws = new WebSocket(`${protocol}//${location.host}/api/ws`)

    ws.onopen = () => {
      set({ status: 'connected', ws })
    }

    ws.onmessage = (msg) => {
      try {
        const event: WsEvent = JSON.parse(msg.data)
        handleEvent(event, get().lastSeq, queryClient)
        set({ lastSeq: event.seq })
      } catch {
        // ignore malformed messages
      }
    }

    ws.onclose = () => {
      set({ status: 'disconnected', ws: null })
      setTimeout(() => get().connect(queryClient), 2000)
    }

    ws.onerror = () => {
      ws.close()
    }
  },

  disconnect: () => {
    const ws = get().ws
    if (ws) {
      ws.close()
      set({ ws: null, status: 'disconnected' })
    }
  },
}))

function mergeJobLogSegment(segments: AgentOutputSegment[], incoming: AgentOutputSegment): AgentOutputSegment[] {
  if (segments.some((segment) => segment.sequence === incoming.sequence)) {
    return segments
  }

  return [...segments, incoming].sort((left, right) => left.sequence - right.sequence)
}

function handleEvent(event: WsEvent, lastSeq: number, qc: QueryClient) {
  const projectId = useProjectsStore.getState().activeProjectId

  // Sequence gap — invalidate everything for the active project
  if (lastSeq > 0 && event.seq > lastSeq + 1) {
    useConnectionStore.setState({ jobLogSyncState: 'resyncing' })
    if (projectId) {
      qc.invalidateQueries({ queryKey: queryKeys.items(projectId) })
      qc.invalidateQueries({ queryKey: queryKeys.jobs(projectId) })
      qc.invalidateQueries({ queryKey: queryKeys.workspaces(projectId) })
      qc.invalidateQueries({ queryKey: queryKeys.convergences(projectId) })
    }
    qc.invalidateQueries({ queryKey: ['job-logs'] })
    return
  }

  if (event.event === 'job_output_delta') {
    const segment = event.payload?.segment
    if (segment && typeof segment === 'object') {
      const nextSyncState = useConnectionStore.getState().jobLogSyncState === 'resyncing' ? 'recovered' : 'live'
      useConnectionStore.setState((state) => ({
        jobLogSyncState: nextSyncState,
        recentLogChunkAtByJobId: {
          ...state.recentLogChunkAtByJobId,
          [event.entity_id]: Date.now(),
        },
      }))
      qc.setQueryData<JobLogs>(queryKeys.jobLogs(event.entity_id), (current) => {
        const next: JobLogs = current ?? {
          prompt: null,
          output: {
            schema_version: 'agent_output:v1',
            segments: [],
          },
          result: null,
        }
        const existingSegments = next.output?.segments ?? []
        const nextSegment = segment as AgentOutputSegment
        const mergedSegments = mergeJobLogSegment(existingSegments, nextSegment)

        if (mergedSegments === existingSegments) {
          return next
        }

        return {
          ...next,
          output: {
            schema_version: next.output?.schema_version ?? 'agent_output:v1',
            segments: mergedSegments,
          },
        }
      })
    }
    return
  }

  // Targeted invalidation by entity type
  switch (event.entity_type) {
    case 'item':
      if (projectId) {
        qc.invalidateQueries({ queryKey: queryKeys.items(projectId) })
        // Also invalidate the specific item detail if cached
        qc.invalidateQueries({ queryKey: queryKeys.item(projectId, event.entity_id) })
      }
      break
    case 'job':
      if (projectId) {
        qc.invalidateQueries({ queryKey: queryKeys.jobs(projectId) })
        // Jobs affect item projections, so invalidate items too
        qc.invalidateQueries({ queryKey: queryKeys.items(projectId) })
      }
      break
    case 'workspace':
      if (projectId) {
        qc.invalidateQueries({ queryKey: queryKeys.workspaces(projectId) })
      }
      break
    case 'convergence':
      if (projectId) {
        qc.invalidateQueries({ queryKey: queryKeys.convergences(projectId) })
        qc.invalidateQueries({ queryKey: queryKeys.items(projectId) })
      }
      break
    case 'project':
      qc.invalidateQueries({ queryKey: queryKeys.projects() })
      if (projectId && event.entity_id === projectId) {
        qc.invalidateQueries({ queryKey: queryKeys.items(projectId) })
      }
      break
  }
}
