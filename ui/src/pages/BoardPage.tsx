import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { AlertTriangleIcon, ChevronDownIcon } from 'lucide-react'
import { useMemo, useState } from 'react'
import { useForm } from 'react-hook-form'
import { Link, useNavigate } from 'react-router'
import { toast } from 'sonner'
import { cn } from '@/lib/utils'
import { createItem } from '../api/client'
import { itemsQuery, queryKeys } from '../api/queries'
import { ActivityPulse } from '../components/ActivityPulse'
import { EmptyState } from '../components/EmptyState'
import { PageHeader } from '../components/PageHeader'
import { PageQueryError } from '../components/PageQueryError'
import { BoardSkeleton } from '../components/PageSkeletons'
import { StatusBadge } from '../components/StatusBadge'
import { Badge } from '../components/ui/badge'
import { Button } from '../components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '../components/ui/card'
import { Form, FormControl, FormField, FormItem, FormLabel, FormMessage } from '../components/ui/form'
import { Input } from '../components/ui/input'
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle, SheetTrigger } from '../components/ui/sheet'
import { Textarea } from '../components/ui/textarea'
import { useRequiredProjectId } from '../hooks/useRequiredRouteParam'
import { boardStatuses, groupItemSummariesByBoardStatus } from '../itemSummaries'
import { formatRelativeTime, formatStepLabel } from '../lib/format'
import { isActivePhaseStatus } from '../lib/status'
import { showErrorToast } from '../lib/toast'
import type { BoardStatus, ItemSummary, Priority } from '../types/domain'

// ── Constants ──────────────────────────────────────────────────

type CreateItemForm = {
  title: string
  description: string
  acceptanceCriteria: string
}

const initialCreateItemForm: CreateItemForm = {
  title: '',
  description: '',
  acceptanceCriteria: '',
}

const PRIORITY_ORDER: Record<Priority, number> = { critical: 0, major: 1, minor: 2 }

const PRIORITY_ACCENT: Record<Priority, string> = {
  critical: 'border-l-red-500',
  major: 'border-l-amber-500',
  minor: 'border-l-border',
}

const DONE_VISIBLE_COUNT = 5

// ── Utilities ──────────────────────────────────────────────────

function sortItems(items: ItemSummary[]): ItemSummary[] {
  return [...items].sort((a, b) => {
    // Escalated first
    const aEsc = a.item.escalation_state === 'operator_required' ? 0 : 1
    const bEsc = b.item.escalation_state === 'operator_required' ? 0 : 1
    if (aEsc !== bEsc) return aEsc - bEsc
    // Then by priority
    const aPri = PRIORITY_ORDER[a.item.priority]
    const bPri = PRIORITY_ORDER[b.item.priority]
    if (aPri !== bPri) return aPri - bPri
    // Then by sort_key (preserves user-defined ordering)
    return a.item.sort_key.localeCompare(b.item.sort_key)
  })
}

function columnSummary(items: ItemSummary[], col: BoardStatus): string | null {
  if (items.length === 0) return null
  if (col === 'WORKING') {
    const running = items.filter((s) => isActivePhaseStatus(s.evaluation.phase_status)).length
    const escalated = items.filter((s) => s.item.escalation_state === 'operator_required').length
    const parts: string[] = []
    if (running > 0) parts.push(`${running} running`)
    if (escalated > 0) parts.push(`${escalated} escalated`)
    return parts.length > 0 ? parts.join(', ') : null
  }
  if (col === 'APPROVAL') {
    const pending = items.filter((s) => s.item.approval_state === 'pending').length
    if (pending > 0) return `${pending} awaiting approval`
  }
  return null
}

// ── Item Card ──────────────────────────────────────────────────

