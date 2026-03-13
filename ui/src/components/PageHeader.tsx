import type { ReactNode } from 'react'
import { cn } from '@/lib/utils'

export function PageHeader({
  title,
  description,
  action,
  titleAs = 'h2',
  className,
  contentClassName,
  descriptionClassName,
}: {
  title: ReactNode
  description?: ReactNode
  action?: ReactNode
  titleAs?: 'h1' | 'h2'
  className?: string
  contentClassName?: string
  descriptionClassName?: string
}) {
  const Heading = titleAs

  return (
    <div className={cn('flex flex-col gap-4 sm:flex-row sm:items-start sm:justify-between', className)}>
      <div className={cn('space-y-1', contentClassName)}>
        <Heading className="text-2xl font-semibold tracking-tight">{title}</Heading>
        {description ? (
          <p className={cn('text-sm text-muted-foreground', descriptionClassName)}>{description}</p>
        ) : null}
      </div>
      {action ? <div className="shrink-0">{action}</div> : null}
    </div>
  )
}
