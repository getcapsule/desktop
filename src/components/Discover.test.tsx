// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, expect, it, vi } from 'vitest'
import { Discover } from './Discover'
import { discover, type SearchResults } from '../lib/discover-ipc'
import { library, type SongRow } from '../lib/ipc'
import { EMPTY_QOL } from '../lib/qol-ipc'
import { ListeningContext } from './ListeningTools'

vi.mock('../lib/discover-ipc', () => ({ discover: { search: vi.fn(), artistTracks: vi.fn(), albumTracks: vi.fn(), play: vi.fn() } }))
vi.mock('@tauri-apps/api/core', () => ({ convertFileSrc: (id: string) => `http://artwork.localhost/${id}` }))
afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.resetAllMocks(); vi.unstubAllGlobals() })
const track = (id: string) => ({ id, title: id, artist: 'Artist', artist_id: null, album: null, duration_ms: 1000, artwork_url: null, permalink_url: null })
const result = (title: string): SearchResults => ({ tracks: [track(title)], artists: [], albums: [] })

it('ignores a stale search response when a newer query finishes first', async () => {
  let finishOld!: (result: SearchResults) => void
  vi.mocked(discover.search).mockImplementationOnce(() => new Promise(resolve => { finishOld = resolve })).mockResolvedValueOnce(result('Newest song'))
  render(<Discover />)
  fireEvent.change(screen.getByRole('searchbox'), { target: { value: 'old' } })
  fireEvent.submit(screen.getByRole('search'))
  await waitFor(() => expect(discover.search).toHaveBeenCalledTimes(1))
  fireEvent.change(screen.getByRole('searchbox'), { target: { value: 'new' } })
  fireEvent.submit(screen.getByRole('search'))
  await screen.findByText('Newest song')
  await act(async () => finishOld(result('Old song')))
  expect(screen.queryByText('Old song')).toBeNull()
  expect(screen.getByText('Newest song')).toBeTruthy()
})

it('starts the selected song at its index in the displayed queue', async () => {
  const storage = new Map<string, string>()
  vi.stubGlobal('localStorage', { getItem: (key: string) => storage.get(key) ?? null, setItem: (key: string, value: string) => storage.set(key, value) })
  vi.mocked(discover.search).mockResolvedValue({ tracks: [track('First'), track('Second')], artists: [], albums: [] })
  vi.mocked(discover.play).mockResolvedValue()
  render(<Discover />)
  fireEvent.change(screen.getByRole('searchbox'), { target: { value: 'songs' } })
  fireEvent.submit(screen.getByRole('search'))
  fireEvent.click(screen.getByRole('button', { name: 'Grid' }))
  expect(screen.getByRole('button', { name: 'Grid' }).getAttribute('aria-pressed')).toBe('true')
  expect(storage.get('capsule-homepage-layout')).toBe('grid')
  fireEvent.click(await screen.findByRole('button', { name: 'Play Second' }))
  await waitFor(() => expect(discover.play).toHaveBeenCalledWith([track('First'), track('Second')], 1))
  fireEvent.click(screen.getByRole('button', { name: 'List' }))
  expect(screen.getByRole('button', { name: 'List' }).getAttribute('aria-pressed')).toBe('true')
  expect(discover.search).toHaveBeenCalledTimes(1)
})

