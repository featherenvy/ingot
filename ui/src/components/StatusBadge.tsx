import { formatStatusLabel, getStatusPresentation } from '../lib/status'
import { cn } from '../lib/utils'
import { Badge } from './ui/badge'

type StatusBadgeProps = {
  status: string
  label?: string
  className?: string
}

export function StatusBadge({ status, label, className }: StatusBadgeProps): React.JSX.Element {
  const presentation = getStatusPresentation(status)
  const Icon = presentation.icon

  return (
    <Badge
      variant={presentation.variant}
      className={cn('inline-flex items-center gap-1.5 rounded-full px-3 [&_svg]:size-3.5', className)}
    >
      {Icon ? <Icon className={presentation.animateIcon ? 'animate-spin' : undefined} aria-hidden="true" /> : null}
      <span>{label ?? formatStatusLabel(status)}</span>
    </Badge>
  )
}
