import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { useForm } from 'react-hook-form'
import { toast } from 'sonner'
import { createAgent, reprobeAgent, updateProject } from '../api/client'
import { agentsQuery, projectConfigQuery, projectsQuery, queryKeys } from '../api/queries'
import { CodeBlock } from '../components/CodeBlock'
import type { ComboboxOption } from '../components/Combobox'
import { Combobox } from '../components/Combobox'
import { DataTable } from '../components/DataTable'
import { EmptyState } from '../components/EmptyState'
import { PageHeader } from '../components/PageHeader'
import { PageQueryError } from '../components/PageQueryError'
import { StatusBadge } from '../components/StatusBadge'
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
import { Skeleton } from '../components/ui/skeleton'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '../components/ui/table'
import { useRequiredProjectId } from '../hooks/useRequiredRouteParam'
import { showErrorToast } from '../lib/toast'
import type { Agent, AgentProvider } from '../types/domain'

type AgentForm = {
  name: string
  provider: AgentProvider
  model: string
  cliPath: string
}

const initialAgentForm: AgentForm = {
  name: 'Codex CLI',
  provider: 'openai',
  model: 'gpt-5-codex',
  cliPath: 'codex',
}

const providerDefaults: AgentProvider[] = ['openai', 'anthropic']
const providerModelDefaults: Record<AgentProvider, string[]> = {
  openai: ['gpt-5-codex', 'gpt-5'],
  anthropic: [],
}

function isAgentProvider(value: string): value is AgentProvider {
  return value === 'openai' || value === 'anthropic'
}

function toComboboxOptions(values: Iterable<string>): ComboboxOption[] {
  return Array.from(values, (value) => ({
    value,
    label: value,
  }))
}

function buildProviderOptions(agents: Agent[] | undefined): ComboboxOption[] {
  const knownProviders = new Set(providerDefaults)

  for (const agent of agents ?? []) {
    knownProviders.add(agent.provider)
  }

  return toComboboxOptions(knownProviders)
}

function buildModelOptions(
  selectedProvider: AgentProvider,
  selectedModel: string,
  agents: Agent[] | undefined,
): ComboboxOption[] {
  const knownModels = new Set(providerModelDefaults[selectedProvider] ?? [])

  for (const agent of agents ?? []) {
    if (agent.provider === selectedProvider) {
      knownModels.add(agent.model)
    }
  }

  if (selectedModel) {
    knownModels.add(selectedModel)
  }

  return toComboboxOptions(knownModels)
}

