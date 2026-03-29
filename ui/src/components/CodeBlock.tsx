import { CheckIcon, CopyIcon } from 'lucide-react'
import { type UIEventHandler, useEffect, useState } from 'react'
import { toast } from 'sonner'
import { showErrorToast } from '../lib/toast'
import { cn } from '../lib/utils'
import { Button } from './ui/button'

type CodeBlockProps = {
  value: string | null | undefined
  emptyMessage?: string
  wrap?: boolean
  maxHeightClassName?: string
  className?: string
  preClassName?: string
  copyLabel?: string
  scrollContainerRef?: React.Ref<HTMLDivElement>
  onScroll?: UIEventHandler<HTMLDivElement>
}

export function CodeBlock({
  value,
  emptyMessage = 'No data.',
  wrap = false,
  maxHeightClassName = 'max-h-72',
  className,
  preClassName,
  copyLabel = 'Copy to clipboard',
  scrollContainerRef,
  onScroll,
}: CodeBlockProps): React.JSX.Element {
  const [copied, setCopied] = useState(false)
  const text = value ?? ''
  const hasValue = text.trim().length > 0
  const displayValue = hasValue ? text : emptyMessage

  useEffect(() => {
    if (!copied) return

    const timeout = window.setTimeout(() => {
      setCopied(false)
    }, 1500)

    return () => window.clearTimeout(timeout)
  }, [copied])

  async function handleCopy(): Promise<void> {
    if (!hasValue || !navigator.clipboard?.writeText) {
      showErrorToast('Copy failed.', new Error('Clipboard access is unavailable.'))
      return
    }

    try {
      await navigator.clipboard.writeText(text)
      setCopied(true)
      toast.success('Copied to clipboard.')
    } catch (error) {
      showErrorToast('Copy failed.', error, 'Clipboard access is unavailable.')
    }
  }
  return (
    <div className={cn('relative min-w-0 overflow-hidden rounded-lg border border-border bg-muted/30', className)}>
      <div className="absolute top-2 right-2 z-10">
        <Button
          type="button"
          variant="ghost"
          size="icon-xs"
          className="rounded-md bg-background/90 shadow-sm hover:bg-background"
          onClick={handleCopy}
          disabled={!hasValue}
          aria-label={copyLabel}
        >
          {copied ? <CheckIcon /> : <CopyIcon />}
        </Button>
      </div>
      <div
        ref={scrollContainerRef}
        onScroll={onScroll}
        className={cn('min-w-0 max-w-full overflow-auto rounded-lg', maxHeightClassName)}
      >
        <pre
          className={cn(
            'p-3 pr-12 text-xs leading-6',
            wrap ? 'w-full whitespace-pre-wrap break-words' : 'w-max min-w-full whitespace-pre',
            preClassName,
          )}
        >
          {displayValue}
        </pre>
      </div>
    </div>
  )
}
