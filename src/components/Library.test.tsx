// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import App from '../App'
import type { Settings, SongRow } from '../lib/ipc'

const boundary = vi.hoisted(() => ({ invoke: vi.fn(), handlers: new Map<string, (event: { payload: unknown }) => void>() }))
vi.mock('@tauri-apps/api/core', () => ({ invoke: boundary.invoke, convertFileSrc: (id: string) => id }))
vi.mock('@tauri-apps/api/event', () => ({ listen: async (name: string, handler: (event: { payload: unknown }) => void) => { boundary.handlers.set(name, handler); return () => boundary.handlers.delete(name) } }))
vi.mock('@tauri-apps/api/window', () => ({ getCurrentWindow: () => ({ isMaximized: async () => false, onResized: async () => () => {} }) }))
let stored: Settings
let songs: SongRow[]
const song = (id: string, name: string): SongRow => ({ id, name, catalog_id: null, artist_name: 'A & B', album_name: 'Record', duration_ms: 1000, artwork_url: null })
const click = (name: string | RegExp) => fireEvent.click(screen.getByRole('button', { name }))
beforeEach(() => {
  stored = { source: 'local', onboarded: true, developer: false, appearance: { glass: 'none', theme: 'dark' }, spotify: { client_id: '' }, navidrome: { url: '', username: '' }, local: { folders: [] }, lyrics: { offset_ms: 0 }, lastfm: { api_key: '', shared_secret: '' }, discord: { client_id: '', serve_art_from_server: false } }
  songs = [song('one', 'First'), song('two', 'Second')]
  boundary.handlers.clear()
  boundary.invoke.mockReset().mockImplementation(async (command: string, args: Record<string, unknown> = {}) => {
    switch (command) {
      case 'settings_get': return structuredClone(stored)
      case 'library_songs': {
        const ordered = args.direction === 'desc' ? [...songs].reverse() : songs
        return ordered.slice(Number(args.offset ?? 0), Number(args.offset ?? 0) + Number(args.limit ?? 500))
      }
      case 'library_albums': return [{ id: 'album', name: 'Record', artist_name: 'A & B', artwork_url: null, track_count: 2 }]
      case 'library_artists': return [{ name: 'A & B', song_count: songs.length, album_count: 1 }, { name: 'Saved only', song_count: 0, album_count: 0 }]
      case 'library_album_songs': return [songs[1], songs[0]]
      case 'library_playlists': return []
      case 'library_counts': return { songs: songs.length, albums: 1, artists: 2, playlists: 0 }
      case 'player_snapshot': return { status: 'idle', queue: [], index: null, position_ms: 0, volume: 100, shuffle: false, repeat: 'off' }
      default: return
    }
  })
  vi.stubGlobal('ResizeObserver', class { observe() {} unobserve() {} disconnect() {} })
})
afterEach(() => { cleanup(); vi.unstubAllGlobals() })

it('retries a missing cover when background metadata enrichment supplies artwork', async () => {
  render(<App />)
  const row = await screen.findByRole('button', { name: /FirstA & B/ })
  const first = row.querySelector('img')!
  fireEvent.error(first)
  const failedUrl = first.getAttribute('src')
  songs = songs.map(song => song.id === 'one' ? { ...song, artwork_url: 'https://i.scdn.co/image/repaired' } : song)
  await act(async () => boundary.handlers.get('library://updated')?.({ payload: { songs: 2, albums: 1, artists: 2, playlists: 0 } }))
  await waitFor(() => {
    const next = screen.getByRole('button', { name: /FirstA & B/ }).querySelector('img')!
    expect(next.getAttribute('src')).not.toBe(failedUrl)
    expect(next.style.visibility).not.toBe('hidden')
  })
})

