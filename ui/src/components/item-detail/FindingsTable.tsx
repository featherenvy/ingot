import {
  AlertTriangleIcon,
  BotIcon,
  ChevronDownIcon,
  CircleDotIcon,
  ClockIcon,
  EyeOffIcon,
  FileIcon,
  ShieldAlertIcon,
  ZapIcon,
} from 'lucide-react'
import { useState } from 'react'
import { cn } from '@/lib/utils'
import { shortId } from '../../lib/git'
import type { Finding, FindingSeverity, FindingTriageState, Job } from '../../types/domain'
import { TooltipValue } from '../TooltipValue'
import { Badge } from '../ui/badge'
import { Button } from '../ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '../ui/card'
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from '../ui/collapsible'
import { Input } from '../ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '../ui/select'

// ── Types ──────────────────────────────────────────────────────

type FindingGroup = {
  jobId: string
  job: Job | undefined
  stepId: string
  findings: Finding[]
  isLatest: boolean
}

type TriagePayload = {
  triage_state: FindingTriageState
  triage_note?: string
  linked_item_id?: string
}

// ── Constants ──────────────────────────────────────────────────

const TRIAGE_OPTIONS: { value: FindingTriageState; label: string; description: string }[] = [
  { value: 'fix_now', label: 'Fix now', description: 'Agent will repair this finding' },
  { value: 'wont_fix', label: "Won't fix", description: 'Acceptable risk, note required' },
  { value: 'backlog', label: 'Backlog', description: 'Promote to separate item' },
  { value: 'duplicate', label: 'Duplicate', description: 'Already tracked elsewhere' },
  { value: 'dismissed_invalid', label: 'Dismiss', description: 'False positive or invalid' },
  { value: 'needs_investigation', label: 'Investigate', description: 'Needs human analysis' },
]

const SEVERITY_CONFIG: Record<FindingSeverity, { className: string; ringClassName: string; label: string }> = {
  critical: {
    className: 'bg-red-500/10 text-red-700 dark:text-red-400',
    ringClassName: 'ring-red-500/20',
    label: 'Critical',
  },
  high: {
    className: 'bg-orange-500/10 text-orange-700 dark:text-orange-400',
    ringClassName: 'ring-orange-500/20',
    label: 'High',
  },
  medium: {
    className: 'bg-amber-500/10 text-amber-700 dark:text-amber-400',
    ringClassName: 'ring-amber-500/20',
    label: 'Medium',
  },
  low: {
    className: 'bg-muted text-muted-foreground',
    ringClassName: 'ring-border',
    label: 'Low',
  },
}

const TRIAGE_STATE_LABELS: Record<FindingTriageState, string> = {
  untriaged: 'Untriaged',
  fix_now: 'Fix now',
  wont_fix: "Won't fix",
  backlog: 'Backlog',
  duplicate: 'Duplicate',
  dismissed_invalid: 'Dismissed',
  needs_investigation: 'Investigating',
}

const NEEDS_NOTE: Set<FindingTriageState> = new Set(['wont_fix', 'dismissed_invalid', 'needs_investigation'])
const NEEDS_LINK: Set<FindingTriageState> = new Set(['backlog', 'duplicate'])

// ── Utilities ──────────────────────────────────────────────────

function groupFindingsByJob(findings: Finding[], jobs: Job[]): FindingGroup[] {
  const jobMap = new Map(jobs.map((j) => [j.id, j]))
  const grouped = new Map<string, Finding[]>()

  for (const finding of findings) {
    const list = grouped.get(finding.source_job_id) ?? []
    list.push(finding)
    grouped.set(finding.source_job_id, list)
  }

  const groups: FindingGroup[] = []
  for (const [jobId, groupFindings] of grouped) {
    const job = jobMap.get(jobId)
    groups.push({
      jobId,
      job,
      stepId: groupFindings[0].source_step_id,
      findings: groupFindings,
      isLatest: false,
    })
  }

  // Sort by job end time (most recent first), falling back to finding created_at
  groups.sort((a, b) => {
    const aTime = a.job?.ended_at ?? a.findings[0]?.created_at ?? ''
    const bTime = b.job?.ended_at ?? b.findings[0]?.created_at ?? ''
    return bTime.localeCompare(aTime)
  })

  // Mark the latest group
  if (groups.length > 0) {
    groups[0].isLatest = true
  }

  return groups
}

function formatStepId(stepId: string): string {
  return stepId.replace(/_/g, ' ').replace(/\b\w/g, (c) => c.toUpperCase())
}

function formatRelativeTime(iso: string): string {
  const diff = Date.now() - new Date(iso).getTime()
  const mins = Math.floor(diff / 60000)
  if (mins < 1) return 'just now'
  if (mins < 60) return `${mins}m ago`
  const hours = Math.floor(mins / 60)
  if (hours < 24) return `${hours}h ago`
  const days = Math.floor(hours / 24)
  return `${days}d ago`
}

