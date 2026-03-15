import {
  AlertTriangleIcon,
  CheckIcon,
  ChevronDownIcon,
  ClockIcon,
  Loader2Icon,
  SearchIcon,
  ShieldAlertIcon,
  XIcon,
} from 'lucide-react'
import { cn } from '@/lib/utils'
import { shortId } from '../../lib/git'
import type { Finding, Job, OutcomeClass } from '../../types/domain'
import { StatusBadge } from '../StatusBadge'
import { TooltipValue } from '../TooltipValue'
import { Badge } from '../ui/badge'
import { Card, CardContent, CardHeader, CardTitle } from '../ui/card'
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from '../ui/collapsible'
import { JobActions } from './JobActions'

// ── Utilities ──────────────────────────────────────────────────

function formatStepLabel(stepId: string): string {
  return stepId.replace(/_/g, ' ').replace(/\b\w/g, (c) => c.toUpperCase())
}

function formatDuration(startIso: string | null, endIso: string | null): string {
  if (!startIso) return '\u2014'
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

function formatRelativeTime(iso: string): string {
  const diff = Date.now() - new Date(iso).getTime()
  const mins = Math.floor(diff / 60000)
  if (mins < 1) return 'just now'
  if (mins < 60) return `${mins}m ago`
  const hours = Math.floor(mins / 60)
  if (hours < 24) return `${hours}h ago`
  const days = Math.floor(hours / 24)
  return `${days}d ago`
}

const OUTCOME_CONFIG: Record<OutcomeClass, { icon: typeof CheckIcon; iconClassName: string; label: string }> = {
  clean: { icon: CheckIcon, iconClassName: 'text-emerald-500', label: 'Clean' },
  findings: { icon: SearchIcon, iconClassName: 'text-amber-500', label: 'Findings' },
  transient_failure: { icon: AlertTriangleIcon, iconClassName: 'text-destructive', label: 'Transient failure' },
  terminal_failure: { icon: XIcon, iconClassName: 'text-destructive', label: 'Terminal failure' },
  protocol_violation: { icon: ShieldAlertIcon, iconClassName: 'text-destructive', label: 'Protocol violation' },
  cancelled: { icon: XIcon, iconClassName: 'text-muted-foreground', label: 'Cancelled' },
}

// ── Active Job Card ────────────────────────────────────────────

function ActiveJobCard({
  job,
  projectId,
  itemId,
  onSuccess,
}: {
  job: Job
  projectId: string
  itemId: string
  onSuccess: () => void
}) {
  return (
    <div className="rounded-lg border bg-card ring-1 ring-blue-500/20">
      <div className="flex flex-col gap-3 p-4">
        <div className="flex flex-wrap items-center gap-2">
          <div className="relative flex items-center justify-center">
            <span className="absolute size-5 animate-ping rounded-full bg-blue-500 opacity-20" />
            <Loader2Icon className="size-4 animate-spin text-blue-500" />
          </div>
          <span className="text-sm font-semibold">{formatStepLabel(job.step_id)}</span>
          <Badge variant="outline" className="h-5 text-[11px]">
            {job.phase_kind}
          </Badge>
          <StatusBadge status={job.status} />
          <div className="ml-auto flex items-center gap-3">
            {job.started_at && (
              <span className="flex items-center gap-1 font-mono text-[11px] tabular-nums text-muted-foreground">
                <ClockIcon className="size-3" />
                {formatDuration(job.started_at, null)}
              </span>
            )}
            <TooltipValue content={job.id}>
              <code className="text-[11px] text-muted-foreground">{shortId(job.id)}</code>
            </TooltipValue>
          </div>
        </div>

        {/* Cancel action for active jobs */}
        <div className="border-t border-border/50 pt-3">
          <JobActions
            projectId={projectId}
            itemId={itemId}
            jobId={job.id}
            canCancel={true}
            canRetry={false}
            onSuccess={onSuccess}
          />
        </div>
      </div>
    </div>
  )
}

// ── Completed Job Row ──────────────────────────────────────────

function CompletedJobRow({
  job,
  findingCount,
  projectId,
  itemId,
  canRetry,
  onSuccess,
}: {
  job: Job
  findingCount: number
  projectId: string
  itemId: string
  canRetry: boolean
  onSuccess: () => void
}) {
  const outcome = job.outcome_class ? OUTCOME_CONFIG[job.outcome_class] : null
  const OutcomeIcon = outcome?.icon

  return (
    <div className="flex flex-wrap items-center gap-x-3 gap-y-1.5 rounded-lg border border-border/50 bg-card px-4 py-3">
      {/* Outcome icon */}
      <div className="flex size-6 shrink-0 items-center justify-center rounded-full bg-muted">
        {OutcomeIcon ? (
          <OutcomeIcon className={cn('size-3.5', outcome.iconClassName)} />
        ) : (
          <CheckIcon className="size-3.5 text-muted-foreground" />
        )}
      </div>

      {/* Step + phase */}
      <div className="flex min-w-0 items-center gap-2">
        <span className="text-sm font-medium">{formatStepLabel(job.step_id)}</span>
        <span className="text-[11px] text-muted-foreground">{job.phase_kind}</span>
      </div>

      {/* Outcome badge */}
      {outcome && <span className={cn('text-[11px] font-medium', outcome.iconClassName)}>{outcome.label}</span>}

      {/* Finding count */}
      {findingCount > 0 && (
        <span className="flex items-center gap-1 text-[11px] text-amber-600 dark:text-amber-400">
          <SearchIcon className="size-3" />
          {findingCount} finding{findingCount !== 1 ? 's' : ''}
        </span>
      )}

      {/* Right side: duration + ID + actions */}
      <div className="ml-auto flex items-center gap-3">
        {job.started_at && (
          <span className="font-mono text-[11px] tabular-nums text-muted-foreground">
            {formatDuration(job.started_at, job.ended_at)}
          </span>
        )}
        {job.ended_at && (
          <span className="text-[11px] text-muted-foreground/60">{formatRelativeTime(job.ended_at)}</span>
        )}
        <TooltipValue content={job.id}>
          <code className="text-[11px] text-muted-foreground">{shortId(job.id)}</code>
        </TooltipValue>
        {canRetry && (
          <JobActions
            projectId={projectId}
            itemId={itemId}
            jobId={job.id}
            canCancel={false}
            canRetry={true}
            onSuccess={onSuccess}
          />
        )}
      </div>
    </div>
  )
}

// ── Failed Job Row ─────────────────────────────────────────────

function FailedJobRow({
  job,
  projectId,
  itemId,
  canRetry,
  onSuccess,
}: {
  job: Job
  projectId: string
  itemId: string
  canRetry: boolean
  onSuccess: () => void
}) {
  return (
    <div className="flex flex-wrap items-center gap-x-3 gap-y-1.5 rounded-lg border border-border/50 bg-muted/20 px-4 py-3">
      <XIcon className="size-3.5 shrink-0 text-destructive" />
      <span className="text-sm text-muted-foreground">{formatStepLabel(job.step_id)}</span>
      <StatusBadge status={job.status} />
      {job.error_message && (
        <span className="max-w-md truncate text-xs text-muted-foreground" title={job.error_message}>
          {job.error_message}
        </span>
      )}
      <div className="ml-auto flex items-center gap-3">
        {job.ended_at && (
          <span className="text-[11px] text-muted-foreground/60">{formatRelativeTime(job.ended_at)}</span>
        )}
        <TooltipValue content={job.id}>
          <code className="text-[11px] text-muted-foreground">{shortId(job.id)}</code>
        </TooltipValue>
        {canRetry && (
          <JobActions
            projectId={projectId}
            itemId={itemId}
            jobId={job.id}
            canCancel={false}
            canRetry={true}
            onSuccess={onSuccess}
          />
        )}
      </div>
    </div>
  )
}

// ── Main Component ─────────────────────────────────────────────

export function JobsTable({
  projectId,
  itemId,
  jobs,
  activeJobId,
  retryableJobIds,
  findings,
  onSuccess,
}: {
  projectId: string
  itemId: string
  jobs: Job[]
  activeJobId: string | null
  retryableJobIds: Set<string>
  findings: Finding[]
  onSuccess: () => void
}) {
  // Compute finding counts per job
  const findingCounts = new Map<string, number>()
  for (const f of findings) {
    findingCounts.set(f.source_job_id, (findingCounts.get(f.source_job_id) ?? 0) + 1)
  }

  // Categorize jobs
  const activeJob = activeJobId ? jobs.find((j) => j.id === activeJobId) : undefined
  const completedJobs = jobs
    .filter((j) => j.status === 'completed' && j.id !== activeJobId)
    .sort((a, b) => (b.ended_at ?? '').localeCompare(a.ended_at ?? ''))
  const terminalJobs = jobs
    .filter(
      (j) =>
        (j.status === 'failed' || j.status === 'cancelled' || j.status === 'expired' || j.status === 'superseded') &&
        j.id !== activeJobId,
    )
    .sort((a, b) => (b.ended_at ?? b.created_at).localeCompare(a.ended_at ?? a.created_at))

  return (
    <Card className="gap-0">
      <CardHeader className="border-b">
        <CardTitle>Jobs ({jobs.length})</CardTitle>
      </CardHeader>
      <CardContent className="space-y-4 p-5">
        {/* Active job */}
        {activeJob && <ActiveJobCard job={activeJob} projectId={projectId} itemId={itemId} onSuccess={onSuccess} />}

        {/* Completed jobs */}
        {completedJobs.length > 0 && (
          <section className="space-y-2">
            {activeJob && (
              <h4 className="text-xs font-medium uppercase tracking-wider text-muted-foreground">Completed</h4>
            )}
            <div className="grid gap-2">
              {completedJobs.map((job) => (
                <CompletedJobRow
                  key={job.id}
                  job={job}
                  findingCount={findingCounts.get(job.id) ?? 0}
                  projectId={projectId}
                  itemId={itemId}
                  canRetry={retryableJobIds.has(job.id)}
                  onSuccess={onSuccess}
                />
              ))}
            </div>
          </section>
        )}

        {/* Terminal (failed/cancelled/superseded) jobs */}
        {terminalJobs.length > 0 && (
          <Collapsible>
            <CollapsibleTrigger className="group flex w-full items-center gap-2 rounded-lg px-1 py-1.5 text-sm text-muted-foreground transition-colors hover:text-foreground">
              <ChevronDownIcon className="size-4 shrink-0 transition-transform duration-200 group-data-[state=closed]:-rotate-90" />
              <span className="font-medium">Failed &amp; Cancelled</span>
              <span className="text-xs">({terminalJobs.length})</span>
            </CollapsibleTrigger>
            <CollapsibleContent>
              <div className="mt-2 grid gap-2">
                {terminalJobs.map((job) => (
                  <FailedJobRow
                    key={job.id}
                    job={job}
                    projectId={projectId}
                    itemId={itemId}
                    canRetry={retryableJobIds.has(job.id)}
                    onSuccess={onSuccess}
                  />
                ))}
              </div>
            </CollapsibleContent>
          </Collapsible>
        )}
      </CardContent>
    </Card>
  )
}
