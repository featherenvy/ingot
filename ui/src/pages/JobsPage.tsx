import { useQuery } from '@tanstack/react-query'
import { AlertTriangleIcon, CheckIcon, ClockIcon, Loader2Icon, SearchIcon, ShieldAlertIcon, XIcon } from 'lucide-react'
import { useMemo, useState } from 'react'
import { Link } from 'react-router'
import { cn } from '@/lib/utils'
import { agentsQuery, itemsQuery, jobLogsQuery, projectJobsQuery } from '../api/queries'
import { EmptyState } from '../components/EmptyState'
import { LogBlock } from '../components/LogBlock'
import { PageHeader } from '../components/PageHeader'
import { PageQueryError } from '../components/PageQueryError'
import { PageHeaderSkeleton, TableCardSkeleton } from '../components/PageSkeletons'
import { StatusBadge } from '../components/StatusBadge'
import { TooltipValue } from '../components/TooltipValue'
import { Alert, AlertDescription, AlertTitle } from '../components/ui/alert'
import { Badge } from '../components/ui/badge'
import { Button } from '../components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '../components/ui/card'
import { Skeleton } from '../components/ui/skeleton'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '../components/ui/tabs'
import { useRequiredProjectId } from '../hooks/useRequiredRouteParam'
import { getQueuedJobBlocker } from '../jobBlockers'
import { formatDuration, formatRelativeTime, formatStepLabel } from '../lib/format'
import { shortId } from '../lib/git'
import type { Job, OutcomeClass } from '../types/domain'

// ── Constants ──────────────────────────────────────────────────

type FilterTab = 'all' | 'active' | 'completed' | 'failed'

const ACTIVE_STATUSES = new Set(['queued', 'assigned', 'running'])
const FAILED_STATUSES = new Set(['failed', 'cancelled', 'expired'])

const OUTCOME_ICON: Record<OutcomeClass, { icon: typeof CheckIcon; className: string }> = {
  clean: { icon: CheckIcon, className: 'text-emerald-500' },
  findings: { icon: SearchIcon, className: 'text-amber-500' },
  transient_failure: { icon: AlertTriangleIcon, className: 'text-destructive' },
  terminal_failure: { icon: XIcon, className: 'text-destructive' },
  protocol_violation: { icon: ShieldAlertIcon, className: 'text-destructive' },
  cancelled: { icon: XIcon, className: 'text-muted-foreground' },
}

// ── Utilities ──────────────────────────────────────────────────

function filterJobs(jobs: Job[], tab: FilterTab): Job[] {
  if (tab === 'active') return jobs.filter((j) => ACTIVE_STATUSES.has(j.status))
  if (tab === 'completed') return jobs.filter((j) => j.status === 'completed')
  if (tab === 'failed') return jobs.filter((j) => FAILED_STATUSES.has(j.status) || j.status === 'superseded')
  return jobs
}

// ── Status Summary ─────────────────────────────────────────────

function StatusSummary({ jobs }: { jobs: Job[] }) {
  const active = jobs.filter((j) => ACTIVE_STATUSES.has(j.status)).length
  const completed = jobs.filter((j) => j.status === 'completed').length
  const failed = jobs.filter((j) => FAILED_STATUSES.has(j.status)).length

  return (
    <div className="flex flex-wrap gap-4 text-sm">
      {active > 0 && (
        <span className="flex items-center gap-1.5 text-blue-600 dark:text-blue-400">
          <Loader2Icon className="size-3.5 animate-spin" />
          <span className="tabular-nums">{active}</span> active
        </span>
      )}
      <span className="flex items-center gap-1.5 text-muted-foreground">
        <CheckIcon className="size-3.5 text-emerald-500" />
        <span className="tabular-nums">{completed}</span> completed
      </span>
      {failed > 0 && (
        <span className="flex items-center gap-1.5 text-muted-foreground">
          <XIcon className="size-3.5 text-destructive" />
          <span className="tabular-nums">{failed}</span> failed
        </span>
      )}
    </div>
  )
}

// ── Job Row ────────────────────────────────────────────────────

