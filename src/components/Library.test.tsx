// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react'
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
async function openTab(name: RegExp) {
  const navigation = await screen.findByRole('navigation', { name: 'Main navigation' })
  fireEvent.click(within(navigation).getByRole('button', { name }))
}
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
  Object.defineProperty(HTMLDialogElement.prototype, 'showModal', {
    configurable: true,
    value(this: HTMLDialogElement) { this.setAttribute('open', '') },
  })
  Object.defineProperty(HTMLDialogElement.prototype, 'close', {
    configurable: true,
    value(this: HTMLDialogElement) { this.removeAttribute('open') },
  })
})
afterEach(() => {
  cleanup()
  vi.restoreAllMocks()
  Reflect.deleteProperty(HTMLDialogElement.prototype, 'showModal')
  Reflect.deleteProperty(HTMLDialogElement.prototype, 'close')
  vi.unstubAllGlobals()
})

it('edits a saved playlist name, description, and cover from its detail view', async () => {
  let playlist = { id: 'qol:editable', name: 'Old mix', description: '', cover_id: null as string | null, songs: [{ song: songs[0], source: 'local' }] }
  const snapshot = () => ({ favorites: [], history: [], playlists: [playlist], navigation: {} })
  const original = boundary.invoke.getMockImplementation()!
  boundary.invoke.mockImplementation(async (command: string, args?: unknown) => {
    if (command === 'qol_snapshot') return snapshot()
    if (command === 'qol_playlist_edit') {
      const edit = args as { name: string; description: string; cover: number[] | null; removeCover: boolean }
      playlist = { ...playlist, name: edit.name, description: edit.description, cover_id: edit.removeCover ? null : 'qol-cover:test' }
      boundary.handlers.get('qol://updated')?.({ payload: snapshot() })
      return
    }
    return original(command, args)
  })
  const drawImage = vi.fn()
  vi.stubGlobal('createImageBitmap', vi.fn(async () => ({ width: 1024, height: 512, close: vi.fn() })))
  vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue({ drawImage } as unknown as CanvasRenderingContext2D)
  vi.spyOn(HTMLCanvasElement.prototype, 'toBlob').mockImplementation(callback => callback(new Blob([new Uint8Array([1, 2, 3])])))
  render(<App />)
  fireEvent.click(await screen.findByRole('button', { name: /^Playlists/ }))
  click('Old mix')
  click('Edit playlist')
  fireEvent.change(screen.getByRole('textbox', { name: 'Playlist name' }), { target: { value: 'Road trip' } })
  fireEvent.change(screen.getByRole('textbox', { name: 'Description' }), { target: { value: 'Songs for the drive.' } })
  fireEvent.change(screen.getByLabelText('Cover image'), { target: { files: [new File(['image'], 'cover.webp', { type: 'image/webp' })] } })
  click('Save changes')
  expect(await screen.findByRole('heading', { name: 'Road trip' })).toBeDefined()
  expect(screen.getByText('Songs for the drive.')).toBeDefined()
  expect(screen.getByRole('img', { name: 'Playlist cover' }).getAttribute('src')).toContain('qol-cover:test')
  expect(screen.getByText('First')).toBeDefined()
  expect(drawImage).toHaveBeenCalledWith(expect.anything(), 0, 0, 512, 256)
  expect(boundary.invoke).toHaveBeenCalledWith('qol_playlist_edit', { playlistId: 'qol:editable', name: 'Road trip', description: 'Songs for the drive.', cover: [1, 2, 3], removeCover: false })
  click('Edit playlist')
  click('Remove cover')
  click('Save changes')
  await waitFor(() => expect(screen.queryByRole('img', { name: 'Playlist cover' })).toBeNull())
  expect(boundary.invoke).toHaveBeenCalledWith('qol_playlist_edit', { playlistId: 'qol:editable', name: 'Road trip', description: 'Songs for the drive.', cover: null, removeCover: true })
})

