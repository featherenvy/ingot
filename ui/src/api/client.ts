import type { Item, ItemDetail, Project } from '../types/domain'

const BASE = '/api'

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(`${BASE}${path}`, {
    ...init,
    headers: {
      'Content-Type': 'application/json',
      ...init?.headers,
    },
  })
  if (!res.ok) {
    const body = await res.json().catch(() => ({}))
    throw new ApiError(res.status, body?.error?.code ?? 'unknown', body?.error?.message ?? res.statusText)
  }
  return res.json()
}

export class ApiError extends Error {
  status: number
  code: string

  constructor(status: number, code: string, message: string) {
    super(message)
    this.status = status
    this.code = code
  }
}

// Projects
export const listProjects = () => request<Project[]>('/projects')
export const createProject = (data: { name: string; path: string; default_branch?: string }) =>
  request<Project>('/projects', { method: 'POST', body: JSON.stringify(data) })

// Items (project-scoped)
export const listItems = (projectId: string) => request<Item[]>(`/projects/${projectId}/items`)
export const getItem = (projectId: string, itemId: string) =>
  request<ItemDetail>(`/projects/${projectId}/items/${itemId}`)
export const createItem = (
  projectId: string,
  data: {
    classification: string
    priority: string
    title: string
    description: string
    acceptance_criteria: string
    target_ref: string
    approval_policy?: string
  },
) => request<Item>(`/projects/${projectId}/items`, { method: 'POST', body: JSON.stringify(data) })

// Item commands
export const deferItem = (projectId: string, itemId: string) =>
  request(`/projects/${projectId}/items/${itemId}/defer`, { method: 'POST' })
export const resumeItem = (projectId: string, itemId: string) =>
  request(`/projects/${projectId}/items/${itemId}/resume`, { method: 'POST' })
export const dismissItem = (projectId: string, itemId: string) =>
  request(`/projects/${projectId}/items/${itemId}/dismiss`, { method: 'POST' })
export const approveItem = (projectId: string, itemId: string) =>
  request(`/projects/${projectId}/items/${itemId}/approval/approve`, { method: 'POST' })
export const rejectItem = (projectId: string, itemId: string) =>
  request(`/projects/${projectId}/items/${itemId}/approval/reject`, { method: 'POST' })

// Jobs
export const dispatchJob = (projectId: string, itemId: string) =>
  request(`/projects/${projectId}/items/${itemId}/jobs`, { method: 'POST' })
export const cancelJob = (projectId: string, itemId: string, jobId: string) =>
  request(`/projects/${projectId}/items/${itemId}/jobs/${jobId}/cancel`, { method: 'POST' })

// Convergence
export const prepareConvergence = (projectId: string, itemId: string) =>
  request(`/projects/${projectId}/items/${itemId}/convergence/prepare`, { method: 'POST' })
