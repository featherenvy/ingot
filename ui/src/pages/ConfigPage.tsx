import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { useForm } from 'react-hook-form'
import { toast } from 'sonner'
import { ApiError, createAgent, reprobeAgent } from '../api/client'
import { agentsQuery, projectConfigQuery, queryKeys } from '../api/queries'
import { Alert, AlertDescription, AlertTitle } from '../components/ui/alert'
import { Badge } from '../components/ui/badge'
import { Button } from '../components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '../components/ui/card'
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
import { ScrollArea } from '../components/ui/scroll-area'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '../components/ui/select'
import { Skeleton } from '../components/ui/skeleton'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '../components/ui/table'
import { useRequiredProjectId } from '../hooks/useRequiredRouteParam'
import { statusVariant } from '../lib/status'
import type { Agent } from '../types/domain'

type AgentForm = {
  name: string
  provider: string
  model: string
  cliPath: string
}

const initialAgentForm: AgentForm = {
  name: 'Codex CLI',
  provider: 'openai',
  model: 'gpt-5-codex',
  cliPath: 'codex',
}

export default function ConfigPage() {
  const projectId = useRequiredProjectId()
  const queryClient = useQueryClient()
  const { data: config, isLoading: isConfigLoading } = useQuery(projectConfigQuery(projectId))
  const { data: agents, isLoading: isAgentsLoading } = useQuery(agentsQuery())
  const [dialogOpen, setDialogOpen] = useState(false)
  const form = useForm<AgentForm>({
    defaultValues: initialAgentForm,
  })

  const createAgentMutation = useMutation({
    mutationFn: (values: AgentForm) =>
      createAgent({
        name: values.name,
        adapter_kind: 'codex',
        provider: values.provider,
        model: values.model,
        cli_path: values.cliPath,
      }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.agents() })
      handleDialogOpenChange(false)
      toast.success('Agent registered.')
    },
  })

  function handleDialogOpenChange(open: boolean) {
    setDialogOpen(open)
    if (!open) {
      form.reset(initialAgentForm)
      createAgentMutation.reset()
    }
  }

  const refreshAgents = () => {
    queryClient.invalidateQueries({ queryKey: queryKeys.agents() })
  }

  return (
    <div className="space-y-8">
      <div className="flex flex-col gap-4 sm:flex-row sm:items-start sm:justify-between">
        <div className="space-y-1">
          <h2 className="text-2xl font-semibold tracking-tight">Config</h2>
          <p className="text-sm text-muted-foreground">
            Set project defaults and register the agents that can execute queued work.
          </p>
        </div>
        <Dialog open={dialogOpen} onOpenChange={handleDialogOpenChange}>
          <DialogTrigger asChild>
            <Button type="button">Register Codex agent</Button>
          </DialogTrigger>
          <DialogContent className="sm:max-w-xl">
            <DialogHeader>
              <DialogTitle>Register Agent</DialogTitle>
              <DialogDescription>
                Define the adapter, provider, model, and CLI path available for project execution.
              </DialogDescription>
            </DialogHeader>
            <Form {...form}>
              <form onSubmit={form.handleSubmit((values) => createAgentMutation.mutate(values))} className="grid gap-4">
                <FormField
                  control={form.control}
                  name="name"
                  rules={{ required: 'Agent name is required.' }}
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Agent name</FormLabel>
                      <FormControl>
                        <Input placeholder="Agent name" {...field} />
                      </FormControl>
                      <FormMessage />
                    </FormItem>
                  )}
                />
                <FormField
                  control={form.control}
                  name="provider"
                  rules={{ required: 'Provider is required.' }}
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Provider</FormLabel>
                      <Select value={field.value} onValueChange={field.onChange}>
                        <FormControl>
                          <SelectTrigger aria-label="Provider">
                            <SelectValue placeholder="Provider" />
                          </SelectTrigger>
                        </FormControl>
                        <SelectContent>
                          <SelectItem value="openai">openai</SelectItem>
                          <SelectItem value="anthropic">anthropic</SelectItem>
                        </SelectContent>
                      </Select>
                      <FormMessage />
                    </FormItem>
                  )}
                />
                <FormField
                  control={form.control}
                  name="model"
                  rules={{ required: 'Model is required.' }}
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Model</FormLabel>
                      <FormControl>
                        <Input placeholder="Model" {...field} />
                      </FormControl>
                      <FormMessage />
                    </FormItem>
                  )}
                />
                <FormField
                  control={form.control}
                  name="cliPath"
                  rules={{ required: 'CLI path is required.' }}
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>CLI path</FormLabel>
                      <FormControl>
                        <Input placeholder="CLI path" {...field} />
                      </FormControl>
                      <FormMessage />
                    </FormItem>
                  )}
                />
                {createAgentMutation.error instanceof ApiError && (
                  <Alert variant="destructive">
                    <AlertTitle>Agent registration failed</AlertTitle>
                    <AlertDescription>{createAgentMutation.error.message}</AlertDescription>
                  </Alert>
                )}
                <div className="flex items-center gap-3">
                  <Button type="submit" disabled={createAgentMutation.isPending}>
                    {createAgentMutation.isPending ? 'Registering…' : 'Register Codex agent'}
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

      <Card>
        <CardHeader>
          <CardTitle>Project Defaults</CardTitle>
          <CardDescription>The resolved project configuration currently known to the daemon.</CardDescription>
        </CardHeader>
        <CardContent>
          {isConfigLoading ? (
            <div className="space-y-2">
              <Skeleton className="h-4 w-40" />
              <Skeleton className="h-40 w-full" />
            </div>
          ) : (
            <ScrollArea className="max-h-72 rounded-lg border border-border bg-muted/30">
              <pre className="w-max min-w-full p-4 text-xs leading-6">{JSON.stringify(config ?? {}, null, 2)}</pre>
            </ScrollArea>
          )}
        </CardContent>
      </Card>

      <Card className="gap-0">
        <CardHeader className="border-b">
          <CardTitle>Agents</CardTitle>
          <CardDescription>
            Reprobe health and confirm which execution endpoints are currently available.
          </CardDescription>
        </CardHeader>
        <CardContent className="px-0">
          {isAgentsLoading ? (
            <div className="space-y-4 px-4 py-4">
              <div className="grid grid-cols-6 gap-3">
                {['name', 'adapter', 'model', 'status', 'health', 'actions'].map((key) => (
                  <Skeleton key={key} className="h-4 w-16" />
                ))}
              </div>
              {['row-1', 'row-2', 'row-3', 'row-4'].map((rowKey) => (
                <div key={rowKey} className="grid grid-cols-6 gap-3">
                  {['name', 'adapter', 'model', 'status', 'health', 'actions'].map((columnKey) => (
                    <Skeleton key={`${rowKey}-${columnKey}`} className="h-5 w-full" />
                  ))}
                </div>
              ))}
            </div>
          ) : agents && agents.length > 0 ? (
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Name</TableHead>
                  <TableHead>Adapter</TableHead>
                  <TableHead>Model</TableHead>
                  <TableHead>Status</TableHead>
                  <TableHead>Health</TableHead>
                  <TableHead>Actions</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {agents.map((agent) => (
                  <AgentRow key={agent.id} agent={agent} onSuccess={refreshAgents} />
                ))}
              </TableBody>
            </Table>
          ) : (
            <p className="px-4 py-4 text-sm text-muted-foreground">No agents configured.</p>
          )}
        </CardContent>
      </Card>
    </div>
  )
}

