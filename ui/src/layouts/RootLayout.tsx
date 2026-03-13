import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useEffect } from 'react'
import { Link, Outlet, useLocation, useMatch } from 'react-router'
import { healthQuery } from '../api/queries'
import ErrorBoundary from '../components/ErrorBoundary'
import { ProjectSwitcher } from '../components/ProjectSwitcher'
import { Alert, AlertDescription, AlertTitle } from '../components/ui/alert'
import { Button } from '../components/ui/button'
import { Separator } from '../components/ui/separator'
import { cn } from '../lib/utils'
import { useConnectionStore } from '../stores/connection'

const NAV_ITEMS = [
  { label: 'Dashboard', path: '' },
  { label: 'Board', path: '/board' },
  { label: 'Jobs', path: '/jobs' },
  { label: 'Workspaces', path: '/workspaces' },
  { label: 'Activity', path: '/activity' },
  { label: 'Config', path: '/config' },
] as const

function getActiveNav(currentPath: string, basePath: string): string {
  if (currentPath === basePath) return ''
  if (currentPath.startsWith(`${basePath}/board`)) return '/board'
  if (currentPath.startsWith(`${basePath}/items/`)) return '/board'
  if (currentPath.startsWith(`${basePath}/jobs`)) return '/jobs'
  if (currentPath.startsWith(`${basePath}/workspaces`)) return '/workspaces'
  if (currentPath.startsWith(`${basePath}/activity`)) return '/activity'
  if (currentPath.startsWith(`${basePath}/config`)) return '/config'
  return ''
}

export default function RootLayout(): React.JSX.Element {
  const queryClient = useQueryClient()
  const location = useLocation()
  const { status: wsStatus, connect } = useConnectionStore()
  const { data: health, isLoading: isHealthLoading } = useQuery(healthQuery())

  const projectMatch = useMatch('/projects/:projectId/*')
  const projectId = projectMatch?.params.projectId ?? null
  const basePath = projectId ? `/projects/${projectId}` : null
  const activeNav = basePath ? getActiveNav(location.pathname, basePath) : null

  useEffect(() => {
    connect(queryClient)
  }, [connect, queryClient])

  return (
    <div className="min-h-screen bg-background text-foreground">
      <header className="sticky top-0 z-40 border-b border-border/50 bg-background/95 backdrop-blur supports-[backdrop-filter]:bg-background/80">
        <div className="flex h-12 w-full items-center gap-3 px-6">
          {/* Logo */}
          <Link to="/" className="mr-1 text-sm font-semibold tracking-tight transition-colors hover:text-foreground/80">
            Ingot
          </Link>

          {/* Project switcher */}
          <Separator orientation="vertical" className="!h-4" />
          <ProjectSwitcher activeProjectId={projectId} />

          {/* Project nav */}
          {basePath ? (
            <>
              <Separator orientation="vertical" className="!h-4" />
              <nav className="flex items-center" aria-label="Project navigation">
                {NAV_ITEMS.map((item) => {
                  const isActive = activeNav === item.path
                  return (
                    <Link
                      key={item.path}
                      to={`${basePath}${item.path}`}
                      className={cn(
                        'relative px-2.5 py-1 text-sm transition-colors',
                        isActive ? 'font-medium text-foreground' : 'text-muted-foreground hover:text-foreground',
                      )}
                    >
                      {item.label}
                      {isActive ? <span className="absolute inset-x-1.5 -bottom-[13px] h-px bg-foreground" /> : null}
                    </Link>
                  )
                })}
              </nav>
            </>
          ) : null}

          {/* Status indicators — pushed right */}
          <div className="ml-auto flex items-center gap-3">
            <StatusDot label="daemon" status={isHealthLoading ? 'loading' : health ? 'ok' : 'error'} />
            <StatusDot
              label="ws"
              status={wsStatus === 'connected' ? 'ok' : wsStatus === 'connecting' ? 'loading' : 'error'}
            />
          </div>
        </div>
      </header>

      <main className="flex w-full flex-1 flex-col gap-8 px-6 py-8">
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

type StatusDotProps = {
  label: string
  status: 'ok' | 'loading' | 'error'
}

function StatusDot({ label, status }: StatusDotProps): React.JSX.Element {
  return (
    <div className="flex items-center gap-1.5" title={`${label}: ${status}`}>
      <span
        className={cn(
          'size-1.5 rounded-full',
          status === 'ok' && 'bg-emerald-500',
          status === 'loading' && 'animate-pulse bg-amber-400',
          status === 'error' && 'bg-red-500',
        )}
      />
      <span className="text-xs text-muted-foreground">{label}</span>
    </div>
  )
}
