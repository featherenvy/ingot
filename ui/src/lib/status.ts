type BadgeVariant = 'default' | 'secondary' | 'destructive' | 'outline'

const variantMap: Record<string, BadgeVariant> = {
  running: 'default',
  assigned: 'default',
  provisioning: 'default',
  busy: 'default',
  probing: 'default',

  completed: 'secondary',
  finalized: 'secondary',
  prepared: 'secondary',
  ready: 'secondary',
  available: 'secondary',
  clean: 'secondary',

  failed: 'destructive',
  cancelled: 'destructive',
  expired: 'destructive',
  error: 'destructive',
  abandoned: 'destructive',
  conflicted: 'destructive',
  unavailable: 'destructive',
  terminal_failure: 'destructive',
  protocol_violation: 'destructive',

  queued: 'outline',
  stale: 'outline',
  retained_for_debug: 'outline',
  removing: 'outline',
  superseded: 'outline',
  transient_failure: 'outline',
  findings: 'outline',
}

export function statusVariant(status: string): BadgeVariant {
  return variantMap[status] ?? 'outline'
}

const activePhaseStatuses = new Set(['running', 'authoring', 'validating', 'reviewing', 'investigating'])

export function isActivePhaseStatus(phaseStatus: string | null | undefined): boolean {
  if (!phaseStatus) return false
  return activePhaseStatuses.has(phaseStatus) || phaseStatus === 'running'
}