it('restores a saved local playlist and searches within it while preserving playback context', async () => {
  const original = boundary.invoke.getMockImplementation()!
  boundary.invoke.mockImplementation(async (command: string, args?: unknown) => {
    if (command === 'qol_snapshot') return { favorites: [], history: [], playlists: [{ id: 'qol:saved', name: 'Saved mix', songs: songs.map(song => ({ song, source: 'local' })) }], navigation: { view: 'playlists', detail: { kind: 'playlist', id: 'qol:saved', title: 'Saved mix' }, detailSort: 'original', detailDirection: 'asc' } }
    return original(command, args)
  })
  render(<App />)
  expect(await screen.findByText('Saved mix')).toBeDefined()
  const input = screen.getByRole('textbox', { name: 'Search your library' })
  fireEvent.change(input, { target: { value: 'Second' } })
  const row = await screen.findByRole('button', { name: /^SecondA & B/ })
  expect(screen.queryByText('First')).toBeNull()
  fireEvent.doubleClick(row)
  await waitFor(() => expect(boundary.invoke).toHaveBeenCalledWith('qol_play', { songs: [songs[1]], source: 'local', startIndex: 0, context: null }))
  expect(boundary.invoke.mock.calls.some(([name]) => name === 'play_songs')).toBe(false)
})

it('queues a selected batch and keeps playback shortcuts out of text fields', async () => {
  render(<App />)
  await screen.findByRole('button', { name: /FirstA & B/ })
  fireEvent.click(screen.getByRole('checkbox', { name: 'Select First' }))
  fireEvent.click(screen.getByRole('checkbox', { name: 'Select Second' }))
  fireEvent.click(screen.getByRole('button', { name: 'Add to queue' }))
  await waitFor(() => expect(boundary.invoke).toHaveBeenCalledWith('qol_queue_add', { songs, source: 'local', next: false, context: null }))
  fireEvent.keyDown(screen.getByRole('textbox', { name: 'Search your library' }), { code: 'Space', key: ' ' })
  expect(boundary.invoke.mock.calls.some(([name]) => name === 'player_toggle')).toBe(false)
  fireEvent.keyDown(window, { code: 'Space', key: ' ' })
  await waitFor(() => expect(boundary.invoke).toHaveBeenCalledWith('player_toggle'))
})

it('opens favorites even when the active provider library is empty', async () => {
  const savedSong = song('saved', 'Saved favorite')
  const original = boundary.invoke.getMockImplementation()!
  songs = []
  boundary.invoke.mockImplementation(async (command: string, args?: unknown) => {
    if (command === 'library_counts') return { songs: 0, albums: 0, artists: 0, playlists: 0 }
    if (command === 'qol_snapshot') return { favorites: [{ song: savedSong, source: 'navidrome', context: 'https://old-server\nold-user' }], history: [], playlists: [], navigation: { view: 'favorites' } }
    return original(command, args)
  })
  render(<App />)
  fireEvent.doubleClick(await screen.findByRole('button', { name: /^Saved favorite/ }))
  await waitFor(() => expect(boundary.invoke).toHaveBeenCalledWith('qol_play', { songs: [savedSong], source: 'navidrome', startIndex: 0, context: 'https://old-server\nold-user' }))
})

it('opens imported playlists when no source songs, albums, or artists are cached', async () => {
  songs = []
  stored.source = 'youtube'
  const imported = [song('canonical-one', 'Imported first'), song('canonical-two', 'Imported second')]
  const original = boundary.invoke.getMockImplementation()!
  boundary.invoke.mockImplementation(async (command: string, args?: unknown) => {
    if (command === 'library_counts') return { songs: 0, albums: 0, artists: 0, playlists: 2 }
    if (command === 'library_albums' || command === 'library_artists') return []
    if (command === 'library_playlists') return [
      { id: 'import-one', name: 'Imported collection', artwork_url: null },
      { id: 'import-two', name: 'Another import', artwork_url: null },
    ]
    if (command === 'library_playlist_songs') return imported
    return original(command, args)
  })
  render(<App />)
  fireEvent.click(await screen.findByRole('button', { name: /^Playlists/ }))
  fireEvent.click(await screen.findByRole('button', { name: 'Imported collection' }))
  expect(await screen.findByText('Imported first')).toBeDefined()
  expect(screen.queryByText('Nothing here yet.')).toBeNull()
  expect(screen.queryByRole('checkbox')).toBeNull()
})

