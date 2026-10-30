// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, expect, it, vi } from 'vitest'
import { ListeningContext, QueueView, TrackTools } from './ListeningTools'
import { EMPTY_QOL } from '../lib/qol-ipc'
const invoke = vi.hoisted(() => vi.fn(async () => undefined))
vi.mock('@tauri-apps/api/core', () => ({ invoke, convertFileSrc: (s: string) => s }))
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => {}) }))
afterEach(() => { cleanup(); invoke.mockClear() })
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