export default function ConfigPage(): React.JSX.Element {
  const projectId = useRequiredProjectId()
  const queryClient = useQueryClient()
  const {
    data: config,
    error: configError,
    isError: isConfigError,
    isFetching: isConfigFetching,
    isLoading: isConfigLoading,
    refetch: refetchConfig,
  } = useQuery(projectConfigQuery(projectId))
  const {
    data: agents,
    error: agentsError,
    isError: isAgentsError,
    isFetching: isAgentsFetching,
    isLoading: isAgentsLoading,
    refetch: refetchAgents,
  } = useQuery(agentsQuery())
  const { data: projects } = useQuery(projectsQuery())
  const project = projects?.find((p) => p.id === projectId)
  const executionModeMutation = useMutation({
    mutationFn: (mode: 'manual' | 'autopilot') => updateProject(projectId, { execution_mode: mode }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.projects() })
      queryClient.invalidateQueries({ queryKey: queryKeys.items(projectId) })
      toast.success('Execution mode updated.')
    },
    onError: (error) => showErrorToast('Failed to update execution mode', error),
  })
  const [dialogOpen, setDialogOpen] = useState(false)
  const form = useForm<AgentForm>({
    defaultValues: initialAgentForm,
  })
  const selectedProvider = form.watch('provider')
  const selectedModel = form.watch('model')

  const providerOptions = buildProviderOptions(agents)
  const modelOptions = buildModelOptions(selectedProvider, selectedModel, agents)

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
    onError: (error) => {
      showErrorToast('Agent registration failed.', error)
    },
  })

  function handleDialogOpenChange(open: boolean) {
    setDialogOpen(open)
    if (!open) {
      form.reset(initialAgentForm)
      createAgentMutation.reset()
    }
  }

  function refreshAgents(): void {
    queryClient.invalidateQueries({ queryKey: queryKeys.agents() })
  }

  if (isConfigError || isAgentsError) {
    return (
      <PageQueryError
        title="Config failed to load"
        error={configError ?? agentsError}
        onRetry={() => Promise.all([refetchConfig(), refetchAgents()])}
        isRetrying={isConfigFetching || isAgentsFetching}
      />
    )
  }

  return (
    <div className="space-y-8">
      <PageHeader
        title="Config"
        description="Set project defaults and register the agents that can execute queued work."
        action={
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
                <form
                  onSubmit={form.handleSubmit((values) => createAgentMutation.mutate(values))}
                  className="grid gap-4"
                >
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
                        <FormControl>
                          <Combobox
                            ariaLabel="Provider"
                            value={field.value}
                            onChange={(provider) => {
                              if (!isAgentProvider(provider)) {
                                return
                              }
                              const previousProvider = form.getValues('provider')
                              const currentModel = form.getValues('model')
                              field.onChange(provider)

                              const previousDefaults = providerModelDefaults[previousProvider] ?? []
                              const nextDefaults = providerModelDefaults[provider] ?? []
                              if (!currentModel || previousDefaults.includes(currentModel)) {
                                form.setValue('model', nextDefaults[0] ?? '', {
                                  shouldDirty: true,
                                  shouldValidate: true,
                                })
                              }
                            }}
                            options={providerOptions}
                            placeholder="Select provider"
                            searchPlaceholder="Filter providers..."
                            emptyText="No providers found."
                          />
                        </FormControl>
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
                          <Combobox
                            ariaLabel="Model"
                            value={field.value}
                            onChange={field.onChange}
                            options={modelOptions}
                            placeholder="Select or type a model"
                            searchPlaceholder="Filter models..."
                            emptyText="No saved models for this provider."
                            allowCustom
                            customLabel={(query) => `Use "${query}"`}
                          />
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
        }
      />

      <Card>
        <CardHeader>
          <CardTitle>Execution Mode</CardTitle>
          <CardDescription>
            In autopilot mode, the daemon automatically dispatches every safe workflow step until it hits a human gate
            (approval, escalation, findings triage, or conflict).
          </CardDescription>
        </CardHeader>
        <CardContent>
          <div className="flex items-center gap-3">
            <Button
              variant={project?.execution_mode === 'manual' ? 'default' : 'outline'}
              size="sm"
              onClick={() => executionModeMutation.mutate('manual')}
              disabled={executionModeMutation.isPending || project?.execution_mode === 'manual'}
            >
              Manual
            </Button>
            <Button
              variant={project?.execution_mode === 'autopilot' ? 'default' : 'outline'}
              size="sm"
              onClick={() => executionModeMutation.mutate('autopilot')}
              disabled={executionModeMutation.isPending || project?.execution_mode === 'autopilot'}
            >
              Autopilot
            </Button>
          </div>
        </CardContent>
      </Card>

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
            <CodeBlock
              value={JSON.stringify(config ?? {}, null, 2)}
              copyLabel="Copy project defaults"
              maxHeightClassName="max-h-72"
            />
          )}
        </CardContent>
      </Card>

      <DataTable
        title="Agents"
        description="Reprobe health and confirm which execution endpoints are currently available."
      >
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
          <EmptyState variant="inline" description="No agents configured." />
        )}
      </DataTable>
    </div>
  )
}

type AgentRowProps = {
  agent: Agent
  onSuccess: () => void
}

function AgentRow({ agent, onSuccess }: AgentRowProps): React.JSX.Element {
  const reprobeMutation = useMutation({
    mutationFn: () => reprobeAgent(agent.id),
    onSuccess: () => {
      onSuccess()
      toast.success(`Reprobe complete for ${agent.name}.`)
    },
    onError: (error) => {
      showErrorToast('Reprobe failed.', error)
    },
  })

  return (
    <TableRow>
      <TableCell>{agent.name}</TableCell>
      <TableCell>{agent.adapter_kind}</TableCell>
      <TableCell>{agent.model}</TableCell>
      <TableCell>
        <StatusBadge status={agent.status} />
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
      </TableCell>
    </TableRow>
  )
}
