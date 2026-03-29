import { type UIEventHandler, useEffect } from 'react'
import { CodeBlock } from './CodeBlock'

export function LogBlock({
  label,
  value,
  emptyMessage,
  autoScrollToBottom,
  scrollContainerRef,
  onScroll,
}: {
  label: string
  value: string | null | undefined
  emptyMessage?: string
  autoScrollToBottom?: boolean
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
        scrollContainerRef={scrollContainerRef}
        onScroll={onScroll}
      />
    </div>
  )
}