it('removes only the chosen occurrence from a playlist shown in reverse order', async () => {
  const entries = ['first-entry', 'second-entry'].map(entry_id => ({ entry_id, song: { ...songs[0] }, source: 'local' }))
  const snapshot = () => ({ favorites: [], history: [], playlists: [{ id: 'qol:mix', name: 'My mix', songs: [...entries] }], navigation: {} })
  const original = boundary.invoke.getMockImplementation()!
  boundary.invoke.mockImplementation(async (command: string, args?: unknown) => {
    if (command === 'qol_snapshot') return snapshot()
    if (command === 'qol_playlist_remove_song') {
      const { entryId } = args as { entryId: string }
      entries.splice(entries.findIndex(entry => entry.entry_id === entryId), 1)
      boundary.handlers.get('qol://updated')?.({ payload: snapshot() })
      return
    }
    return original(command, args)
  })
  render(<App />)
  fireEvent.click(await screen.findByRole('button', { name: /^Playlists/ }))
  click('My mix')
  const tracks = await screen.findAllByRole('button', { name: 'More actions for First' })
  expect(tracks).toHaveLength(2)
  fireEvent.change(screen.getByRole('combobox', { name: 'Sort direction' }), { target: { value: 'desc' } })
  fireEvent.click(screen.getAllByRole('button', { name: 'More actions for First' })[0]!)
  fireEvent.click(screen.getByRole('menuitem', { name: 'Remove from playlist' }))
  await waitFor(() => expect(screen.getAllByRole('button', { name: 'More actions for First' })).toHaveLength(1))
  expect(boundary.invoke).toHaveBeenCalledWith('qol_playlist_remove_song', { playlistId: 'qol:mix', entryId: 'second-entry' })
  click('Back')
  click(/^Songs/)
  expect(await screen.findByRole('button', { name: /^FirstA & B/ })).toBeDefined()
})

it('creates an empty playlist from Playlists even with an empty library', async () => {
  const original = boundary.invoke.getMockImplementation()!
  boundary.invoke.mockImplementation(async (command: string, args?: unknown) => {
    if (command === 'library_counts') return { songs: 0, albums: 0, artists: 0, playlists: 0 }
    if (command === 'qol_playlist_add') {
      boundary.handlers.get('qol://updated')?.({ payload: { favorites: [], history: [], playlists: [{ id: 'qol:new', name: 'Evening mix', songs: [] }], navigation: {} } })
      return 'qol:new'
    }
    return original(command, args)
  })
  render(<App />)
  fireEvent.click(await screen.findByRole('button', { name: /^Playlists/ }))
  click('New playlist')
  click(/^Brand new/)
  fireEvent.change(screen.getByRole('textbox', { name: 'Playlist name' }), { target: { value: ' Evening mix ' } })
  click('Create playlist')
  expect(await screen.findByRole('button', { name: 'Evening mix' })).toBeDefined()
  expect(boundary.invoke).toHaveBeenCalledWith('qol_playlist_add', { songs: [], source: 'local', playlistId: null, name: 'Evening mix', context: null })
})

it('imports a playlist from a link without leaving Playlists', async () => {
  const url = 'https://open.spotify.com/playlist/abc'
  const job = { id: 'job-1', source_url: url, provider: 'spotify', status: 'ready', collection_title: 'Road trip mix', total_tracks: 1, processed: 0, matched: 0, needs_review: 0, unavailable: 0, playlist_id: null }
  let saved = false
  const original = boundary.invoke.getMockImplementation()!
  boundary.invoke.mockImplementation(async (command: string, args?: unknown) => {
    switch (command) {
      case 'import_detect': return { provider: 'spotify', kind: 'playlist' }
      case 'import_job':
      case 'import_status': return saved ? { ...job, status: 'completed', playlist_id: 'playlist-1' } : job
      case 'import_preview': return { title: job.collection_title, tracks: [{ origin: { platform: 'spotify', external_id: 'track-1' } }], warnings: [] }
      case 'import_entries': return []
      case 'import_confirm':
        saved = true
        boundary.handlers.get('library://updated')?.({ payload: { songs: 2, albums: 1, artists: 2, playlists: 1 } })
        return 'playlist-1'
      case 'library_playlists': return saved ? [{ id: 'playlist-1', name: job.collection_title, artwork_url: null }] : []
      default: return original(command, args)
    }
  })

  render(<App />)
  fireEvent.click(await screen.findByRole('button', { name: /^Playlists/ }))
  click('New playlist')
  click(/^Import from a link/)
  expect(screen.getByRole('dialog', { name: 'Import playlist' })).toBeDefined()
  fireEvent.change(screen.getByRole('textbox', { name: 'Music URL' }), { target: { value: url } })
  await waitFor(() => expect((screen.getByRole('button', { name: 'Preview' }) as HTMLButtonElement).disabled).toBe(false))
  click('Preview')
  fireEvent.click(await screen.findByRole('button', { name: 'Save playlist' }))
  await screen.findByText(/Playlist saved/)
  click('Close import')

  expect(await screen.findByRole('button', { name: job.collection_title })).toBeDefined()
  expect(boundary.invoke).toHaveBeenCalledWith('import_confirm', { jobId: job.id })
  expect(boundary.invoke.mock.calls.some(([command]) => command === 'qol_playlist_add')).toBe(false)
})

