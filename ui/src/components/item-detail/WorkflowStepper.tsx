import { CheckIcon, CircleDotIcon, CircleIcon } from 'lucide-react'
import { cn } from '@/lib/utils'
import type { PhaseKind } from '../../types/domain'
import { Tooltip, TooltipContent, TooltipTrigger } from '../ui/tooltip'

// ── Phase & step definitions ───────────────────────────────────

type StepDef = { id: string; label: string; phase: PhaseKind }
type PhaseDef = { id: string; label: string; steps: StepDef[] }

const WORKFLOW_PHASES: PhaseDef[] = [
  {
    id: 'candidate',
    label: 'Candidate',
    steps: [
      { id: 'author_initial', label: 'Author', phase: 'author' },
      { id: 'review_incremental_initial', label: 'Incr. Review', phase: 'review' },
      { id: 'review_candidate_initial', label: 'Cand. Review', phase: 'review' },
      { id: 'validate_candidate_initial', label: 'Validate', phase: 'validate' },
      { id: 'repair_candidate', label: 'Repair', phase: 'author' },
      { id: 'review_incremental_repair', label: 'Re-review', phase: 'review' },
      { id: 'review_candidate_repair', label: 'Cand. Re-review', phase: 'review' },
      { id: 'validate_candidate_repair', label: 'Re-validate', phase: 'validate' },
      { id: 'investigate_item', label: 'Investigate', phase: 'investigate' },
    ],
  },
  {
    id: 'converge',
    label: 'Converge',
    steps: [{ id: 'prepare_convergence', label: 'Prepare', phase: 'system' }],
  },
  {
    id: 'integration',
    label: 'Integration',
    steps: [
      { id: 'validate_integrated', label: 'Validate', phase: 'validate' },
      { id: 'repair_after_integration', label: 'Repair', phase: 'author' },
      { id: 'review_incremental_after_integration_repair', label: 'Incr. Review', phase: 'review' },
      { id: 'review_after_integration_repair', label: 'Cand. Review', phase: 'review' },
      { id: 'validate_after_integration_repair', label: 'Re-validate', phase: 'validate' },
    ],
  },
]

const ALL_STEP_IDS = WORKFLOW_PHASES.flatMap((p) => p.steps.map((s) => s.id))

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

function stepIndex(stepId: string | null): number {
  if (!stepId) return -1
  return ALL_STEP_IDS.indexOf(stepId)
}

function getStepState(id: string, currentId: string | null): StepState {
  const cur = stepIndex(currentId)
  const idx = stepIndex(id)
  if (cur === -1 || idx === -1) return 'future'
  if (idx < cur) return 'completed'
  if (idx === cur) return 'current'
  return 'future'
}

function getPhaseState(phase: PhaseDef, currentId: string | null): PhaseState {
  const states = phase.steps.map((s) => getStepState(s.id, currentId))
  if (states.includes('current')) return 'active'
  if (states.every((s) => s === 'completed')) return 'completed'
  if (states.every((s) => s === 'future')) return 'future'
  // Mix of completed and future → phase is done (current is in a later phase)
  return 'completed'
}

// ── Component ──────────────────────────────────────────────────

export function WorkflowStepper({ currentStepId }: { currentStepId: string | null }) {
  const currentStep = WORKFLOW_PHASES.flatMap((p) => p.steps).find((s) => s.id === currentStepId)

  return (
    <div className="space-y-2">
      {/* Phase segments */}
      <div className="flex items-center">
        {WORKFLOW_PHASES.map((phase, i) => {
          const state = getPhaseState(phase, currentStepId)

          return (
            <div key={phase.id} className="flex items-center">
              {/* Connector line */}
              {i > 0 && (
                <div className={cn('mx-1 h-px w-5 sm:w-8', state === 'future' ? 'bg-border' : 'bg-foreground/15')} />
              )}

              {/* Phase block */}
              <div
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
                      const ss = getStepState(step.id, currentStepId)
                      return (
                        <Tooltip key={step.id}>
                          <TooltipTrigger asChild>
                            <div
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
