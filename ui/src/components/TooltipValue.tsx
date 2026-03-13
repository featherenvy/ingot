import type { ReactElement, ReactNode } from 'react'
import { Tooltip, TooltipContent, TooltipTrigger } from './ui/tooltip'

export function TooltipValue({ content, children }: { content: ReactNode; children: ReactElement }) {
  return (
    <Tooltip>
      <TooltipTrigger asChild>{children}</TooltipTrigger>
      <TooltipContent>
        <span className="font-mono text-[11px]">{content}</span>
      </TooltipContent>
    </Tooltip>
  )
}
