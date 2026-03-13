import { useMutation } from '@tanstack/react-query'
import { toast } from 'sonner'
import { cancelItemJob, retryItemJob } from '../../api/client'
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
  })
  const cancelMutation = useMutation({
    mutationFn: () => cancelItemJob(projectId, itemId, jobId),
    onSuccess: () => {
      onSuccess()
      toast.success('Job cancelled.')
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
        <Button
          type="button"
          size="sm"
          variant="secondary"
          onClick={() => cancelMutation.mutate()}
          disabled={cancelMutation.isPending}
        >
          {cancelMutation.isPending ? 'Cancelling…' : 'Cancel'}
        </Button>
      )}
    </div>
  )
}