it('renders the real App at startup with local source and onboarding complete', async () => {
  render(<App />)
  expect(await screen.findByRole('button', { name: /FirstA & B/ })).toBeDefined()
  expect(screen.getByRole('navigation')).toBeDefined()
  expect(screen.getByRole('combobox', { name: 'Sort songs' })).toBeDefined()
  expect(screen.getByText('your folders')).toBeDefined()
  expect(screen.queryByRole('alert')).toBeNull()
})

it('loads all pages and plays the full displayed sorted queue at the clicked row', async () => {
  songs = Array.from({ length: 1007 }, (_, i) => song(`id-${i}`, `Song ${i}`))
  render(<App />)
  await screen.findByRole('button', { name: /Song 0Artist|Song 0A & B/ })
  fireEvent.change(screen.getByRole('combobox', { name: 'Sort songs' }), { target: { value: 'duration' } })
  fireEvent.change(screen.getByRole('combobox', { name: 'Sort direction' }), { target: { value: 'desc' } })
  const row = await screen.findByRole('button', { name: /^Song 1005/ })
  fireEvent.doubleClick(row)
  await waitFor(() => expect(boundary.invoke).toHaveBeenCalledWith('play_songs', { songs: [...songs].reverse(), startIndex: 1 }))
  expect(boundary.invoke).toHaveBeenCalledWith('library_songs', expect.objectContaining({ offset: 1000, sort: 'duration', direction: 'desc' }))
})

it('opens exact artist credits and cached albums while preserving album track order', async () => {
  render(<App />)
  fireEvent.click(await screen.findByRole('button', { name: /^Artists/ }))
  fireEvent.click(await screen.findByRole('button', { name: /A & B.*cached songs/ }))
  await waitFor(() => expect(boundary.invoke).toHaveBeenCalledWith('library_songs', expect.objectContaining({ artistName: 'A & B' })))
  expect(screen.getByText(/exact artist metadata/i)).toBeDefined()
  click('Cached albums')
  fireEvent.click(await screen.findByRole('button', { name: /RecordA & B/ }))
  fireEvent.doubleClick(await screen.findByRole('button', { name: /SecondA & B/ }))
  expect(boundary.invoke).toHaveBeenCalledWith('play_songs', { songs: [songs[1], songs[0]], startIndex: 0 })
  click('Back')
  expect(await screen.findByRole('heading', { name: 'A & B' })).toBeDefined()
})

it('shows saved-only artists even with no songs or albums in the library', async () => {
  songs = []
  const original = boundary.invoke.getMockImplementation()!
  boundary.invoke.mockImplementation(async (command: string, args?: unknown) => {
    if (command === 'library_counts') return { songs: 0, albums: 0, artists: 1, playlists: 0 }
    if (command === 'library_albums') return []
    if (command === 'library_artists') return [{ name: 'Saved only', song_count: 0, album_count: 0 }]
    return original(command, args)
  })
  render(<App />)
  fireEvent.click(await screen.findByRole('button', { name: /^Artists/ }))
  fireEvent.click(await screen.findByRole('button', { name: /Saved only/ }))
  expect(await screen.findByText('No cached songs for this artist credit.')).toBeDefined()
})

