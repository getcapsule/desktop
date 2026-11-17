// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import { afterEach, expect, it, vi } from 'vitest'
import { Discover } from './Discover'
import { discover, type SearchResults } from '../lib/discover-ipc'
import { library, type SongRow } from '../lib/ipc'
import { editorialPlaylists } from '../lib/editorial-playlists'
import { EMPTY_QOL } from '../lib/qol-ipc'
import { ListeningContext } from './ListeningTools'

vi.mock('../lib/discover-ipc', () => ({ discover: { search: vi.fn(), artistPhoto: vi.fn(async () => null), artistTracks: vi.fn(), albumTracks: vi.fn(), play: vi.fn(), youtubeCharts: vi.fn() } }))
vi.mock('@tauri-apps/api/core', () => ({ convertFileSrc: (id: string) => `http://artwork.localhost/${id}` }))
afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.resetAllMocks(); vi.unstubAllGlobals() })
const track = (id: string) => ({ id, title: id, artist: 'Artist', artist_id: null, album: null, duration_ms: 1000, artwork_url: null, permalink_url: null })
const result = (title: string): SearchResults => ({ tracks: [track(title)], artists: [], albums: [] })

it('loads curated artist photos once per session and uses cache-only covers for personalized mixes', async () => {
  const songs = ['First', 'Second', 'Third', 'Fourth'].map((name, index): SongRow => ({
    id: name, name, artist_name: `Artist ${index}`, album_name: '', duration_ms: 1000,
    artwork_url: `https://example.invalid/cover-${index}.jpg`, catalog_id: null,
  }))
  const played = songs.slice(0, 2).map(song => ({ song, source: 'local' as const }))
  const favorites = songs.slice(2).map(song => ({ song, source: 'local' as const }))
  const snapshot = { ...EMPTY_QOL, history: [...played, ...played], favorites }
  vi.spyOn(library, 'songs').mockResolvedValue(songs)
  vi.spyOn(library, 'search').mockResolvedValue([])
  vi.mocked(discover.artistPhoto).mockImplementation(async artist => `https://yt3.googleusercontent.com/${encodeURIComponent(artist)}`)
  const homepage = <ListeningContext.Provider value={{ source: 'local', snapshot, problem: vi.fn(), artist: vi.fn(), album: vi.fn(), replace: vi.fn() }}><Discover /></ListeningContext.Provider>
  const { unmount } = render(homepage)

  const personalized = await screen.findByRole('region', { name: 'Made for you' })
  const curated = screen.getByRole('region', { name: 'Capsule Picks' })
  const mixes = within(personalized).getAllByRole('button')
  expect(mixes).toHaveLength(3)
  for (const mix of mixes) expect(mix.querySelector('img')?.getAttribute('src')).toMatch(/^http:\/\/artwork\.localhost\/.*&cache=only$/)

  const repeat = within(personalized).getByRole('button', { name: 'Open mix On repeat' })
  const firstCover = repeat.querySelector('img')!
  const firstUrl = firstCover.getAttribute('src')
  fireEvent.error(firstCover)
  const fallbackUrl = repeat.querySelector('img')?.getAttribute('src')
  expect(fallbackUrl).toMatch(/^http:\/\/artwork\.localhost\/.*&cache=only$/)
  expect(fallbackUrl).not.toBe(firstUrl)

  await waitFor(() => expect(curated.querySelectorAll('img')).toHaveLength(6))
  expect(discover.artistPhoto).toHaveBeenCalledTimes(6)
  for (const [index, [artist]] of vi.mocked(discover.artistPhoto).mock.calls.entries()) {
    expect(editorialPlaylists[index]?.tracks.some(track => track.artist === artist)).toBe(true)
  }
  const covers = [...curated.querySelectorAll('img')].map(image => image.src)
  expect(new Set(covers).size).toBe(6)
  unmount()
  render(homepage)
  const reopened = screen.getByRole('region', { name: 'Capsule Picks' })
  await waitFor(() => expect([...reopened.querySelectorAll('img')].map(image => image.src)).toEqual(covers))
  expect(discover.artistPhoto).toHaveBeenCalledTimes(6)
})

it('loads public YouTube Music picks and plays the selected chart recording', async () => {
  const quick = { ...track('chartVideo1'), title: 'First chart song' }
  const trending = { ...track('chartVideo2'), title: 'Trending chart song' }
  vi.mocked(discover.youtubeCharts).mockResolvedValue({ quick: [quick], trending: [trending] })
  vi.spyOn(library, 'songs').mockResolvedValue([])
  const play = vi.spyOn(library, 'play').mockResolvedValue()
  render(<ListeningContext.Provider value={{ source: 'youtube', snapshot: EMPTY_QOL, problem: vi.fn(), artist: vi.fn(), album: vi.fn(), replace: vi.fn() }}><Discover /></ListeningContext.Provider>)

  const pick = await screen.findByRole('button', { name: 'Play First chart song' })
  expect(screen.getByRole('heading', { name: 'Quick picks' })).toBeDefined()
  expect(screen.getByRole('heading', { name: 'Trending on YouTube Music' })).toBeDefined()
  expect(discover.search).not.toHaveBeenCalled()
  fireEvent.click(pick)
  await waitFor(() => expect(play).toHaveBeenCalledWith([expect.objectContaining({ name: quick.title, catalog_id: quick.id })], 0))
  vi.mocked(discover.search).mockResolvedValue(result('Focus track'))
  fireEvent.click(screen.getByRole('button', { name: 'Focus' }))
  await waitFor(() => expect(discover.search).toHaveBeenCalledWith('tracks', expect.stringContaining('focus'), 0, 'youtube'))
})

it('keeps public catalog suggestions out of the local files view', async () => {
  vi.spyOn(library, 'songs').mockResolvedValue([])
  render(<ListeningContext.Provider value={{ source: 'local', snapshot: EMPTY_QOL, problem: vi.fn(), artist: vi.fn(), album: vi.fn(), replace: vi.fn() }}><Discover /></ListeningContext.Provider>)
  await screen.findByText('Add music to your library to see picks here.')
  expect(screen.queryByRole('searchbox')).toBeNull()
  expect(discover.search).not.toHaveBeenCalled()
  expect(discover.youtubeCharts).not.toHaveBeenCalled()
})

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
  await waitFor(() => expect(reportVisibility).toBeDefined())
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
  render(<ListeningContext.Provider value={{ source: 'soundcloud', snapshot: EMPTY_QOL, problem: vi.fn(), artist: vi.fn(), album: vi.fn(), replace: vi.fn() }}><Discover /></ListeningContext.Provider>)

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
