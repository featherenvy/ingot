import { ScrollArea } from './ui/scroll-area'

export function LogBlock({ label, value }: { label: string; value: string | null | undefined }) {
  return (
    <div className="space-y-2">
      <strong className="text-sm font-medium">{label}</strong>
      <ScrollArea className="max-h-72 rounded-lg border border-border bg-muted/30">
        <pre className="w-max min-w-full p-3 text-xs leading-6 whitespace-pre-wrap break-words">
          {value && value.trim().length > 0 ? value : 'No data.'}
        </pre>
      </ScrollArea>
    </div>
  )
}
