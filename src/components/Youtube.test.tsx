// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import App from '../App'
import { Onboarding } from './Onboarding'
import { SettingsView } from './SettingsView'
import { SOURCES } from './fields'
import type { Settings } from '../lib/ipc'

const boundary = vi.hoisted(() => ({ invoke: vi.fn(), handlers: new Map<string, Set<(event: { payload: unknown }) => void>>() }))
vi.mock('@tauri-apps/api/core', () => ({ invoke: boundary.invoke, convertFileSrc: (id: string) => id }))
vi.mock('@tauri-apps/api/event', () => ({ listen: async (name: string, handler: (event: { payload: unknown }) => void) => {
  const handlers = boundary.handlers.get(name) ?? new Set()
  handlers.add(handler)
  boundary.handlers.set(name, handlers)
  return () => handlers.delete(handler)
} }))
vi.mock('@tauri-apps/api/window', () => ({ getCurrentWindow: () => ({ isMaximized: async () => false, onResized: async () => () => {} }) }))

const counts = { songs: 0, albums: 0, playlists: 0, artists: 0 }
const idle = { status: 'idle', queue: [], index: null, position_ms: 0, volume: 100, shuffle: false, repeat: 'off' }
let stored: Settings
let session: { authenticated: boolean; playback_supported: boolean }
const click = (name: string | RegExp) => fireEvent.click(screen.getByRole('button', { name }))
function emit(name: string, payload: unknown) {
  for (const handler of boundary.handlers.get(name) ?? []) handler({ payload })
}
beforeEach(() => {
  stored = {
    source: 'youtube', onboarded: true, developer: false,
    spotify: { client_id: '' }, appearance: { glass: 'none', theme: 'dark' },
    navidrome: { url: '', username: '' }, local: { folders: [] }, lyrics: { offset_ms: 0 },
    lastfm: { api_key: '', shared_secret: '' }, discord: { client_id: '', serve_art_from_server: false },
  }
  session = { authenticated: false, playback_supported: true }
  boundary.handlers.clear()
  boundary.invoke.mockReset().mockImplementation(async (command: string, args?: { settings?: Settings; cookie?: string }) => {
    switch (command) {
      case 'settings_get': return structuredClone(stored)
      case 'settings_set': {
        const changed = stored.source !== args!.settings!.source
        stored = structuredClone(args!.settings!)
        if (changed) emit('library://updated', counts)
        return
      }
      case 'youtube_status': return { ...session }
      case 'youtube_connect':
        if (stored.source !== 'youtube') throw new Error('Select YouTube first')
        return
      case 'youtube_disconnect': return
      case 'auth_status': return { authenticated: true, storefront: 'us' }
      case 'lastfm_status': return { configured: false, linked: false, username: null }
      case 'themes_list': case 'library_songs': case 'library_albums': case 'library_artists': case 'library_playlists': return []
      case 'library_counts': return counts
      case 'player_snapshot': return idle
      default: return
    }
  })
  vi.stubGlobal('ResizeObserver', class { observe() {} unobserve() {} disconnect() {} })
  Element.prototype.scrollIntoView = vi.fn()
})
afterEach(() => { cleanup(); vi.unstubAllGlobals() })

it('offers Local files, Navidrome, YouTube Music and SoundCloud as playback sources', () => {
  expect(SOURCES.map(source => source.id)).toEqual(['local', 'navidrome', 'youtube', 'soundcloud'])
})

it('shows public playback guidance without cookie or private-account setup', async () => {
  render(<SettingsView />)
  expect(await screen.findByText(/Ready to play/)).toBeDefined()
  expect(screen.queryByLabelText('YouTube Music Cookie')).toBeNull()
  expect(screen.queryByRole('button', { name: 'Connect YouTube Music' })).toBeNull()
  expect(boundary.invoke).not.toHaveBeenCalledWith('youtube_status')
})

it('continues YouTube onboarding without authentication or a private library sync', async () => {
  stored.source = 'local'
  render(<Onboarding onFinished={() => {}} />)
  await act(async () => {})
  click('Get started'); click(/^YouTube Music/); click('Continue')
  await screen.findByText(/Ready to play/)
  click('Continue')
  expect(await screen.findByRole('heading', { name: 'Last.fm scrobbling' })).toBeDefined()
  await waitFor(() => expect(stored.source).toBe('youtube'))
  for (const command of ['youtube_connect', 'youtube_status', 'youtube_open_player', 'library_sync']) {
    expect(boundary.invoke.mock.calls.some(([name]) => name === command)).toBe(false)
  }
})

