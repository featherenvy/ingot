import type {
  Activity,
  Agent,
  AgentRouting,
  AutoTriagePolicy,
  ExecutionMode,
  ItemDetail,
  ItemSummary,
  Job,
  JobLogs,
  JsonObject,
  Project,
  Workspace,
} from '../types/domain'

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
export const listProjectActivity = (projectId: string, params?: { limit?: number; offset?: number }) => {
  const search = new URLSearchParams()
  if (params?.limit !== undefined) search.set('limit', String(params.limit))
  if (params?.offset !== undefined) search.set('offset', String(params.offset))

  const query = search.toString()
  return request<Activity[]>(`/projects/${projectId}/activity${query ? `?${query}` : ''}`)
}
export const createProject = (payload: {
  name?: string
  path: string
  default_branch?: string
  color?: string
  execution_mode?: ExecutionMode
  agent_routing?: AgentRouting | null
  auto_triage_policy?: AutoTriagePolicy | null
}) =>
  request<Project>('/projects', {
    method: 'POST',
    body: JSON.stringify(payload),
  })

export const updateProject = (
  projectId: string,
  payload: {
    name?: string
    path?: string
    default_branch?: string
    color?: string
    execution_mode?: ExecutionMode
    agent_routing?: AgentRouting | null
    auto_triage_policy?: AutoTriagePolicy | null
  },
) =>
  request<Project>(`/projects/${projectId}`, {
    method: 'PUT',
    body: JSON.stringify(payload),
  })

// Demo catalog
export interface DemoTemplateSummary {
  slug: string
  name: string
  description: string
  color: string
  item_count: number
  stacks: DemoStackSummary[]
}

export interface DemoStackSummary {
  slug: string
  label: string
}

export const getDemoCatalog = () => request<{ templates: DemoTemplateSummary[] }>('/demo-catalog')

export const createDemoProject = (payload?: { name?: string; template?: string; stack?: string }) =>
  request<{ project: Project; items_created: number }>('/demo-project', {
    method: 'POST',
    body: JSON.stringify(payload ?? {}),
  })

export const getProjectConfig = (projectId: string) => request<JsonObject>(`/projects/${projectId}/config`)

// Agents
export const listAgents = () => request<Agent[]>('/agents')
export const createAgent = (payload: {
  slug?: string
  name: string
  adapter_kind: 'claude_code' | 'codex'
  provider: 'anthropic' | 'openai'
  model: string
  cli_path: string
  capabilities?: string[]
}) =>
  request<Agent>('/agents', {
    method: 'POST',
    body: JSON.stringify(payload),
  })

export const reprobeAgent = (agentId: string) =>
  request<Agent>(`/agents/${agentId}/reprobe`, {
    method: 'POST',
  })

// Items (project-scoped)
export const listItems = (projectId: string) => request<ItemSummary[]>(`/projects/${projectId}/items`)
export const getItem = (projectId: string, itemId: string) =>
  request<ItemDetail>(`/projects/${projectId}/items/${itemId}`)

export interface CreateItemPayload {
  title: string
  description: string
  acceptance_criteria: string
}

export const createItem = (projectId: string, payload: CreateItemPayload) =>
  request<ItemDetail>(`/projects/${projectId}/items`, {
    method: 'POST',
    body: JSON.stringify(payload),
  })

export const dispatchItemJob = (projectId: string, itemId: string, stepId?: string) =>
  request(`/projects/${projectId}/items/${itemId}/jobs`, {
    method: 'POST',
    body: JSON.stringify(stepId ? { step_id: stepId } : {}),
  })

export const retryItemJob = (projectId: string, itemId: string, jobId: string) =>
  request(`/projects/${projectId}/items/${itemId}/jobs/${jobId}/retry`, {
    method: 'POST',
  })

export const cancelItemJob = (projectId: string, itemId: string, jobId: string) =>
  request(`/projects/${projectId}/items/${itemId}/jobs/${jobId}/cancel`, {
    method: 'POST',
  })

export const triageFinding = (
  findingId: string,
  payload: {
    triage_state: string
    triage_note?: string
    linked_item_id?: string
    target_ref?: string
    approval_policy?: 'required' | 'not_required'
  },
) =>
  request(`/findings/${findingId}/triage`, {
    method: 'POST',
    body: JSON.stringify(payload),
  })

export const prepareConvergence = (projectId: string, itemId: string) =>
  request(`/projects/${projectId}/items/${itemId}/convergence/prepare`, {
    method: 'POST',
  })

export const approveItem = (projectId: string, itemId: string) =>
  request<ItemDetail>(`/projects/${projectId}/items/${itemId}/approval/approve`, {
    method: 'POST',
  })

export const rejectApproval = (projectId: string, itemId: string) =>
  request<ItemDetail>(`/projects/${projectId}/items/${itemId}/approval/reject`, {
    method: 'POST',
    body: '{}',
  })

export const listProjectJobs = (projectId: string) => request<Job[]>(`/projects/${projectId}/jobs`)
export const getJobLogs = (jobId: string) => request<JobLogs>(`/jobs/${jobId}/logs`)
export const listProjectWorkspaces = (projectId: string) => request<Workspace[]>(`/projects/${projectId}/workspaces`)
export const resetWorkspace = (projectId: string, workspaceId: string) =>
  request<Workspace>(`/projects/${projectId}/workspaces/${workspaceId}/reset`, {
    method: 'POST',
  })

export const abandonWorkspace = (projectId: string, workspaceId: string) =>
  request<Workspace>(`/projects/${projectId}/workspaces/${workspaceId}/abandon`, {
    method: 'POST',
  })

export const removeWorkspace = (projectId: string, workspaceId: string) =>
  request<Workspace>(`/projects/${projectId}/workspaces/${workspaceId}/remove`, {
    method: 'POST',
  })
