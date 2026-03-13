import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import { CodeBlock } from '../components/CodeBlock'

describe('CodeBlock', () => {
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
