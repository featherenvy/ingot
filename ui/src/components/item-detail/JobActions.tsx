import { useMutation } from '@tanstack/react-query'
import { toast } from 'sonner'
import { cancelItemJob, retryItemJob } from '../../api/client'
import { showErrorToast } from '../../lib/toast'
import { ConfirmActionButton } from '../ConfirmActionButton'
import { Button } from '../ui/button'

export function JobActions({
  projectId,
  itemId,
  jobId,
  canCancel,
  canRetry,
  onSuccess,
}: {
  projectId: string
  itemId: string
  jobId: string
  canCancel: boolean
  canRetry: boolean
  onSuccess: () => void
}) {
  const retryMutation = useMutation({
    mutationFn: () => retryItemJob(projectId, itemId, jobId),
    onSuccess: () => {
      onSuccess()
      toast.success('Job retry queued.')
    },
    onError: (error) => {
      showErrorToast('Job retry failed.', error)
    },
  })
  const cancelMutation = useMutation({
    mutationFn: () => cancelItemJob(projectId, itemId, jobId),
    onSuccess: () => {
      onSuccess()
      toast.success('Job cancelled.')
    },
    onError: (error) => {
      showErrorToast('Job cancellation failed.', error)
    },
  })

  if (!canCancel && !canRetry) return <span className="text-muted-foreground">—</span>

  return (
    <div className="flex flex-wrap gap-2">
      {canRetry && (
        <Button
          type="button"
          size="sm"
          variant="outline"
          onClick={() => retryMutation.mutate()}
          disabled={retryMutation.isPending}
        >
          {retryMutation.isPending ? 'Retrying…' : 'Retry'}
        </Button>
      )}
      {canCancel && (
        <ConfirmActionButton
          title="Cancel job?"
          description={
            <>
              This stops job <code>{jobId}</code> for item <code>{itemId}</code>.
            </>
          }
          triggerLabel="Cancel"
          confirmLabel="Cancel job"
          pendingLabel="Cancelling…"
          onConfirm={() => cancelMutation.mutate()}
          pending={cancelMutation.isPending}
          triggerVariant="secondary"
        />
      )}
    </div>
  )
}
