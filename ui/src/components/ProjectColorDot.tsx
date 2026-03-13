import { cn } from '../lib/utils'

type ProjectColorDotProps = {
  color: string
  className?: string
}

export function ProjectColorDot({ color, className }: ProjectColorDotProps): React.JSX.Element {
  return (
    <span
      aria-hidden="true"
      className={cn('size-3 shrink-0 rounded-full border border-black/10', className)}
      style={{ backgroundColor: color }}
    />
  )
}
