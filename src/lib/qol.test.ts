import { describe, expect, it } from 'vitest'
import { filterSongs, selectionAfterClick, validNavigation, playlistContext } from './qol'
import type { SongRow } from './ipc'

const rows = ['First', 'Second', 'Third'].map((name, i) => ({ id: String(i), name, artist_name: 'Artist', album_name: 'Album', duration_ms: i * 1000 } as SongRow))
describe('library quality of life', () => {
  it('searches playlist metadata without changing source order', () => {
    expect(filterSongs(rows, 'second', 'original', 'asc')).toEqual([rows[1]])
    expect(filterSongs(rows, 'artist', 'duration', 'desc')).toEqual([...rows].reverse())
    expect(rows[0]!.name).toBe('First')
  })
  it('selects occurrences rather than collapsing duplicate track IDs', () => {
    expect([...selectionAfterClick(new Set([0]), 2, 0, true, false)]).toEqual([0, 1, 2])
    expect([...selectionAfterClick(new Set([0]), 2, 0, false, true)]).toEqual([0, 2])
    expect([...selectionAfterClick(new Set([0, 2]), 0, 2, false, true)]).toEqual([2])
  })
  it('ignores invalid persisted navigation values', () => {
    expect(validNavigation({ view: 'queue', songSort: 'invalid', direction: 'desc' })).toEqual({ view: 'queue', direction: 'desc' })
    expect(validNavigation(null)).toEqual({})
  })
  it('rejects mixed source or server playlists before playback', () => {
    expect(() => playlistContext([{ song: rows[0]!, source: 'local' }, { song: rows[1]!, source: 'navidrome', context: 'server-a' }])).toThrow(/different sources/)
    expect(() => playlistContext([{ song: rows[0]!, source: 'navidrome', context: 'server-a' }, { song: rows[1]!, source: 'navidrome', context: 'server-b' }])).toThrow(/different sources/)
    expect(playlistContext([{ song: rows[0]!, source: 'navidrome', context: 'server-a' }])).toEqual({ source: 'navidrome', context: 'server-a' })
  })
})
