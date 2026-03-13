import { CodeBlock } from './CodeBlock'

export function LogBlock({ label, value }: { label: string; value: string | null | undefined }) {
  return (
    <div className="space-y-2">
      <strong className="text-sm font-medium">{label}</strong>
      <CodeBlock value={value} wrap copyLabel={`Copy ${label.toLowerCase()}`} maxHeightClassName="max-h-72" />
    </div>
  )
}
