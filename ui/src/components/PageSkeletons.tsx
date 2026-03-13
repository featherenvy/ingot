import { Card, CardContent, CardHeader } from './ui/card'
import { Skeleton } from './ui/skeleton'

function placeholderKeys(prefix: string, count: number) {
  return Array.from({ length: count }, (_, position) => `${prefix}-${position + 1}`)
}

export function PageHeaderSkeleton({ width = 'w-56' }: { width?: string }) {
  return (
    <div className="space-y-2">
      <Skeleton className={`h-8 ${width}`} />
      <Skeleton className="h-4 w-full max-w-xl" />
    </div>
  )
}

export function StatCardsSkeleton({ count = 4 }: { count?: number }) {
  return (
    <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-4">
      {placeholderKeys('stat-card', count).map((key) => (
        <Card key={key} size="sm">
          <CardHeader className="gap-3">
            <Skeleton className="h-6 w-20 rounded-full" />
            <Skeleton className="h-10 w-16" />
          </CardHeader>
          <CardContent className="pt-0">
            <Skeleton className="h-4 w-36" />
          </CardContent>
        </Card>
      ))}
    </div>
  )
}

export function ListCardsSkeleton({ count = 3 }: { count?: number }) {
  return (
    <div className="grid gap-3">
      {placeholderKeys('list-card', count).map((key) => (
        <Card key={key} size="sm">
          <CardContent className="flex items-center gap-4 py-1">
            <Skeleton className="size-3 rounded-full" />
            <div className="min-w-0 flex-1 space-y-2">
              <Skeleton className="h-4 w-32" />
              <Skeleton className="h-4 w-full max-w-md" />
            </div>
            <Skeleton className="h-6 w-16 rounded-full" />
          </CardContent>
        </Card>
      ))}
    </div>
  )
}

export function TableCardSkeleton({ columns, rows = 5 }: { columns: number; rows?: number }) {
  const headerKeys = placeholderKeys('table-head', columns)
  const rowKeys = placeholderKeys('table-row', rows)
  const columnKeys = placeholderKeys('table-cell', columns)

  return (
    <Card className="gap-0">
      <CardHeader className="space-y-2 border-b">
        <Skeleton className="h-6 w-40" />
        <Skeleton className="h-4 w-full max-w-lg" />
      </CardHeader>
      <CardContent className="space-y-4 px-4 py-4">
        <div className="grid gap-3">
          <div className="grid gap-3" style={{ gridTemplateColumns: `repeat(${columns}, minmax(0, 1fr))` }}>
            {headerKeys.map((key) => (
              <Skeleton key={key} className="h-4 w-16" />
            ))}
          </div>
          {rowKeys.map((rowKey) => (
            <div
              key={rowKey}
              className="grid gap-3"
              style={{ gridTemplateColumns: `repeat(${columns}, minmax(0, 1fr))` }}
            >
              {columnKeys.map((columnKey) => (
                <Skeleton key={`${rowKey}-${columnKey}`} className="h-5 w-full" />
              ))}
            </div>
          ))}
        </div>
      </CardContent>
    </Card>
  )
}

export function BoardSkeleton() {
  const columnKeys = placeholderKeys('board-column', 4)
  const itemKeys = placeholderKeys('board-item', 3)

  return (
    <div className="space-y-8">
      <PageHeaderSkeleton width="w-40" />
      <Skeleton className="h-8 w-24" />
      <div className="grid gap-4 xl:grid-cols-4">
        {columnKeys.map((columnKey) => (
          <Card key={columnKey} size="sm" className="gap-3">
            <CardHeader className="flex-row items-center justify-between gap-3">
              <Skeleton className="h-5 w-20" />
              <Skeleton className="h-6 w-10 rounded-full" />
            </CardHeader>
            <CardContent className="space-y-3">
              {itemKeys.map((itemKey) => (
                <div key={`${columnKey}-${itemKey}`} className="rounded-lg border border-border px-3 py-3">
                  <div className="flex items-start justify-between gap-3">
                    <Skeleton className="h-4 w-24" />
                    <Skeleton className="h-5 w-14 rounded-full" />
                  </div>
                  <Skeleton className="mt-3 h-3 w-full" />
                </div>
              ))}
            </CardContent>
          </Card>
        ))}
      </div>
    </div>
  )
}

export function ItemDetailSkeleton() {
  return (
    <div className="space-y-6">
      <Skeleton className="h-4 w-56" />
      <div className="space-y-2">
        <Skeleton className="h-8 w-80" />
        <Skeleton className="h-4 w-full max-w-3xl" />
      </div>
      <Card>
        <CardHeader className="space-y-2 border-b">
          <Skeleton className="h-6 w-40" />
          <Skeleton className="h-4 w-full max-w-lg" />
        </CardHeader>
        <CardContent className="space-y-4 py-4">
          <div className="flex flex-wrap gap-2">
            <Skeleton className="h-8 w-32" />
            <Skeleton className="h-8 w-36" />
          </div>
          <Skeleton className="h-24 w-full" />
        </CardContent>
      </Card>
      <TableCardSkeleton columns={6} rows={4} />
    </div>
  )
}
