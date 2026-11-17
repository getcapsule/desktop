import { afterEach, expect, it, vi } from 'vitest'

afterEach(() => {
  vi.restoreAllMocks()
  vi.unstubAllGlobals()
  vi.resetModules()
})

it('keeps the cached-art rotation stable until the next launch', async () => {
  const storage = new Map<string, string>()
  const setItem = vi.fn((key: string, value: string) => storage.set(key, value))
  vi.stubGlobal('window', { localStorage: { getItem: (key: string) => storage.get(key) ?? null, setItem } })

  const first = await import('./cover-session')
  expect(first.rotateCoverCandidates(['a', 'b'])).toEqual(['b', 'a'])
  expect(await import('./cover-session')).toBe(first)
  expect(setItem).toHaveBeenCalledTimes(1)

  vi.resetModules()
  const second = await import('./cover-session')
  expect(second.rotateCoverCandidates(['a', 'b'])).toEqual(['a', 'b'])
  expect(second.rotateCoverCandidates(['only'])).toEqual(['only'])
  expect(second.rotateCoverCandidates([])).toEqual([])
  expect(setItem).toHaveBeenCalledTimes(2)
})

it('picks artists independently and excludes the last choice for each playlist', async () => {
  const storage = new Map<string, string>()
  vi.stubGlobal('window', { localStorage: {
    getItem: (key: string) => storage.get(key) ?? null,
    setItem: (key: string, value: string) => storage.set(key, value),
  } })
  vi.spyOn(Math, 'random').mockReturnValueOnce(0.1).mockReturnValueOnce(0.7).mockReturnValue(0)
  const first = await import('./cover-session')
  const artists = ['first', 'second', 'third']
  expect(first.playlistCoverArtists('late-night', artists)[0]).toBe('first')
  expect(first.playlistCoverArtists('indie', artists)[0]).toBe('third')

  vi.resetModules()
  const second = await import('./cover-session')
  expect(second.playlistCoverArtists('late-night', artists)).toEqual(['second', 'third'])
  expect(second.playlistCoverArtists('indie', artists)).toEqual(['first', 'second'])
  expect(second.playlistCoverArtists('solo', ['only'])).toEqual(['only'])
})
