import type { QueryClient } from '@tanstack/react-query'
import { create } from 'zustand'
import { queryKeys } from '../api/queries'
import type { WsEvent } from '../types/domain'
import { useProjectsStore } from './projects'

type ConnectionStatus = 'disconnected' | 'connecting' | 'connected'

interface ConnectionState {
  status: ConnectionStatus
  lastSeq: number
  ws: WebSocket | null

  connect: (queryClient: QueryClient) => void
  disconnect: () => void
}

export const useConnectionStore = create<ConnectionState>((set, get) => ({
  status: 'disconnected',
  lastSeq: 0,
  ws: null,

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

function handleEvent(event: WsEvent, lastSeq: number, qc: QueryClient) {
  const projectId = useProjectsStore.getState().activeProjectId

  // Sequence gap — invalidate everything for the active project
  if (lastSeq > 0 && event.seq > lastSeq + 1) {
    if (projectId) {
      qc.invalidateQueries({ queryKey: queryKeys.items(projectId) })
      qc.invalidateQueries({ queryKey: queryKeys.jobs(projectId) })
      qc.invalidateQueries({ queryKey: queryKeys.workspaces(projectId) })
      qc.invalidateQueries({ queryKey: queryKeys.convergences(projectId) })
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
