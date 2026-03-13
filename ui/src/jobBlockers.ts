import type { Agent, Job } from './types/domain'

export function getQueuedJobBlocker(jobs: Job[], agents: Agent[] | undefined): string | null {
  if (!jobs.some((job) => job.status === 'queued') || !agents) {
    return null
  }

  if (agents.length === 0) {
    return 'Queued jobs are waiting because no agents are configured.'
  }

  const availableCodexAgents = agents.filter((agent) => agent.adapter_kind === 'codex' && agent.status === 'available')
  if (availableCodexAgents.length === 0) {
    return 'Queued jobs are waiting because no Codex agents are currently available.'
  }

  return null
}
