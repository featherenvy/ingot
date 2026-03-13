import { useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { projectActivityQuery } from '../api/queries'
import { PageHeaderSkeleton, TableCardSkeleton } from '../components/PageSkeletons'
import { Timestamp } from '../components/Timestamp'
import { Button } from '../components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '../components/ui/card'
import { Collapsible, CollapsibleTrigger } from '../components/ui/collapsible'
import { ScrollArea } from '../components/ui/scroll-area'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '../components/ui/table'
import { useRequiredProjectId } from '../hooks/useRequiredRouteParam'

const ACTIVITY_PAGE_SIZE = 50

function CollapsiblePayload({ payload }: { payload: unknown }) {
  const [expanded, setExpanded] = useState(false)
  const text = JSON.stringify(payload, null, 2)
  const isLong = text.split('\n').length > 3

  if (!isLong) {
    return <pre className="m-0 overflow-x-auto text-xs leading-6 whitespace-pre-wrap break-words">{text}</pre>
  }

  return (
    <Collapsible open={expanded} onOpenChange={setExpanded}>
      <div className="relative">
        <ScrollArea className={expanded ? 'max-h-64 rounded-md' : 'max-h-[4.5rem] rounded-md'}>
          <pre className="m-0 text-xs leading-6 whitespace-pre-wrap break-words">{text}</pre>
        </ScrollArea>
        {!expanded && (
          <div className="pointer-events-none absolute inset-x-0 bottom-6 h-8 bg-gradient-to-t from-background to-transparent" />
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
    isLoading,
    isFetching,
  } = useQuery(projectActivityQuery(projectId, { limit: ACTIVITY_PAGE_SIZE, offset }))

  if (isLoading) {
    return (
      <div className="space-y-6">
        <PageHeaderSkeleton width="w-32" />
        <TableCardSkeleton columns={4} rows={6} />
      </div>
    )
  }

  const hasActivity = !!activity && activity.length > 0
  const hasPreviousPage = page > 0
  const hasNextPage = (activity?.length ?? 0) === ACTIVITY_PAGE_SIZE
  const rangeStart = offset + 1
  const rangeEnd = offset + (activity?.length ?? 0)

  return (
    <div className="space-y-6">
      <div className="space-y-1">
        <h2 className="text-2xl font-semibold tracking-tight">Activity</h2>
        <p className="text-sm text-muted-foreground">
          Audit the event stream for this project. Results are paged 50 events at a time to keep the table bounded.
        </p>
      </div>

      {hasActivity ? (
        <Card className="gap-0">
          <CardHeader className="border-b">
            <CardTitle>Project activity</CardTitle>
            <CardDescription>
              Showing events {rangeStart}-{rangeEnd}. Use pagination to inspect older activity without rendering the
              full log.
            </CardDescription>
          </CardHeader>
          <CardContent className="px-0">
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
          </CardContent>
          <div className="flex flex-col gap-3 border-t px-6 py-4 sm:flex-row sm:items-center sm:justify-between">
            <p className="text-sm text-muted-foreground">
              Page {page + 1}
              {isFetching ? ' · Loading…' : null}
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
          </div>
        </Card>
      ) : (
        <Card>
          <CardContent className="flex flex-col gap-4 py-6">
            <p className="text-sm text-muted-foreground">
              {page === 0 ? 'No activity yet.' : 'No activity on this page.'}
            </p>
            {page > 0 && (
              <div>
                <Button type="button" variant="outline" onClick={() => setPage((current) => Math.max(current - 1, 0))}>
                  Back to newer activity
                </Button>
              </div>
            )}
          </CardContent>
        </Card>
      )}
    </div>
  )
}
