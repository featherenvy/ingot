import type { LucideIcon } from 'lucide-react'
import {
  AlertTriangleIcon,
  CheckIcon,
  ChevronDownIcon,
  GitMergeIcon,
  Loader2Icon,
  SearchIcon,
  XIcon,
} from 'lucide-react'
import { useMemo, useState } from 'react'
import { cn } from '@/lib/utils'
import type { Convergence, Finding, FindingSeverity, Job } from '../../types/domain'
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from '../ui/collapsible'

// ── Types ──────────────────────────────────────────────────────

type JobStory = {
  type: 'job'
  key: string
  sortTimestamp: string
  job: Job
  findings: Finding[]
}

type ConvergenceStory = {
  type: 'convergence'
  key: string
  sortTimestamp: string
  convergence: Convergence
}

type Story = JobStory | ConvergenceStory

// ── Utilities ──────────────────────────────────────────────────

function formatStepLabel(stepId: string): string {
  return stepId.replace(/_/g, ' ').replace(/\b\w/g, (c) => c.toUpperCase())
}

function formatDuration(startIso: string | null, endIso: string | null): string {
  if (!startIso) return ''
  const start = new Date(startIso).getTime()
  const end = endIso ? new Date(endIso).getTime() : Date.now()
  const secs = Math.floor((end - start) / 1000)
  if (secs < 60) return `${secs}s`
  const mins = Math.floor(secs / 60)
  const remSecs = secs % 60
  if (mins < 60) return `${mins}m ${remSecs}s`
  const hrs = Math.floor(mins / 60)
  const remMins = mins % 60
  return `${hrs}h ${remMins}m`
}

function formatTimestamp(iso: string): string {
  if (!iso) return ''
  const d = new Date(iso)
  const date = d.toLocaleDateString('en-US', { month: 'short', day: 'numeric' })
  const time = d.toLocaleTimeString('en-US', { hour: '2-digit', minute: '2-digit', hour12: false })
  return `${date} ${time}`
}

function summarizeFindings(findings: Finding[]): string {
  if (findings.length === 0) return ''
  const bySeverity = new Map<FindingSeverity, number>()
  for (const f of findings) {
    bySeverity.set(f.severity, (bySeverity.get(f.severity) ?? 0) + 1)
  }
  const parts: string[] = []
  for (const sev of ['critical', 'high', 'medium', 'low'] as FindingSeverity[]) {
    const count = bySeverity.get(sev)
    if (count) parts.push(`${count} ${sev}`)
  }
  return `${findings.length} finding${findings.length !== 1 ? 's' : ''} (${parts.join(', ')})`
}

// ── Story builders ─────────────────────────────────────────────

function buildStories(jobs: Job[], findings: Finding[], convergences: Convergence[]): Story[] {
  // Index findings by source job
  const findingsByJob = new Map<string, Finding[]>()
  for (const f of findings) {
    const list = findingsByJob.get(f.source_job_id) ?? []
    list.push(f)
    findingsByJob.set(f.source_job_id, list)
  }

  const stories: Story[] = []

  // Job stories
  for (const job of jobs) {
    const jobFindings = findingsByJob.get(job.id) ?? []
    const sortTimestamp = job.ended_at ?? job.started_at ?? job.created_at
    stories.push({
      type: 'job',
      key: `job-${job.id}`,
      sortTimestamp,
      job,
      findings: jobFindings,
    })
  }

  // Convergence stories
  for (const c of convergences) {
    stories.push({
      type: 'convergence',
      key: `conv-${c.id}`,
      sortTimestamp: '', // no timestamp on convergences — sort to end
      convergence: c,
    })
  }

  // Sort newest first
  stories.sort((a, b) => {
    if (!a.sortTimestamp) return 1
    if (!b.sortTimestamp) return -1
    return b.sortTimestamp.localeCompare(a.sortTimestamp)
  })

  return stories
}

// ── Story icon logic ───────────────────────────────────────────

function jobIcon(job: Job): { icon: LucideIcon; className: string } {
  if (['running', 'assigned'].includes(job.status)) {
    return { icon: Loader2Icon, className: 'text-blue-500 animate-spin' }
  }
  if (job.status === 'queued') {
    return { icon: Loader2Icon, className: 'text-muted-foreground' }
  }
  if (['failed', 'cancelled', 'expired'].includes(job.status)) {
    return { icon: XIcon, className: 'text-destructive' }
  }
  if (job.outcome_class === 'findings') {
    return { icon: SearchIcon, className: 'text-amber-500' }
  }
  if (job.outcome_class === 'clean') {
    return { icon: CheckIcon, className: 'text-emerald-500' }
  }
  if (job.outcome_class === 'protocol_violation') {
    return { icon: AlertTriangleIcon, className: 'text-destructive' }
  }
  return { icon: CheckIcon, className: 'text-muted-foreground' }
}

function convergenceIcon(c: Convergence): { icon: LucideIcon; className: string } {
  if (c.status === 'conflicted' || c.status === 'failed' || c.status === 'cancelled') {
    return { icon: AlertTriangleIcon, className: 'text-destructive' }
  }
  if (c.status === 'finalized') {
    return { icon: GitMergeIcon, className: 'text-emerald-500' }
  }
  if (c.status === 'running') {
    return { icon: Loader2Icon, className: 'text-blue-500 animate-spin' }
  }
  return { icon: GitMergeIcon, className: 'text-muted-foreground' }
}

