import { render, screen } from '@testing-library/react'
import { WorkflowStepper } from '../components/item-detail/WorkflowStepper'
import { TooltipProvider } from '../components/ui/tooltip'
import type { Item } from '../types/domain'

function renderStepper(props: {
  workflowVersion: Item['workflow_version']
  currentStepId: string | null
  dispatchableStepId: string | null
}) {
  return render(
    <TooltipProvider>
      <WorkflowStepper {...props} />
    </TooltipProvider>,
  )
}

describe('WorkflowStepper', () => {
  it('uses the dispatchable investigation step as the visual current step for new items', () => {
    const { container } = renderStepper({
      workflowVersion: 'investigation:v1',
      currentStepId: null,
      dispatchableStepId: 'investigate_project',
    })

    expect(screen.getByText('Investigation')).toBeInTheDocument()
    expect(screen.getByText('Investigate')).toBeInTheDocument()
    expect(screen.getByText('investigate_project')).toBeInTheDocument()
    expect(container.querySelector('[data-phase-id="investigation"]')).toHaveAttribute('data-phase-state', 'active')
    expect(container.querySelector('[data-step-id="investigate_project"]')).toHaveAttribute(
      'data-step-state',
      'current',
    )
    expect(container.querySelector('[data-step-id="reinvestigate_project"]')).toHaveAttribute(
      'data-step-state',
      'future',
    )
  })

  it('marks the first investigation step completed during reinvestigation', () => {
    const { container } = renderStepper({
      workflowVersion: 'investigation:v1',
      currentStepId: 'reinvestigate_project',
      dispatchableStepId: null,
    })

    expect(screen.getByText('Reinvestigate')).toBeInTheDocument()
    expect(screen.getByText('reinvestigate_project')).toBeInTheDocument()
    expect(container.querySelector('[data-step-id="investigate_project"]')).toHaveAttribute(
      'data-step-state',
      'completed',
    )
    expect(container.querySelector('[data-step-id="reinvestigate_project"]')).toHaveAttribute(
      'data-step-state',
      'current',
    )
  })

  it('keeps delivery workflow phases distinct', () => {
    const { container } = renderStepper({
      workflowVersion: 'delivery:v1',
      currentStepId: 'author_initial',
      dispatchableStepId: null,
    })

    expect(screen.getByText('Candidate')).toBeInTheDocument()
    expect(screen.getByText('Converge')).toBeInTheDocument()
    expect(screen.getByText('Integration')).toBeInTheDocument()
    expect(container.querySelector('[data-phase-id="candidate"]')).toHaveAttribute('data-phase-state', 'active')
    expect(container.querySelector('[data-phase-id="converge"]')).toHaveAttribute('data-phase-state', 'future')
  })
})
