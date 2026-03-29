import { type UIEventHandler, useEffect } from 'react'
import { cn } from '@/lib/utils'
import { CodeBlock } from './CodeBlock'

export function LogBlock({
  label,
  value,
  emptyMessage,
  autoScrollToBottom,
  className,
  preClassName,
  scrollContainerRef,
  onScroll,
}: {
  label: string
  value: string | null | undefined
  emptyMessage?: string
  autoScrollToBottom?: boolean
  className?: string
  preClassName?: string
  scrollContainerRef?: React.Ref<HTMLDivElement>
  onScroll?: UIEventHandler<HTMLDivElement>
}) {
  useEffect(() => {
    void value
    if (!autoScrollToBottom || !scrollContainerRef || !('current' in scrollContainerRef)) return
    const element = scrollContainerRef.current
    if (!element) return
    element.scrollTop = element.scrollHeight
  }, [autoScrollToBottom, scrollContainerRef, value])

  return (
    <div className="space-y-2">
      <strong className="text-sm font-medium">{label}</strong>
      <CodeBlock
        value={value}
        emptyMessage={emptyMessage}
        wrap
        copyLabel={`Copy ${label.toLowerCase()}`}
        maxHeightClassName="max-h-72"
        className={cn(className)}
        preClassName={cn(preClassName)}
        scrollContainerRef={scrollContainerRef}
        onScroll={onScroll}
      />
    </div>
  )
}
