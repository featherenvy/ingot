import { cn } from '../lib/utils'

type ActivityPulseProps = {
  className?: string
}

export function ActivityPulse({ className }: ActivityPulseProps): React.JSX.Element {
  return (
    <span aria-hidden="true" className={cn('relative flex size-2', className)}>
      <span className="absolute inline-flex size-full animate-ping rounded-full bg-primary opacity-75" />
      <span className="relative inline-flex size-2 rounded-full bg-primary" />
    </span>
  )
}
