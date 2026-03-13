import type { ReactNode } from 'react'
import { cn } from '@/lib/utils'
import { Card, CardAction, CardContent, CardDescription, CardFooter, CardHeader, CardTitle } from './ui/card'

export function DataTable({
  title,
  description,
  action,
  footer,
  children,
  className,
  headerClassName,
  contentClassName,
  footerClassName,
}: {
  title?: ReactNode
  description?: ReactNode
  action?: ReactNode
  footer?: ReactNode
  children: ReactNode
  className?: string
  headerClassName?: string
  contentClassName?: string
  footerClassName?: string
}) {
  const hasHeader = !!title || !!description || !!action

  return (
    <Card className={cn('gap-0', className)}>
      {hasHeader ? (
        <CardHeader className={cn('border-b', headerClassName)}>
          {title ? <CardTitle>{title}</CardTitle> : null}
          {description ? <CardDescription>{description}</CardDescription> : null}
          {action ? <CardAction>{action}</CardAction> : null}
        </CardHeader>
      ) : null}
      <CardContent className={cn('px-0', contentClassName)}>{children}</CardContent>
      {footer ? <CardFooter className={cn('bg-transparent px-6 py-4', footerClassName)}>{footer}</CardFooter> : null}
    </Card>
  )
}
