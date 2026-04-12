import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { AlertTriangleIcon, ChevronDownIcon } from 'lucide-react'
import { useMemo } from 'react'
import { Link } from 'react-router'
import { toast } from 'sonner'
import {
  approveItem,
  dispatchItemJob,
  prepareConvergence,
  promoteFinding,
  rejectApproval,
  triageFinding,
} from '../api/client'
import { agentsQuery, itemDetailQuery, queryKeys } from '../api/queries'
import {
  ActivityTimeline,
  ConvergencesTable,
  DiagnosticsSection,
  FindingsTable,
  JobsTable,
  OperatorActions,
  OverviewPanels,
  RevisionContextPanel,
  WorkflowStepper,
} from '../components/item-detail'
import { PageQueryError } from '../components/PageQueryError'
import { ItemDetailSkeleton } from '../components/PageSkeletons'
import { Prose } from '../components/Prose'
import { type SectionIndicator, SectionNav } from '../components/SectionNav'
import { StatusBadge } from '../components/StatusBadge'
import { Badge } from '../components/ui/badge'
import {
  Breadcrumb,
  BreadcrumbItem,
  BreadcrumbLink,
  BreadcrumbList,
  BreadcrumbPage,
  BreadcrumbSeparator,
} from '../components/ui/breadcrumb'
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from '../components/ui/collapsible'
import { useRequiredItemId, useRequiredProjectId } from '../hooks/useRequiredRouteParam'
import { useSectionObserver } from '../hooks/useSectionObserver'
import { getQueuedJobBlocker } from '../jobBlockers'
import { showErrorToast } from '../lib/toast'
import type { FindingTriageState } from '../types/domain'

type DetailSection = {
  id: string
  label: string
  count: number
  indicator?: SectionIndicator
}

