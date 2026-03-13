import { useQuery } from '@tanstack/react-query'
import { useEffect } from 'react'
import { Link, Outlet, useLocation } from 'react-router'
import { projectsQuery } from '../api/queries'
import { Separator } from '../components/ui/separator'
import { Tabs, TabsList, TabsTrigger } from '../components/ui/tabs'
import { useRequiredProjectId } from '../hooks/useRequiredRouteParam'
import { useProjectsStore } from '../stores/projects'

export default function ProjectLayout() {
  const projectId = useRequiredProjectId()
  const location = useLocation()
  const { data: projects } = useQuery(projectsQuery())
  const project = projects?.find((p) => p.id === projectId)
  const setActive = useProjectsStore((s) => s.setActive)
  const basePath = `/projects/${projectId}`
  const currentPath = location.pathname
  const activeTab =
    currentPath === basePath
      ? 'dashboard'
      : currentPath.startsWith(`${basePath}/board`)
        ? 'board'
        : currentPath.startsWith(`${basePath}/jobs`)
          ? 'jobs'
          : currentPath.startsWith(`${basePath}/workspaces`)
            ? 'workspaces'
            : currentPath.startsWith(`${basePath}/activity`)
              ? 'activity'
              : currentPath.startsWith(`${basePath}/config`)
                ? 'config'
                : 'dashboard'

  useEffect(() => {
    setActive(projectId)
    return () => setActive(null)
  }, [projectId, setActive])

  return (
    <div className="space-y-6">
      <div className="space-y-4">
        <div className="flex flex-col gap-2 lg:flex-row lg:items-end lg:justify-between">
          <div className="space-y-2">
            <div className="flex items-center gap-3">
              <span
                className="size-3 rounded-full border border-black/10"
                style={{ backgroundColor: project?.color ?? '#000' }}
              />
              <h1 className="text-2xl font-semibold tracking-tight">{project?.name ?? projectId}</h1>
            </div>
            <p className="text-sm text-muted-foreground">
              Track board state, jobs, activity, and operator controls for this project.
            </p>
          </div>
        </div>
        <Separator />
        <Tabs value={activeTab}>
          <TabsList variant="line" className="h-auto flex-wrap justify-start gap-2 bg-transparent p-0">
            <TabsTrigger value="dashboard" asChild className="rounded-full px-4 py-1.5 data-active:bg-secondary">
              <Link to={basePath}>Dashboard</Link>
            </TabsTrigger>
            <TabsTrigger value="board" asChild className="rounded-full px-4 py-1.5 data-active:bg-secondary">
              <Link to={`${basePath}/board`}>Board</Link>
            </TabsTrigger>
            <TabsTrigger value="jobs" asChild className="rounded-full px-4 py-1.5 data-active:bg-secondary">
              <Link to={`${basePath}/jobs`}>Jobs</Link>
            </TabsTrigger>
            <TabsTrigger value="workspaces" asChild className="rounded-full px-4 py-1.5 data-active:bg-secondary">
              <Link to={`${basePath}/workspaces`}>Workspaces</Link>
            </TabsTrigger>
            <TabsTrigger value="activity" asChild className="rounded-full px-4 py-1.5 data-active:bg-secondary">
              <Link to={`${basePath}/activity`}>Activity</Link>
            </TabsTrigger>
            <TabsTrigger value="config" asChild className="rounded-full px-4 py-1.5 data-active:bg-secondary">
              <Link to={`${basePath}/config`}>Config</Link>
            </TabsTrigger>
          </TabsList>
        </Tabs>
      </div>
      <Outlet />
    </div>
  )
}
