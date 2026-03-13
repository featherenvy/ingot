import { useQuery } from '@tanstack/react-query'
import { itemsQuery } from '../api/queries'
import { PageHeader } from '../components/PageHeader'
import { PageQueryError } from '../components/PageQueryError'
import { PageHeaderSkeleton, StatCardsSkeleton } from '../components/PageSkeletons'
import { StatusBadge } from '../components/StatusBadge'
import { Card, CardContent, CardHeader, CardTitle } from '../components/ui/card'
import { useRequiredProjectId } from '../hooks/useRequiredRouteParam'
import { boardStatuses, countItemSummariesByBoardStatus, createEmptyBoardCounts } from '../itemSummaries'

export default function DashboardPage() {
  const projectId = useRequiredProjectId()
  const { data: itemSummaries, error, isError, isFetching, isLoading, refetch } = useQuery(itemsQuery(projectId))

  if (isLoading) {
    return (
      <div className="space-y-6">
        <PageHeaderSkeleton width="w-44" />
        <StatCardsSkeleton />
      </div>
    )
  }
  if (isError) {
    return <PageQueryError title="Dashboard failed to load" error={error} onRetry={refetch} isRetrying={isFetching} />
  }

  const counts = itemSummaries ? countItemSummariesByBoardStatus(itemSummaries) : createEmptyBoardCounts()

  return (
    <div className="space-y-6">
      <PageHeader title="Dashboard" description="A quick snapshot of how work is distributed across the board." />

      <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-4">
        {boardStatuses.map((col) => (
          <Card key={col} size="sm">
            <CardHeader className="gap-3">
              <StatusBadge status={col} className="w-fit" />
              <CardTitle className="text-4xl font-semibold tracking-tight">{counts[col]}</CardTitle>
            </CardHeader>
            <CardContent className="pt-0 text-sm text-muted-foreground">
              {counts[col] === 1 ? 'item currently in this lane' : 'items currently in this lane'}
            </CardContent>
          </Card>
        ))}
      </div>
    </div>
  )
}
