import { CheckIcon, CircleDotIcon, CircleIcon } from 'lucide-react'
import { cn } from '@/lib/utils'
import type { Item, PhaseKind } from '../../types/domain'
import { Tooltip, TooltipContent, TooltipTrigger } from '../ui/tooltip'
import { WORKFLOW_PHASES_BY_VERSION, type WorkflowPhaseDef } from './workflowPresentation'

const PHASE_DOT_COLORS: Record<PhaseKind, string> = {
  author: 'bg-blue-500',
  review: 'bg-amber-500',
  validate: 'bg-emerald-500',
  investigate: 'bg-purple-500',
  system: 'bg-muted-foreground',
}

const PHASE_TEXT_COLORS: Record<PhaseKind, string> = {
  author: 'text-blue-600 dark:text-blue-400',
  review: 'text-amber-600 dark:text-amber-400',
  validate: 'text-emerald-600 dark:text-emerald-400',
  investigate: 'text-purple-600 dark:text-purple-400',
  system: 'text-muted-foreground',
}

// ── State derivation ───────────────────────────────────────────

type StepState = 'completed' | 'current' | 'future'
type PhaseState = 'completed' | 'active' | 'future'

function stepIndex(stepId: string | null, allStepIds: string[]): number {
  if (!stepId) return -1
  return allStepIds.indexOf(stepId)
}

function getStepState(id: string, currentId: string | null, allStepIds: string[]): StepState {
  const cur = stepIndex(currentId, allStepIds)
  const idx = stepIndex(id, allStepIds)
  if (cur === -1 || idx === -1) return 'future'
  if (idx < cur) return 'completed'
  if (idx === cur) return 'current'
  return 'future'
}

function getPhaseState(phase: WorkflowPhaseDef, currentId: string | null, allStepIds: string[]): PhaseState {
  const states = phase.steps.map((s) => getStepState(s.id, currentId, allStepIds))
  if (states.includes('current')) return 'active'
  if (states.every((s) => s === 'completed')) return 'completed'
  if (states.every((s) => s === 'future')) return 'future'
  // Mix of completed and future → phase is done (current is in a later phase)
  return 'completed'
}

// ── Component ──────────────────────────────────────────────────

export function WorkflowStepper({
  workflowVersion,
  currentStepId,
  dispatchableStepId,
}: {
  workflowVersion: Item['workflow_version']
  currentStepId: string | null
  dispatchableStepId: string | null
}) {
  const workflowPhases = WORKFLOW_PHASES_BY_VERSION[workflowVersion]
  const allStepIds = workflowPhases.flatMap((phase) => phase.steps.map((step) => step.id))
  const visualCurrentStepId = currentStepId ?? dispatchableStepId
  const currentStep = workflowPhases.flatMap((phase) => phase.steps).find((step) => step.id === visualCurrentStepId)

  return (
    <div className="space-y-2">
      {/* Phase segments */}
      <div className="flex items-center">
        {workflowPhases.map((phase, i) => {
          const state = getPhaseState(phase, visualCurrentStepId, allStepIds)

          return (
            <div key={phase.id} className="flex items-center">
              {/* Connector line */}
              {i > 0 && (
                <div className={cn('mx-1 h-px w-5 sm:w-8', state === 'future' ? 'bg-border' : 'bg-foreground/15')} />
              )}

              {/* Phase block */}
              <div
                data-phase-id={phase.id}
                data-phase-state={state}
                className={cn(
                  'flex items-center gap-2 rounded-lg px-3 py-2 text-sm transition-colors',
                  state === 'active' && 'bg-foreground/[0.04] ring-1 ring-foreground/[0.08]',
                  state === 'future' && 'opacity-40',
                )}
              >
                {/* Phase icon */}
                {state === 'completed' && <CheckIcon className="size-3.5 text-emerald-500" strokeWidth={2.5} />}
                {state === 'active' && (
                  <div className="relative flex items-center justify-center">
                    <span
                      className={cn(
                        'absolute size-4 animate-ping rounded-full opacity-20',
                        currentStep ? PHASE_DOT_COLORS[currentStep.phase] : 'bg-foreground',
                      )}
                    />
                    <CircleDotIcon
                      className={cn('size-3.5', currentStep ? PHASE_TEXT_COLORS[currentStep.phase] : '')}
                      strokeWidth={2.5}
                    />
                  </div>
                )}
                {state === 'future' && <CircleIcon className="size-3 text-muted-foreground" strokeWidth={2} />}

                {/* Phase label */}
                <span className={cn('font-medium', state === 'active' ? 'text-foreground' : 'text-muted-foreground')}>
                  {phase.label}
                </span>

                {/* Step progress dots (active phase only) */}
                {state === 'active' && phase.steps.length > 1 && (
                  <div className="ml-0.5 flex items-center gap-1">
                    {phase.steps.map((step) => {
                      const ss = getStepState(step.id, visualCurrentStepId, allStepIds)
                      return (
                        <Tooltip key={step.id}>
                          <TooltipTrigger asChild>
                            <div
                              data-step-id={step.id}
                              data-step-state={ss}
                              className={cn(
                                'rounded-full transition-all',
                                ss === 'completed' && cn('size-1.5', PHASE_DOT_COLORS[step.phase]),
                                ss === 'current' && cn('size-2', PHASE_DOT_COLORS[step.phase]),
                                ss === 'future' && 'size-1.5 bg-foreground/10',
                              )}
                            />
                          </TooltipTrigger>
                          <TooltipContent side="bottom" className="text-xs">
                            {step.label}
                            <span className="ml-1.5 text-muted-foreground">{step.id}</span>
                          </TooltipContent>
                        </Tooltip>
                      )
                    })}
                  </div>
                )}
              </div>
            </div>
          )
        })}
      </div>

      {/* Current step callout */}
      {currentStep && (
        <div className="flex items-center gap-2 pl-1">
          <span className={cn('size-1.5 rounded-full', PHASE_DOT_COLORS[currentStep.phase])} />
          <span className="text-sm font-medium">{currentStep.label}</span>
          <code className="text-[11px] text-muted-foreground">{currentStep.id}</code>
        </div>
      )}
    </div>
  )
}
