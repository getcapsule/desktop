// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react'
import { afterEach, expect, it, vi } from 'vitest'
import { AppBoundary } from './AppBoundary'
afterEach(() => { cleanup(); vi.restoreAllMocks() })
it('keeps a visible recovery screen when the application throws during startup', () => {
  vi.spyOn(console, 'error').mockImplementation(() => {})
  function Broken(): never { throw new Error('Missing library component') }
  render(<AppBoundary><Broken /></AppBoundary>)
  expect(screen.getByRole('alert').textContent).toContain('Missing library component')
  expect(screen.getByRole('button', { name: 'Reload capsule' })).toBeTruthy()
})
it('renders the application normally when startup succeeds', () => {
  render(<AppBoundary><div>Library loaded</div></AppBoundary>)
  expect(screen.getByText('Library loaded')).toBeTruthy()
  expect(screen.queryByRole('alert')).toBeNull()
})
