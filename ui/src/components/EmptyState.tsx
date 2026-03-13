import type { ReactNode } from 'react'
import { cn } from '@/lib/utils'
import { Card, CardContent } from './ui/card'

export function EmptyState({
  title,
  description,
  action,
  variant = 'card',
  className,
  contentClassName,
}: {
  title?: ReactNode
  description: ReactNode
  action?: ReactNode
  variant?: 'card' | 'inline'
  className?: string
  contentClassName?: string
}) {
  const content = (
    <>
      {title ? <p className="font-medium">{title}</p> : null}
      <p className="text-sm text-muted-foreground">{description}</p>
      {action ? <div>{action}</div> : null}
    </>
  )

  if (variant === 'inline') {
    return <div className={cn('flex flex-col gap-4 px-4 py-6', className, contentClassName)}>{content}</div>
  }

  return (
    <Card className={cn('max-w-xl', className)}>
      <CardContent className={cn('flex flex-col gap-4 py-6', contentClassName)}>{content}</CardContent>
    </Card>
  )
}
