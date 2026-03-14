import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useMemo } from 'react'
import { Link } from 'react-router'
import { toast } from 'sonner'
import { approveItem, dispatchItemJob, prepareConvergence, rejectApproval, triageFinding } from '../api/client'
import { agentsQuery, itemDetailQuery, queryKeys } from '../api/queries'
import {
  AcceptanceCriteriaSection,
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
import { PageHeader } from '../components/PageHeader'
import { PageQueryError } from '../components/PageQueryError'
import { ItemDetailSkeleton } from '../components/PageSkeletons'
import { SectionNav } from '../components/SectionNav'
import {
  Breadcrumb,
  BreadcrumbItem,
  BreadcrumbLink,
  BreadcrumbList,
  BreadcrumbPage,
  BreadcrumbSeparator,
} from '../components/ui/breadcrumb'
import { useRequiredItemId, useRequiredProjectId } from '../hooks/useRequiredRouteParam'
import { useSectionObserver } from '../hooks/useSectionObserver'
import { getQueuedJobBlocker } from '../jobBlockers'
import { showErrorToast } from '../lib/toast'
import type { FindingTriageState } from '../types/domain'

type DetailSection = {
  id: string
  label: string
  count: number
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

  const activeJob = detail?.jobs.find((job) => ['queued', 'assigned', 'running'].includes(job.status))
  const retryableJobs = detail?.jobs.filter((job) => ['failed', 'cancelled', 'expired'].includes(job.status)) ?? []
  const isAgentAvailabilityLoading = isAgentsLoading && !!activeJob
  const queueBlocker = isAgentAvailabilityLoading ? null : getQueuedJobBlocker(activeJob ? [activeJob] : [], agents)
  const operatorBlocker = detail?.queue.checkout_sync_message ?? queueBlocker

  const retryableJobIds = new Set(retryableJobs.map((job) => job.id))

  const hasActivity = (detail?.jobs.length ?? 0) + (detail?.findings.length ?? 0) + (detail?.convergences.length ?? 0)

  const sections: DetailSection[] = useMemo(() => {
    if (!detail) return []
    return [
      { id: 'jobs', label: 'Jobs', count: detail.jobs.length },
      { id: 'findings', label: 'Findings', count: detail.findings.length },
      { id: 'convergences', label: 'Convergences', count: detail.convergences.length },
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
    <div className="space-y-6">
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

      <PageHeader
        title={current_revision.title}
        description={current_revision.description}
        descriptionClassName="max-w-3xl"
      />

      <WorkflowStepper currentStepId={evaluation.current_step_id} />

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
      />
      <OverviewPanels item={item} evaluation={evaluation} revision={current_revision} />
      <AcceptanceCriteriaSection acceptanceCriteria={current_revision.acceptance_criteria} />

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
            onSuccess={refresh}
          />
        </div>
      )}
      {findings.length > 0 && (
        <div id="findings">
          <FindingsTable
            findings={findings}
            pendingFindingId={triageMutation.isPending ? (triageMutation.variables?.findingId ?? null) : null}
            onTriage={(findingId, payload) =>
              triageMutation.mutate({
                findingId,
                ...payload,
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