it('opens a recommended album once and plays its cached tracklist in release order', async () => {
  let reportVisibility: IntersectionObserverCallback | undefined
  vi.stubGlobal('IntersectionObserver', class {
    constructor(callback: IntersectionObserverCallback) { reportVisibility = callback }
    observe() {}
    disconnect() {}
  })
  const song = (id: string, name: string, artist_name: string): SongRow => ({ id, name, artist_name, album_name: 'Record', duration_ms: 1000, artwork_url: null, catalog_id: null })
  const favorite = song('favorite', 'Favorite song', 'Favorite artist')
  const related = { ...song('related', 'More from the artist', 'Favorite artist'), artwork_url: 'https://example.invalid/cover.jpg' }
  const other = song('other', 'Unrelated track', 'Other artist')
  const readSongs = vi.spyOn(library, 'songs').mockImplementation(async (_limit, _offset, options) => options?.artistName ? [favorite, related] : [other])
  const play = vi.spyOn(library, 'play').mockResolvedValue()
  const albumSongs = vi.spyOn(library, 'albumSongs').mockResolvedValue([favorite, related])
  const albums = [{ id: 'release', name: 'Record', artist_name: favorite.artist_name, artwork_url: related.artwork_url, track_count: 2 }]
  const snapshot = { ...EMPTY_QOL, history: [{ song: favorite, source: 'local' as const }], favorites: [{ song: favorite, source: 'local' as const }] }
  render(<ListeningContext.Provider value={{ source: 'local', snapshot, problem: vi.fn(), artist: vi.fn(), album: vi.fn(), replace: vi.fn() }}><Discover albums={albums} /></ListeningContext.Provider>)

  fireEvent.click(screen.getByRole('button', { name: 'Grid' }))
  const pick = await screen.findByRole('button', { name: 'Open album Record by Favorite artist' })
  expect(screen.getByRole('heading', { name: 'Picked for you' })).toBeDefined()
  expect(screen.getAllByRole('button', { name: 'Open album Record by Favorite artist' })).toHaveLength(1)
  expect(readSongs).toHaveBeenCalledWith(expect.any(Number), 0, { artistName: 'Favorite artist' })
  expect(pick.querySelector('img')?.getAttribute('src')).toBe('http://artwork.localhost/release?w=320&cache=only')
  expect(pick.querySelector('img[src*="quality=high"]')).toBeNull()
  expect(reportVisibility).toBeDefined()
  await act(async () => reportVisibility?.([{ isIntersecting: true } as IntersectionObserverEntry], {} as IntersectionObserver))
  expect(pick.querySelector('img[src*="quality=high"]')?.getAttribute('src')).toBe('http://artwork.localhost/release?w=320&quality=high')
  expect(discover.search).not.toHaveBeenCalled()
  expect(discover.artistTracks).not.toHaveBeenCalled()
  expect(discover.albumTracks).not.toHaveBeenCalled()
  expect(play).not.toHaveBeenCalled()
  fireEvent.click(pick)
  fireEvent.click(await screen.findByRole('button', { name: 'Play More from the artist' }))
  expect(albumSongs).toHaveBeenCalledWith('release')
  await waitFor(() => expect(play).toHaveBeenCalledWith([favorite, related], 1))
  fireEvent.click(screen.getByRole('button', { name: 'Back to Homepage' }))
  expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Open album Record by Favorite artist' }))
  fireEvent.click(screen.getByRole('button', { name: 'Open mix Your rotation' }))
  await screen.findByRole('button', { name: 'Play More from the artist' })
  fireEvent.click(screen.getByRole('button', { name: 'Play all' }))
  await waitFor(() => expect(play).toHaveBeenLastCalledWith([related, favorite, other], 0))
  expect(discover.search).not.toHaveBeenCalled()
})

it('shows a fixed editorial tracklist and searches externally only on request', async () => {
  const cached = { id: 'been-away', name: 'Been Away', artist_name: 'Brent Faiyaz', album_name: 'Fuck the World', duration_ms: 1000, artwork_url: null, catalog_id: null }
  vi.spyOn(library, 'songs').mockResolvedValue([])
  vi.spyOn(library, 'search').mockImplementation(async title => title === cached.name ? [{ ...cached, id: 'cover', artist_name: 'Another artist' }, cached] : [])
  const play = vi.spyOn(library, 'play').mockResolvedValue()
  vi.mocked(discover.search).mockResolvedValue(result('Good Days'))
  render(<ListeningContext.Provider value={{ source: 'local', snapshot: EMPTY_QOL, problem: vi.fn(), artist: vi.fn(), album: vi.fn(), replace: vi.fn() }}><Discover /></ListeningContext.Provider>)

  fireEvent.click(await screen.findByRole('button', { name: 'Open playlist Late Night R&B' }))
  const lookup = await screen.findByRole('button', { name: 'Search SoundCloud for Good Days by SZA' })
  expect(discover.search).not.toHaveBeenCalled()
  fireEvent.click(screen.getByRole('button', { name: 'Play cached tracks' }))
  await waitFor(() => expect(play).toHaveBeenCalledWith([cached], 0))
  fireEvent.click(lookup)
  await waitFor(() => expect(discover.search).toHaveBeenCalledWith('tracks', 'SZA Good Days', 0))
})

it('uses a matching compilation cover to open the album ahead of standalone tracks', async () => {
  const artwork_url = 'https://example.invalid/compilation.jpg'
  const single: SongRow = { id: 'single', name: 'Single', artist_name: 'Guest', album_name: '', duration_ms: 1000, artwork_url: null, catalog_id: null }
  const compilationTrack = { ...single, id: 'compilation-song', name: 'Compilation song', album_name: 'Collection', artwork_url }
  const albums = [{ id: 'compilation', name: 'Collection', artist_name: 'Various Artists', artwork_url, track_count: 4 }]
  vi.spyOn(library, 'songs').mockResolvedValue([single, compilationTrack])
  const albumSongs = vi.spyOn(library, 'albumSongs').mockResolvedValue([compilationTrack])
  render(<ListeningContext.Provider value={{ source: 'local', snapshot: EMPTY_QOL, problem: vi.fn(), artist: vi.fn(), album: vi.fn(), replace: vi.fn() }}><Discover albums={albums} /></ListeningContext.Provider>)

  const album = await screen.findByRole('button', { name: 'Open album Collection by Various Artists' })
  expect(album.parentElement?.firstElementChild).toBe(album)
  expect(screen.queryByRole('button', { name: 'Open track Compilation song' })).toBeNull()
  fireEvent.click(album)
  expect(await screen.findByRole('button', { name: 'Play Compilation song' })).toBeDefined()
  expect(albumSongs).toHaveBeenCalledWith('compilation')
})
