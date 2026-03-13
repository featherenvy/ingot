import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import { ApiError, approveItem, dispatchItemJob, prepareConvergence, rejectApproval } from '../api/client'
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
import { ItemDetailSkeleton } from '../components/PageSkeletons'
import { Alert, AlertDescription, AlertTitle } from '../components/ui/alert'
import { useRequiredItemId, useRequiredProjectId } from '../hooks/useRequiredRouteParam'
import { getQueuedJobBlocker } from '../jobBlockers'

export default function ItemDetailPage() {
  const projectId = useRequiredProjectId()
  const itemId = useRequiredItemId()
  const queryClient = useQueryClient()
  const { data: detail, isLoading, error } = useQuery(itemDetailQuery(projectId, itemId))
  const { data: agents } = useQuery(agentsQuery())

  const refresh = () => {
    queryClient.invalidateQueries({ queryKey: queryKeys.items(projectId) })
    queryClient.invalidateQueries({ queryKey: queryKeys.item(projectId, itemId) })
  }

  const dispatchMutation = useMutation({
    mutationFn: () => dispatchItemJob(projectId, itemId, detail?.evaluation.dispatchable_step_id ?? undefined),
    onSuccess: () => {
      refresh()
      toast.success('Job dispatched.')
    },
  })
  const prepareMutation = useMutation({
    mutationFn: () => prepareConvergence(projectId, itemId),
    onSuccess: () => {
      refresh()
      toast.success('Convergence prepared.')
    },
  })
  const approveMutation = useMutation({
    mutationFn: () => approveItem(projectId, itemId),
    onSuccess: () => {
      refresh()
      toast.success('Approval recorded.')
    },
  })
  const rejectMutation = useMutation({
    mutationFn: () => rejectApproval(projectId, itemId),
    onSuccess: () => {
      refresh()
      toast.success('Approval rejected.')
    },
  })

  const activeJob = detail?.jobs.find((job) => ['queued', 'assigned', 'running'].includes(job.status))
  const retryableJobs = detail?.jobs.filter((job) => ['failed', 'cancelled', 'expired'].includes(job.status)) ?? []
  const queueBlocker = getQueuedJobBlocker(activeJob ? [activeJob] : [], agents)

  const currentError = dispatchMutation.error ?? prepareMutation.error ?? approveMutation.error ?? rejectMutation.error
  const currentErrorMessage = currentError instanceof ApiError ? currentError.message : null
  const retryableJobIds = new Set(retryableJobs.map((job) => job.id))

  if (isLoading) return <ItemDetailSkeleton />
  if (error) {
    return (
      <Alert variant="destructive">
        <AlertTitle>Item detail failed to load</AlertTitle>
        <AlertDescription>{String(error)}</AlertDescription>
      </Alert>
    )
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

  const sections: { id: string; label: string; count: number }[] = [
    { id: 'jobs', label: 'Jobs', count: detail.jobs.length },
    { id: 'findings', label: 'Findings', count: findings.length },
    { id: 'convergences', label: 'Convergences', count: detail.convergences.length },
    { id: 'revision-context', label: 'Revision Context', count: revisionContextSummary ? 1 : 0 },
    { id: 'diagnostics', label: 'Diagnostics', count: diagnostics.length },
  ].filter((s) => s.count > 0)

  return (
    <div className="space-y-6">
      <div className="space-y-2">
        <h2 className="text-2xl font-semibold tracking-tight">{current_revision.title}</h2>
        <p className="max-w-3xl text-sm text-muted-foreground">{current_revision.description}</p>
      </div>

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
        errorMessage={currentErrorMessage}
        queueBlocker={queueBlocker}
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
          <FindingsTable findings={findings} />
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