it('adds a Discover result and an imported playlist song to the same local playlist', async () => {
  const discovered = { id: '123', title: 'Discovered track', artist: 'Artist', artist_id: null, album: null, duration_ms: 1000, artwork_url: null, permalink_url: null }
  const importedId = '45e8508d-cf1e-42c5-9a0a-83077b41befe'
  const imported = { ...song(importedId, 'Imported track'), catalog_id: importedId }
  const discoveredId = '7ea92f55-8cc7-49b6-bfb9-ed5a661f1af6'
  const savedDiscovery = { ...song(discoveredId, discovered.title), catalog_id: discoveredId }
  const route = { provider: 'soundcloud', external_id: discovered.id }
  const entries: unknown[] = []
  const snapshot = () => ({
    favorites: [], history: [], navigation: {},
    playlists: entries.length ? [{ id: 'qol:mix', name: 'My mix', songs: [...entries] }] : [],
  })
  const original = boundary.invoke.getMockImplementation()!
  boundary.invoke.mockImplementation(async (command: string, args?: unknown) => {
    if (command === 'qol_snapshot') return snapshot()
    if (command === 'discover_search') return { tracks: [discovered], artists: [], albums: [] }
    if (command === 'library_playlists') return [{ id: 'imported', name: 'Imported playlist', artwork_url: null }]
    if (command === 'library_playlist_songs') return [imported]
    if (command === 'qol_playlist_add_discovered' || command === 'qol_playlist_add') {
      entries.push(command === 'qol_playlist_add_discovered'
        ? { song: savedDiscovery, source: 'local', playback_route: route }
        : { song: imported, source: 'local' })
      boundary.handlers.get('qol://updated')?.({ payload: snapshot() })
      return 'qol:mix'
    }
    return original(command, args)
  })
  render(<App />)
  const homepage = await screen.findByRole('button', { name: 'Homepage' })
  expect(screen.getByRole('navigation', { name: 'Main navigation' }).querySelector('button')).toBe(homepage)
  fireEvent.click(homepage)
  fireEvent.change(screen.getByRole('searchbox'), { target: { value: 'Discovered track' } })
  click('Search')
  fireEvent.click(await screen.findByRole('button', { name: 'Add Discovered track to playlist' }))
  fireEvent.change(screen.getByRole('textbox', { name: 'Playlist name' }), { target: { value: 'My mix' } })
  click('Create and add')
  await waitFor(() => expect(boundary.invoke).toHaveBeenCalledWith('qol_playlist_add_discovered', { track: discovered, playlistId: null, name: 'My mix' }))
  await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull())
  click(/^Playlists/)
  click('Imported playlist')
  fireEvent.click(await screen.findByRole('button', { name: 'More actions for Imported track' }))
  fireEvent.click(screen.getByRole('menuitem', { name: 'Add to playlist' }))
  fireEvent.change(await screen.findByRole('combobox', { name: 'Playlist' }), { target: { value: 'qol:mix' } })
  click('Add to playlist')
  await waitFor(() => expect(boundary.invoke).toHaveBeenCalledWith('qol_playlist_add', { songs: [imported], source: 'local', playlistId: 'qol:mix', name: null, context: null }))
  await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull())
  click('Back')
  click('My mix')
  expect(await screen.findByText('Imported track')).toBeDefined()
  fireEvent.doubleClick(await screen.findByRole('button', { name: /^Discovered trackA & B/ }))
  await waitFor(() => expect(boundary.invoke).toHaveBeenCalledWith('qol_play', {
    songs: [savedDiscovery, imported], source: 'local', startIndex: 0, context: null, playbackRoutes: [route, null],
  }))
})