// ── Job outcome label ──────────────────────────────────────────

function jobOutcomeLabel(job: Job): { text: string; className: string } {
  if (['running', 'assigned'].includes(job.status)) {
    return { text: 'running', className: 'text-blue-600 dark:text-blue-400' }
  }
  if (job.status === 'queued') {
    return { text: 'queued', className: 'text-muted-foreground' }
  }
  if (['failed', 'cancelled', 'expired'].includes(job.status)) {
    return { text: job.status, className: 'text-destructive' }
  }
  if (job.outcome_class === 'clean') {
    return { text: 'clean', className: 'text-emerald-600 dark:text-emerald-400' }
  }
  if (job.outcome_class === 'findings') {
    return { text: 'findings', className: 'text-amber-600 dark:text-amber-400' }
  }
  if (job.outcome_class) {
    return { text: job.outcome_class.replace(/_/g, ' '), className: 'text-destructive' }
  }
  return { text: 'completed', className: 'text-muted-foreground' }
}

// ── Render ─────────────────────────────────────────────────────

function JobStoryEntry({ story }: { story: JobStory }) {
  const { job, findings } = story
  const { icon: Icon, className: iconCls } = jobIcon(job)
  const outcome = jobOutcomeLabel(job)
  const duration = formatDuration(job.started_at, job.ended_at)
  const ts = job.ended_at ?? job.started_at ?? job.created_at

  return (
    <div className="group/entry relative pb-5 last:pb-0">
      {/* Timeline dot */}
      <div className="absolute -left-[1.625rem] top-0.5 flex size-5 items-center justify-center rounded-full bg-background ring-2 ring-border/60">
        <Icon className={cn('size-3', iconCls)} />
      </div>

      <div className="space-y-0.5">
        {/* Primary line: step label + outcome + timestamp */}
        <div className="flex min-w-0 flex-wrap items-baseline gap-x-2">
          <span className="text-sm font-medium">{formatStepLabel(job.step_id)}</span>
          <span className={cn('text-xs font-medium', outcome.className)}>{outcome.text}</span>
          <span className="ml-auto flex shrink-0 items-baseline gap-2 font-mono text-[11px] tabular-nums text-muted-foreground/70">
            {duration && <span>{duration}</span>}
            {ts && <span>{formatTimestamp(ts)}</span>}
          </span>
        </div>

        {/* Finding summary */}
        {findings.length > 0 && (
          <div className="flex items-center gap-1.5 text-xs text-muted-foreground">
            <SearchIcon className="size-3 text-amber-500" />
            <span>{summarizeFindings(findings)}</span>
          </div>
        )}

        {/* Error message for failed jobs */}
        {job.error_message && (
          <p className="max-w-lg truncate text-xs text-destructive/80" title={job.error_message}>
            {job.error_message}
          </p>
        )}
      </div>
    </div>
  )
}

function ConvergenceStoryEntry({ story }: { story: ConvergenceStory }) {
  const { convergence: c } = story
  const { icon: Icon, className: iconCls } = convergenceIcon(c)

  return (
    <div className="group/entry relative pb-5 last:pb-0">
      <div className="absolute -left-[1.625rem] top-0.5 flex size-5 items-center justify-center rounded-full bg-background ring-2 ring-border/60">
        <Icon className={cn('size-3', iconCls)} />
      </div>
      <div className="flex min-w-0 flex-wrap items-baseline gap-x-2">
        <span className="text-sm font-medium">Convergence</span>
        <span
          className={cn(
            'text-xs font-medium',
            c.status === 'finalized'
              ? 'text-emerald-600 dark:text-emerald-400'
              : c.status === 'conflicted' || c.status === 'failed'
                ? 'text-destructive'
                : 'text-muted-foreground',
          )}
        >
          {c.status}
        </span>
        {c.status === 'finalized' && <span className="text-xs text-muted-foreground">merged to target</span>}
      </div>
    </div>
  )
}

// ── Main component ─────────────────────────────────────────────

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

  const stories = useMemo(() => buildStories(jobs, findings, convergences), [jobs, findings, convergences])

  if (stories.length === 0) return null

  return (
    <Collapsible open={open} onOpenChange={setOpen}>
      <CollapsibleTrigger className="group flex w-full items-center gap-2 rounded-lg px-3 py-2 text-sm font-semibold tracking-tight text-muted-foreground transition-colors hover:bg-muted/50 hover:text-foreground">
        <ChevronDownIcon className="size-4 shrink-0 transition-transform duration-200 group-data-[state=closed]:-rotate-90" />
        Activity
        <span className="inline-flex h-5 min-w-5 items-center justify-center rounded-full bg-muted px-1.5 text-[11px] font-medium tabular-nums text-muted-foreground">
          {stories.length}
        </span>
      </CollapsibleTrigger>
      <CollapsibleContent>
        <div className="relative ml-5 border-l border-border/60 pl-5 pt-2">
          {stories.map((story) =>
            story.type === 'job' ? (
              <JobStoryEntry key={story.key} story={story} />
            ) : (
              <ConvergenceStoryEntry key={story.key} story={story} />
            ),
          )}
        </div>
      </CollapsibleContent>
    </Collapsible>
  )
}
