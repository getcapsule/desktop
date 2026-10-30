import type { SongRow, SortDirection } from './ipc'
import type { SongEntry } from './qol-ipc'

export type LibraryView = 'songs' | 'albums' | 'artists' | 'playlists' | 'lyrics' | 'settings' | 'queue' | 'favorites' | 'history' | 'discover' | 'downloads'
export interface Navigation {
  view?: LibraryView; songSort?: 'title' | 'artist' | 'album' | 'duration'
  albumSort?: 'title' | 'artist'; direction?: SortDirection
  detail?: { kind: 'album' | 'playlist'; id: string; title: string; subtitle?: string } | null
  artistName?: string | null; artistTab?: 'songs' | 'albums'
  detailSort?: string; detailDirection?: SortDirection
}
export function validNavigation(value: unknown): Navigation {
  if (!value || typeof value !== 'object') return {}
  const n = value as Record<string, unknown>, next: Navigation = {}
  if (['songs', 'albums', 'artists', 'playlists', 'lyrics', 'settings', 'queue', 'favorites', 'history', 'discover', 'downloads'].includes(String(n.view))) next.view = n.view as LibraryView
  if (['title', 'artist', 'album', 'duration'].includes(String(n.songSort))) next.songSort = n.songSort as Navigation['songSort']
  if (['title', 'artist'].includes(String(n.albumSort))) next.albumSort = n.albumSort as Navigation['albumSort']
  if (n.direction === 'asc' || n.direction === 'desc') next.direction = n.direction
  if (n.detailDirection === 'asc' || n.detailDirection === 'desc') next.detailDirection = n.detailDirection
  if (['original', 'title', 'artist', 'album', 'duration'].includes(String(n.detailSort))) next.detailSort = String(n.detailSort)
  if (n.artistName === null || typeof n.artistName === 'string') next.artistName = n.artistName
  if (n.artistTab === 'songs' || n.artistTab === 'albums') next.artistTab = n.artistTab
  const d = n.detail as Record<string, unknown> | null
  if (d === null) next.detail = null
  else if (d && (d.kind === 'album' || d.kind === 'playlist') && typeof d.id === 'string' && typeof d.title === 'string') next.detail = { kind: d.kind, id: d.id, title: d.title, ...(typeof d.subtitle === 'string' ? { subtitle: d.subtitle } : {}) }
  return next
}
export function playlistContext(entries: SongEntry[]) {
  const first = entries[0]
  if (!first) throw new Error('This playlist is empty.')
  if (entries.every(e => /^[0-9a-f]{8}-[0-9a-f-]{27}$/i.test(e.song.id))) return { source: first.source, context: undefined }
  if (entries.some(e => e.source !== first.source || e.context !== first.context)) throw new Error('This playlist contains different sources or server accounts. Play individual tracks or use separate playlists.')
  return { source: first.source, context: first.context }
}
export function filterSongs(songs: SongRow[], query: string, sort: string, direction: SortDirection): SongRow[] {
  const q = query.trim().toLocaleLowerCase()
  const rows = q ? songs.filter(s => `${s.name} ${s.artist_name} ${s.album_name}`.toLocaleLowerCase().includes(q)) : [...songs]
  if (sort !== 'original') rows.sort((a, b) => (sort === 'duration' ? a.duration_ms - b.duration_ms : (sort === 'artist' ? a.artist_name : sort === 'album' ? a.album_name : a.name).localeCompare(sort === 'artist' ? b.artist_name : sort === 'album' ? b.album_name : b.name, undefined, { numeric: true })) * (direction === 'desc' ? -1 : 1))
  else if (direction === 'desc') rows.reverse()
  return rows
}
export function selectionAfterClick(selected: Set<number>, index: number, anchor: number, shift: boolean, toggle: boolean): Set<number> {
  if (shift) return new Set(Array.from({ length: Math.abs(index - anchor) + 1 }, (_, i) => Math.min(index, anchor) + i))
  const next = toggle ? new Set(selected) : new Set<number>()
  if (next.has(index)) next.delete(index); else next.add(index)
  return next
}
