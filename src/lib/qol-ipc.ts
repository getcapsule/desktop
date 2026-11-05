import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import type { SongRow, Source } from './ipc'
import { playlistContext, type Navigation } from './qol'
import type { DiscoverTrack } from './discover-ipc'
import type { MusicPlatform } from './music-import'

export interface SongEntry {
  entry_id?: string
  song: SongRow
  source: Source
  context?: string
  playback_route?: { provider: MusicPlatform; external_id: string }
}
export interface LocalPlaylist {
  id: string
  name: string
  description?: string
  cover_id?: string | null
  songs: SongEntry[]
}
export interface QolSnapshot { favorites: SongEntry[]; history: SongEntry[]; playlists: LocalPlaylist[]; navigation: Navigation }
export const EMPTY_QOL: QolSnapshot = { favorites: [], history: [], playlists: [], navigation: {} }
export const qol = {
  snapshot: () => invoke<QolSnapshot>('qol_snapshot'),
  onUpdated: (fn: (snapshot: QolSnapshot) => void) => listen<QolSnapshot>('qol://updated', e => fn(e.payload)),
  navigation: (value: Navigation) => invoke<void>('qol_navigation_set', { value }),
  play: (songs: SongRow[], source: Source, startIndex = 0, context?: string) => invoke<void>('qol_play', { songs, source, startIndex, context: context ?? null }),
  playEntries: async (entries: SongEntry[], startIndex = 0) => {
    const { source, context } = playlistContext(entries)
    const playbackRoutes = entries.map(entry => entry.playback_route ?? null)
    return invoke<void>('qol_play', {
      songs: entries.map(entry => entry.song), source, startIndex, context: context ?? null,
      ...(playbackRoutes.some(Boolean) ? { playbackRoutes } : {}),
    })
  },
  add: (songs: SongRow[], source: Source, next = false, context?: string) => invoke<void>('qol_queue_add', { songs, source, next, context: context ?? null }),
  move: (from: number, to: number) => invoke<void>('qol_queue_move', { from, to }),
  remove: (index: number) => invoke<void>('qol_queue_remove', { index }),
  clear: () => invoke<void>('qol_queue_clear'),
  undo: () => invoke<void>('qol_queue_undo'),
  jump: (index: number) => invoke<void>('qol_queue_jump', { index }),
  favorite: (song: SongRow, source: Source, on: boolean, context?: string) => invoke<void>('qol_favorite', { song, source, on, context: context ?? null }),
  favorites: (songs: SongRow[], source: Source, on: boolean, context?: string) => invoke<void>('qol_favorites', { songs, source, on, context: context ?? null }),
  playlistAdd: (songs: SongRow[], source: Source, playlistId: string | null, name: string | null, context?: string) => invoke<string>('qol_playlist_add', { songs, source, playlistId, name, context: context ?? null }),
  playlistAddDiscovered: (track: DiscoverTrack, playlistId: string | null, name: string | null) => invoke<string>('qol_playlist_add_discovered', { track, playlistId, name }),
  playlistEdit: (playlistId: string, name: string, description: string, cover: number[] | null, removeCover: boolean) =>
    invoke<void>('qol_playlist_edit', { playlistId, name, description, cover, removeCover }),
  playlistRemoveSong: (playlistId: string, entryId: string) => invoke<void>('qol_playlist_remove_song', { playlistId, entryId }),
  reveal: (song: SongRow, source: Source) => invoke<void>('qol_reveal', { song, source }),
}
