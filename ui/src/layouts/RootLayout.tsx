import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useEffect } from 'react'
import { Link, Outlet, useLocation } from 'react-router'
import { healthQuery } from '../api/queries'
import ErrorBoundary from '../components/ErrorBoundary'
import { Alert, AlertDescription, AlertTitle } from '../components/ui/alert'
import { Badge } from '../components/ui/badge'
import { Button } from '../components/ui/button'
import { useConnectionStore } from '../stores/connection'

export default function RootLayout() {
  const queryClient = useQueryClient()
  const location = useLocation()
  const { status: wsStatus, connect } = useConnectionStore()
  const { data: health } = useQuery(healthQuery())

  useEffect(() => {
    connect(queryClient)
  }, [connect, queryClient])

  return (
    <div className="min-h-screen bg-background text-foreground">
      <header className="border-b border-border bg-background/95 backdrop-blur supports-[backdrop-filter]:bg-background/80">
        <div className="mx-auto flex w-full max-w-7xl flex-col gap-4 px-6 py-4 sm:flex-row sm:items-center sm:justify-between">
          <div className="space-y-1">
            <Link to="/" className="text-lg font-semibold tracking-tight">
              Ingot
            </Link>
            <p className="text-sm text-muted-foreground">
              Review work, dispatch agents, and steer delivery across projects.
            </p>
          </div>
          <div className="flex flex-wrap items-center gap-2">
            <StatusBadge label="daemon" value={health ?? '...'} />
            <StatusBadge label="ws" value={wsStatus} />
          </div>
        </div>
      </header>
      <main className="mx-auto flex w-full max-w-7xl flex-1 flex-col gap-8 px-6 py-8">
        <ErrorBoundary
          resetKey={`${location.pathname}${location.search}${location.hash}`}
          fallback={({ error, reset }) => (
            <Alert variant="destructive" className="max-w-2xl">
              <AlertTitle>Something went wrong</AlertTitle>
              <AlertDescription className="space-y-4">
                <p>This page failed to render. The rest of the app is still available.</p>
                <p className="rounded-md border border-destructive/20 bg-destructive/5 px-3 py-2 font-mono text-xs">
                  {error.message}
                </p>
                <div className="flex flex-wrap gap-3">
                  <Button type="button" onClick={reset}>
                    Try again
                  </Button>
                  <Button asChild variant="outline">
                    <Link to="/">Back to projects</Link>
                  </Button>
                </div>
              </AlertDescription>
            </Alert>
          )}
        >
          <Outlet />
        </ErrorBoundary>
      </main>
    </div>
  )
}

function StatusBadge({ label, value }: { label: string; value: string }) {
  return (
    <Badge variant="outline" className="gap-2 rounded-full bg-background/70 px-3 py-1 text-xs font-medium">
      <span className="uppercase tracking-[0.16em] text-muted-foreground">{label}</span>
      <span>{value}</span>
    </Badge>
  )
}
