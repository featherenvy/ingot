import { useQuery } from '@tanstack/react-query'
import {
  AlertTriangleIcon,
  CheckIcon,
  ChevronRightIcon,
  Loader2Icon,
  SearchIcon,
  ShieldCheckIcon,
  XIcon,
} from 'lucide-react'
import { useMemo } from 'react'
import { Link } from 'react-router'
import { cn } from '@/lib/utils'
import { agentsQuery, itemsQuery, projectJobsQuery } from '../api/queries'
import { ActivityPulse } from '../components/ActivityPulse'
import { PageHeader } from '../components/PageHeader'
import { PageQueryError } from '../components/PageQueryError'
import { PageHeaderSkeleton, StatCardsSkeleton } from '../components/PageSkeletons'
import { StatusBadge } from '../components/StatusBadge'
import { Badge } from '../components/ui/badge'
import { Card, CardContent, CardHeader, CardTitle } from '../components/ui/card'
import { useRequiredProjectId } from '../hooks/useRequiredRouteParam'
import { boardStatuses, countItemSummariesByBoardStatus, createEmptyBoardCounts } from '../itemSummaries'
import { getQueuedJobBlocker } from '../jobBlockers'
import { formatDuration, formatRelativeTime, formatStepLabel } from '../lib/format'
import { isActivePhaseStatus } from '../lib/status'
import type { BoardStatus, ItemSummary, Job, OutcomeClass } from '../types/domain'

// ── Constants ──────────────────────────────────────────────────

const LANE_DESCRIPTIONS: Record<BoardStatus, string> = {
  INBOX: 'Awaiting first dispatch',
  WORKING: 'Active in the workflow',
  APPROVAL: 'Ready for human review',
  DONE: 'Completed',
}

const OUTCOME_ICON: Record<OutcomeClass, { icon: typeof CheckIcon; className: string }> = {
  clean: { icon: CheckIcon, className: 'text-emerald-500' },
  findings: { icon: SearchIcon, className: 'text-amber-500' },
  transient_failure: { icon: AlertTriangleIcon, className: 'text-destructive' },
  terminal_failure: { icon: XIcon, className: 'text-destructive' },
  protocol_violation: { icon: AlertTriangleIcon, className: 'text-destructive' },
  cancelled: { icon: XIcon, className: 'text-muted-foreground' },
}

// ── Attention Section ──────────────────────────────────────────

function AttentionSection({
  escalatedItems,
  approvalItems,
  queueBlocker,
  projectId,
}: {
  escalatedItems: ItemSummary[]
  approvalItems: ItemSummary[]
  queueBlocker: string | null
  projectId: string
}) {
  if (escalatedItems.length === 0 && approvalItems.length === 0 && !queueBlocker) return null

  return (
    <div className="space-y-2">
      {queueBlocker && (
        <div className="flex items-center gap-2 rounded-lg border border-amber-500/30 bg-amber-500/5 px-4 py-2.5 text-sm">
          <AlertTriangleIcon className="size-4 shrink-0 text-amber-500" />
          <span className="text-amber-700 dark:text-amber-400">{queueBlocker}</span>
          <Link
            to={`/projects/${projectId}/config`}
            className="ml-auto text-xs text-muted-foreground underline-offset-2 hover:text-foreground hover:underline"
          >
            Config
          </Link>
        </div>
      )}
      {escalatedItems.map((s) => (
        <Link
          key={s.item.id}
          to={`/projects/${projectId}/items/${s.item.id}`}
          className="flex items-center gap-2 rounded-lg border border-destructive/30 bg-destructive/5 px-4 py-2.5 text-sm transition-colors hover:bg-destructive/10"
        >
          <AlertTriangleIcon className="size-4 shrink-0 text-destructive" />
          <span className="font-medium text-destructive">Escalated</span>
          <span className="min-w-0 truncate text-foreground">{s.title}</span>
          {s.item.escalation_reason && (
            <span className="truncate text-xs text-destructive/70">{s.item.escalation_reason.replace(/_/g, ' ')}</span>
          )}
          <ChevronRightIcon className="ml-auto size-4 shrink-0 text-muted-foreground" />
        </Link>
      ))}
      {approvalItems.map((s) => (
        <Link
          key={s.item.id}
          to={`/projects/${projectId}/items/${s.item.id}`}
          className="flex items-center gap-2 rounded-lg border border-border/60 bg-muted/30 px-4 py-2.5 text-sm transition-colors hover:bg-muted/50"
        >
          <ShieldCheckIcon className="size-4 shrink-0 text-amber-500" />
          <span className="font-medium">Awaiting approval</span>
          <span className="min-w-0 truncate text-muted-foreground">{s.title}</span>
          <ChevronRightIcon className="ml-auto size-4 shrink-0 text-muted-foreground" />
        </Link>
      ))}
    </div>
  )
}

