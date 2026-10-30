// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { Lyrics } from './Lyrics'
import type { PlayerState } from '../lib/ipc'
const invoke = vi.hoisted(() => vi.fn())
vi.mock('@tauri-apps/api/core', () => ({ invoke, convertFileSrc: (s: string) => s }))
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn() }))
const state = (id: string): PlayerState => ({ queue: [{ id, title: id, artist: 'Artist', album: '', duration_ms: 60000 }], index: 0, status: 'paused', position_ms: 0, volume: 50, shuffle: false, repeat: 'off' })
beforeEach(() => { Element.prototype.scrollIntoView = vi.fn(); invoke.mockImplementation(async (cmd: string) => cmd === 'settings_get' ? { lyrics: { offset_ms: 500 } } : { lines: [], plain: null }) })
afterEach(() => { cleanup(); invoke.mockReset() })
it('clears the previous track while the next lookup is pending and ignores stale responses', async () => {
  const pending = new Map<string, (r: unknown) => void>()
  invoke.mockImplementation(async (cmd: string, args: { trackId?: string }) => cmd === 'settings_get' ? { lyrics: { offset_ms: 0 } } : args.trackId === 'a' ? { lines: [], plain: 'Old words' } : new Promise(resolve => { pending.set(args.trackId!, resolve) }))
  const view = render(<Lyrics state={state('a')} />)
  await screen.findByText('Old words')
  view.rerender(<Lyrics state={state('b')} />)
  expect(screen.queryByText('Old words')).toBeNull()
  view.rerender(<Lyrics state={state('c')} />)
  await act(async () => pending.get('c')!({ lines: [], plain: 'New words' }))
  await act(async () => pending.get('b')!({ lines: [], plain: 'Stale words' }))
  expect(screen.getByText('New words')).toBeDefined()
  expect(screen.queryByText('Stale words')).toBeNull()
})
it('shows a retry action when lookup fails and can recover', async () => {
  invoke.mockImplementation(async (cmd: string) => { if (cmd === 'settings_get') return { lyrics: { offset_ms: 0 } }; throw new Error('offline') })
  render(<Lyrics state={state('a')} />)
  const retry = await screen.findByRole('button', { name: 'Retry' })
  invoke.mockResolvedValue({ lines: [], plain: 'Recovered' })
  fireEvent.click(retry)
  await screen.findByText('Recovered')
})
it('seeks to the lyric timestamp plus the saved timing offset', async () => {
  invoke.mockImplementation(async (cmd: string) => cmd === 'settings_get' ? { lyrics: { offset_ms: 500 } } : { lines: [{ at_ms: 10000, text: 'Jump here' }], plain: null })
  render(<Lyrics state={state('a')} />)
  fireEvent.click(await screen.findByRole('button', { name: 'Jump here' }))
  await waitFor(() => expect(invoke).toHaveBeenCalledWith('player_seek', { ms: 10500 }))
})
