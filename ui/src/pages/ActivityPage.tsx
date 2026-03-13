import { useQuery } from '@tanstack/react-query'
import { Loader2Icon } from 'lucide-react'
import { useState } from 'react'
import { projectActivityQuery } from '../api/queries'
import { CodeBlock } from '../components/CodeBlock'
import { DataTable } from '../components/DataTable'
import { EmptyState } from '../components/EmptyState'
import { PageHeader } from '../components/PageHeader'
import { PageQueryError } from '../components/PageQueryError'
import { PageHeaderSkeleton, TableCardSkeleton } from '../components/PageSkeletons'
import { Timestamp } from '../components/Timestamp'
import { Button } from '../components/ui/button'
import { Collapsible, CollapsibleTrigger } from '../components/ui/collapsible'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '../components/ui/table'
import { useRequiredProjectId } from '../hooks/useRequiredRouteParam'

const ACTIVITY_PAGE_SIZE = 50

function CollapsiblePayload({ payload }: { payload: unknown }) {
  const [expanded, setExpanded] = useState(false)
  const text = JSON.stringify(payload, null, 2)
  const isLong = text.split('\n').length > 3

  if (!isLong) {
    return <CodeBlock value={text} wrap copyLabel="Copy payload" />
  }

  return (
    <Collapsible open={expanded} onOpenChange={setExpanded}>
      <div className="relative">
        <CodeBlock
          value={text}
          wrap
          copyLabel="Copy payload"
          maxHeightClassName={expanded ? 'max-h-64' : 'max-h-[4.5rem]'}
        />
        {!expanded && (
          <div className="pointer-events-none absolute inset-x-0 bottom-0 h-8 rounded-b-lg bg-gradient-to-t from-background via-background/95 to-transparent" />
        )}
      </div>
      <CollapsibleTrigger asChild>
        <button type="button" className="mt-1 text-xs font-medium text-muted-foreground hover:text-foreground">
          {expanded ? 'Show less' : 'Show more'}
        </button>
      </CollapsibleTrigger>
    </Collapsible>
  )
}

export default function ActivityPage() {
  const projectId = useRequiredProjectId()
  const [page, setPage] = useState(0)
  const offset = page * ACTIVITY_PAGE_SIZE
  const {
    data: activity,
    error,
    isError,
    isFetching,
    isLoading,
    refetch,
  } = useQuery(projectActivityQuery(projectId, { limit: ACTIVITY_PAGE_SIZE, offset }))

  if (isLoading) {
    return (
      <div className="space-y-6">
        <PageHeaderSkeleton width="w-32" />
        <TableCardSkeleton columns={4} rows={6} />
      </div>
    )
  }
  if (isError) {
    return <PageQueryError title="Activity failed to load" error={error} onRetry={refetch} isRetrying={isFetching} />
  }

  const hasActivity = !!activity && activity.length > 0
  const hasPreviousPage = page > 0
  const hasNextPage = (activity?.length ?? 0) === ACTIVITY_PAGE_SIZE
  const rangeStart = offset + 1
  const rangeEnd = offset + (activity?.length ?? 0)

  return (
    <div className="space-y-6">
      <PageHeader
        title="Activity"
        description="Audit the event stream for this project. Results are paged 50 events at a time to keep the table bounded."
      />

      {hasActivity ? (
        <DataTable
          title="Project activity"
          description={`Showing events ${rangeStart}-${rangeEnd}. Use pagination to inspect older activity without rendering the full log.`}
          footer={
            <>
              <p className="flex items-center gap-2 text-sm text-muted-foreground">
                Page {page + 1}
                {isFetching ? (
                  <output
                    className="inline-flex items-center gap-1"
                    aria-label="Loading activity page"
                    aria-live="polite"
                  >
                    <Loader2Icon className="size-3 animate-spin" />
                    Loading…
                  </output>
                ) : null}
              </p>
              <div className="flex gap-2">
                <Button
                  type="button"
                  variant="outline"
                  onClick={() => setPage((current) => Math.max(current - 1, 0))}
                  disabled={!hasPreviousPage || isFetching}
                >
                  Newer
                </Button>
                <Button
                  type="button"
                  variant="outline"
                  onClick={() => setPage((current) => current + 1)}
                  disabled={!hasNextPage || isFetching}
                >
                  Older
                </Button>
              </div>
            </>
          }
          footerClassName="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between"
        >
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>When</TableHead>
                <TableHead>Event</TableHead>
                <TableHead>Entity</TableHead>
                <TableHead>Payload</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {activity.map((entry) => (
                <TableRow key={entry.id}>
                  <TableCell className="align-top">
                    <Timestamp value={entry.created_at} />
                  </TableCell>
                  <TableCell className="align-top">{entry.event_type}</TableCell>
                  <TableCell className="align-top whitespace-normal">
                    {entry.entity_type}: <code>{entry.entity_id}</code>
                  </TableCell>
                  <TableCell className="align-top whitespace-normal">
                    <CollapsiblePayload payload={entry.payload} />
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </DataTable>
      ) : (
        <EmptyState
          description={page === 0 ? 'No activity yet.' : 'No activity on this page.'}
          action={
            page > 0 ? (
              <Button type="button" variant="outline" onClick={() => setPage((current) => Math.max(current - 1, 0))}>
                Back to newer activity
              </Button>
            ) : undefined
          }
        />
      )}
    </div>
  )
}
