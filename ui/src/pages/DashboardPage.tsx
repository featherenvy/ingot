import { useQuery } from '@tanstack/react-query'
import { itemsQuery } from '../api/queries'
import { PageHeaderSkeleton, StatCardsSkeleton } from '../components/PageSkeletons'
import { Badge } from '../components/ui/badge'
import { Card, CardContent, CardHeader, CardTitle } from '../components/ui/card'
import { useRequiredProjectId } from '../hooks/useRequiredRouteParam'
import { boardStatuses, countItemSummariesByBoardStatus, createEmptyBoardCounts } from '../itemSummaries'

export default function DashboardPage() {
  const projectId = useRequiredProjectId()
  const { data: itemSummaries, isLoading } = useQuery(itemsQuery(projectId))

  if (isLoading) {
    return (
      <div className="space-y-6">
        <PageHeaderSkeleton width="w-44" />
        <StatCardsSkeleton />
      </div>
    )
  }

  const counts = itemSummaries ? countItemSummariesByBoardStatus(itemSummaries) : createEmptyBoardCounts()

  return (
    <div className="space-y-6">
      <div className="space-y-1">
        <h2 className="text-2xl font-semibold tracking-tight">Dashboard</h2>
        <p className="text-sm text-muted-foreground">A quick snapshot of how work is distributed across the board.</p>
      </div>

      <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-4">
        {boardStatuses.map((col) => (
          <Card key={col} size="sm">
            <CardHeader className="gap-3">
              <Badge variant="outline" className="w-fit rounded-full px-3">
                {col}
              </Badge>
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
