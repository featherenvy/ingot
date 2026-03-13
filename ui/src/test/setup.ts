import '@testing-library/jest-dom/vitest'

Object.defineProperty(window, 'matchMedia', {
  writable: true,
  value: (query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addListener: () => {},
    removeListener: () => {},
    addEventListener: () => {},
    removeEventListener: () => {},
    dispatchEvent: () => false,
  }),
})

Object.defineProperty(HTMLElement.prototype, 'scrollIntoView', {
  writable: true,
  value: () => {},
})

Object.defineProperty(HTMLElement.prototype, 'hasPointerCapture', {
  writable: true,
  value: () => false,
})

Object.defineProperty(HTMLElement.prototype, 'setPointerCapture', {
  writable: true,
  value: () => {},
})

Object.defineProperty(HTMLElement.prototype, 'releasePointerCapture', {
  writable: true,
  value: () => {},
})
