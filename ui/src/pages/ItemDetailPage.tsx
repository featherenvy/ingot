import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Link } from 'react-router'
import { toast } from 'sonner'
import { approveItem, dispatchItemJob, prepareConvergence, rejectApproval, triageFinding } from '../api/client'
import type { FindingTriageState } from '../types/domain'
import { agentsQuery, itemDetailQuery, queryKeys } from '../api/queries'
import {
  AcceptanceCriteriaSection,
  ConvergencesTable,
  DiagnosticsSection,
  FindingsTable,
  JobsTable,
  OperatorActions,
  OverviewPanels,
  RevisionContextPanel,
} from '../components/item-detail'
import { PageHeader } from '../components/PageHeader'
import { PageQueryError } from '../components/PageQueryError'
import { ItemDetailSkeleton } from '../components/PageSkeletons'
import {
  Breadcrumb,
  BreadcrumbItem,
  BreadcrumbLink,
  BreadcrumbList,
  BreadcrumbPage,
  BreadcrumbSeparator,
} from '../components/ui/breadcrumb'
import { useRequiredItemId, useRequiredProjectId } from '../hooks/useRequiredRouteParam'
import { getQueuedJobBlocker } from '../jobBlockers'
import { showErrorToast } from '../lib/toast'

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
      toast.success('Convergence prepared.')
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

  const retryableJobIds = new Set(retryableJobs.map((job) => job.id))

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

  const sections: DetailSection[] = [
    { id: 'jobs', label: 'Jobs', count: detail.jobs.length },
    { id: 'findings', label: 'Findings', count: findings.length },
    { id: 'convergences', label: 'Convergences', count: detail.convergences.length },
    { id: 'revision-context', label: 'Revision Context', count: revisionContextSummary ? 1 : 0 },
    { id: 'diagnostics', label: 'Diagnostics', count: diagnostics.length },
  ].filter((s) => s.count > 0)

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

      {sections.length > 0 && (
        <nav className="flex flex-wrap gap-x-4 gap-y-1 text-sm text-muted-foreground">
          {sections.map((s) => (
            <a key={s.id} href={`#${s.id}`} className="hover:text-foreground">
              {s.label} ({s.count})
            </a>
          ))}
        </nav>
      )}

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
        queueBlocker={queueBlocker}
        agentsLoading={isAgentAvailabilityLoading}
      />
      <OverviewPanels item={item} evaluation={evaluation} revision={current_revision} />
      <AcceptanceCriteriaSection acceptanceCriteria={current_revision.acceptance_criteria} />
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
