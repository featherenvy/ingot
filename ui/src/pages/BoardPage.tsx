import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useMemo, useState } from 'react'
import { useForm } from 'react-hook-form'
import { Link, useNavigate } from 'react-router'
import { ApiError, createItem } from '../api/client'
import { itemsQuery, queryKeys } from '../api/queries'
import { BoardSkeleton } from '../components/PageSkeletons'
import { Alert, AlertDescription, AlertTitle } from '../components/ui/alert'
import { Badge } from '../components/ui/badge'
import { Button } from '../components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '../components/ui/card'
import { Form, FormControl, FormField, FormItem, FormLabel, FormMessage } from '../components/ui/form'
import { Input } from '../components/ui/input'
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle, SheetTrigger } from '../components/ui/sheet'
import { Textarea } from '../components/ui/textarea'
import { useRequiredProjectId } from '../hooks/useRequiredRouteParam'
import { boardStatuses, groupItemSummariesByBoardStatus } from '../itemSummaries'
import { isActivePhaseStatus } from '../lib/status'

type CreateItemForm = {
  title: string
  description: string
  acceptanceCriteria: string
}

const initialCreateItemForm: CreateItemForm = {
  title: '',
  description: '',
  acceptanceCriteria: '',
}

export default function BoardPage() {
  const projectId = useRequiredProjectId()
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const { data: itemSummaries, isLoading } = useQuery(itemsQuery(projectId))
  const [formOpen, setFormOpen] = useState(false)
  const form = useForm<CreateItemForm>({
    defaultValues: initialCreateItemForm,
  })

  const createItemMutation = useMutation({
    mutationFn: (values: CreateItemForm) =>
      createItem(projectId, {
        title: values.title,
        description: values.description,
        acceptance_criteria: values.acceptanceCriteria,
      }),
    onSuccess: (detail) => {
      queryClient.invalidateQueries({ queryKey: queryKeys.items(projectId) })
      queryClient.setQueryData(queryKeys.item(projectId, detail.item.id), detail)
      handleSheetOpenChange(false)
      navigate(`/projects/${projectId}/items/${detail.item.id}`)
    },
  })

  function handleSheetOpenChange(open: boolean) {
    setFormOpen(open)
    if (!open) {
      form.reset(initialCreateItemForm)
      createItemMutation.reset()
    }
  }

  const columns = useMemo(() => {
    return groupItemSummariesByBoardStatus(itemSummaries ?? [])
  }, [itemSummaries])

  if (isLoading) return <BoardSkeleton />

  return (
    <div className="space-y-8">
      <div className="flex flex-col gap-4 sm:flex-row sm:items-start sm:justify-between">
        <div className="space-y-1">
          <h2 className="text-2xl font-semibold tracking-tight">Board</h2>
          <p className="text-sm text-muted-foreground">
            Create new work and scan the current board state across all workflow lanes.
          </p>
        </div>
        <Sheet open={formOpen} onOpenChange={handleSheetOpenChange}>
          <SheetTrigger asChild>
            <Button type="button">New item</Button>
          </SheetTrigger>
          <SheetContent side="right" className="overflow-y-auto sm:max-w-2xl">
            <SheetHeader>
              <SheetTitle>Create Item</SheetTitle>
              <SheetDescription>
                Define the title, context, and acceptance criteria the workflow should drive toward.
              </SheetDescription>
            </SheetHeader>
            <Form {...form}>
              <form onSubmit={form.handleSubmit((values) => createItemMutation.mutate(values))} className="grid gap-4">
                <FormField
                  control={form.control}
                  name="title"
                  rules={{ required: 'Title is required.' }}
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Title</FormLabel>
                      <FormControl>
                        <Input placeholder="Title" {...field} />
                      </FormControl>
                      <FormMessage />
                    </FormItem>
                  )}
                />
                <FormField
                  control={form.control}
                  name="description"
                  rules={{ required: 'Description is required.' }}
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Description</FormLabel>
                      <FormControl>
                        <Textarea placeholder="Description" rows={5} {...field} />
                      </FormControl>
                      <FormMessage />
                    </FormItem>
                  )}
                />
                <FormField
                  control={form.control}
                  name="acceptanceCriteria"
                  rules={{ required: 'Acceptance criteria are required.' }}
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Acceptance criteria</FormLabel>
                      <FormControl>
                        <Textarea placeholder="Acceptance criteria" rows={5} {...field} />
                      </FormControl>
                      <FormMessage />
                    </FormItem>
                  )}
                />
                {createItemMutation.error instanceof ApiError && (
                  <Alert variant="destructive">
                    <AlertTitle>Item creation failed</AlertTitle>
                    <AlertDescription>{createItemMutation.error.message}</AlertDescription>
                  </Alert>
                )}
                <div className="flex items-center gap-3">
                  <Button type="submit" disabled={createItemMutation.isPending}>
                    {createItemMutation.isPending ? 'Creating…' : 'Create item'}
                  </Button>
                  <Button type="button" variant="outline" onClick={() => handleSheetOpenChange(false)}>
                    Cancel
                  </Button>
                </div>
              </form>
            </Form>
          </SheetContent>
        </Sheet>
      </div>

      <div className="grid gap-4 xl:grid-cols-4">
        {boardStatuses.map((col) => (
          <Card key={col} size="sm" className="gap-3">
            <CardHeader className="flex-row items-center justify-between gap-3">
              <CardTitle>{col}</CardTitle>
              <Badge variant="outline" className="rounded-full px-3">
                {columns[col].length}
              </Badge>
            </CardHeader>
            <CardContent className="space-y-3">
              {columns[col].length > 0 ? (
                columns[col].map(({ item, title: itemTitle, evaluation }) => (
                  <Card key={item.id} asChild size="sm" className="px-3 hover:bg-muted/40">
                    <Link to={`/projects/${projectId}/items/${item.id}`}>
                      <div className="flex items-start justify-between gap-3">
                        <strong className="text-sm font-medium">{itemTitle || item.id}</strong>
                        <Badge variant="secondary" className="rounded-full">
                          {item.priority}
                        </Badge>
                      </div>
                      <div className="mt-2 flex items-center gap-2 text-xs text-muted-foreground">
                        {isActivePhaseStatus(evaluation.phase_status) && (
                          <span className="relative flex size-2">
                            <span className="absolute inline-flex size-full animate-ping rounded-full bg-primary opacity-75" />
                            <span className="relative inline-flex size-2 rounded-full bg-primary" />
                          </span>
                        )}
                        <span>
                          {item.classification} · {evaluation.phase_status ?? 'unknown'}
                        </span>
                      </div>
                    </Link>
                  </Card>
                ))
              ) : (
                <p className="text-sm text-muted-foreground">No items in this lane.</p>
              )}
            </CardContent>
          </Card>
        ))}
      </div>
    </div>
  )
}