export default function ItemDetailPage(): React.JSX.Element {
  const projectId = useRequiredProjectId()
  const itemId = useRequiredItemId()
  const queryClient = useQueryClient()
  const { data: detail, error, isError, isFetching, isLoading, refetch } = useQuery(itemDetailQuery(projectId, itemId))
  const { data: agents, isLoading: isAgentsLoading } = useQuery(agentsQuery())

  function refresh(): void {
    queryClient.invalidateQueries({ queryKey: queryKeys.items(projectId) })
    queryClient.invalidateQueries({ queryKey: queryKeys.item(projectId, itemId) })
  }

  const dispatchMutation = useMutation({
    mutationFn: () => dispatchItemJob(projectId, itemId, detail?.evaluation.dispatchable_step_id ?? undefined),
    onSuccess: () => {
      refresh()
      toast.success('Job dispatched.')
    },
    onError: (error) => {
      showErrorToast('Job dispatch failed.', error)
    },
  })
  const prepareMutation = useMutation({
    mutationFn: () => prepareConvergence(projectId, itemId),
    onSuccess: () => {
      refresh()
      toast.success('Convergence queued.')
    },
    onError: (error) => {
      showErrorToast('Convergence preparation failed.', error)
    },
  })
  const approveMutation = useMutation({
    mutationFn: () => approveItem(projectId, itemId),
    onSuccess: () => {
      refresh()
      toast.success('Approval recorded.')
    },
    onError: (error) => {
      showErrorToast('Approval failed.', error)
    },
  })
  const rejectMutation = useMutation({
    mutationFn: () => rejectApproval(projectId, itemId),
    onSuccess: () => {
      refresh()
      toast.success('Approval rejected.')
    },
    onError: (error) => {
      showErrorToast('Approval rejection failed.', error)
    },
  })
  const triageMutation = useMutation({
    mutationFn: (payload: {
      findingId: string
      triage_state: FindingTriageState
      triage_note?: string
      linked_item_id?: string
    }) =>
      triageFinding(payload.findingId, {
        triage_state: payload.triage_state,
        triage_note: payload.triage_note,
        linked_item_id: payload.linked_item_id,
      }),
    onSuccess: () => {
      refresh()
      toast.success('Finding triage saved.')
    },
    onError: (error) => {
      showErrorToast('Finding triage failed.', error)
    },
  })
  const promoteMutation = useMutation({
    mutationFn: (payload: { findingId: string; dispatchImmediately: boolean }) =>
      promoteFinding(payload.findingId, { dispatch_immediately: payload.dispatchImmediately }),
    onSuccess: (result, variables) => {
      refresh()
      queryClient.invalidateQueries({ queryKey: queryKeys.item(projectId, result.item.id) })

      if (result.launch_status === 'dispatched') {
        toast.success(`Change item ${result.current_revision.title} created and launched.`)
        return
      }

      if (result.launch_status === 'dispatch_failed') {
        toast.error(
          result.launch_error ?? `Change item ${result.current_revision.title} was created, but launch failed.`,
        )
        return
      }

      const actionLabel = variables.dispatchImmediately ? 'created' : 'saved to backlog'
      toast.success(`Change item ${result.current_revision.title} ${actionLabel}.`)
    },
    onError: (error) => {
      showErrorToast('Finding promotion failed.', error)
    },
  })

  const activeJob = detail?.jobs.find((job) => ['queued', 'assigned', 'running'].includes(job.status))
  const retryableJobs = detail?.jobs.filter((job) => ['failed', 'cancelled', 'expired'].includes(job.status)) ?? []
  const isAgentAvailabilityLoading = isAgentsLoading && !!activeJob
  const queueBlocker = isAgentAvailabilityLoading ? null : getQueuedJobBlocker(activeJob ? [activeJob] : [], agents)
  const operatorBlocker = detail?.finalization.checkout_adoption_message ?? queueBlocker

  const retryableJobIds = new Set(retryableJobs.map((job) => job.id))

  const hasActivity = (detail?.jobs.length ?? 0) + (detail?.findings.length ?? 0) + (detail?.convergences.length ?? 0)

  const sections: DetailSection[] = useMemo(() => {
    if (!detail) return []

    const hasActiveJob = detail.jobs.some((j) => ['queued', 'assigned', 'running'].includes(j.status))
    const hasFailedJob = detail.jobs.some((j) => ['failed', 'cancelled', 'expired'].includes(j.status))
    const hasUntriagedFinding = detail.findings.some(
      (f) => f.triage_state === 'untriaged' || f.triage_state === 'needs_investigation',
    )
    const hasProblematicConvergence = detail.convergences.some(
      (c) => c.status === 'conflicted' || c.status === 'failed',
    )
    const hasRunningConvergence = detail.convergences.some((c) => c.status === 'running')

    const jobIndicator: SectionIndicator = hasActiveJob ? 'active' : hasFailedJob ? 'error' : null
    const findingIndicator: SectionIndicator = hasUntriagedFinding ? 'warning' : null
    const convergenceIndicator: SectionIndicator = hasRunningConvergence
      ? 'active'
      : hasProblematicConvergence
        ? 'error'
        : null

    return [
      { id: 'jobs', label: 'Jobs', count: detail.jobs.length, indicator: jobIndicator },
      { id: 'findings', label: 'Findings', count: detail.findings.length, indicator: findingIndicator },
      { id: 'convergences', label: 'Convergences', count: detail.convergences.length, indicator: convergenceIndicator },
      { id: 'revision-context', label: 'Revision Context', count: detail.revision_context_summary ? 1 : 0 },
      { id: 'diagnostics', label: 'Diagnostics', count: detail.diagnostics.length },
    ].filter((s) => s.count > 0)
  }, [detail])

  const sectionIds = useMemo(() => sections.map((s) => s.id), [sections])
  const activeSectionId = useSectionObserver(sectionIds)

  if (isLoading) return <ItemDetailSkeleton />
  if (isError) {
    return <PageQueryError title="Item detail failed to load" error={error} onRetry={refetch} isRetrying={isFetching} />
  }
  if (!detail) return <p>Item not found.</p>

  const {
    item,
    current_revision,
    evaluation,
    findings,
    revision_context_summary: revisionContextSummary,
    diagnostics,
  } = detail

  return (
    <div className="space-y-5">
      <Breadcrumb>
        <BreadcrumbList>
          <BreadcrumbItem>
            <BreadcrumbLink asChild>
              <Link to={`/projects/${projectId}/board`}>Board</Link>
            </BreadcrumbLink>
          </BreadcrumbItem>
          <BreadcrumbSeparator />
          <BreadcrumbItem>
            <BreadcrumbPage>{current_revision.title}</BreadcrumbPage>
          </BreadcrumbItem>
        </BreadcrumbList>
      </Breadcrumb>

      {/* ─── Header: title, description, inline status ─── */}
      <div className="space-y-3">
        <h2 className="text-2xl font-semibold tracking-tight">{current_revision.title}</h2>
        {current_revision.description && (
          <div className="max-w-3xl text-muted-foreground">
            <Prose content={current_revision.description} />
          </div>
        )}
        <div className="flex flex-wrap items-center gap-2">
          <StatusBadge status={evaluation.board_status} />
          {detail.finalization.checkout_adoption_state && detail.finalization.checkout_adoption_state !== 'synced' && (
            <StatusBadge status="awaiting_checkout_sync" label="Awaiting checkout sync" />
          )}
          <Badge variant="secondary">{item.priority}</Badge>
          {item.approval_state !== 'not_required' && item.approval_state !== 'not_requested' && (
            <StatusBadge status={item.approval_state} />
          )}
          {item.parking_state === 'deferred' && <StatusBadge status="deferred" />}
          <span className="text-xs text-muted-foreground">
            rev {current_revision.revision_no} · <code>{current_revision.target_ref}</code>
          </span>
        </div>
        {item.escalation_state === 'operator_required' && (
          <div className="flex items-center gap-2 rounded-lg border border-destructive/30 bg-destructive/5 px-4 py-2.5 text-sm text-destructive">
            <AlertTriangleIcon className="size-4 shrink-0" />
            <span className="font-medium">Escalated</span>
            {item.escalation_reason && (
              <span className="text-destructive/80">{item.escalation_reason.replace(/_/g, ' ')}</span>
            )}
          </div>
        )}
        {evaluation.attention_badges.length > 0 && (
          <div className="flex flex-wrap gap-1.5">
            {evaluation.attention_badges.map((badge) => (
              <Badge key={badge} variant="destructive">
                {badge.replace(/_/g, ' ')}
              </Badge>
            ))}
          </div>
        )}
      </div>

      {/* ─── Acceptance criteria (collapsed by default) ─── */}
      <Collapsible>
        <CollapsibleTrigger className="group flex w-full items-center gap-2 rounded-lg px-3 py-2 text-sm font-semibold tracking-tight text-muted-foreground transition-colors hover:bg-muted/50 hover:text-foreground">
          <ChevronDownIcon className="size-4 shrink-0 transition-transform duration-200 group-data-[state=closed]:-rotate-90" />
          Acceptance Criteria
        </CollapsibleTrigger>
        <CollapsibleContent>
          <div className="max-h-64 overflow-y-auto px-3 pt-1 pb-2">
            <Prose content={current_revision.acceptance_criteria} />
          </div>
        </CollapsibleContent>
      </Collapsible>

      <WorkflowStepper
        workflowVersion={item.workflow_version}
        currentStepId={evaluation.current_step_id}
        dispatchableStepId={evaluation.dispatchable_step_id}
      />

      <OperatorActions
        projectId={projectId}
        evaluation={evaluation}
        actions={{
          dispatch: {
            pending: dispatchMutation.isPending,
            run: () => dispatchMutation.mutate(),
          },
          prepareConvergence: {
            pending: prepareMutation.isPending,
            run: () => prepareMutation.mutate(),
          },
          approve: {
            pending: approveMutation.isPending,
            run: () => approveMutation.mutate(),
          },
          reject: {
            pending: rejectMutation.isPending,
            run: () => rejectMutation.mutate(),
          },
        }}
        queueBlocker={operatorBlocker}
        queue={detail.queue}
        agentsLoading={isAgentAvailabilityLoading}
        executionMode={detail.execution_mode}
      />

      {/* ─── State details (collapsed by default) ─── */}
      <Collapsible>
        <CollapsibleTrigger className="group flex w-full items-center gap-2 rounded-lg px-3 py-2 text-sm font-semibold tracking-tight text-muted-foreground transition-colors hover:bg-muted/50 hover:text-foreground">
          <ChevronDownIcon className="size-4 shrink-0 transition-transform duration-200 group-data-[state=closed]:-rotate-90" />
          State Details
        </CollapsibleTrigger>
        <CollapsibleContent>
          <div className="pt-2">
            <OverviewPanels item={item} evaluation={evaluation} revision={current_revision} />
          </div>
        </CollapsibleContent>
      </Collapsible>

      {hasActivity > 0 && (
        <ActivityTimeline jobs={detail.jobs} findings={findings} convergences={detail.convergences} />
      )}

      {sections.length > 0 && <SectionNav sections={sections} activeSectionId={activeSectionId} />}

      {detail.jobs.length > 0 && (
        <div id="jobs">
          <JobsTable
            projectId={projectId}
            itemId={itemId}
            jobs={detail.jobs}
            activeJobId={activeJob?.id ?? null}
            retryableJobIds={retryableJobIds}
            findings={findings}
            onSuccess={refresh}
          />
        </div>
      )}
      {findings.length > 0 && (
        <div id="findings">
          <FindingsTable
            findings={findings}
            jobs={detail.jobs}
            linkedFindingItems={detail.linked_finding_items}
            workflowVersion={item.workflow_version}
            pendingFindingId={
              promoteMutation.isPending
                ? (promoteMutation.variables?.findingId ?? null)
                : triageMutation.isPending
                  ? (triageMutation.variables?.findingId ?? null)
                  : null
            }
            onTriage={(findingId, payload) =>
              triageMutation.mutate({
                findingId,
                ...payload,
              })
            }
            onPromote={(findingId, dispatchImmediately) =>
              promoteMutation.mutate({
                findingId,
                dispatchImmediately,
              })
            }
          />
        </div>
      )}
      {detail.convergences.length > 0 && (
        <div id="convergences">
          <ConvergencesTable convergences={detail.convergences} />
        </div>
      )}
      {revisionContextSummary && (
        <div id="revision-context">
          <RevisionContextPanel summary={revisionContextSummary} />
        </div>
      )}
      {diagnostics.length > 0 && (
        <div id="diagnostics">
          <DiagnosticsSection diagnostics={diagnostics} />
        </div>
      )}
    </div>
  )
}