it('retries a missing cover when background metadata enrichment supplies artwork', async () => {
  render(<App />)
  await openTab(/^Songs/)
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

it('opens a saved local playlist and searches within it while preserving playback context', async () => {
  const original = boundary.invoke.getMockImplementation()!
  boundary.invoke.mockImplementation(async (command: string, args?: unknown) => {
    if (command === 'qol_snapshot') return { favorites: [], history: [], playlists: [{ id: 'qol:saved', name: 'Saved mix', songs: songs.map(song => ({ song, source: 'local' })) }], navigation: { view: 'playlists', detail: { kind: 'playlist', id: 'qol:saved', title: 'Saved mix' }, detailSort: 'original', detailDirection: 'asc' } }
    return original(command, args)
  })
  render(<App />)
  fireEvent.click(await screen.findByRole('button', { name: /^Playlists/ }))
  fireEvent.click(await screen.findByRole('button', { name: 'Saved mix' }))
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
  await openTab(/^Songs/)
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
  fireEvent.click(await screen.findByRole('button', { name: /^Favorites/ }))
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

it('opens Homepage at startup even with a saved playlist detail, and restores sort preferences', async () => {
  const original = boundary.invoke.getMockImplementation()!
  boundary.invoke.mockImplementation(async (command: string, args?: unknown) => {
    if (command === 'qol_snapshot') return {
      favorites: [], history: [], playlists: [],
      navigation: { view: 'playlists', detail: { kind: 'playlist', id: 'old', title: 'Old playlist' }, artistName: 'Old artist', songSort: 'duration', direction: 'desc' },
    }
    return original(command, args)
  })
  render(<App />)
  expect(await screen.findByRole('heading', { name: 'Homepage' })).toBeDefined()
  expect(await screen.findByRole('button', { name: 'Play First' })).toBeDefined()
  expect(screen.queryByText('Old playlist')).toBeNull()
  expect(screen.getByRole('button', { name: 'Homepage' }).getAttribute('aria-current')).toBe('page')
  expect(screen.getByText('your folders')).toBeDefined()
  expect(screen.queryByRole('alert')).toBeNull()
  await openTab(/^Songs/)
  expect((screen.getByRole('combobox', { name: 'Sort songs' }) as HTMLSelectElement).value).toBe('duration')
  expect((screen.getByRole('combobox', { name: 'Sort direction' }) as HTMLSelectElement).value).toBe('desc')
})

it('loads all pages and plays the full displayed sorted queue at the clicked row', async () => {
  songs = Array.from({ length: 1007 }, (_, i) => song(`id-${i}`, `Song ${i}`))
  render(<App />)
  await openTab(/^Songs/)
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
  await openTab(/^Artists/)
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
  await openTab(/^Artists/)
  fireEvent.click(await screen.findByRole('button', { name: /Saved only/ }))
  expect(await screen.findByText('No cached songs for this artist credit.')).toBeDefined()
})

it('searches the library from Homepage, ignoring old results and playing only the current query', async () => {
  let finish!: (rows: SongRow[]) => void
  const old = new Promise<SongRow[]>((resolve) => { finish = resolve })
  const original = boundary.invoke.getMockImplementation()!
  boundary.invoke.mockImplementation(async (command: string, args: Record<string, unknown> = {}) => {
    if (command === 'library_songs' && args.query === 'old') return old
    if (command === 'library_songs' && args.query === 'new') return [song('new', 'New match')]
    return original(command, args)
  })
  render(<App />)
  await screen.findByRole('heading', { name: 'Homepage' })
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
  await openTab(/^Songs/)
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
  await openTab(/^Songs/)
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
  await openTab(/^Songs/)
  await screen.findByRole('button', { name: /FirstA & B/ })
  fireEvent.change(screen.getByLabelText('Sort songs'), { target: { value: 'duration' } })
  click(/^Playlists/)
  click('My mix')
  fireEvent.doubleClick(await screen.findByRole('button', { name: /FirstA & B/ }))
  expect(boundary.invoke).toHaveBeenCalledWith('play_songs', { songs: playlistSongs, startIndex: 1 })
  expect(screen.queryByLabelText('Sort songs')).toBeNull()
})