function JobRow({
  job,
  itemTitle,
  projectId,
  isSelected,
  onSelect,
}: {
  job: Job
  itemTitle: string | null
  projectId: string
  isSelected: boolean
  onSelect: () => void
}) {
  const isActive = ACTIVE_STATUSES.has(job.status)
  const outcome = job.outcome_class ? OUTCOME_ICON[job.outcome_class] : null
  const OutcomeIcon = outcome?.icon

  return (
    <button
      type="button"
      onClick={onSelect}
      aria-pressed={isSelected}
      className={cn(
        'flex w-full flex-wrap items-center gap-x-3 gap-y-1.5 rounded-lg border px-4 py-3 text-left transition-colors',
        isSelected
          ? 'border-foreground/20 bg-muted/60 ring-1 ring-foreground/10'
          : 'border-border/50 bg-card hover:bg-muted/30',
        isActive && 'border-blue-500/20',
      )}
    >
      {/* Outcome / status icon */}
      <div className="flex size-6 shrink-0 items-center justify-center rounded-full bg-muted">
        {isActive ? (
          <Loader2Icon className="size-3.5 animate-spin text-blue-500" />
        ) : OutcomeIcon ? (
          <OutcomeIcon className={cn('size-3.5', outcome.className)} />
        ) : (
          <ClockIcon className="size-3.5 text-muted-foreground" />
        )}
      </div>

      {/* Step + phase */}
      <div className="flex min-w-0 items-center gap-2">
        <span className="text-sm font-medium">{formatStepLabel(job.step_id)}</span>
        <span className="text-[11px] text-muted-foreground">{job.phase_kind}</span>
      </div>

      {/* Status badge */}
      <StatusBadge status={job.status} className="h-5 text-[11px]" />

      {/* Item title link */}
      {itemTitle && (
        <Link
          to={`/projects/${projectId}/items/${job.item_id}`}
          onClick={(e) => e.stopPropagation()}
          className="max-w-48 truncate text-xs text-muted-foreground underline-offset-2 hover:text-foreground hover:underline"
          title={itemTitle}
        >
          {itemTitle}
        </Link>
      )}

      {/* Right side: duration + time + ID */}
      <div className="ml-auto flex shrink-0 items-center gap-3">
        <span className="font-mono text-[11px] tabular-nums text-muted-foreground">
          {formatDuration(job.started_at, job.ended_at)}
        </span>
        <span className="text-[11px] text-muted-foreground/60">
          {formatRelativeTime(job.ended_at ?? job.started_at)}
        </span>
        <TooltipValue content={job.id}>
          <code className="text-[11px] text-muted-foreground/50">{shortId(job.id)}</code>
        </TooltipValue>
      </div>
    </button>
  )
}

// ── Page ───────────────────────────────────────────────────────

