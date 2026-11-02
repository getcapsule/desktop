// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, expect, it, vi } from 'vitest'
import { ListeningContext, QueueView, TrackTools } from './ListeningTools'
import { EMPTY_QOL } from '../lib/qol-ipc'
const invoke = vi.hoisted(() => vi.fn(async () => undefined))
vi.mock('@tauri-apps/api/core', () => ({ invoke, convertFileSrc: (s: string) => s }))
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => {}) }))
afterEach(() => { cleanup(); invoke.mockClear(); vi.unstubAllGlobals() })
const song = { id: '/music/one.flac', name: 'One', artist_name: 'Artist', album_name: 'Album', catalog_id: null, duration_ms: 1000, artwork_url: null }
it('offers separate play-next and append actions without replacing playback', async () => {
  render(<ListeningContext.Provider value={{ source: 'local', snapshot: EMPTY_QOL, problem: vi.fn(), artist: vi.fn(), album: vi.fn(), replace: vi.fn() }}><TrackTools songs={[song]} /></ListeningContext.Provider>)
  fireEvent.click(screen.getByRole('button', { name: 'More actions for One' }))
  fireEvent.click(screen.getByRole('menuitem', { name: 'Play next' }))
  await waitFor(() => expect(invoke).toHaveBeenCalledWith('qol_queue_add', { songs: [song], source: 'local', next: true, context: null }))
  fireEvent.click(screen.getByRole('button', { name: 'More actions for One' }))
  fireEvent.click(screen.getByRole('menuitem', { name: 'Add to queue' }))
  await waitFor(() => expect(invoke).toHaveBeenCalledWith('qol_queue_add', { songs: [song], source: 'local', next: false, context: null }))
})
it('keeps queue controls accessible without rendering a large queue eagerly', () => {
  vi.stubGlobal('ResizeObserver', class { observe() {} disconnect() {} })
  render(<QueueView state={{ queue: [], index: null, status: 'paused', position_ms: 0, volume: 50, shuffle: false, repeat: 'off' }} onProblem={vi.fn()} />)
  expect(screen.getByRole('button', { name: 'Undo queue change' })).toBeDefined()
  expect(screen.getByText('Your queue is empty.')).toBeDefined()
  cleanup()
  const queue = Array.from({ length: 10000 }, (_, i) => ({ id: String(i), title: `Track ${i}`, artist: 'Artist', album: '', duration_ms: 1000 }))
  render(<QueueView state={{ queue, index: 0, status: 'paused', position_ms: 0, volume: 50, shuffle: false, repeat: 'off' }} onProblem={vi.fn()} />)
  expect(screen.getAllByRole('button', { name: /^Play queue item/ }).length).toBeLessThan(100)
  vi.unstubAllGlobals()
})
it('supports menu keyboard navigation and preserves original favorite context', async () => {
  render(<ListeningContext.Provider value={{ source: 'navidrome', context: 'current', snapshot: { ...EMPTY_QOL, favorites: [{ song, source: 'navidrome', context: 'original' }] }, problem: vi.fn(), artist: vi.fn(), album: vi.fn(), replace: vi.fn() }}><TrackTools songs={[song]} source="navidrome" context="original" /></ListeningContext.Provider>)
  fireEvent.click(screen.getByRole('button', { name: 'More actions for One' }))
  const playNext = screen.getByRole('menuitem', { name: 'Play next' })
  playNext.focus()
  fireEvent.keyDown(playNext, { key: 'ArrowDown' })
  expect(document.activeElement).toBe(screen.getByRole('menuitem', { name: 'Add to queue' }))
  fireEvent.click(screen.getByRole('menuitem', { name: 'Remove from favorites' }))
  await waitFor(() => expect(invoke).toHaveBeenCalledWith('qol_favorites', { songs: [song], source: 'navidrome', on: false, context: 'original' }))
  fireEvent.click(screen.getByRole('button', { name: 'More actions for One' }))
  fireEvent.keyDown(screen.getByRole('menuitem', { name: 'Play next' }), { key: 'Escape' })
  expect(screen.queryByRole('menu')).toBeNull()
})

it('starts the visible queue at the current song and follows playback advances', () => {
  vi.stubGlobal('ResizeObserver', class { observe() {} disconnect() {} })
  const queue = Array.from({ length: 20 }, (_, i) => ({ id: String(i), title: `Track ${i}`, artist: 'Artist', album: '', duration_ms: 1000 }))
  const state = { queue, index: 7, status: 'playing' as const, position_ms: 0, volume: 50, shuffle: false, repeat: 'off' as const }
  const { rerender } = render(<QueueView state={state} onProblem={vi.fn()} />)
  expect(screen.queryByRole('button', { name: 'Play queue item 1: Track 0' })).toBeNull()
  const current = screen.getByRole('button', { name: 'Play queue item 1: Track 7' })
  expect(current.closest('[data-playing]')?.getAttribute('data-playing')).toBe('true')
  expect(screen.getByText('13 tracks')).toBeDefined()
  fireEvent.scroll(current.closest('.overflow-y-auto')!, { target: { scrollTop: 1040 } })
  rerender(<QueueView state={{ ...state, index: 8 }} onProblem={vi.fn()} />)
  expect(screen.getByRole('button', { name: 'Play queue item 1: Track 8' })).toBeDefined()
  expect(screen.getByText('12 tracks')).toBeDefined()
})

it('edits upcoming queue entries using their original positions and keeps the current song first', async () => {
  vi.stubGlobal('ResizeObserver', class { observe() {} disconnect() {} })
  const queue = Array.from({ length: 10 }, (_, i) => ({ id: 'duplicate', title: `Track ${i}`, artist: 'Artist', album: '', duration_ms: 1000 }))
  render(<QueueView state={{ queue, index: 7, status: 'playing', position_ms: 0, volume: 50, shuffle: false, repeat: 'off' }} onProblem={vi.fn()} />)
  expect((screen.getByRole('button', { name: 'Move queue item 1 down' }) as HTMLButtonElement).disabled).toBe(true)
  expect((screen.getByRole('button', { name: 'Move queue item 2 up' }) as HTMLButtonElement).disabled).toBe(true)
  fireEvent.click(screen.getByRole('button', { name: 'Play queue item 2: Track 8' }))
  await waitFor(() => expect(invoke).toHaveBeenCalledWith('qol_queue_jump', { index: 8 }))
  fireEvent.click(screen.getByRole('button', { name: 'Remove queue item 2' }))
  await waitFor(() => expect(invoke).toHaveBeenCalledWith('qol_queue_remove', { index: 8 }))
  fireEvent.click(screen.getByRole('button', { name: 'Move queue item 3 up' }))
  await waitFor(() => expect(invoke).toHaveBeenCalledWith('qol_queue_move', { from: 9, to: 8 }))
  const last = screen.getByRole('button', { name: 'Play queue item 3: Track 9' }).closest('[draggable]')!
  const next = screen.getByRole('button', { name: 'Play queue item 2: Track 8' }).closest('[draggable]')!
  const dataTransfer = { setData: vi.fn(), effectAllowed: '', dropEffect: '' }
  invoke.mockClear()
  fireEvent.dragStart(last, { dataTransfer })
  fireEvent.drop(next, { dataTransfer })
  await waitFor(() => expect(invoke).toHaveBeenCalledWith('qol_queue_move', { from: 9, to: 8 }))
})
