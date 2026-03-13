import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { Link, useNavigate } from 'react-router'
import { toast } from 'sonner'
import { createDemoProject } from '../api/client'
import { projectsQuery, queryKeys } from '../api/queries'
import { EmptyState } from '../components/EmptyState'
import { PageHeader } from '../components/PageHeader'
import { PageQueryError } from '../components/PageQueryError'
import { ListCardsSkeleton, PageHeaderSkeleton } from '../components/PageSkeletons'
import { ProjectColorDot } from '../components/ProjectColorDot'
import { RegisterProjectDialog } from '../components/RegisterProjectDialog'
import { Badge } from '../components/ui/badge'
import { Button } from '../components/ui/button'
import { Card, CardContent } from '../components/ui/card'
import { showErrorToast } from '../lib/toast'

export default function ProjectsPage() {
  const { data: projects, error, isError, isFetching, isLoading, refetch } = useQuery(projectsQuery())
  const [dialogOpen, setDialogOpen] = useState(false)
  const queryClient = useQueryClient()
  const navigate = useNavigate()

  const demoMutation = useMutation({
    mutationFn: () => createDemoProject(),
    onSuccess: (result) => {
      queryClient.invalidateQueries({ queryKey: queryKeys.projects() })
      toast.success(`Demo project created with ${result.items_created} items.`)
      navigate(`/projects/${result.project.id}`)
    },
    onError: (error) => {
      showErrorToast('Failed to create demo project.', error)
    },
  })

  if (isLoading) {
    return (
      <div className="space-y-8">
        <div className="flex flex-col gap-4 sm:flex-row sm:items-start sm:justify-between">
          <PageHeaderSkeleton width="w-40" />
          <div className="h-8 w-32 animate-pulse rounded-lg bg-muted" />
        </div>
        <ListCardsSkeleton />
      </div>
    )
  }
  if (isError) {
    return <PageQueryError title="Projects failed to load" error={error} onRetry={refetch} isRetrying={isFetching} />
  }

  return (
    <div className="space-y-8">
      <PageHeader
        title="Projects"
        description="Register repositories and jump into the boards already under management."
        action={
          <div className="flex items-center gap-2">
            <Button
              type="button"
              variant="outline"
              onClick={() => demoMutation.mutate()}
              disabled={demoMutation.isPending}
            >
              {demoMutation.isPending ? 'Creating…' : 'Try demo project'}
            </Button>
            <Button type="button" onClick={() => setDialogOpen(true)}>
              Register project
            </Button>
          </div>
        }
      />
      <RegisterProjectDialog open={dialogOpen} onOpenChange={setDialogOpen} />

      {projects && projects.length > 0 ? (
        <div className="grid gap-3">
          {projects.map((project) => (
            <Link key={project.id} to={`/projects/${project.id}`} className="group">
              <Card size="sm" className="transition-colors group-hover:bg-muted/40">
                <CardContent className="flex items-center gap-4 py-1">
                  <ProjectColorDot color={project.color} />
                  <div className="min-w-0 flex-1">
                    <div className="font-medium">{project.name}</div>
                    <div className="truncate text-sm text-muted-foreground">{project.path}</div>
                  </div>
                  <Badge variant="outline" className="rounded-full px-3">
                    {project.default_branch}
                  </Badge>
                </CardContent>
              </Card>
            </Link>
          ))}
        </div>
      ) : (
        <EmptyState description="No projects registered." />
      )}
    </div>
  )
}
