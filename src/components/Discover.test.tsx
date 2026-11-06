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

it('recommends cached songs from favorite artists without contacting the catalog', async () => {
  const song = (id: string, name: string, artist_name: string): SongRow => ({ id, name, artist_name, album_name: 'Record', duration_ms: 1000, artwork_url: null, catalog_id: null })
  const favorite = song('favorite', 'Favorite song', 'Favorite artist')
  const related = { ...song('related', 'More from the artist', 'Favorite artist'), artwork_url: 'https://example.invalid/cover.jpg' }
  const other = song('other', 'Unrelated track', 'Other artist')
  const readSongs = vi.spyOn(library, 'songs').mockImplementation(async (_limit, _offset, options) => options?.artistName ? [favorite, related] : [other])
  const play = vi.spyOn(library, 'play').mockResolvedValue()
  const snapshot = { ...EMPTY_QOL, history: [{ song: favorite, source: 'local' as const }], favorites: [{ song: favorite, source: 'local' as const }] }
  render(<ListeningContext.Provider value={{ source: 'local', snapshot, problem: vi.fn(), artist: vi.fn(), album: vi.fn(), replace: vi.fn() }}><Discover /></ListeningContext.Provider>)

  const pick = await screen.findByRole('button', { name: 'Play More from the artist' })
  expect(screen.getByRole('heading', { name: 'Picked for you' })).toBeDefined()
  expect(screen.getAllByRole('button', { name: /^Play / })[0]).toBe(pick)
  expect(readSongs).toHaveBeenCalledWith(expect.any(Number), 0, { artistName: 'Favorite artist' })
  expect(screen.getByRole('region', { name: 'Picked for you' }).querySelector('img')?.getAttribute('src')).toBe('http://artwork.localhost/related?w=192&cache=only')
  expect(discover.search).not.toHaveBeenCalled()
  expect(discover.artistTracks).not.toHaveBeenCalled()
  expect(discover.albumTracks).not.toHaveBeenCalled()
  expect(play).not.toHaveBeenCalled()
  fireEvent.click(pick)
  await waitFor(() => expect(play).toHaveBeenCalledWith([related], 0))
})