function BoardItemCard({ summary, projectId }: { summary: ItemSummary; projectId: string }) {
  const { item, title: itemTitle, evaluation } = summary
  const isActive = isActivePhaseStatus(evaluation.phase_status)
  const isEscalated = item.escalation_state === 'operator_required'

  return (
    <Card
      asChild
      size="sm"
      className={cn(
        'border-l-2 px-3 transition-colors hover:bg-muted/40',
        isEscalated ? 'border-l-red-500' : PRIORITY_ACCENT[item.priority],
      )}
    >
      <Link to={`/projects/${projectId}/items/${item.id}`}>
        {/* Title + priority */}
        <div className="flex items-start justify-between gap-2">
          <strong className="text-sm font-medium leading-snug">{itemTitle || item.id}</strong>
          <Badge
            variant="secondary"
            className={cn(
              'shrink-0 rounded-full',
              item.priority === 'critical' && 'bg-red-500/10 text-red-700 dark:text-red-400',
            )}
          >
            {item.priority}
          </Badge>
        </div>

        {/* Step + phase status */}
        <div className="mt-1.5 flex items-center gap-1.5 text-xs">
          {isActive && <ActivityPulse className="mr-0.5" />}
          {evaluation.current_step_id ? (
            <span className="text-muted-foreground">{formatStepLabel(evaluation.current_step_id)}</span>
          ) : (
            <span className="text-muted-foreground">{item.classification}</span>
          )}
          {evaluation.phase_status && (
            <StatusBadge status={evaluation.phase_status} className="h-4 gap-1 px-1.5 text-[10px] [&_svg]:size-2.5" />
          )}
          <span className="ml-auto shrink-0 text-[11px] tabular-nums text-muted-foreground/60">
            {formatRelativeTime(item.updated_at, { compact: true })}
          </span>
        </div>

        {/* Escalation alert */}
        {isEscalated && (
          <div className="mt-1.5 flex items-center gap-1 text-[11px] text-destructive">
            <AlertTriangleIcon className="size-3 shrink-0" />
            <span className="truncate">
              Escalated{item.escalation_reason ? `: ${item.escalation_reason.replace(/_/g, ' ')}` : ''}
            </span>
          </div>
        )}

        {/* Attention badges */}
        {evaluation.attention_badges.length > 0 && (
          <div className="mt-1 flex flex-wrap gap-1">
            {evaluation.attention_badges.map((badge) => (
              <Badge key={badge} variant="destructive" className="h-4 px-1.5 text-[10px]">
                {badge.replace(/_/g, ' ')}
              </Badge>
            ))}
          </div>
        )}
      </Link>
    </Card>
  )
}

// ── Board Column ───────────────────────────────────────────────

function BoardColumn({ col, items, projectId }: { col: BoardStatus; items: ItemSummary[]; projectId: string }) {
  const [showAll, setShowAll] = useState(false)
  const sorted = useMemo(() => sortItems(items), [items])
  const isDone = col === 'DONE'
  const truncated = isDone && !showAll && sorted.length > DONE_VISIBLE_COUNT
  const visible = truncated ? sorted.slice(0, DONE_VISIBLE_COUNT) : sorted
  const summary = columnSummary(items, col)

  return (
    <Card size="sm" className="gap-3">
      <CardHeader className="flex-row items-center justify-between gap-3">
        <div className="space-y-0.5">
          <CardTitle>{col}</CardTitle>
          {summary && <p className="text-[11px] text-muted-foreground">{summary}</p>}
        </div>
        <Badge variant="outline" className="rounded-full px-3">
          {items.length}
        </Badge>
      </CardHeader>
      <CardContent className="space-y-2">
        {visible.length > 0 ? (
          <>
            {visible.map((s) => (
              <BoardItemCard key={s.item.id} summary={s} projectId={projectId} />
            ))}
            {truncated && (
              <button
                type="button"
                onClick={() => setShowAll(true)}
                className="flex w-full items-center justify-center gap-1 rounded-md py-1.5 text-xs text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
              >
                <ChevronDownIcon className="size-3" />
                Show {sorted.length - DONE_VISIBLE_COUNT} more
              </button>
            )}
          </>
        ) : (
          <EmptyState variant="inline" description="No items in this lane." className="px-0 py-2" />
        )}
      </CardContent>
    </Card>
  )
}

// ── Page ───────────────────────────────────────────────────────

