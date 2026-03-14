import { CheckIcon, CircleDotIcon, CircleIcon } from 'lucide-react'
import { cn } from '@/lib/utils'
import type { PhaseKind } from '../../types/domain'
import { Tooltip, TooltipContent, TooltipTrigger } from '../ui/tooltip'

type WorkflowStep = {
  id: string
  phase: PhaseKind
  label: string
}

/**
 * The canonical delivery:v1 workflow steps, condensed for display.
 * Groups of related steps (e.g. initial + repair review) are collapsed
 * into logical phases the operator cares about.
 */
const WORKFLOW_STEPS: WorkflowStep[] = [
  { id: 'author_initial', phase: 'author', label: 'Author' },
  { id: 'review_incremental_initial', phase: 'review', label: 'Incr. Review' },
  { id: 'review_candidate_initial', phase: 'review', label: 'Cand. Review' },
  { id: 'validate_candidate_initial', phase: 'validate', label: 'Validate' },
  { id: 'repair_candidate', phase: 'author', label: 'Repair' },
  { id: 'review_incremental_repair', phase: 'review', label: 'Re-review' },
  { id: 'review_candidate_repair', phase: 'review', label: 'Cand. Re-review' },
  { id: 'validate_candidate_repair', phase: 'validate', label: 'Re-validate' },
  { id: 'investigate_item', phase: 'investigate', label: 'Investigate' },
  { id: 'prepare_convergence', phase: 'system', label: 'Converge' },
  { id: 'validate_integrated', phase: 'validate', label: 'Int. Validate' },
  { id: 'repair_after_integration', phase: 'author', label: 'Int. Repair' },
  { id: 'review_incremental_after_integration_repair', phase: 'review', label: 'Int. Review' },
  { id: 'review_after_integration_repair', phase: 'review', label: 'Int. Cand. Review' },
  { id: 'validate_after_integration_repair', phase: 'validate', label: 'Int. Re-validate' },
]

const phaseColors: Record<PhaseKind, string> = {
  author: 'text-blue-600 dark:text-blue-400',
  review: 'text-amber-600 dark:text-amber-400',
  validate: 'text-emerald-600 dark:text-emerald-400',
  investigate: 'text-purple-600 dark:text-purple-400',
  system: 'text-muted-foreground',
}

const phaseBgColors: Record<PhaseKind, string> = {
  author: 'bg-blue-600 dark:bg-blue-400',
  review: 'bg-amber-600 dark:bg-amber-400',
  validate: 'bg-emerald-600 dark:bg-emerald-400',
  investigate: 'bg-purple-600 dark:bg-purple-400',
  system: 'bg-muted-foreground',
}

type StepState = 'completed' | 'current' | 'future'

function getStepState(stepId: string, currentStepId: string | null): StepState {
  if (!currentStepId) return 'future'
  if (stepId === currentStepId) return 'current'
  const currentIdx = WORKFLOW_STEPS.findIndex((s) => s.id === currentStepId)
  const stepIdx = WORKFLOW_STEPS.findIndex((s) => s.id === stepId)
  if (currentIdx === -1 || stepIdx === -1) return 'future'
  return stepIdx < currentIdx ? 'completed' : 'future'
}

export function WorkflowStepper({ currentStepId }: { currentStepId: string | null }) {
  return (
    <ol className="flex items-center gap-0.5 overflow-x-auto py-1" aria-label="Workflow progress">
      {WORKFLOW_STEPS.map((step, i) => {
        const state = getStepState(step.id, currentStepId)
        return (
          <li key={step.id} className="flex list-none items-center">
            {i > 0 && (
              <div
                className={cn(
                  'h-px w-2 shrink-0',
                  state === 'future' ? 'bg-border' : phaseBgColors[step.phase],
                  state === 'future' && 'opacity-40',
                )}
              />
            )}
            <Tooltip>
              <TooltipTrigger asChild>
                <div
                  className={cn(
                    'flex shrink-0 items-center justify-center',
                    state === 'current' && 'relative',
                    state === 'future' && 'opacity-30',
                  )}
                >
                  {state === 'completed' && (
                    <CheckIcon className={cn('size-3.5', phaseColors[step.phase])} strokeWidth={2.5} />
                  )}
                  {state === 'current' && (
                    <>
                      <span
                        className={cn(
                          'absolute inset-0 m-auto size-5 animate-ping rounded-full opacity-20',
                          phaseBgColors[step.phase],
                        )}
                      />
                      <CircleDotIcon className={cn('size-4', phaseColors[step.phase])} strokeWidth={2.5} />
                    </>
                  )}
                  {state === 'future' && <CircleIcon className="size-3 text-muted-foreground" strokeWidth={2} />}
                </div>
              </TooltipTrigger>
              <TooltipContent side="bottom" className="text-xs">
                <span className="font-mono">{step.id}</span>
                <span className="ml-1.5 text-muted-foreground">({step.label})</span>
              </TooltipContent>
            </Tooltip>
          </li>
        )
      })}
    </ol>
  )
}
