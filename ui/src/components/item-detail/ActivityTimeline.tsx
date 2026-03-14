import type { LucideIcon } from 'lucide-react'
import {
  AlertTriangleIcon,
  CheckIcon,
  ChevronDownIcon,
  GitMergeIcon,
  Loader2Icon,
  PlayIcon,
  SearchIcon,
  XIcon,
} from 'lucide-react'
import { useMemo, useState } from 'react'
import { cn } from '@/lib/utils'
import type { Convergence, Finding, Job } from '../../types/domain'
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from '../ui/collapsible'

type TimelineEntry = {
  key: string
  timestamp: string
  icon: LucideIcon
  iconClassName: string
  label: string
  detail: string
}

function jobEntries(jobs: Job[]): TimelineEntry[] {
  const entries: TimelineEntry[] = []
  for (const job of jobs) {
    if (job.ended_at) {
      // Completed job: show a single "started" entry and an "ended" entry
      entries.push({
        key: `job-start-${job.id}`,
        timestamp: job.started_at ?? job.created_at,
        icon: PlayIcon,
        iconClassName: 'text-blue-500',
        label: 'Job started',
        detail: `${job.step_id} (${job.phase_kind})`,
      })
      const failed = job.status === 'failed' || job.status === 'cancelled' || job.status === 'expired'
      entries.push({
        key: `job-end-${job.id}`,
        timestamp: job.ended_at,
        icon: failed ? XIcon : CheckIcon,
        iconClassName: failed ? 'text-destructive' : 'text-emerald-500',
        label: failed ? `Job ${job.status}` : 'Job completed',
        detail: `${job.step_id}${job.outcome_class ? ` \u2192 ${job.outcome_class}` : ''}`,
      })
    } else if (['running', 'assigned'].includes(job.status)) {
      // In-flight job: show a single "running" entry (not a separate "started" + "running")
      entries.push({
        key: `job-running-${job.id}`,
        timestamp: job.started_at ?? job.created_at,
        icon: Loader2Icon,
        iconClassName: 'text-blue-500 animate-spin',
        label: 'Job running',
        detail: `${job.step_id} (${job.phase_kind})`,
      })
    } else {
      // Queued or other pre-start state
      entries.push({
        key: `job-queued-${job.id}`,
        timestamp: job.created_at,
        icon: PlayIcon,
        iconClassName: 'text-muted-foreground',
        label: `Job ${job.status}`,
        detail: `${job.step_id} (${job.phase_kind})`,
      })
    }
  }
  return entries
}

function findingEntries(findings: Finding[]): TimelineEntry[] {
  return findings.map((f) => ({
    key: `finding-${f.id}`,
    timestamp: f.created_at,
    icon: f.severity === 'critical' || f.severity === 'high' ? AlertTriangleIcon : SearchIcon,
    iconClassName: f.severity === 'critical' || f.severity === 'high' ? 'text-destructive' : 'text-amber-500',
    label: `Finding: ${f.severity}`,
    detail: f.summary,
  }))
}

function convergenceEntries(convergences: Convergence[]): TimelineEntry[] {
  return convergences.map((c) => {
    const failed = c.status === 'failed' || c.status === 'conflicted' || c.status === 'cancelled'
    return {
      key: `convergence-${c.id}`,
      timestamp: '', // convergences don't have timestamps — will sort to end
      icon: failed ? AlertTriangleIcon : GitMergeIcon,
      iconClassName: failed
        ? 'text-destructive'
        : c.status === 'finalized'
          ? 'text-emerald-500'
          : 'text-muted-foreground',
      label: `Convergence ${c.status}`,
      detail: c.status === 'finalized' ? 'Merged to target' : c.id,
    }
  })
}

function formatTime(iso: string): string {
  if (!iso) return ''
  const d = new Date(iso)
  return d.toLocaleTimeString('en-US', { hour: '2-digit', minute: '2-digit', hour12: false })
}

function formatDate(iso: string): string {
  if (!iso) return ''
  const d = new Date(iso)
  return d.toLocaleDateString('en-US', { month: 'short', day: 'numeric' })
}

export function ActivityTimeline({
  jobs,
  findings,
  convergences,
}: {
  jobs: Job[]
  findings: Finding[]
  convergences: Convergence[]
}) {
  const [open, setOpen] = useState(false)

  const entries = useMemo(
    () =>
      [...jobEntries(jobs), ...findingEntries(findings), ...convergenceEntries(convergences)].sort((a, b) => {
        if (!a.timestamp) return 1
        if (!b.timestamp) return -1
        return a.timestamp.localeCompare(b.timestamp)
      }),
    [jobs, findings, convergences],
  )

  if (entries.length === 0) return null

  return (
    <Collapsible open={open} onOpenChange={setOpen}>
      <CollapsibleTrigger className="group flex w-full items-center gap-2 rounded-lg px-3 py-2 text-sm font-semibold tracking-tight text-muted-foreground transition-colors hover:bg-muted/50 hover:text-foreground">
        <ChevronDownIcon className="size-4 shrink-0 transition-transform duration-200 group-data-[state=closed]:-rotate-90" />
        Activity
        <span className="inline-flex h-5 min-w-5 items-center justify-center rounded-full bg-muted px-1.5 text-[11px] font-medium tabular-nums text-muted-foreground">
          {entries.length}
        </span>
      </CollapsibleTrigger>
      <CollapsibleContent>
        <div className="relative ml-5 border-l border-border/60 pl-5 pt-1">
          {entries.map((entry) => {
            const Icon = entry.icon
            return (
              <div key={entry.key} className="group/entry relative pb-4 last:pb-0">
                {/* Timeline dot */}
                <div className="absolute -left-[1.625rem] top-0.5 flex size-5 items-center justify-center rounded-full bg-background ring-2 ring-border/60">
                  <Icon className={cn('size-3', entry.iconClassName)} />
                </div>
                <div className="flex min-w-0 flex-wrap items-baseline gap-x-2">
                  <span className="text-sm font-medium">{entry.label}</span>
                  <span className="truncate font-mono text-xs text-muted-foreground">{entry.detail}</span>
                  {entry.timestamp && (
                    <span className="ml-auto shrink-0 font-mono text-[11px] tabular-nums text-muted-foreground/70">
                      {formatDate(entry.timestamp)} {formatTime(entry.timestamp)}
                    </span>
                  )}
                </div>
              </div>
            )
          })}
        </div>
      </CollapsibleContent>
    </Collapsible>
  )
}