// ── Agent Scope Summary ────────────────────────────────────────

function AgentScopeSummary({ findings }: { findings: Finding[] }) {
  const fixNow = findings.filter((f) => f.triage_state === 'fix_now')
  const nonBlocking = findings.filter(
    (f) =>
      f.triage_state === 'wont_fix' ||
      f.triage_state === 'backlog' ||
      f.triage_state === 'duplicate' ||
      f.triage_state === 'dismissed_invalid',
  )
  const untriaged = findings.filter((f) => f.triage_state === 'untriaged' || f.triage_state === 'needs_investigation')

  return (
    <div className="flex items-start gap-3 rounded-lg border border-dashed border-border/80 bg-muted/30 px-4 py-3">
      <BotIcon className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
      <div className="min-w-0 space-y-1.5 text-sm">
        <p className="font-medium text-foreground">Agent scope for next repair job</p>
        <div className="flex flex-wrap gap-x-4 gap-y-1 text-muted-foreground">
          {fixNow.length > 0 && (
            <span className="flex items-center gap-1.5">
              <ZapIcon className="size-3 text-orange-500" />
              <span className="tabular-nums">{fixNow.length}</span> to fix
            </span>
          )}
          {nonBlocking.length > 0 && (
            <span className="flex items-center gap-1.5">
              <EyeOffIcon className="size-3" />
              <span className="tabular-nums">{nonBlocking.length}</span> non-blocking context
            </span>
          )}
          {untriaged.length > 0 && (
            <span className="flex items-center gap-1.5">
              <CircleDotIcon className="size-3 text-amber-500" />
              <span className="tabular-nums">{untriaged.length}</span> awaiting triage
            </span>
          )}
          {fixNow.length === 0 && untriaged.length === 0 && nonBlocking.length === 0 && (
            <span>No findings in scope</span>
          )}
        </div>
        {untriaged.length > 0 && (
          <p className="text-xs text-amber-600 dark:text-amber-500">
            Triage all findings before the agent can proceed.
          </p>
        )}
      </div>
    </div>
  )
}

// ── Severity Badge ─────────────────────────────────────────────

function SeverityBadge({ severity }: { severity: FindingSeverity }) {
  const config = SEVERITY_CONFIG[severity]
  return (
    <span
      className={cn(
        'inline-flex h-5 items-center gap-1 rounded-full px-2 text-[11px] font-semibold uppercase tracking-wider',
        config.className,
      )}
    >
      {severity === 'critical' || severity === 'high' ? <ShieldAlertIcon className="size-3" /> : null}
      {config.label}
    </span>
  )
}

// ── Triage State Indicator ─────────────────────────────────────

function TriageIndicator({ state }: { state: FindingTriageState }) {
  const label = TRIAGE_STATE_LABELS[state]

  if (state === 'untriaged') {
    return (
      <span className="inline-flex h-5 items-center gap-1 rounded-full bg-amber-500/10 px-2 text-[11px] font-medium text-amber-700 dark:text-amber-400">
        <CircleDotIcon className="size-3" />
        {label}
      </span>
    )
  }

  if (state === 'fix_now') {
    return (
      <span className="inline-flex h-5 items-center gap-1 rounded-full bg-orange-500/10 px-2 text-[11px] font-medium text-orange-700 dark:text-orange-400">
        <ZapIcon className="size-3" />
        {label}
      </span>
    )
  }

  if (state === 'needs_investigation') {
    return (
      <span className="inline-flex h-5 items-center gap-1 rounded-full bg-blue-500/10 px-2 text-[11px] font-medium text-blue-700 dark:text-blue-400">
        <AlertTriangleIcon className="size-3" />
        {label}
      </span>
    )
  }

  return (
    <span className="inline-flex h-5 items-center gap-1 rounded-full bg-muted px-2 text-[11px] font-medium text-muted-foreground">
      {label}
    </span>
  )
}

// ── Finding Card ───────────────────────────────────────────────