// ── Lane Cards ─────────────────────────────────────────────────

function LaneCard({
  lane,
  count,
  items,
  projectId,
}: {
  lane: BoardStatus
  count: number
  items: ItemSummary[]
  projectId: string
}) {
  const activeCount = items.filter((s) => isActivePhaseStatus(s.evaluation.phase_status)).length
  const escalatedCount = items.filter((s) => s.item.escalation_state === 'operator_required').length

  return (
    <Card size="sm" asChild className="transition-colors hover:bg-muted/30">
      <Link to={`/projects/${projectId}/board`}>
        <CardHeader className="gap-1">
          <StatusBadge status={lane} className="w-fit" />
          <CardTitle className="text-3xl font-semibold tabular-nums tracking-tight">{count}</CardTitle>
        </CardHeader>
        <CardContent className="space-y-1 pt-0 text-xs text-muted-foreground">
          <p>{LANE_DESCRIPTIONS[lane]}</p>
          {activeCount > 0 && (
            <span className="flex items-center gap-1 text-blue-600 dark:text-blue-400">
              <ActivityPulse className="mr-0.5" />
              {activeCount} running
            </span>
          )}
          {escalatedCount > 0 && (
            <span className="flex items-center gap-1 text-destructive">
              <AlertTriangleIcon className="size-3" />
              {escalatedCount} escalated
            </span>
          )}
        </CardContent>
      </Link>
    </Card>
  )
}

// ── Active Jobs ────────────────────────────────────────────────

function ActiveJobsSection({
  jobs,
  itemTitles,
  projectId,
}: {
  jobs: Job[]
  itemTitles: Map<string, string>
  projectId: string
}) {
  if (jobs.length === 0) return null

  return (
    <Card size="sm">
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <Loader2Icon className="size-4 animate-spin text-blue-500" />
          Active Jobs
          <Badge variant="secondary" className="ml-1 rounded-full">
            {jobs.length}
          </Badge>
        </CardTitle>
      </CardHeader>
      <CardContent className="space-y-2">
        {jobs.map((job) => {
          const title = itemTitles.get(job.item_id)
          return (
            <Link
              key={job.id}
              to={`/projects/${projectId}/items/${job.item_id}`}
              className="flex items-center gap-3 rounded-lg border border-blue-500/10 bg-blue-500/[0.03] px-3 py-2 text-sm transition-colors hover:bg-blue-500/[0.06]"
            >
              <ActivityPulse />
              <span className="font-medium">{formatStepLabel(job.step_id)}</span>
              <span className="text-[11px] text-muted-foreground">{job.phase_kind}</span>
              {title && <span className="min-w-0 truncate text-xs text-muted-foreground">{title}</span>}
              <span className="ml-auto shrink-0 font-mono text-[11px] tabular-nums text-muted-foreground">
                {formatDuration(job.started_at, null)}
              </span>
            </Link>
          )
        })}
      </CardContent>
    </Card>
  )
}

// ── Recent Completions ─────────────────────────────────────────

