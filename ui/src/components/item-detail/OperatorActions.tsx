import { Loader2Icon } from 'lucide-react'
import { Link } from 'react-router'
import type { Evaluation } from '../../types/domain'
import { ConfirmActionButton } from '../ConfirmActionButton'
import { Alert, AlertDescription, AlertTitle } from '../ui/alert'
import { Button } from '../ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '../ui/card'

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

export function OperatorActions({
  projectId,
  evaluation,
  actions,
  queueBlocker,
  agentsLoading,
}: {
  projectId: string
  evaluation: Evaluation
  actions: OperatorActionSet
  queueBlocker: string | null
  agentsLoading: boolean
}) {
  const hasDispatch = !!evaluation.dispatchable_step_id
  const hasConvergence = evaluation.next_recommended_action === 'prepare_convergence'
  const hasApprove = evaluation.allowed_actions.includes('approval_approve')
  const hasReject = evaluation.allowed_actions.includes('approval_reject')
  const hasActions = hasDispatch || hasConvergence || hasApprove || hasReject

  return (
    <Card>
      <CardHeader className="border-b">
        <CardTitle>Operator Actions</CardTitle>
        <CardDescription>
          Review the current workflow state and trigger the next operator-approved action.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        {hasActions ? (
          <div className="flex flex-wrap gap-2">
            {hasDispatch && (
              <Button type="button" size="sm" onClick={actions.dispatch.run} disabled={actions.dispatch.pending}>
                {actions.dispatch.pending ? 'Dispatching…' : `Dispatch ${evaluation.dispatchable_step_id}`}
              </Button>
            )}
            {hasConvergence && (
              <Button
                type="button"
                size="sm"
                variant="outline"
                onClick={actions.prepareConvergence.run}
                disabled={actions.prepareConvergence.pending}
              >
                {actions.prepareConvergence.pending ? 'Preparing…' : 'Prepare convergence'}
              </Button>
            )}
            {hasApprove && (
              <Button type="button" size="sm" onClick={actions.approve.run} disabled={actions.approve.pending}>
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
      </CardContent>
    </Card>
  )
}