export default function JobsPage(): React.JSX.Element {
  const projectId = useRequiredProjectId()
  const [selectedJobId, setSelectedJobId] = useState<string | null>(null)
  const [activeTab, setActiveTab] = useState<FilterTab>('all')
  const { data: jobs, error, isError, isFetching, isLoading, refetch } = useQuery(projectJobsQuery(projectId))
  const { data: itemSummaries } = useQuery(itemsQuery(projectId))
  const { data: agents } = useQuery(agentsQuery())
  const { data: logs, isLoading: isLogsLoading } = useQuery(jobLogsQuery(selectedJobId ?? ''))
  const queueBlocker = getQueuedJobBlocker(jobs ?? [], agents)

  // Build item title lookup
  const itemTitles = useMemo(() => {
    const map = new Map<string, string>()
    if (itemSummaries) {
      for (const s of itemSummaries) {
        map.set(s.item.id, s.title)
      }
    }
    return map
  }, [itemSummaries])

  const filteredJobs = useMemo(() => filterJobs(jobs ?? [], activeTab), [jobs, activeTab])

  // Tab counts
  const tabCounts = useMemo(() => {
    const all = jobs ?? []
    return {
      all: all.length,
      active: all.filter((j) => ACTIVE_STATUSES.has(j.status)).length,
      completed: all.filter((j) => j.status === 'completed').length,
      failed: all.filter((j) => FAILED_STATUSES.has(j.status) || j.status === 'superseded').length,
    }
  }, [jobs])

  // Selected job metadata
  const selectedJob = selectedJobId ? jobs?.find((j) => j.id === selectedJobId) : null

  if (isLoading) {
    return (
      <div className="space-y-6">
        <PageHeaderSkeleton width="w-28" />
        <div className="grid gap-6 xl:grid-cols-[minmax(0,1.2fr)_minmax(22rem,1fr)]">
          <TableCardSkeleton columns={5} rows={5} />
          <Card className="min-h-[24rem]">
            <CardHeader className="space-y-2">
              <Skeleton className="h-6 w-24" />
              <Skeleton className="h-4 w-full max-w-xs" />
            </CardHeader>
            <CardContent className="space-y-4">
              <Skeleton className="h-24 w-full" />
              <Skeleton className="h-24 w-full" />
            </CardContent>
          </Card>
        </div>
      </div>
    )
  }
  if (isError) {
    return <PageQueryError title="Jobs failed to load" error={error} onRetry={refetch} isRetrying={isFetching} />
  }

  return (
    <div className="space-y-5">
      <PageHeader title="Jobs" />

      {queueBlocker && (
        <Alert>
          <AlertTitle>Agents required</AlertTitle>
          <AlertDescription className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
            <span>{queueBlocker}</span>
            <Button asChild size="sm" variant="outline">
              <Link to={`/projects/${projectId}/config`}>Open Config</Link>
            </Button>
          </AlertDescription>
        </Alert>
      )}

      {jobs && jobs.length > 0 ? (
        <>
          <StatusSummary jobs={jobs} />

          <div className="grid gap-6 xl:grid-cols-[minmax(0,1.2fr)_minmax(22rem,1fr)]">
            {/* Job list with filter tabs */}
            <Tabs value={activeTab} onValueChange={(v) => setActiveTab(v as FilterTab)}>
              <TabsList variant="line">
                <TabsTrigger value="all">
                  All
                  <Badge variant="secondary" className="ml-1 h-4 rounded-full px-1.5 text-[10px]">
                    {tabCounts.all}
                  </Badge>
                </TabsTrigger>
                {tabCounts.active > 0 && (
                  <TabsTrigger value="active">
                    Active
                    <Badge variant="secondary" className="ml-1 h-4 rounded-full px-1.5 text-[10px]">
                      {tabCounts.active}
                    </Badge>
                  </TabsTrigger>
                )}
                <TabsTrigger value="completed">
                  Completed
                  <Badge variant="secondary" className="ml-1 h-4 rounded-full px-1.5 text-[10px]">
                    {tabCounts.completed}
                  </Badge>
                </TabsTrigger>
                {tabCounts.failed > 0 && (
                  <TabsTrigger value="failed">
                    Failed
                    <Badge variant="destructive" className="ml-1 h-4 rounded-full px-1.5 text-[10px]">
                      {tabCounts.failed}
                    </Badge>
                  </TabsTrigger>
                )}
              </TabsList>

              <TabsContent value={activeTab} className="mt-3">
                <div className="grid gap-2">
                  {filteredJobs.length > 0 ? (
                    filteredJobs.map((job) => (
                      <JobRow
                        key={job.id}
                        job={job}
                        itemTitle={itemTitles.get(job.item_id) ?? null}
                        projectId={projectId}
                        isSelected={selectedJobId === job.id}
                        onSelect={() => setSelectedJobId(job.id)}
                      />
                    ))
                  ) : (
                    <EmptyState variant="inline" description={`No ${activeTab} jobs.`} className="py-8" />
                  )}
                </div>
              </TabsContent>
            </Tabs>

            {/* Logs panel */}
            <Card className="sticky top-14 max-h-[calc(100vh-5rem)] overflow-y-auto">
              <CardHeader>
                <CardTitle className="flex items-center gap-2">
                  Logs
                  {selectedJob && (
                    <Badge variant="outline" className="font-mono text-[11px] font-normal">
                      {formatStepLabel(selectedJob.step_id)}
                    </Badge>
                  )}
                </CardTitle>
                <CardDescription>
                  {selectedJob
                    ? `${selectedJob.status}${selectedJob.outcome_class ? ` \u2192 ${selectedJob.outcome_class}` : ''}`
                    : 'Select a job to inspect prompt and logs.'}
                </CardDescription>
              </CardHeader>
              <CardContent>
                {!selectedJobId ? (
                  <EmptyState
                    variant="inline"
                    contentClassName="px-0 py-0"
                    description="Select a job to inspect prompt and logs."
                  />
                ) : isLogsLoading ? (
                  <div className="grid gap-4">
                    <Skeleton className="h-24 w-full" />
                    <Skeleton className="h-24 w-full" />
                    <Skeleton className="h-24 w-full" />
                  </div>
                ) : (
                  <div className="grid gap-4">
                    <LogBlock label="Prompt" value={logs?.prompt} />
                    <LogBlock label="Stdout" value={logs?.stdout} />
                    <LogBlock label="Stderr" value={logs?.stderr} />
                    <LogBlock label="Result" value={logs?.result ? JSON.stringify(logs.result, null, 2) : null} />
                  </div>
                )}
              </CardContent>
            </Card>
          </div>
        </>
      ) : (
        <EmptyState description="No jobs yet." />
      )}
    </div>
  )
}