function RecentCompletionsSection({
  jobs,
  itemTitles,
  projectId,
}: {
  jobs: Job[]
  itemTitles: Map<string, string>
  projectId: string
}) {
  if (jobs.length === 0) return null

  return (
    <Card size="sm">
      <CardHeader>
        <CardTitle>Recent Completions</CardTitle>
      </CardHeader>
      <CardContent className="space-y-1.5">
        {jobs.map((job) => {
          const title = itemTitles.get(job.item_id)
          const outcome = job.outcome_class ? OUTCOME_ICON[job.outcome_class] : null
          const OutcomeIcon = outcome?.icon ?? CheckIcon
          const outcomeClass = outcome?.className ?? 'text-muted-foreground'

          return (
            <Link
              key={job.id}
              to={`/projects/${projectId}/items/${job.item_id}`}
              className="flex items-center gap-2.5 rounded-md px-2 py-1.5 text-sm transition-colors hover:bg-muted/50"
            >
              <OutcomeIcon className={cn('size-3.5 shrink-0', outcomeClass)} />
              <span className="font-medium">{formatStepLabel(job.step_id)}</span>
              {job.outcome_class && (
                <span className={cn('text-[11px]', outcomeClass)}>{job.outcome_class.replace(/_/g, ' ')}</span>
              )}
              {title && <span className="min-w-0 truncate text-xs text-muted-foreground">{title}</span>}
              <span className="ml-auto shrink-0 text-[11px] text-muted-foreground/60">
                {job.ended_at ? formatRelativeTime(job.ended_at) : ''}
              </span>
            </Link>
          )
        })}
      </CardContent>
    </Card>
  )
}

// ── Page ───────────────────────────────────────────────────────

const RECENT_COMPLETIONS_COUNT = 8

export default function DashboardPage() {
  const projectId = useRequiredProjectId()
  const { data: itemSummaries, error, isError, isFetching, isLoading, refetch } = useQuery(itemsQuery(projectId))
  const { data: jobs } = useQuery(projectJobsQuery(projectId))
  const { data: agents } = useQuery(agentsQuery())
  const queueBlocker = getQueuedJobBlocker(jobs ?? [], agents)

  const counts = useMemo(
    () => (itemSummaries ? countItemSummariesByBoardStatus(itemSummaries) : createEmptyBoardCounts()),
    [itemSummaries],
  )

  const itemsByLane = useMemo(() => {
    const lanes: Record<BoardStatus, ItemSummary[]> = { INBOX: [], WORKING: [], APPROVAL: [], DONE: [] }
    if (itemSummaries) {
      for (const s of itemSummaries) {
        lanes[s.evaluation.board_status].push(s)
      }
    }
    return lanes
  }, [itemSummaries])

  const escalatedItems = useMemo(
    () => (itemSummaries ?? []).filter((s) => s.item.escalation_state === 'operator_required'),
    [itemSummaries],
  )

  const approvalItems = useMemo(
    () => (itemSummaries ?? []).filter((s) => s.evaluation.board_status === 'APPROVAL'),
    [itemSummaries],
  )

  const itemTitles = useMemo(() => {
    const map = new Map<string, string>()
    if (itemSummaries) {
      for (const s of itemSummaries) {
        map.set(s.item.id, s.title)
      }
    }
    return map
  }, [itemSummaries])

  const activeJobs = useMemo(
    () => (jobs ?? []).filter((j) => j.status === 'running' || j.status === 'assigned' || j.status === 'queued'),
    [jobs],
  )

  const recentCompletions = useMemo(
    () =>
      (jobs ?? [])
        .filter((j) => j.ended_at)
        .sort((a, b) => (b.ended_at ?? '').localeCompare(a.ended_at ?? ''))
        .slice(0, RECENT_COMPLETIONS_COUNT),
    [jobs],
  )

  if (isLoading) {
    return (
      <div className="space-y-6">
        <PageHeaderSkeleton width="w-44" />
        <StatCardsSkeleton />
      </div>
    )
  }
  if (isError) {
    return <PageQueryError title="Dashboard failed to load" error={error} onRetry={refetch} isRetrying={isFetching} />
  }

  return (
    <div className="space-y-6">
      <PageHeader title="Dashboard" />

      <AttentionSection
        escalatedItems={escalatedItems}
        approvalItems={approvalItems}
        queueBlocker={queueBlocker}
        projectId={projectId}
      />

      <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-4">
        {boardStatuses.map((lane) => (
          <LaneCard key={lane} lane={lane} count={counts[lane]} items={itemsByLane[lane]} projectId={projectId} />
        ))}
      </div>

      <div className="grid gap-6 lg:grid-cols-2">
        <ActiveJobsSection jobs={activeJobs} itemTitles={itemTitles} projectId={projectId} />
        <RecentCompletionsSection jobs={recentCompletions} itemTitles={itemTitles} projectId={projectId} />
      </div>
    </div>
  )
}