function AgentRow({ agent, onSuccess }: { agent: Agent; onSuccess: () => void }) {
  const reprobeMutation = useMutation({
    mutationFn: () => reprobeAgent(agent.id),
    onSuccess: () => {
      onSuccess()
      toast.success(`Reprobe complete for ${agent.name}.`)
    },
  })

  return (
    <TableRow>
      <TableCell>{agent.name}</TableCell>
      <TableCell>{agent.adapter_kind}</TableCell>
      <TableCell>{agent.model}</TableCell>
      <TableCell>
        <Badge variant={statusVariant(agent.status)}>{agent.status}</Badge>
      </TableCell>
      <TableCell className="whitespace-normal">{agent.health_check ?? '—'}</TableCell>
      <TableCell className="whitespace-normal">
        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={() => reprobeMutation.mutate()}
          disabled={reprobeMutation.isPending}
        >
          {reprobeMutation.isPending ? 'Reprobing…' : 'Reprobe'}
        </Button>
        {reprobeMutation.error instanceof ApiError && (
          <Alert variant="destructive" className="mt-2">
            <AlertTitle>Reprobe failed</AlertTitle>
            <AlertDescription>{reprobeMutation.error.message}</AlertDescription>
          </Alert>
        )}
      </TableCell>
    </TableRow>
  )
}
