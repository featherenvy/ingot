import { queryOptions } from '@tanstack/react-query'
import type { Activity, Agent, ItemDetail, ItemSummary, Job, JobLogs, JsonObject, Project } from '../types/domain'
import type { DemoTemplateSummary } from './client'
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
  activity: (projectId: string, limit: number, offset: number) => ['activity', projectId, limit, offset] as const,
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

export const agentsQuery = () =>
  queryOptions<Agent[]>({
    queryKey: queryKeys.agents(),
    queryFn: api.listAgents,
    staleTime: 15_000,
  })

export const projectConfigQuery = (projectId: string) =>
  queryOptions<JsonObject>({
    queryKey: ['project-config', projectId],
    queryFn: () => api.getProjectConfig(projectId),
    enabled: !!projectId,
    staleTime: 15_000,
  })

export const itemsQuery = (projectId: string) =>
  queryOptions<ItemSummary[]>({
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

export const projectJobsQuery = (projectId: string) =>
  queryOptions<Job[]>({
    queryKey: queryKeys.jobs(projectId),
    queryFn: () => api.listProjectJobs(projectId),
    enabled: !!projectId,
    staleTime: 5_000,
  })

export const projectWorkspacesQuery = (projectId: string) =>
  queryOptions({
    queryKey: queryKeys.workspaces(projectId),
    queryFn: () => api.listProjectWorkspaces(projectId),
    enabled: !!projectId,
    staleTime: 5_000,
  })

export const projectActivityQuery = (projectId: string, params: { limit: number; offset: number }) =>
  queryOptions<Activity[]>({
    queryKey: queryKeys.activity(projectId, params.limit, params.offset),
    queryFn: () => api.listProjectActivity(projectId, params),
    enabled: !!projectId,
    staleTime: 5_000,
  })

export const jobLogsQuery = (jobId: string) =>
  queryOptions<JobLogs>({
    queryKey: ['job-logs', jobId],
    queryFn: () => api.getJobLogs(jobId),
    enabled: !!jobId,
    staleTime: 5_000,
  })

export const demoCatalogQuery = () =>
  queryOptions<{ templates: DemoTemplateSummary[] }>({
    queryKey: ['demo-catalog'],
    queryFn: api.getDemoCatalog,
    staleTime: Infinity,
  })
