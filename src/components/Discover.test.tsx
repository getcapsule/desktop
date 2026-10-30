// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, expect, it, vi } from 'vitest'
import { Discover } from './Discover'
import { discover, type SearchResults } from '../lib/discover-ipc'

vi.mock('../lib/discover-ipc', () => ({ discover: { search: vi.fn(), artistTracks: vi.fn(), albumTracks: vi.fn(), play: vi.fn() } }))
afterEach(() => { cleanup(); vi.resetAllMocks() })
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
  vi.mocked(discover.search).mockResolvedValue({ tracks: [track('First'), track('Second')], artists: [], albums: [] })
  vi.mocked(discover.play).mockResolvedValue()
  render(<Discover />)
  fireEvent.change(screen.getByRole('searchbox'), { target: { value: 'songs' } })
  fireEvent.submit(screen.getByRole('search'))
  fireEvent.click(await screen.findByRole('button', { name: 'Play Second' }))
  await waitFor(() => expect(discover.play).toHaveBeenCalledWith([track('First'), track('Second')], 1))
})
