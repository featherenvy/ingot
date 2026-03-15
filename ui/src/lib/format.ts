/** Converts a snake_case step ID into a Title Cased label. */
export function formatStepLabel(stepId: string): string {
  return stepId.replace(/_/g, ' ').replace(/\b\w/g, (c) => c.toUpperCase())
}

/** Formats the elapsed time between two ISO timestamps (or from start until now). */
export function formatDuration(startIso: string | null, endIso: string | null): string {
  if (!startIso) return '\u2014'
  const start = new Date(startIso).getTime()
  const end = endIso ? new Date(endIso).getTime() : Date.now()
  const secs = Math.floor((end - start) / 1000)
  if (secs < 60) return `${secs}s`
  const mins = Math.floor(secs / 60)
  const remSecs = secs % 60
  if (mins < 60) return `${mins}m ${remSecs}s`
  const hrs = Math.floor(mins / 60)
  const remMins = mins % 60
  return `${hrs}h ${remMins}m`
}

/** Formats an ISO timestamp as a relative time string (e.g. "5m ago", "2h ago"). */
export function formatRelativeTime(iso: string | null, opts?: { compact?: boolean }): string {
  if (!iso) return '\u2014'
  const diff = Date.now() - new Date(iso).getTime()
  const mins = Math.floor(diff / 60000)
  const suffix = opts?.compact ? '' : ' ago'
  if (mins < 1) return 'just now'
  if (mins < 60) return `${mins}m${suffix}`
  const hours = Math.floor(mins / 60)
  if (hours < 24) return `${hours}h${suffix}`
  const days = Math.floor(hours / 24)
  return `${days}d${suffix}`
}