export default function BoardPage() {
  const projectId = useRequiredProjectId()
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const { data: itemSummaries, error, isError, isFetching, isLoading, refetch } = useQuery(itemsQuery(projectId))
  const [formOpen, setFormOpen] = useState(false)
  const form = useForm<CreateItemForm>({
    defaultValues: initialCreateItemForm,
  })

  const createItemMutation = useMutation({
    mutationFn: (values: CreateItemForm) =>
      createItem(projectId, {
        title: values.title,
        description: values.description,
        acceptance_criteria: values.acceptanceCriteria,
      }),
    onSuccess: (detail) => {
      queryClient.invalidateQueries({ queryKey: queryKeys.items(projectId) })
      queryClient.setQueryData(queryKeys.item(projectId, detail.item.id), detail)
      handleSheetOpenChange(false)
      toast.success('Item created.')
      navigate(`/projects/${projectId}/items/${detail.item.id}`)
    },
    onError: (error) => {
      showErrorToast('Item creation failed.', error)
    },
  })

  function handleSheetOpenChange(open: boolean) {
    setFormOpen(open)
    if (!open) {
      form.reset(initialCreateItemForm)
      createItemMutation.reset()
    }
  }

  const columns = useMemo(() => {
    return groupItemSummariesByBoardStatus(itemSummaries ?? [])
  }, [itemSummaries])

  if (isLoading) return <BoardSkeleton />
  if (isError) {
    return <PageQueryError title="Board failed to load" error={error} onRetry={refetch} isRetrying={isFetching} />
  }

  return (
    <div className="space-y-6">
      <PageHeader
        title="Board"
        description="Scan the current board state across all workflow lanes."
        action={
          <Sheet open={formOpen} onOpenChange={handleSheetOpenChange}>
            <SheetTrigger asChild>
              <Button type="button">New item</Button>
            </SheetTrigger>
            <SheetContent side="right" className="overflow-y-auto sm:max-w-2xl">
              <SheetHeader>
                <SheetTitle>Create Item</SheetTitle>
                <SheetDescription>
                  Define the title, context, and acceptance criteria the workflow should drive toward.
                </SheetDescription>
              </SheetHeader>
              <Form {...form}>
                <form
                  onSubmit={form.handleSubmit((values) => createItemMutation.mutate(values))}
                  className="grid gap-4"
                >
                  <FormField
                    control={form.control}
                    name="title"
                    rules={{ required: 'Title is required.' }}
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Title</FormLabel>
                        <FormControl>
                          <Input placeholder="Title" {...field} />
                        </FormControl>
                        <FormMessage />
                      </FormItem>
                    )}
                  />
                  <FormField
                    control={form.control}
                    name="description"
                    rules={{ required: 'Description is required.' }}
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Description</FormLabel>
                        <FormControl>
                          <Textarea placeholder="Description" rows={5} {...field} />
                        </FormControl>
                        <FormMessage />
                      </FormItem>
                    )}
                  />
                  <FormField
                    control={form.control}
                    name="acceptanceCriteria"
                    rules={{ required: 'Acceptance criteria are required.' }}
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Acceptance criteria</FormLabel>
                        <FormControl>
                          <Textarea placeholder="Acceptance criteria" rows={5} {...field} />
                        </FormControl>
                        <FormMessage />
                      </FormItem>
                    )}
                  />
                  <div className="flex items-center gap-3">
                    <Button type="submit" disabled={createItemMutation.isPending}>
                      {createItemMutation.isPending ? 'Creating…' : 'Create item'}
                    </Button>
                    <Button type="button" variant="outline" onClick={() => handleSheetOpenChange(false)}>
                      Cancel
                    </Button>
                  </div>
                </form>
              </Form>
            </SheetContent>
          </Sheet>
        }
      />

      <div className="grid gap-4 xl:grid-cols-4">
        {boardStatuses.map((col) => (
          <BoardColumn key={col} col={col} items={columns[col]} projectId={projectId} />
        ))}
      </div>
    </div>
  )
}
