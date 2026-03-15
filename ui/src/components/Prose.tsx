import Markdown from 'react-markdown'
import { cn } from '@/lib/utils'

export function Prose({ content, className }: { content: string; className?: string }) {
  return (
    <div className={cn('space-y-2 text-sm leading-relaxed', className)}>
      <Markdown
        components={{
          h1: ({ children }) => <h1 className="mt-4 mb-2 text-lg font-semibold tracking-tight">{children}</h1>,
          h2: ({ children }) => <h2 className="mt-3 mb-1.5 text-base font-semibold tracking-tight">{children}</h2>,
          h3: ({ children }) => <h3 className="mt-2 mb-1 text-sm font-semibold">{children}</h3>,
          p: ({ children }) => <p className="leading-relaxed last:mb-0">{children}</p>,
          ul: ({ children }) => <ul className="list-disc space-y-0.5 pl-5">{children}</ul>,
          ol: ({ children }) => <ol className="list-decimal space-y-0.5 pl-5">{children}</ol>,
          li: ({ children }) => <li className="leading-relaxed">{children}</li>,
          code: ({ children }) => (
            <code className="rounded bg-muted px-1.5 py-0.5 font-mono text-[0.85em]">{children}</code>
          ),
          pre: ({ children }) => (
            <pre className="overflow-x-auto rounded-lg bg-muted p-3 font-mono text-xs leading-6">{children}</pre>
          ),
          blockquote: ({ children }) => (
            <blockquote className="border-l-2 border-border pl-3 text-muted-foreground italic">{children}</blockquote>
          ),
          strong: ({ children }) => <strong className="font-semibold">{children}</strong>,
          a: ({ href, children }) => (
            <a href={href} className="underline underline-offset-2" target="_blank" rel="noopener noreferrer">
              {children}
            </a>
          ),
          hr: () => <hr className="my-3 border-border" />,
        }}
      >
        {content}
      </Markdown>
    </div>
  )
}
