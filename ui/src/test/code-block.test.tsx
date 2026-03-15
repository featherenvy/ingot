import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import { CodeBlock } from '../components/CodeBlock'

describe('CodeBlock', () => {
  it('renders inside a natively scrollable container for overflowing content', () => {
    const { container } = render(<CodeBlock value={'x'.repeat(400)} />)

    expect(container.querySelector('div.min-w-0.max-w-full.overflow-auto.rounded-lg')).toBeInTheDocument()
    expect(container.querySelector('pre')).toHaveClass('w-max', 'min-w-full', 'whitespace-pre')
  })

  it('copies the raw value to the clipboard', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined)

    Object.defineProperty(navigator, 'clipboard', {
      configurable: true,
      value: {
        writeText,
      },
    })

    render(<CodeBlock value={'{"ok":true}'} copyLabel="Copy payload" />)

    fireEvent.click(screen.getByRole('button', { name: 'Copy payload' }))

    await waitFor(() => expect(writeText).toHaveBeenCalledWith('{"ok":true}'))
  })
})
