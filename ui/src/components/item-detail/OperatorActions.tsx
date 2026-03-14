import { ChevronRightIcon, Loader2Icon, ZapIcon } from 'lucide-react'
import { Link } from 'react-router'
import type { Evaluation, QueueStatus } from '../../types/domain'
import { ConfirmActionButton } from '../ConfirmActionButton'
import { Alert, AlertDescription, AlertTitle } from '../ui/alert'
import { Badge } from '../ui/badge'
import { Button } from '../ui/button'

type OperatorActionControl = {
  pending: boolean
  run: () => void
}

type OperatorActionSet = {
  dispatch: OperatorActionControl
  prepareConvergence: OperatorActionControl
  approve: OperatorActionControl
  reject: OperatorActionControl
}

function formatActionLabel(action: string): string {
  return action.replace(/_/g, ' ')
}

export function OperatorActions({
  projectId,
  evaluation,
  actions,
  queueBlocker,
  queue,
  agentsLoading,
}: {
  projectId: string
  evaluation: Evaluation
  actions: OperatorActionSet
  queueBlocker: string | null
  queue: QueueStatus | null
  agentsLoading: boolean
}) {
  const hasDispatch = !!evaluation.dispatchable_step_id
  const hasConvergence = evaluation.next_recommended_action === 'prepare_convergence'
  const hasApprove = evaluation.allowed_actions.includes('approval_approve')
  const hasReject = evaluation.allowed_actions.includes('approval_reject')
  const hasActions = hasDispatch || hasConvergence || hasApprove || hasReject

  return (
    <div className="relative overflow-hidden rounded-xl border border-foreground/[0.08] bg-gradient-to-br from-foreground/[0.03] via-transparent to-foreground/[0.02]">
      {/* Subtle grid pattern overlay */}
      <div
        className="pointer-events-none absolute inset-0 opacity-[0.03]"
        style={{
          backgroundImage:
            'linear-gradient(to right, currentColor 1px, transparent 1px), linear-gradient(to bottom, currentColor 1px, transparent 1px)',
          backgroundSize: '24px 24px',
        }}
      />

      <div className="relative space-y-4 p-5">
        {/* Header row: recommended action + queue status */}
        <div className="flex flex-wrap items-start justify-between gap-3">
          <div className="space-y-1">
            <p className="text-xs font-medium uppercase tracking-wider text-muted-foreground">Next Action</p>
            <div className="flex items-center gap-2">
              <ChevronRightIcon className="size-5 text-muted-foreground/60" />
              <span className="text-lg font-semibold tracking-tight">
                {formatActionLabel(evaluation.next_recommended_action)}
              </span>
              {evaluation.current_step_id && (
                <Badge variant="outline" className="ml-1 font-mono text-[11px]">
                  {evaluation.current_step_id}
                </Badge>
              )}
            </div>
          </div>
          {queue?.state && (
            <div className="flex items-center gap-2 rounded-md bg-muted/60 px-3 py-1.5 text-xs text-muted-foreground">
              <span className="font-medium">Lane</span>
              <span>{queue.state}</span>
              {queue.position ? <span>#{queue.position}</span> : null}
            </div>
          )}
        </div>

        {/* Action buttons */}
        {hasActions ? (
          <div className="flex flex-wrap items-center gap-2">
            {hasDispatch && (
              <Button type="button" onClick={actions.dispatch.run} disabled={actions.dispatch.pending}>
                {actions.dispatch.pending ? (
                  <>
                    <Loader2Icon className="size-4 animate-spin" />
                    Dispatching…
                  </>
                ) : (
                  <>
                    <ZapIcon className="size-4" />
                    Dispatch {evaluation.dispatchable_step_id}
                  </>
                )}
              </Button>
            )}
            {hasConvergence && (
              <Button
                type="button"
                variant="secondary"
                onClick={actions.prepareConvergence.run}
                disabled={actions.prepareConvergence.pending}
              >
                {actions.prepareConvergence.pending ? 'Queuing…' : 'Queue convergence'}
              </Button>
            )}
            {hasApprove && (
              <Button type="button" onClick={actions.approve.run} disabled={actions.approve.pending}>
                {actions.approve.pending ? 'Approving…' : 'Approve'}
              </Button>
            )}
            {hasReject && (
              <ConfirmActionButton
                title="Reject approval?"
                description="This sends the item back for rework and clears the current approval decision."
                triggerLabel="Reject approval"
                confirmLabel="Reject approval"
                pendingLabel="Rejecting…"
                onConfirm={actions.reject.run}
                pending={actions.reject.pending}
                triggerVariant="destructive"
              />
            )}
          </div>
        ) : (
          <p className="text-sm text-muted-foreground">
            Waiting for workflow to advance — no operator actions available.
          </p>
        )}

        {/* Loading / blocker alerts */}
        {agentsLoading ? (
          <output className="flex items-center gap-2 text-sm text-muted-foreground" aria-live="polite">
            <Loader2Icon className="size-4 animate-spin" />
            Checking agent availability…
          </output>
        ) : null}
        {queueBlocker && (
          <Alert>
            <AlertTitle>Operator attention required</AlertTitle>
            <AlertDescription className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
              <span>{queueBlocker}</span>
              <Button asChild size="sm" variant="outline">
                <Link to={`/projects/${projectId}/config`}>Open Config</Link>
              </Button>
            </AlertDescription>
          </Alert>
        )}
      </div>
    </div>
  )
}
