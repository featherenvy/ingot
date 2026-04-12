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
import { Link } from 'react-router'
import { cn } from '@/lib/utils'
import { formatRelativeTime, formatStepLabel } from '../../lib/format'
import { shortId } from '../../lib/git'
import type {
  Finding,
  FindingSeverity,
  FindingTriageState,
  InvestigationScope,
  Job,
  LinkedFindingItemSummary,
} from '../../types/domain'
import { TooltipValue } from '../TooltipValue'
import { Badge } from '../ui/badge'
import { Button } from '../ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '../ui/card'
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from '../ui/collapsible'
import { Input } from '../ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '../ui/select'
import { WORKFLOW_FINDINGS_COPY, type WorkflowFindingsCopy, type WorkflowVersion } from './workflowPresentation'

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

type FindingActionMode = 'delivery' | 'investigation'

type FindingTriageCopy = {
  mode: FindingActionMode
  fixNowLabel: string
  fixNowDescription: string
  quickFixNowLabel: string
  backlogDescription: string
  quickBacklogLabel?: string
}

// ── Constants ──────────────────────────────────────────────────

const DELIVERY_TRIAGE_COPY: FindingTriageCopy = {
  mode: 'delivery',
  fixNowLabel: 'Fix now',
  fixNowDescription: 'Agent will repair this finding',
  quickFixNowLabel: 'Fix now',
  backlogDescription: 'Promote to separate item',
}

const INVESTIGATION_TRIAGE_COPY: FindingTriageCopy = {
  mode: 'investigation',
  fixNowLabel: 'Fix now',
  fixNowDescription: 'Create and launch a linked change item from this finding',
  quickFixNowLabel: 'Fix now',
  backlogDescription: 'Create a linked change item without launching it',
  quickBacklogLabel: 'Backlog',
}

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

const NEEDS_NOTE: Set<FindingTriageState> = new Set(['wont_fix', 'dismissed_invalid', 'needs_investigation'])
const NEEDS_LINK: Set<FindingTriageState> = new Set(['backlog', 'duplicate'])

const ESTIMATED_SCOPE_LABELS = {
  small: 'Small',
  medium: 'Medium',
  large: 'Large',
} as const

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

function InvestigationScopePanel({ scope }: { scope: InvestigationScope }) {
  return (
    <div className="rounded-lg border border-blue-500/20 bg-blue-500/5 px-4 py-3">
      <div className="flex flex-wrap items-center gap-2">
        <Badge variant="outline" className="h-5 border-blue-500/30 text-[11px] text-blue-700 dark:text-blue-300">
          Investigation scope
        </Badge>
        <span className="text-xs text-muted-foreground">{scope.description}</span>
      </div>
      <div className="mt-2 flex flex-wrap gap-x-4 gap-y-1 text-xs text-muted-foreground">
        <span>
          Methodology: <span className="text-foreground">{scope.methodology}</span>
        </span>
        {scope.paths_examined.length > 0 && (
          <span>
            Paths examined: <span className="font-mono text-foreground">{scope.paths_examined.join(', ')}</span>
          </span>
        )}
      </div>
    </div>
  )
}

function isInvestigationGroup(group: FindingGroup): boolean {
  return (
    group.job?.phase_kind === 'investigate' ||
    group.stepId === 'investigate_item' ||
    group.stepId === 'investigate_project' ||
    group.stepId === 'reinvestigate_project' ||
    group.findings.some((finding) => finding.investigation !== null)
  )
}

function findingsCopyForGroup(group: FindingGroup | undefined, workflowVersion: WorkflowVersion): WorkflowFindingsCopy {
  if (group && isInvestigationGroup(group)) {
    return WORKFLOW_FINDINGS_COPY['investigation:v1']
  }

  return WORKFLOW_FINDINGS_COPY[workflowVersion]
}

function triageCopyForGroup(group: FindingGroup | undefined, workflowVersion: WorkflowVersion): FindingTriageCopy {
  if (group && isInvestigationGroup(group)) {
    return INVESTIGATION_TRIAGE_COPY
  }

  return workflowVersion === 'investigation:v1' ? INVESTIGATION_TRIAGE_COPY : DELIVERY_TRIAGE_COPY
}

function triageOptions(
  copy: FindingTriageCopy,
  hasLinkedItem: boolean,
  currentState?: FindingTriageState,
): { value: FindingTriageState; label: string; description: string }[] {
  const options: { value: FindingTriageState; label: string; description: string }[] = [
    { value: 'fix_now', label: copy.fixNowLabel, description: copy.fixNowDescription },
    { value: 'backlog', label: 'Backlog', description: copy.backlogDescription },
    { value: 'duplicate', label: 'Duplicate', description: 'Already tracked elsewhere' },
    { value: 'wont_fix', label: "Won't fix", description: 'Acceptable risk, note required' },
    { value: 'dismissed_invalid', label: 'Dismiss', description: 'False positive or invalid' },
    { value: 'needs_investigation', label: 'Investigate', description: 'Needs human analysis' },
  ]

  if (copy.mode === 'investigation' && hasLinkedItem) {
    return options.filter(
      (option) => (option.value !== 'fix_now' && option.value !== 'backlog') || option.value === currentState,
    )
  }

  return options
}

