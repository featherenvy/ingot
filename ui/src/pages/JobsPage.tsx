import { useQuery } from '@tanstack/react-query'
import {
  AlertTriangleIcon,
  ArrowDownIcon,
  CheckIcon,
  ClockIcon,
  Loader2Icon,
  SearchIcon,
  ShieldAlertIcon,
  XIcon,
  ZapIcon,
} from 'lucide-react'
import { useEffect, useMemo, useRef, useState } from 'react'
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
import { useConnectionStore } from '../stores/connection'
import type { Job, OutcomeClass } from '../types/domain'

// ── Constants ──────────────────────────────────────────────────

type FilterTab = 'all' | 'active' | 'completed' | 'failed'
type LogTab = 'stdout' | 'stderr' | 'prompt' | 'result'

const ACTIVE_STATUSES = new Set(['queued', 'assigned', 'running'])
const FAILED_STATUSES = new Set(['failed', 'cancelled', 'expired'])
const FOLLOW_TAIL_THRESHOLD_PX = 24
const RECENT_STREAM_WINDOW_MS = 6_000

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

function isNearBottom(element: HTMLDivElement): boolean {
  return element.scrollHeight - element.scrollTop - element.clientHeight <= FOLLOW_TAIL_THRESHOLD_PX
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
  streamState,
  onSelect,
}: {
  job: Job
  itemTitle: string | null
  projectId: string
  isSelected: boolean
  streamState: 'streaming' | 'waiting' | null
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

      {streamState === 'streaming' ? (
        <Badge variant="outline" className="h-5 gap-1 text-[10px] text-emerald-700">
          <ZapIcon className="size-3 animate-pulse text-emerald-500" />
          streaming
        </Badge>
      ) : streamState === 'waiting' ? (
        <Badge variant="outline" className="h-5 text-[10px] text-muted-foreground">
          waiting
        </Badge>
      ) : null}

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
  const [logTab, setLogTab] = useState<LogTab>('stdout')
  const [followTail, setFollowTail] = useState(true)
  const { data: jobs, error, isError, isFetching, isLoading, refetch } = useQuery(projectJobsQuery(projectId))
  const { data: itemSummaries } = useQuery(itemsQuery(projectId))
  const { data: agents } = useQuery(agentsQuery())
  const { data: logs, isLoading: isLogsLoading } = useQuery(jobLogsQuery(selectedJobId ?? ''))
  const wsStatus = useConnectionStore((state) => state.status)
  const jobLogSyncState = useConnectionStore((state) => state.jobLogSyncState)
  const recentLogChunkAtByJobId = useConnectionStore((state) => state.recentLogChunkAtByJobId)
  const queueBlocker = getQueuedJobBlocker(jobs ?? [], agents)
  const stdoutRef = useRef<HTMLDivElement>(null)
  const stderrRef = useRef<HTMLDivElement>(null)
  const [now, setNow] = useState(() => Date.now())

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
  const isSelectedJobRunning = selectedJob?.status === 'running'
  const activeOutputRef = logTab === 'stdout' ? stdoutRef : logTab === 'stderr' ? stderrRef : null
  const hasStdout = !!logs?.stdout?.trim()
  const hasStderr = !!logs?.stderr?.trim()
  const hasPrompt = !!logs?.prompt?.trim()
  const hasResult = logs?.result != null
  const showResyncingNotice = isSelectedJobRunning && jobLogSyncState === 'resyncing'
  const showRecoveredNotice = isSelectedJobRunning && jobLogSyncState === 'recovered'

  useEffect(() => {
    setFollowTail(true)
    if (selectedJob?.status === 'running') {
      setLogTab('stdout')
    }
  }, [selectedJob?.status])

  useEffect(() => {
    const hasRunningJobs = (jobs ?? []).some((job) => job.status === 'running')
    if (!hasRunningJobs) return

    const interval = window.setInterval(() => {
      setNow(Date.now())
    }, 1_000)

    return () => window.clearInterval(interval)
  }, [jobs])

  const streamStateByJobId = useMemo(() => {
    const states = new Map<string, 'streaming' | 'waiting' | null>()
    for (const job of jobs ?? []) {
      if (job.status !== 'running') {
        states.set(job.id, null)
        continue
      }

      const lastChunkAt = recentLogChunkAtByJobId[job.id]
      if (lastChunkAt && now - lastChunkAt <= RECENT_STREAM_WINDOW_MS) {
        states.set(job.id, 'streaming')
      } else {
        states.set(job.id, 'waiting')
      }
    }
    return states
  }, [jobs, now, recentLogChunkAtByJobId])

  function handleOutputScroll(event: React.UIEvent<HTMLDivElement>) {
    setFollowTail(isNearBottom(event.currentTarget))
  }

  function jumpToLatest() {
    const element = activeOutputRef?.current
    if (!element) return
    element.scrollTop = element.scrollHeight
    setFollowTail(true)
  }

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
                        streamState={streamStateByJobId.get(job.id) ?? null}
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
                <div className="flex items-start justify-between gap-3">
                  <div className="space-y-2">
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
                  </div>
                  {selectedJob ? (
                    <div className="flex flex-col items-end gap-2">
                      {isSelectedJobRunning ? (
                        <Badge
                          variant="outline"
                          className={cn(
                            'gap-1.5',
                            wsStatus === 'connected' && 'border-emerald-500/40 text-emerald-700',
                            wsStatus === 'connecting' && 'border-amber-500/40 text-amber-700',
                            wsStatus === 'disconnected' && 'border-destructive/40 text-destructive',
                          )}
                        >
                          <span
                            className={cn(
                              'size-1.5 rounded-full',
                              wsStatus === 'connected' && 'animate-pulse bg-emerald-500',
                              wsStatus === 'connecting' && 'animate-pulse bg-amber-500',
                              wsStatus === 'disconnected' && 'bg-destructive',
                            )}
                          />
                          {wsStatus === 'connected' ? 'Live' : wsStatus === 'connecting' ? 'Reconnecting' : 'Offline'}
                        </Badge>
                      ) : null}
                      {(logTab === 'stdout' || logTab === 'stderr') && !followTail ? (
                        <Button type="button" variant="outline" size="sm" onClick={jumpToLatest}>
                          <ArrowDownIcon className="size-4" />
                          Jump to latest
                        </Button>
                      ) : null}
                    </div>
                  ) : null}
                </div>
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
                  <div className="space-y-4">
                    {showResyncingNotice ? (
                      <Alert>
                        <AlertTitle>Resyncing log stream</AlertTitle>
                        <AlertDescription>
                          Websocket events were missed. The panel is reloading from persisted job logs before live
                          chunks resume.
                        </AlertDescription>
                      </Alert>
                    ) : null}

                    {showRecoveredNotice ? (
                      <Alert>
                        <AlertTitle>Log stream recovered</AlertTitle>
                        <AlertDescription>
                          The live stream caught up after a sequence gap. Current output now reflects the persisted log
                          plus new chunks.
                        </AlertDescription>
                      </Alert>
                    ) : null}

                    <Tabs value={logTab} onValueChange={(value) => setLogTab(value as LogTab)}>
                      <TabsList variant="line" className="w-full justify-start overflow-x-auto">
                        <TabsTrigger value="stdout">
                          Stdout
                          {hasStdout || isSelectedJobRunning ? (
                            <Badge variant="secondary" className="ml-1 h-4 rounded-full px-1.5 text-[10px]">
                              {hasStdout ? 'Live' : '...'}
                            </Badge>
                          ) : null}
                        </TabsTrigger>
                        <TabsTrigger value="stderr">
                          Stderr
                          {hasStderr ? (
                            <Badge variant="destructive" className="ml-1 h-4 rounded-full px-1.5 text-[10px]">
                              !
                            </Badge>
                          ) : null}
                        </TabsTrigger>
                        <TabsTrigger value="prompt">Prompt</TabsTrigger>
                        <TabsTrigger value="result">Result</TabsTrigger>
                      </TabsList>

                      <TabsContent value="stdout" className="mt-4">
                        <LogBlock
                          label="Stdout"
                          value={logs?.stdout}
                          emptyMessage={isSelectedJobRunning ? 'Waiting for agent output...' : 'No stdout captured.'}
                          className={cn(
                            isSelectedJobRunning && 'border-blue-500/20 bg-blue-500/5',
                            isSelectedJobRunning && !hasStdout && 'border-dashed',
                          )}
                          autoScrollToBottom={isSelectedJobRunning && followTail && logTab === 'stdout'}
                          scrollContainerRef={stdoutRef}
                          onScroll={handleOutputScroll}
                        />
                      </TabsContent>

                      <TabsContent value="stderr" className="mt-4">
                        <LogBlock
                          label="Stderr"
                          value={logs?.stderr}
                          emptyMessage={isSelectedJobRunning ? 'No stderr yet.' : 'No stderr captured.'}
                          className={cn('border-amber-500/30 bg-amber-500/10', !hasStderr && 'border-dashed')}
                          preClassName={cn(hasStderr && 'text-amber-950 dark:text-amber-100')}
                          autoScrollToBottom={isSelectedJobRunning && followTail && logTab === 'stderr'}
                          scrollContainerRef={stderrRef}
                          onScroll={handleOutputScroll}
                        />
                      </TabsContent>

                      <TabsContent value="prompt" className="mt-4">
                        {hasPrompt ? (
                          <LogBlock label="Prompt" value={logs?.prompt} />
                        ) : (
                          <EmptyState
                            variant="inline"
                            contentClassName="px-0 py-0"
                            description="No prompt artifact available for this job."
                          />
                        )}
                      </TabsContent>

                      <TabsContent value="result" className="mt-4">
                        {hasResult ? (
                          <LogBlock label="Result" value={logs?.result ? JSON.stringify(logs.result, null, 2) : null} />
                        ) : (
                          <EmptyState
                            variant="inline"
                            contentClassName="px-0 py-0"
                            description={
                              isSelectedJobRunning
                                ? 'Result will appear after the agent finishes.'
                                : 'No structured result recorded for this job.'
                            }
                          />
                        )}
                      </TabsContent>
                    </Tabs>
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
