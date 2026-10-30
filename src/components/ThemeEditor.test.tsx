// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { ThemeEditor } from './ThemeEditor'
import { themeFiles } from '../lib/theme-ipc'

vi.mock('../lib/theme-ipc', () => ({ themeFiles: { document: vi.fn(), preview: vi.fn(), save: vi.fn(), import: vi.fn(), export: vi.fn() } }))
const doc = { raw: 'name = "Original"', name: 'Original', colors: { ground: '#101010', panel: '#202020', ink: '#eeeeee', accent: '#ccccff' }, css: '--color-ground:#101010;', failures: [] }
const click = (name: string) => fireEvent.click(screen.getByRole('button', { name }))
beforeEach(() => {
  vi.resetAllMocks()
  vi.mocked(themeFiles.document).mockResolvedValue(doc)
  vi.mocked(themeFiles.preview).mockResolvedValue(doc)
  vi.mocked(themeFiles.save).mockResolvedValue('custom-new')
})
afterEach(cleanup)

it('edits and validates a copy before applying it', async () => {
  const applied = vi.fn()
  render(<ThemeEditor activeId="capsule" onSaved={applied} />)
  click('Edit theme')
  fireEvent.change(await screen.findByLabelText('Theme name'), { target: { value: 'My palette' } })
  await waitFor(() => expect(themeFiles.preview).toHaveBeenCalledWith(expect.stringContaining('My palette')))
  await waitFor(() => expect((screen.getByRole('button', { name: 'Save as new theme' }) as HTMLButtonElement).disabled).toBe(false))
  click('Save as new theme')
  await waitFor(() => expect(applied).toHaveBeenCalledWith('custom-new'))
  expect(themeFiles.save).toHaveBeenCalledWith(expect.stringContaining('My palette'))
})
it('import previews the file and cancel leaves settings untouched', async () => {
  vi.mocked(themeFiles.import).mockResolvedValue(doc)
  const applied = vi.fn()
  render(<ThemeEditor activeId="capsule" onSaved={applied} />)
  click('Import theme')
  await screen.findByLabelText('Theme name')
  click('Cancel editing')
  expect(themeFiles.save).not.toHaveBeenCalled()
  expect(applied).not.toHaveBeenCalled()
})
it('an invalid preview blocks saving and shows its error', async () => {
  vi.mocked(themeFiles.preview).mockRejectedValue('Invalid color')
  render(<ThemeEditor activeId="capsule" onSaved={() => {}} />)
  click('Edit theme')
  await screen.findByText('Invalid color')
  expect((screen.getByRole('button', { name: 'Save as new theme' }) as HTMLButtonElement).disabled).toBe(true)
  expect(themeFiles.save).not.toHaveBeenCalled()
})
it('export cancellation produces no false success message', async () => {
  vi.mocked(themeFiles.export).mockResolvedValue(false)
  render(<ThemeEditor activeId="paper" onSaved={() => {}} />)
  click('Export theme')
  await waitFor(() => expect(themeFiles.export).toHaveBeenCalledWith('paper'))
  expect(screen.queryByText('Theme exported.')).toBeNull()
})
it('uses the latest settings callback when a save completes after other settings changed', async () => {
  let resolve!: (id: string) => void
  vi.mocked(themeFiles.save).mockReturnValue(new Promise<string>(yes => { resolve = yes }))
  const before = vi.fn()
  const after = vi.fn()
  const view = render(<ThemeEditor activeId="capsule" onSaved={before} />)
  click('Edit theme')
  await screen.findByLabelText('Theme name')
  await waitFor(() => expect((screen.getByRole('button', { name: 'Save as new theme' }) as HTMLButtonElement).disabled).toBe(false))
  click('Save as new theme')
  view.rerender(<ThemeEditor activeId="capsule" onSaved={after} />)
  resolve('custom-new')
  await waitFor(() => expect(after).toHaveBeenCalledWith('custom-new'))
  expect(before).not.toHaveBeenCalled()
})
