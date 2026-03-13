import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useForm } from 'react-hook-form'
import { useNavigate } from 'react-router'
import { toast } from 'sonner'
import { createProject } from '../api/client'
import { queryKeys } from '../api/queries'
import { showErrorToast } from '../lib/toast'
import { Button } from './ui/button'
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from './ui/dialog'
import { Form, FormControl, FormField, FormItem, FormLabel, FormMessage } from './ui/form'
import { Input } from './ui/input'

type CreateProjectForm = {
  name: string
  path: string
  defaultBranch: string
}

const initialValues: CreateProjectForm = {
  name: '',
  path: '',
  defaultBranch: '',
}

type RegisterProjectDialogProps = {
  open: boolean
  onOpenChange: (open: boolean) => void
}

export function RegisterProjectDialog({ open, onOpenChange }: RegisterProjectDialogProps): React.JSX.Element {
  const queryClient = useQueryClient()
  const navigate = useNavigate()
  const form = useForm<CreateProjectForm>({ defaultValues: initialValues })

  const mutation = useMutation({
    mutationFn: (values: CreateProjectForm) =>
      createProject({
        name: values.name || undefined,
        path: values.path,
        default_branch: values.defaultBranch || undefined,
      }),
    onSuccess: (project) => {
      queryClient.invalidateQueries({ queryKey: queryKeys.projects() })
      handleOpenChange(false)
      toast.success('Project registered.')
      navigate(`/projects/${project.id}`)
    },
    onError: (error) => {
      showErrorToast('Project registration failed.', error)
    },
  })

  function handleOpenChange(next: boolean) {
    onOpenChange(next)
    if (!next) {
      form.reset(initialValues)
      mutation.reset()
    }
  }

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      <DialogContent className="sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>Register Project</DialogTitle>
          <DialogDescription>
            Point Ingot at a repository path and define the default branch it should target.
          </DialogDescription>
        </DialogHeader>
        <Form {...form}>
          <form onSubmit={form.handleSubmit((values) => mutation.mutate(values))} className="grid gap-4">
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
            <div className="flex items-center gap-3">
              <Button type="submit" disabled={mutation.isPending}>
                {mutation.isPending ? 'Registering…' : 'Register project'}
              </Button>
              <Button type="button" variant="outline" onClick={() => handleOpenChange(false)}>
                Cancel
              </Button>
            </div>
          </form>
        </Form>
      </DialogContent>
    </Dialog>
  )
}