function FindingCard({
  finding,
  isActionable,
  onTriage,
  pending,
}: {
  finding: Finding
  isActionable: boolean
  onTriage: (findingId: string, payload: TriagePayload) => void
  pending: boolean
}) {
  const [editing, setEditing] = useState(false)
  const [triageState, setTriageState] = useState<FindingTriageState>(
    finding.triage_state === 'untriaged' ? 'fix_now' : finding.triage_state,
  )
  const [triageNote, setTriageNote] = useState(finding.triage_note ?? '')
  const [linkedItemId, setLinkedItemId] = useState(finding.linked_item_id ?? '')
  const severityConfig = SEVERITY_CONFIG[finding.severity]

  function handleSubmit() {
    onTriage(finding.id, {
      triage_state: triageState,
      triage_note: triageNote || undefined,
      linked_item_id: linkedItemId || undefined,
    })
    setEditing(false)
  }

  const showNote = NEEDS_NOTE.has(triageState)
  const showLink = NEEDS_LINK.has(triageState)
  const alreadyTriaged = finding.triage_state !== 'untriaged' && finding.triage_state !== 'needs_investigation'

  return (
    <div
      className={cn(
        'group relative rounded-lg border transition-colors',
        isActionable ? cn('bg-card', severityConfig.ringClassName, 'ring-1') : 'border-border/50 bg-muted/20',
      )}
    >
      <div className="flex flex-col gap-3 p-4">
        {/* Header: severity + code + triage state */}
        <div className="flex flex-wrap items-center gap-2">
          <SeverityBadge severity={finding.severity} />
          <TooltipValue content={finding.id}>
            <code className="rounded bg-muted px-1.5 py-0.5 text-[11px] text-muted-foreground">{finding.code}</code>
          </TooltipValue>
          <Badge variant="outline" className="h-5 text-[11px]">
            {finding.source_subject_kind}
          </Badge>
          <div className="ml-auto flex items-center gap-2">
            <TriageIndicator state={finding.triage_state} />
          </div>
        </div>

        {/* Summary */}
        <p className={cn('text-sm leading-relaxed', isActionable ? 'text-foreground' : 'text-muted-foreground')}>
          {finding.summary}
        </p>

        {/* Paths */}
        {finding.paths.length > 0 && (
          <div className="flex flex-wrap gap-1.5">
            {finding.paths.map((path) => (
              <span
                key={path}
                className="inline-flex items-center gap-1 rounded bg-muted px-1.5 py-0.5 font-mono text-[11px] text-muted-foreground"
              >
                <FileIcon className="size-3 shrink-0" />
                {path}
              </span>
            ))}
          </div>
        )}

        {/* Existing triage note / linked item */}
        {!editing && finding.triage_note && (
          <p className="text-xs text-muted-foreground italic">Note: {finding.triage_note}</p>
        )}
        {!editing && finding.linked_item_id && (
          <p className="text-xs text-muted-foreground">
            Linked: <code>{shortId(finding.linked_item_id)}</code>
          </p>
        )}

        {/* Triage controls — only for actionable findings */}
        {isActionable &&
          (!editing ? (
            <div className="flex items-center gap-2 border-t border-border/50 pt-3">
              {/* Quick triage buttons for untriaged findings */}
              {!alreadyTriaged ? (
                <>
                  <Button
                    size="sm"
                    variant="default"
                    className="h-7 gap-1.5 text-xs"
                    onClick={() => onTriage(finding.id, { triage_state: 'fix_now' })}
                    disabled={pending}
                  >
                    <ZapIcon className="size-3" />
                    Fix now
                  </Button>
                  <Button
                    size="sm"
                    variant="outline"
                    className="h-7 text-xs"
                    onClick={() => {
                      setTriageState('wont_fix')
                      setEditing(true)
                    }}
                    disabled={pending}
                  >
                    More options
                  </Button>
                </>
              ) : (
                <Button
                  size="sm"
                  variant="outline"
                  className="h-7 text-xs"
                  onClick={() => {
                    setTriageState(finding.triage_state === 'untriaged' ? 'fix_now' : finding.triage_state)
                    setEditing(true)
                  }}
                  disabled={pending}
                >
                  Change triage
                </Button>
              )}
            </div>
          ) : (
            <div className="space-y-3 border-t border-border/50 pt-3">
              <div className="flex items-center gap-2">
                <Select
                  value={triageState}
                  onValueChange={(v) => setTriageState(v as FindingTriageState)}
                  disabled={pending}
                >
                  <SelectTrigger size="sm" className="w-44">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {TRIAGE_OPTIONS.map((opt) => (
                      <SelectItem key={opt.value} value={opt.value}>
                        {opt.label}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                <span className="text-xs text-muted-foreground">
                  {TRIAGE_OPTIONS.find((o) => o.value === triageState)?.description}
                </span>
              </div>

              {showNote && (
                <Input
                  value={triageNote}
                  onChange={(e) => setTriageNote(e.target.value)}
                  placeholder={triageState === 'dismissed_invalid' ? 'Dismissal reason (required)' : 'Note (required)'}
                  className="h-8 text-sm"
                  disabled={pending}
                />
              )}

              {showLink && (
                <Input
                  value={linkedItemId}
                  onChange={(e) => setLinkedItemId(e.target.value)}
                  placeholder="Linked item ID (for backlog or duplicate)"
                  className="h-8 text-sm"
                  disabled={pending}
                />
              )}

              <div className="flex gap-2">
                <Button
                  size="sm"
                  className="h-7 text-xs"
                  onClick={handleSubmit}
                  disabled={pending || (showNote && !triageNote)}
                >
                  {pending ? 'Saving\u2026' : 'Apply'}
                </Button>
                <Button
                  size="sm"
                  variant="ghost"
                  className="h-7 text-xs"
                  onClick={() => setEditing(false)}
                  disabled={pending}
                >
                  Cancel
                </Button>
              </div>
            </div>
          ))}
      </div>
    </div>
  )
}

// ── Job Group Header ───────────────────────────────────────────

function JobGroupHeader({ group }: { group: FindingGroup }) {
  const endedAt = group.job?.ended_at ?? group.findings[0]?.created_at
  const untriaged = group.findings.filter(
    (f) => f.triage_state === 'untriaged' || f.triage_state === 'needs_investigation',
  ).length

  return (
    <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
      <code className="text-sm font-semibold">{formatStepId(group.stepId)}</code>
      {group.job && (
        <TooltipValue content={group.job.id}>
          <code className="text-[11px] text-muted-foreground">{shortId(group.jobId)}</code>
        </TooltipValue>
      )}
      <span className="text-xs text-muted-foreground">
        {group.findings.length} finding{group.findings.length !== 1 ? 's' : ''}
      </span>
      {endedAt && (
        <span className="flex items-center gap-1 text-[11px] text-muted-foreground">
          <ClockIcon className="size-3" />
          {formatRelativeTime(endedAt)}
        </span>
      )}
      {group.isLatest && untriaged > 0 && (
        <span className="ml-auto text-xs font-medium text-amber-600 dark:text-amber-500">
          {untriaged} need{untriaged !== 1 ? '' : 's'} triage
        </span>
      )}
    </div>
  )
}

// ── Main Component ─────────────────────────────────────────────

export function FindingsTable({
  findings,
  jobs,
  onTriage,
  pendingFindingId,
}: {
  findings: Finding[]
  jobs: Job[]
  onTriage: (findingId: string, payload: TriagePayload) => void
  pendingFindingId: string | null
}) {
  const groups = groupFindingsByJob(findings, jobs)
  const latestGroup = groups.find((g) => g.isLatest)
  const historicalGroups = groups.filter((g) => !g.isLatest)

  if (findings.length === 0) return null

  return (
    <Card className="gap-0">
      <CardHeader className="border-b">
        <CardTitle>Findings ({findings.length})</CardTitle>
      </CardHeader>
      <CardContent className="space-y-6 p-5">
        {/* Agent scope summary for the latest review */}
        {latestGroup && <AgentScopeSummary findings={latestGroup.findings} />}

        {/* Latest (actionable) group */}
        {latestGroup && (
          <section className="space-y-3">
            <div className="flex items-center gap-2">
              <div className="h-5 w-1 rounded-full bg-foreground" />
              <h3 className="text-sm font-semibold tracking-tight">Current Review</h3>
              <span className="text-xs text-muted-foreground">\u2014 agent acts on these findings only</span>
            </div>
            <JobGroupHeader group={latestGroup} />
            <div className="grid gap-3">
              {latestGroup.findings.map((finding) => (
                <FindingCard
                  key={finding.id}
                  finding={finding}
                  isActionable={true}
                  onTriage={onTriage}
                  pending={pendingFindingId === finding.id}
                />
              ))}
            </div>
          </section>
        )}

        {/* Historical groups */}
        {historicalGroups.length > 0 && (
          <Collapsible>
            <CollapsibleTrigger className="group flex w-full items-center gap-2 rounded-lg px-1 py-1.5 text-sm text-muted-foreground transition-colors hover:text-foreground">
              <ChevronDownIcon className="size-4 shrink-0 transition-transform duration-200 group-data-[state=closed]:-rotate-90" />
              <span className="font-medium">Previous Reviews</span>
              <span className="text-xs">
                ({historicalGroups.reduce((sum, g) => sum + g.findings.length, 0)} findings from{' '}
                {historicalGroups.length} earlier job{historicalGroups.length !== 1 ? 's' : ''})
              </span>
            </CollapsibleTrigger>
            <CollapsibleContent>
              <div className="mt-3 space-y-5 border-l-2 border-border/40 pl-4">
                {historicalGroups.map((group) => (
                  <section key={group.jobId} className="space-y-3">
                    <JobGroupHeader group={group} />
                    <div className="grid gap-2">
                      {group.findings.map((finding) => (
                        <FindingCard
                          key={finding.id}
                          finding={finding}
                          isActionable={false}
                          onTriage={onTriage}
                          pending={pendingFindingId === finding.id}
                        />
                      ))}
                    </div>
                  </section>
                ))}
              </div>
            </CollapsibleContent>
          </Collapsible>
        )}
      </CardContent>
    </Card>
  )
}