function triageStateLabel(
  state: FindingTriageState,
  copy: FindingTriageCopy,
  linkedItemSummary?: LinkedFindingItemSummary,
): string {
  switch (state) {
    case 'untriaged':
      return 'Untriaged'
    case 'fix_now':
      return copy.fixNowLabel
    case 'wont_fix':
      return "Won't fix"
    case 'backlog':
      return copy.mode === 'investigation' && (linkedItemSummary?.job_count ?? 0) > 0 ? 'Fixing' : 'Backlog'
    case 'duplicate':
      return 'Duplicate'
    case 'dismissed_invalid':
      return 'Dismissed'
    case 'needs_investigation':
      return 'Investigating'
  }
}

function formatBoardStatus(status: LinkedFindingItemSummary['board_status']): string {
  return status.toLowerCase()
}

// ── Agent Scope Summary ────────────────────────────────────────

function AgentScopeSummary({ findings, copy }: { findings: Finding[]; copy: WorkflowFindingsCopy }) {
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
        <p className="font-medium text-foreground">{copy.agentScopeTitle}</p>
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
        {untriaged.length > 0 && <p className="text-xs text-amber-600 dark:text-amber-500">{copy.triageWarning}</p>}
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

function TriageIndicator({
  state,
  copy,
  linkedItemSummary,
}: {
  state: FindingTriageState
  copy: FindingTriageCopy
  linkedItemSummary?: LinkedFindingItemSummary
}) {
  const label = triageStateLabel(state, copy, linkedItemSummary)

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

  if (state === 'backlog' && copy.mode === 'investigation' && (linkedItemSummary?.job_count ?? 0) > 0) {
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
  triageCopy,
  linkedItemSummary,
  onTriage,
  onPromote,
  pending,
}: {
  finding: Finding
  isActionable: boolean
  triageCopy: FindingTriageCopy
  linkedItemSummary?: LinkedFindingItemSummary
  onTriage: (findingId: string, payload: TriagePayload) => void
  onPromote?: (findingId: string, dispatchImmediately: boolean) => void
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
    if (
      triageCopy.mode === 'investigation' &&
      !finding.linked_item_id &&
      onPromote &&
      (triageState === 'fix_now' || triageState === 'backlog')
    ) {
      onPromote(finding.id, triageState === 'fix_now')
      setEditing(false)
      return
    }

    const linkedItemIdForSubmit = NEEDS_LINK.has(triageState) ? linkedItemId || undefined : undefined

    onTriage(finding.id, {
      triage_state: triageState,
      triage_note: triageNote || undefined,
      linked_item_id: linkedItemIdForSubmit,
    })
    setEditing(false)
  }

  const showNote = NEEDS_NOTE.has(triageState)
  const showLink = NEEDS_LINK.has(triageState) && !(triageCopy.mode === 'investigation' && triageState === 'backlog')
  const alreadyTriaged = finding.triage_state !== 'untriaged' && finding.triage_state !== 'needs_investigation'
  const investigation = finding.investigation
  const options = triageOptions(triageCopy, !!finding.linked_item_id, triageState)

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
            <TriageIndicator state={finding.triage_state} copy={triageCopy} linkedItemSummary={linkedItemSummary} />
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
        {!editing && linkedItemSummary && (
          <div className="flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
            <span>Linked item:</span>
            <Link
              to={`/projects/${linkedItemSummary.item.project_id}/items/${linkedItemSummary.item.id}`}
              className="font-medium text-foreground underline underline-offset-4"
            >
              {linkedItemSummary.title}
            </Link>
            <Badge variant="outline" className="h-5 text-[11px]">
              {formatBoardStatus(linkedItemSummary.board_status)}
            </Badge>
          </div>
        )}
        {!editing && !linkedItemSummary && finding.linked_item_id && (
          <p className="text-xs text-muted-foreground">
            Linked: <code>{shortId(finding.linked_item_id)}</code>
          </p>
        )}
        {investigation && (
          <div className="space-y-2 rounded-lg border border-dashed border-border/70 bg-muted/20 px-3 py-2">
            <div className="flex flex-wrap items-center gap-2">
              <span className="text-xs font-medium text-foreground">Promotion preview</span>
              <Badge variant="secondary" className="h-5 text-[11px]">
                {investigation.promotion.classification}
              </Badge>
              <Badge variant="outline" className="h-5 text-[11px]">
                {ESTIMATED_SCOPE_LABELS[investigation.promotion.estimated_scope]}
              </Badge>
              {investigation.group_key && (
                <Badge variant="outline" className="h-5 text-[11px]">
                  Group {investigation.group_key}
                </Badge>
              )}
            </div>
            <p className="text-sm text-foreground">{investigation.promotion.title}</p>
            <p className="text-xs text-muted-foreground">{investigation.promotion.description}</p>
            <p className="text-xs text-muted-foreground">
              Acceptance criteria: {investigation.promotion.acceptance_criteria}
            </p>
          </div>
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
                    onClick={() =>
                      triageCopy.mode === 'investigation' && onPromote
                        ? onPromote(finding.id, true)
                        : onTriage(finding.id, { triage_state: 'fix_now' })
                    }
                    disabled={pending}
                  >
                    <ZapIcon className="size-3" />
                    {triageCopy.quickFixNowLabel}
                  </Button>
                  {triageCopy.mode === 'investigation' && onPromote && triageCopy.quickBacklogLabel && (
                    <Button
                      size="sm"
                      variant="secondary"
                      className="h-7 text-xs"
                      onClick={() => onPromote(finding.id, false)}
                      disabled={pending}
                    >
                      {triageCopy.quickBacklogLabel}
                    </Button>
                  )}
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
                    {options.map((opt) => (
                      <SelectItem key={opt.value} value={opt.value}>
                        {opt.label}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                <span className="text-xs text-muted-foreground">
                  {options.find((o) => o.value === triageState)?.description}
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
      <code className="text-sm font-semibold">{formatStepLabel(group.stepId)}</code>
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
  linkedFindingItems,
  workflowVersion,
  onTriage,
  onPromote,
  pendingFindingId,
}: {
  findings: Finding[]
  jobs: Job[]
  linkedFindingItems: LinkedFindingItemSummary[]
  workflowVersion: WorkflowVersion
  onTriage: (findingId: string, payload: TriagePayload) => void
  onPromote?: (findingId: string, dispatchImmediately: boolean) => void
  pendingFindingId: string | null
}) {
  const groups = groupFindingsByJob(findings, jobs)
  const latestGroup = groups.find((g) => g.isLatest)
  const historicalGroups = groups.filter((g) => !g.isLatest)
  const linkedFindingItemsByFindingId = new Map(linkedFindingItems.map((summary) => [summary.finding_id, summary]))
  const copy = WORKFLOW_FINDINGS_COPY[workflowVersion]
  const latestGroupCopy = findingsCopyForGroup(latestGroup, workflowVersion)
  const latestGroupTriageCopy = triageCopyForGroup(latestGroup, workflowVersion)

  if (findings.length === 0) return null

  return (
    <Card className="gap-0">
      <CardHeader className="border-b">
        <CardTitle>Findings ({findings.length})</CardTitle>
      </CardHeader>
      <CardContent className="space-y-6 p-5">
        {/* Agent scope summary for the latest review */}
        {latestGroup && <AgentScopeSummary findings={latestGroup.findings} copy={latestGroupCopy} />}

        {/* Latest (actionable) group */}
        {latestGroup && (
          <section className="space-y-3">
            <div className="flex items-center gap-2">
              <div className="h-5 w-1 rounded-full bg-foreground" />
              <h3 className="text-sm font-semibold tracking-tight">{latestGroupCopy.currentSectionTitle}</h3>
              <span className="text-xs text-muted-foreground">\u2014 {latestGroupCopy.currentSectionHint}</span>
            </div>
            <JobGroupHeader group={latestGroup} />
            {latestGroup.findings[0]?.investigation && (
              <InvestigationScopePanel scope={latestGroup.findings[0].investigation.scope} />
            )}
            <div className="grid gap-3">
              {latestGroup.findings.map((finding) => (
                <FindingCard
                  key={finding.id}
                  finding={finding}
                  isActionable={true}
                  triageCopy={latestGroupTriageCopy}
                  linkedItemSummary={linkedFindingItemsByFindingId.get(finding.id)}
                  onTriage={onTriage}
                  onPromote={onPromote}
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
              <span className="font-medium">{copy.previousSectionTitle}</span>
              <span className="text-xs">
                ({historicalGroups.reduce((sum, g) => sum + g.findings.length, 0)} findings from{' '}
                {historicalGroups.length} {copy.previousSectionSummaryNoun}
                {historicalGroups.length !== 1 ? 's' : ''})
              </span>
            </CollapsibleTrigger>
            <CollapsibleContent>
              <div className="mt-3 space-y-5 border-l-2 border-border/40 pl-4">
                {historicalGroups.map((group) => (
                  <section key={group.jobId} className="space-y-3">
                    <JobGroupHeader group={group} />
                    {group.findings[0]?.investigation && (
                      <InvestigationScopePanel scope={group.findings[0].investigation.scope} />
                    )}
                    <div className="grid gap-2">
                      {group.findings.map((finding) => (
                        <FindingCard
                          key={finding.id}
                          finding={finding}
                          isActionable={false}
                          triageCopy={triageCopyForGroup(group, workflowVersion)}
                          linkedItemSummary={linkedFindingItemsByFindingId.get(finding.id)}
                          onTriage={onTriage}
                          onPromote={onPromote}
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
