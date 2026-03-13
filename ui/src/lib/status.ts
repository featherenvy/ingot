import type { LucideIcon } from 'lucide-react'
import { AlertTriangleIcon, CheckIcon, Clock3Icon, Loader2Icon, PauseIcon, ShieldAlertIcon, XIcon } from 'lucide-react'

type BadgeVariant = 'default' | 'secondary' | 'destructive' | 'outline'

type StatusPresentation = {
  variant: BadgeVariant
  icon?: LucideIcon
  animateIcon?: boolean
}

const presentationMap: Record<string, StatusPresentation> = {
  running: { variant: 'default', icon: Loader2Icon, animateIcon: true },
  assigned: { variant: 'default', icon: Loader2Icon },
  provisioning: { variant: 'default', icon: Loader2Icon, animateIcon: true },
  busy: { variant: 'default', icon: Loader2Icon, animateIcon: true },
  probing: { variant: 'default', icon: Loader2Icon, animateIcon: true },
  WORKING: { variant: 'default', icon: Loader2Icon },
  active: { variant: 'default', icon: Loader2Icon },

  completed: { variant: 'secondary', icon: CheckIcon },
  finalized: { variant: 'secondary', icon: CheckIcon },
  prepared: { variant: 'secondary', icon: CheckIcon },
  ready: { variant: 'secondary', icon: CheckIcon },
  available: { variant: 'secondary', icon: CheckIcon },
  clean: { variant: 'secondary', icon: CheckIcon },
  approved: { variant: 'secondary', icon: CheckIcon },
  done: { variant: 'secondary', icon: CheckIcon },
  DONE: { variant: 'secondary', icon: CheckIcon },

  failed: { variant: 'destructive', icon: XIcon },
  cancelled: { variant: 'destructive', icon: XIcon },
  expired: { variant: 'destructive', icon: XIcon },
  error: { variant: 'destructive', icon: XIcon },
  abandoned: { variant: 'destructive', icon: XIcon },
  unavailable: { variant: 'destructive', icon: XIcon },
  terminal_failure: { variant: 'destructive', icon: XIcon },
  protocol_violation: { variant: 'destructive', icon: ShieldAlertIcon },
  conflicted: { variant: 'destructive', icon: AlertTriangleIcon },
  operator_required: { variant: 'destructive', icon: AlertTriangleIcon },

  queued: { variant: 'outline', icon: Clock3Icon },
  stale: { variant: 'outline', icon: Clock3Icon },
  retained_for_debug: { variant: 'outline', icon: PauseIcon },
  removing: { variant: 'outline', icon: Clock3Icon },
  superseded: { variant: 'outline', icon: PauseIcon },
  transient_failure: { variant: 'outline', icon: AlertTriangleIcon },
  findings: { variant: 'outline', icon: AlertTriangleIcon },
  triaging: { variant: 'outline', icon: AlertTriangleIcon },
  open: { variant: 'outline', icon: Clock3Icon },
  deferred: { variant: 'outline', icon: PauseIcon },
  pending: { variant: 'outline', icon: Clock3Icon },
  not_requested: { variant: 'outline', icon: PauseIcon },
  not_required: { variant: 'outline', icon: PauseIcon },
  none: { variant: 'outline', icon: PauseIcon },
  INBOX: { variant: 'outline', icon: Clock3Icon },
  APPROVAL: { variant: 'outline', icon: AlertTriangleIcon },
}

export function getStatusPresentation(status: string): StatusPresentation {
  return presentationMap[status] ?? { variant: 'outline', icon: Clock3Icon }
}

export function statusVariant(status: string): BadgeVariant {
  return getStatusPresentation(status).variant
}

const activePhaseStatuses = new Set(['running', 'authoring', 'validating', 'reviewing', 'investigating'])

export function isActivePhaseStatus(phaseStatus: string | null | undefined): boolean {
  if (!phaseStatus) return false
  return activePhaseStatuses.has(phaseStatus) || phaseStatus === 'running'
}
