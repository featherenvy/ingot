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
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '../components/ui/select'
import { Skeleton } from '../components/ui/skeleton'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '../components/ui/table'
import { useRequiredProjectId } from '../hooks/useRequiredRouteParam'
import { showErrorToast } from '../lib/toast'
import type { Agent, AgentProvider, AgentRouting, AutoTriageDecision, AutoTriagePolicy } from '../types/domain'

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

      <AgentRoutingCard projectId={projectId} routing={project?.agent_routing ?? null} agents={agents} />

      <AutoTriagePolicyCard projectId={projectId} policy={project?.auto_triage_policy ?? null} />

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

const DEFAULT_AUTO_TRIAGE_POLICY: AutoTriagePolicy = {
  critical: 'fix_now',
  high: 'fix_now',
  medium: 'fix_now',
  low: 'backlog',
}

const SEVERITY_ROWS: { key: keyof AutoTriagePolicy; label: string }[] = [
  { key: 'critical', label: 'Critical' },
  { key: 'high', label: 'High' },
  { key: 'medium', label: 'Medium' },
  { key: 'low', label: 'Low' },
]

const TRIAGE_DECISION_OPTIONS: { value: AutoTriageDecision; label: string }[] = [
  { value: 'fix_now', label: 'Fix now' },
  { value: 'backlog', label: 'Backlog' },
  { value: 'skip', label: 'Skip (manual)' },
]

function AutoTriagePolicyCard({ projectId, policy }: { projectId: string; policy: AutoTriagePolicy | null }) {
  const queryClient = useQueryClient()
  const mutation = useMutation({
    mutationFn: (newPolicy: AutoTriagePolicy | null) => updateProject(projectId, { auto_triage_policy: newPolicy }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.projects() })
      toast.success('Auto-triage policy updated.')
    },
    onError: (error) => showErrorToast('Failed to update auto-triage policy', error),
  })

  const enabled = policy !== null
  const current = policy ?? DEFAULT_AUTO_TRIAGE_POLICY

  function handleToggle() {
    mutation.mutate(enabled ? null : DEFAULT_AUTO_TRIAGE_POLICY)
  }

  function handleChange(severity: keyof AutoTriagePolicy, value: string) {
    mutation.mutate({ ...current, [severity]: value as AutoTriageDecision })
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>Auto-Triage Policy</CardTitle>
        <CardDescription>
          When enabled in autopilot mode, findings are automatically triaged by severity instead of blocking for human
          review.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="flex items-center gap-3">
          <Button
            variant={enabled ? 'default' : 'outline'}
            size="sm"
            onClick={handleToggle}
            disabled={mutation.isPending}
          >
            {enabled ? 'Enabled' : 'Disabled'}
          </Button>
        </div>
        {enabled && (
          <div className="grid gap-4 sm:grid-cols-4">
            {SEVERITY_ROWS.map(({ key, label }) => (
              <div key={key} className="space-y-1.5">
                <span className="text-sm font-medium">{label}</span>
                <Select value={current[key]} onValueChange={(v) => handleChange(key, v)} disabled={mutation.isPending}>
                  <SelectTrigger aria-label={`${label} triage decision`}>
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {TRIAGE_DECISION_OPTIONS.map((opt) => (
                      <SelectItem key={opt.value} value={opt.value}>
                        {opt.label}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
            ))}
          </div>
        )}
      </CardContent>
    </Card>
  )
}

const ROUTING_PHASES = [
  { key: 'author' as const, label: 'Author' },
  { key: 'review' as const, label: 'Review' },
  { key: 'investigate' as const, label: 'Investigate' },
]

const AUTO_VALUE = '__auto__'

function AgentRoutingCard({
  projectId,
  routing,
  agents,
}: {
  projectId: string
  routing: AgentRouting | null
  agents: Agent[] | undefined
}) {
  const queryClient = useQueryClient()
  const routingMutation = useMutation({
    mutationFn: (newRouting: AgentRouting) => updateProject(projectId, { agent_routing: newRouting }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.projects() })
      toast.success('Agent routing updated.')
    },
    onError: (error) => showErrorToast('Failed to update agent routing', error),
  })

  const current: AgentRouting = routing ?? { author: null, review: null, investigate: null }

  function handleChange(phase: keyof AgentRouting, value: string) {
    const slug = value === AUTO_VALUE ? null : value
    routingMutation.mutate({ ...current, [phase]: slug })
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>Agent Routing</CardTitle>
        <CardDescription>
          Choose which agent handles each workflow phase. Default (auto) picks the first available.
        </CardDescription>
      </CardHeader>
      <CardContent>
        <div className="grid gap-4 sm:grid-cols-3">
          {ROUTING_PHASES.map(({ key, label }) => (
            <div key={key} className="space-y-1.5">
              <span className="text-sm font-medium">{label}</span>
              <Select
                value={current[key] ?? AUTO_VALUE}
                onValueChange={(v) => handleChange(key, v)}
                disabled={routingMutation.isPending}
              >
                <SelectTrigger aria-label={`${label} agent`}>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value={AUTO_VALUE}>Default (auto)</SelectItem>
                  {agents?.map((agent) => (
                    <SelectItem key={agent.id} value={agent.slug}>
                      {agent.name} ({agent.slug})
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
          ))}
        </div>
      </CardContent>
    </Card>
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
