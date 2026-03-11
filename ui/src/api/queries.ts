import { queryOptions } from '@tanstack/react-query'
import type { Item, ItemDetail, Project } from '../types/domain'
import * as api from './client'

// Query key factories — consistent keys for invalidation from WS events.
export const queryKeys = {
  projects: () => ['projects'] as const,
  agents: () => ['agents'] as const,
  items: (projectId: string) => ['items', projectId] as const,
  item: (projectId: string, itemId: string) => ['items', projectId, itemId] as const,
  jobs: (projectId: string) => ['jobs', projectId] as const,
  workspaces: (projectId: string) => ['workspaces', projectId] as const,
  convergences: (projectId: string) => ['convergences', projectId] as const,
  health: () => ['health'] as const,
} as const

export const healthQuery = () =>
  queryOptions({
    queryKey: queryKeys.health(),
    queryFn: () => fetch('/api/health').then((r) => r.text()),
    staleTime: 10_000,
  })

export const projectsQuery = () =>
  queryOptions<Project[]>({
    queryKey: queryKeys.projects(),
    queryFn: api.listProjects,
    staleTime: 30_000,
  })

export const itemsQuery = (projectId: string) =>
  queryOptions<Item[]>({
    queryKey: queryKeys.items(projectId),
    queryFn: () => api.listItems(projectId),
    enabled: !!projectId,
    staleTime: 5_000,
  })

export const itemDetailQuery = (projectId: string, itemId: string) =>
  queryOptions<ItemDetail>({
    queryKey: queryKeys.item(projectId, itemId),
    queryFn: () => api.getItem(projectId, itemId),
    enabled: !!projectId && !!itemId,
    staleTime: 5_000,
  })
