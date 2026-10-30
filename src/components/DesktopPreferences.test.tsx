// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, expect, it, vi } from 'vitest'
import { DesktopPreferences } from './DesktopPreferences'
const api = vi.hoisted(() => ({ snapshot: vi.fn(), preferences: vi.fn(), sleep: vi.fn(), devices: vi.fn(), output: vi.fn(), subscribe: vi.fn() }))
vi.mock('../lib/desktop-ipc', () => ({ desktop: api }))
afterEach(cleanup)
it('uses native preferences and shows a cancellable sleep deadline', async () => {
  const original = { close_to_tray: true, output_device: null, sleep_deadline_ms: null }
  api.snapshot.mockResolvedValue(original)
  api.devices.mockResolvedValue(['Speakers', 'Headphones'])
  api.subscribe.mockResolvedValue(() => {})
  api.preferences.mockResolvedValue({ ...original, close_to_tray: false })
  api.sleep.mockResolvedValue({ ...original, sleep_deadline_ms: Date.now() + 900000 })
  api.output.mockRejectedValue('Headphones is disconnected')
  render(<DesktopPreferences />)
  fireEvent.click(await screen.findByRole('switch', { name: 'Close to tray' }))
  await waitFor(() => expect(api.preferences).toHaveBeenCalledWith(false))
  fireEvent.change(screen.getByLabelText('Sleep timer'), { target: { value: '15' } })
  await waitFor(() => expect(api.sleep).toHaveBeenCalledWith(15))
  expect(await screen.findByText(/Playback pauses at/)).toBeTruthy()
  fireEvent.change(screen.getByLabelText('Audio output'), { target: { value: 'Headphones' } })
  expect(await screen.findByRole('alert')).toHaveProperty('textContent', 'Headphones is disconnected')
  expect((screen.getByLabelText('Audio output') as HTMLSelectElement).value).toBe('')
})
