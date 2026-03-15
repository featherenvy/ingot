import { Loader2Icon } from 'lucide-react'
import { useCallback } from 'react'
import { cn } from '@/lib/utils'

export type SectionIndicator = 'active' | 'warning' | 'error' | null

export type Section = {
  id: string
  label: string
  count: number
  indicator?: SectionIndicator
}

/** Root header is h-12 (48px). SectionNav sits below it. */
const ROOT_HEADER_HEIGHT = 48

export function SectionNav({ sections, activeSectionId }: { sections: Section[]; activeSectionId: string | null }) {
  const scrollTo = useCallback((id: string) => {
    const el = document.getElementById(id)
    if (!el) return
    // Offset by root header + this nav's own height (~40px) + 8px breathing room
    const y = el.getBoundingClientRect().top + window.scrollY - ROOT_HEADER_HEIGHT - 48
    window.scrollTo({ top: y, behavior: 'smooth' })
  }, [])

  return (
    <nav
      className="sticky z-10 -mx-1 border-b border-border/60 bg-background/95 px-1 backdrop-blur-sm"
      style={{ top: ROOT_HEADER_HEIGHT }}
    >
      <div className="flex gap-1 overflow-x-auto py-1">
        {sections.map((s) => {
          const isActive = activeSectionId === s.id
          return (
            <button
              key={s.id}
              type="button"
              onClick={() => scrollTo(s.id)}
              className={cn(
                'relative inline-flex shrink-0 items-center gap-1.5 rounded-md px-3 py-1.5 text-sm font-medium transition-colors',
                isActive ? 'text-foreground' : 'text-muted-foreground hover:text-foreground',
              )}
            >
              {s.label}
              <span
                className={cn(
                  'inline-flex h-5 min-w-5 items-center justify-center gap-1 rounded-full px-1.5 text-[11px] font-medium tabular-nums',
                  isActive ? 'bg-foreground text-background' : 'bg-muted text-muted-foreground',
                  !isActive && s.indicator === 'warning' && 'bg-amber-500/15 text-amber-700 dark:text-amber-400',
                  !isActive && s.indicator === 'error' && 'bg-destructive/10 text-destructive',
                  !isActive && s.indicator === 'active' && 'bg-blue-500/15 text-blue-700 dark:text-blue-400',
                )}
              >
                {s.indicator === 'active' && !isActive && <Loader2Icon className="size-3 animate-spin" />}
                {s.indicator === 'warning' && !isActive && <span className="size-1.5 rounded-full bg-amber-500" />}
                {s.indicator === 'error' && !isActive && <span className="size-1.5 rounded-full bg-destructive" />}
                {s.count}
              </span>
              {isActive && <span className="absolute inset-x-1 -bottom-1 h-0.5 rounded-full bg-foreground" />}
            </button>
          )
        })}
      </div>
    </nav>
  )
}
