import { formatTimestamp } from '../lib/date'
import { TooltipValue } from './TooltipValue'

export function Timestamp({ value, fallback = '—' }: { value: string | null | undefined; fallback?: string }) {
  if (!value) {
    return <>{fallback}</>
  }

  return (
    <TooltipValue content={value}>
      <time dateTime={value}>{formatTimestamp(value)}</time>
    </TooltipValue>
  )
}
