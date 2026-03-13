import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { MoreHorizontalIcon } from 'lucide-react'
import { useState } from 'react'
import { toast } from 'sonner'
import { ApiError, abandonWorkspace, removeWorkspace, resetWorkspace } from '../api/client'
import { projectWorkspacesQuery, queryKeys } from '../api/queries'
import { PageHeaderSkeleton, TableCardSkeleton } from '../components/PageSkeletons'
import { TooltipValue } from '../components/TooltipValue'
import {
  AlertDialog,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '../components/ui/alert-dialog'
import { Alert, AlertDescription, AlertTitle } from '../components/ui/alert'
import { Badge } from '../components/ui/badge'
import { Button } from '../components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '../components/ui/card'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '../components/ui/dropdown-menu'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '../components/ui/table'
import { useRequiredProjectId } from '../hooks/useRequiredRouteParam'
import { shortOid } from '../lib/git'
import { statusVariant } from '../lib/status'
import type { Workspace } from '../types/domain'

type WorkspaceActionKind = 'reset' | 'abandon' | 'remove'

const workspaceActionCopy: Record<
  WorkspaceActionKind,
  {
    label: string
    pendingLabel: string
    successMessage: string
    title: string
    description: string
  }
> = {
  reset: {
    label: 'Reset',
    pendingLabel: 'Resetting…',
    successMessage: 'Workspace reset.',
    title: 'Reset workspace?',
    description: 'This discards local workspace changes and restores the managed state for this workspace.',
  },
  abandon: {
    label: 'Abandon',
    pendingLabel: 'Abandoning…',
    successMessage: 'Workspace abandoned.',
    title: 'Abandon workspace?',
    description: 'This marks the workspace as abandoned so operators stop using it for active work.',
  },
  remove: {
    label: 'Remove',
    pendingLabel: 'Removing…',
    successMessage: 'Workspace removed.',
    title: 'Remove workspace?',
    description: 'This permanently removes the retained workspace record from the project inventory.',
  },
}

export default function WorkspacesPage() {
  const projectId = useRequiredProjectId()
  const queryClient = useQueryClient()
  const { data: workspaces, isLoading } = useQuery(projectWorkspacesQuery(projectId))

  const refresh = () => {
    queryClient.invalidateQueries({ queryKey: queryKeys.workspaces(projectId) })
    queryClient.invalidateQueries({ queryKey: queryKeys.items(projectId) })
  }

  if (isLoading) {
    return (
      <div className="space-y-6">
        <PageHeaderSkeleton width="w-40" />
        <TableCardSkeleton columns={7} rows={5} />
      </div>
    )
  }

  return (
    <div className="space-y-6">
      <div className="space-y-1">
        <h2 className="text-2xl font-semibold tracking-tight">Workspaces</h2>
        <p className="text-sm text-muted-foreground">
          Inspect retained authoring environments and recover or remove them as needed.
        </p>
      </div>

      {workspaces && workspaces.length > 0 ? (
        <Card className="gap-0">
          <CardHeader className="border-b">
            <CardTitle>Workspace inventory</CardTitle>
            <CardDescription>Track workspace refs, head commits, and operator recovery actions.</CardDescription>
          </CardHeader>
          <CardContent className="px-0">
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>ID</TableHead>
                  <TableHead>Kind</TableHead>
                  <TableHead>Status</TableHead>
                  <TableHead>Target ref</TableHead>
                  <TableHead>Base</TableHead>
                  <TableHead>Head</TableHead>
                  <TableHead>Actions</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {workspaces.map((workspace) => (
                  <WorkspaceRow key={workspace.id} projectId={projectId} workspace={workspace} onSuccess={refresh} />
                ))}
              </TableBody>
            </Table>
          </CardContent>
        </Card>
      ) : (
        <Card>
          <CardContent className="py-6 text-sm text-muted-foreground">No workspaces yet.</CardContent>
        </Card>
      )}
    </div>
  )
}

function WorkspaceRow({
  projectId,
  workspace,
  onSuccess,
}: {
  projectId: string
  workspace: Workspace
  onSuccess: () => void
}) {
  const [menuOpen, setMenuOpen] = useState(false)
  const [confirmAction, setConfirmAction] = useState<WorkspaceActionKind | null>(null)
  const resetMutation = useMutation({
    mutationFn: () => resetWorkspace(projectId, workspace.id),
    onSuccess: () => handleMutationSuccess(workspaceActionCopy.reset.successMessage),
  })
  const abandonMutation = useMutation({
    mutationFn: () => abandonWorkspace(projectId, workspace.id),
    onSuccess: () => handleMutationSuccess(workspaceActionCopy.abandon.successMessage),
  })
  const removeMutation = useMutation({
    mutationFn: () => removeWorkspace(projectId, workspace.id),
    onSuccess: () => handleMutationSuccess(workspaceActionCopy.remove.successMessage),
  })

  const error = resetMutation.error ?? abandonMutation.error ?? removeMutation.error
  const isPending = resetMutation.isPending || abandonMutation.isPending || removeMutation.isPending
  const currentActionCopy = confirmAction ? workspaceActionCopy[confirmAction] : null

  function resetMutationState() {
    resetMutation.reset()
    abandonMutation.reset()
    removeMutation.reset()
  }

  function handleMutationSuccess(message: string) {
    onSuccess()
    setMenuOpen(false)
    setConfirmAction(null)
    resetMutationState()
    toast.success(message)
  }

  function handleMenuOpenChange(open: boolean) {
    setMenuOpen(open)
  }

  function handleConfirmOpenChange(open: boolean) {
    if (!open && isPending) return
    if (!open) {
      setConfirmAction(null)
      resetMutationState()
    }
  }

  function openConfirm(action: WorkspaceActionKind) {
    setMenuOpen(false)
    setConfirmAction(action)
    resetMutationState()
  }

  function runConfirmedAction() {
    if (confirmAction === 'reset') {
      resetMutation.mutate()
      return
    }

    if (confirmAction === 'abandon') {
      abandonMutation.mutate()
      return
    }

    if (confirmAction === 'remove') {
      removeMutation.mutate()
    }
  }

  return (
    <>
      <TableRow>
        <TableCell className="align-top">
          <code>{workspace.id}</code>
        </TableCell>
        <TableCell className="align-top">{workspace.kind}</TableCell>
        <TableCell className="align-top">
          <Badge variant={statusVariant(workspace.status)}>{workspace.status}</Badge>
        </TableCell>
        <TableCell className="align-top">{workspace.target_ref ?? '—'}</TableCell>
        <TableCell className="align-top">
          <TooltipValue content={workspace.base_commit_oid}>
            <code>{shortOid(workspace.base_commit_oid)}</code>
          </TooltipValue>
        </TableCell>
        <TableCell className="align-top">
          <TooltipValue content={workspace.head_commit_oid}>
            <code>{shortOid(workspace.head_commit_oid)}</code>
          </TooltipValue>
        </TableCell>
        <TableCell className="align-top whitespace-normal">
          <DropdownMenu open={menuOpen} onOpenChange={handleMenuOpenChange}>
            <DropdownMenuTrigger asChild>
              <Button
                type="button"
                variant="outline"
                size="icon-sm"
                aria-label={`Actions for workspace ${workspace.id}`}
              >
                <MoreHorizontalIcon />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              <DropdownMenuItem
                onSelect={(event) => {
                  event.preventDefault()
                  openConfirm('reset')
                }}
                disabled={isPending}
              >
                {workspaceActionCopy.reset.label}
              </DropdownMenuItem>
              <DropdownMenuItem
                onSelect={(event) => {
                  event.preventDefault()
                  openConfirm('abandon')
                }}
                disabled={isPending}
              >
                {workspaceActionCopy.abandon.label}
              </DropdownMenuItem>
              <DropdownMenuItem
                onSelect={(event) => {
                  event.preventDefault()
                  openConfirm('remove')
                }}
                disabled={isPending}
                variant="destructive"
              >
                {workspaceActionCopy.remove.label}
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
          {error instanceof ApiError && (
            <Alert variant="destructive" className="mt-2">
              <AlertTitle>Workspace action failed</AlertTitle>
              <AlertDescription>{error.message}</AlertDescription>
            </Alert>
          )}
        </TableCell>
      </TableRow>

      <AlertDialog open={confirmAction !== null} onOpenChange={handleConfirmOpenChange}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>{currentActionCopy?.title}</AlertDialogTitle>
            <AlertDialogDescription>
              {currentActionCopy?.description} Workspace <code>{workspace.id}</code> is currently targeting{' '}
              <code>{workspace.target_ref ?? 'no ref'}</code>.
            </AlertDialogDescription>
          </AlertDialogHeader>
          {error instanceof ApiError && (
            <Alert variant="destructive">
              <AlertTitle>Workspace action failed</AlertTitle>
              <AlertDescription>{error.message}</AlertDescription>
            </Alert>
          )}
          <AlertDialogFooter>
            <AlertDialogCancel disabled={isPending}>Cancel</AlertDialogCancel>
            <Button type="button" variant="destructive" onClick={runConfirmedAction} disabled={isPending}>
              {isPending && currentActionCopy ? currentActionCopy.pendingLabel : currentActionCopy?.label}
            </Button>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </>
  )
}