it('ignores obsolete account state and keeps public playback available', async () => {
  session.playback_supported = false
  render(<App />)
  await screen.findByText('youtube music')
  await act(async () => { emit('youtube://changed', session); emit('youtube://error', 'Expired cookie') })
  expect(screen.queryByText('Expired cookie')).toBeNull()
  expect((screen.getByRole('button', { name: 'Shuffle' }) as HTMLButtonElement).disabled).toBe(false)
  expect(boundary.invoke).not.toHaveBeenCalledWith('youtube_status')
})

it.each([null, 'catalogVideoId'])('plays YouTube songs using catalog id %s or fallback video id', async (catalogId) => {
  session.authenticated = true
  const song = { id: 'fallbackVideo', catalog_id: catalogId, name: 'Public library song', artist_name: 'Artist', album_name: '', duration_ms: 1000, artwork_url: null }
  const original = boundary.invoke.getMockImplementation()!
  boundary.invoke.mockImplementation(async (command: string, args?: unknown) => {
    if (command === 'library_songs') return [song]
    if (command === 'library_counts') return { ...counts, songs: 1 }
    return original(command, args)
  })
  render(<App />)
  const row = await screen.findByRole('button', { name: /^Public library song/ })
  await waitFor(() => expect(row.getAttribute('aria-disabled')).toBe('false'))
  fireEvent.doubleClick(row)
  expect(boundary.invoke).toHaveBeenCalledWith('play_songs', { songs: [song], startIndex: 0 })
  await act(async () => emit('player://state', { ...idle, index: 0, queue: [{ id: catalogId ?? 'fallbackVideo', title: song.name, artist: 'Artist', album: '', duration_ms: 1000 }] }))
  expect(row.getAttribute('data-playing')).toBe('true')
})

it('enables native YouTube queue modes and transport controls without authentication', async () => {
  session.authenticated = true
  render(<App />)
  await screen.findByText('youtube music')
  await act(async () => emit('player://state', {
    ...idle, status: 'playing', shuffle: true, repeat: 'all', index: 0,
    queue: [{ id: 'videoId', title: 'Song', artist: 'Artist', album: '', duration_ms: 10000 }],
  }))
  const shuffle = screen.getByRole('button', { name: 'Shuffle' }) as HTMLButtonElement
  const repeat = screen.getByRole('button', { name: 'Repeat: all' }) as HTMLButtonElement
  expect(shuffle.disabled).toBe(false)
  expect(repeat.disabled).toBe(false)
  expect(shuffle.getAttribute('aria-pressed')).toBe('true')
  fireEvent.click(shuffle)
  fireEvent.click(repeat)
  click('Pause')
  click('Next')
  click('Previous')
  fireEvent.change(screen.getByRole('slider', { name: 'Seek' }), { target: { value: '5000' } })
  fireEvent.change(screen.getByRole('slider', { name: 'Volume' }), { target: { value: '25' } })
  expect(boundary.invoke).toHaveBeenCalledWith('player_toggle')
  expect(boundary.invoke).toHaveBeenCalledWith('player_next')
  expect(boundary.invoke).toHaveBeenCalledWith('player_previous')
  expect(boundary.invoke).toHaveBeenCalledWith('player_seek', { ms: 5000 })
  expect(boundary.invoke).toHaveBeenCalledWith('player_set_volume', { percent: 25 })
  fireEvent.click(screen.getByTitle('Command palette (Ctrl+K)'))
  const shuffleCommand = screen.getByRole('option', { name: 'Toggle shuffle' })
  const repeatCommand = screen.getByRole('option', { name: 'Cycle repeat' })
  expect(shuffleCommand.getAttribute('aria-disabled')).toBe('false')
  expect(repeatCommand.getAttribute('aria-disabled')).toBe('false')
  fireEvent.click(shuffleCommand)
  fireEvent.click(repeatCommand)
  expect(boundary.invoke).toHaveBeenCalledWith('player_toggle_shuffle')
  expect(boundary.invoke).toHaveBeenCalledWith('player_cycle_repeat')
})


it('keeps YouTube onboarding on setup if source settings cannot be saved', async () => {
  const original = boundary.invoke.getMockImplementation()!
  boundary.invoke.mockImplementation(async (name: string, args: unknown) => {
    if (name === 'settings_set') throw 'Settings disk full'
    return original(name, args)
  })
  render(<Onboarding onFinished={() => {}} />)
  await act(async () => {})
  click('Get started'); click(/^YouTube Music/); click('Continue'); click('Continue')
  expect(await screen.findByRole('alert')).toHaveProperty('textContent', 'Settings disk full')
  expect(screen.getByRole('heading', { name: 'Set up YouTube Music' })).toBeDefined()
})
