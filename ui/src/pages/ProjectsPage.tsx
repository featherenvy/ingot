import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { useForm } from 'react-hook-form'
import { Link } from 'react-router'
import { toast } from 'sonner'
import { ApiError, createProject } from '../api/client'
import { projectsQuery, queryKeys } from '../api/queries'
import { ListCardsSkeleton, PageHeaderSkeleton } from '../components/PageSkeletons'
import { Alert, AlertDescription, AlertTitle } from '../components/ui/alert'
import { Badge } from '../components/ui/badge'
import { Button } from '../components/ui/button'
import { Card, CardContent } from '../components/ui/card'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from '../components/ui/dialog'
import { Form, FormControl, FormField, FormItem, FormLabel, FormMessage } from '../components/ui/form'
import { Input } from '../components/ui/input'

type CreateProjectForm = {
  name: string
  path: string
  defaultBranch: string
}

const initialCreateProjectForm: CreateProjectForm = {
  name: '',
  path: '',
  defaultBranch: '',
}

export default function ProjectsPage() {
  const queryClient = useQueryClient()
  const { data: projects, isLoading } = useQuery(projectsQuery())
  const [dialogOpen, setDialogOpen] = useState(false)
  const form = useForm<CreateProjectForm>({
    defaultValues: initialCreateProjectForm,
  })

  const createProjectMutation = useMutation({
    mutationFn: (values: CreateProjectForm) =>
      createProject({
        name: values.name || undefined,
        path: values.path,
        default_branch: values.defaultBranch || undefined,
      }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.projects() })
      handleDialogOpenChange(false)
      toast.success('Project registered.')
    },
  })

  function handleDialogOpenChange(open: boolean) {
    setDialogOpen(open)
    if (!open) {
      form.reset(initialCreateProjectForm)
      createProjectMutation.reset()
    }
  }

  if (isLoading) {
    return (
      <div className="space-y-8">
        <div className="flex flex-col gap-4 sm:flex-row sm:items-start sm:justify-between">
          <PageHeaderSkeleton width="w-40" />
          <div className="h-8 w-32 rounded-lg bg-muted animate-pulse" />
        </div>
        <ListCardsSkeleton />
      </div>
    )
  }

  return (
    <div className="space-y-8">
      <div className="flex flex-col gap-4 sm:flex-row sm:items-start sm:justify-between">
        <div className="space-y-1">
          <h2 className="text-2xl font-semibold tracking-tight">Projects</h2>
          <p className="text-sm text-muted-foreground">
            Register repositories and jump into the boards already under management.
          </p>
        </div>
        <Dialog open={dialogOpen} onOpenChange={handleDialogOpenChange}>
          <DialogTrigger asChild>
            <Button type="button">Register project</Button>
          </DialogTrigger>
          <DialogContent className="sm:max-w-xl">
            <DialogHeader>
              <DialogTitle>Register Project</DialogTitle>
              <DialogDescription>
                Point Ingot at a repository path and define the default branch it should target.
              </DialogDescription>
            </DialogHeader>
            <Form {...form}>
              <form onSubmit={form.handleSubmit((values) => createProjectMutation.mutate(values))} className="grid gap-4">
                <FormField
                  control={form.control}
                  name="name"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Name</FormLabel>
                      <FormControl>
                        <Input placeholder="Name (optional)" {...field} />
                      </FormControl>
                      <FormMessage />
                    </FormItem>
                  )}
                />
                <FormField
                  control={form.control}
                  name="path"
                  rules={{ required: 'Repository path is required.' }}
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Repository path</FormLabel>
                      <FormControl>
                        <Input placeholder="Repository path" {...field} />
                      </FormControl>
                      <FormMessage />
                    </FormItem>
                  )}
                />
                <FormField
                  control={form.control}
                  name="defaultBranch"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Default branch</FormLabel>
                      <FormControl>
                        <Input placeholder="Default branch (optional)" {...field} />
                      </FormControl>
                      <FormMessage />
                    </FormItem>
                  )}
                />
                {createProjectMutation.error instanceof ApiError && (
                  <Alert variant="destructive">
                    <AlertTitle>Project registration failed</AlertTitle>
                    <AlertDescription>{createProjectMutation.error.message}</AlertDescription>
                  </Alert>
                )}
                <div className="flex items-center gap-3">
                  <Button type="submit" disabled={createProjectMutation.isPending}>
                    {createProjectMutation.isPending ? 'Registering…' : 'Register project'}
                  </Button>
                  <Button type="button" variant="outline" onClick={() => handleDialogOpenChange(false)}>
                    Cancel
                  </Button>
                </div>
              </form>
            </Form>
          </DialogContent>
        </Dialog>
      </div>

      {projects && projects.length > 0 ? (
        <div className="grid gap-3">
          {projects.map((project) => (
            <Link key={project.id} to={`/projects/${project.id}`} className="group">
              <Card size="sm" className="transition-colors group-hover:bg-muted/40">
                <CardContent className="flex items-center gap-4 py-1">
                  <span
                    className="size-3 shrink-0 rounded-full border border-black/10"
                    style={{ backgroundColor: project.color }}
                  />
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
        <Card className="max-w-xl">
          <CardContent className="py-6 text-sm text-muted-foreground">No projects registered.</CardContent>
        </Card>
      )}
    </div>
  )
}