it('ignores old query results and plays only the current query', async () => {
  let finish!: (rows: SongRow[]) => void
  const old = new Promise<SongRow[]>((resolve) => { finish = resolve })
  const original = boundary.invoke.getMockImplementation()!
  boundary.invoke.mockImplementation(async (command: string, args: Record<string, unknown> = {}) => {
    if (command === 'library_songs' && args.query === 'old') return old
    if (command === 'library_songs' && args.query === 'new') return [song('new', 'New match')]
    return original(command, args)
  })
  render(<App />)
  await screen.findByRole('button', { name: /FirstA & B/ })
  const search = screen.getByLabelText('Search your library')
  fireEvent.change(search, { target: { value: 'old' } })
  await waitFor(() => expect(boundary.invoke).toHaveBeenCalledWith('library_songs', expect.objectContaining({ query: 'old' })))
  fireEvent.change(search, { target: { value: 'new' } })
  const row = await screen.findByRole('button', { name: /^New match/ })
  await act(async () => finish([song('old', 'Old match')]))
  expect(screen.queryByText('Old match')).toBeNull()
  fireEvent.doubleClick(row)
  expect(boundary.invoke).toHaveBeenCalledWith('play_songs', { songs: [song('new', 'New match')], startIndex: 0 })
})

it('drops pending pages from the previous source after a library change', async () => {
  let finish!: (rows: SongRow[]) => void
  const oldPage = new Promise<SongRow[]>((resolve) => { finish = resolve })
  const original = boundary.invoke.getMockImplementation()!
  boundary.invoke.mockImplementation(async (command: string, args: Record<string, unknown> = {}) => {
    if (command === 'library_songs') {
      if (stored.source === 'local') return oldPage
      return [song('navidrome-track', 'New source')]
    }
    if (command === 'navidrome_status') return { configured: true, url: 'https://music.example.com', username: 'listener', insecure: false }
    return original(command, args)
  })
  render(<App />)
  await screen.findByText('your folders')
  stored.source = 'navidrome'
  await act(async () => boundary.handlers.get('library://updated')!({ payload: { songs: 1, albums: 0, artists: 1, playlists: 0 } }))
  const row = await screen.findByRole('button', { name: /^New source/ })
  await act(async () => finish([song('old', 'Old source')]))
  expect(screen.queryByText('Old source')).toBeNull()
  fireEvent.doubleClick(row)
  expect(boundary.invoke).toHaveBeenCalledWith('play_songs', { songs: [song('navidrome-track', 'New source')], startIndex: 0 })
})

it('offers retry instead of playing a partial library when a later page fails', async () => {
  songs = Array.from({ length: 501 }, (_, i) => song(`id-${i}`, `Song ${i}`))
  let fail = true
  const original = boundary.invoke.getMockImplementation()!
  boundary.invoke.mockImplementation(async (command: string, args: Record<string, unknown> = {}) => {
    if (command === 'library_songs' && args.offset === 500 && fail) throw new Error('Disk unavailable')
    return original(command, args)
  })
  render(<App />)
  expect(await screen.findByRole('alert')).toHaveProperty('textContent', expect.stringContaining('Disk unavailable'))
  expect(screen.queryByRole('button', { name: /Song 0A & B/ })).toBeNull()
  fail = false
  click('Retry')
  fireEvent.doubleClick(await screen.findByRole('button', { name: /Song 0A & B/ }))
  expect(boundary.invoke).toHaveBeenCalledWith('play_songs', { songs, startIndex: 0 })
})

it('keeps a playlist original order and repeated tracks after changing the song sort', async () => {
  const original = boundary.invoke.getMockImplementation()!
  const playlistSongs = [songs[1], songs[0], songs[1]]
  boundary.invoke.mockImplementation(async (command: string, args?: unknown) => {
    if (command === 'library_playlists') return [{ id: 'mix', name: 'My mix', artwork_url: null }]
    if (command === 'library_playlist_songs') return playlistSongs
    return original(command, args)
  })
  render(<App />)
  await screen.findByRole('button', { name: /FirstA & B/ })
  fireEvent.change(screen.getByLabelText('Sort songs'), { target: { value: 'duration' } })
  click(/^Playlists/)
  click('My mix')
  fireEvent.doubleClick(await screen.findByRole('button', { name: /FirstA & B/ }))
  expect(boundary.invoke).toHaveBeenCalledWith('play_songs', { songs: playlistSongs, startIndex: 1 })
  expect(screen.queryByLabelText('Sort songs')).toBeNull()
})
