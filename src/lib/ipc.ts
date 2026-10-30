import { convertFileSrc, invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'

export type Status = 'idle' | 'loading' | 'playing' | 'paused' | 'stalled' | 'ended'
export type Repeat = 'off' | 'all' | 'one'

export interface Track {
  id: string
  title: string
  artist: string
  album: string
  duration_ms: number
}

export type Source = 'youtube' | 'soundcloud' | 'navidrome' | 'local'
export type SortDirection = 'asc' | 'desc'

export interface Settings {
  source: Source
  onboarded: boolean
  developer: boolean
  appearance: { glass: string; theme: string }
  spotify: { client_id: string }
  navidrome: { url: string; username: string }
  local: { folders: string[] }
  lyrics: { offset_ms: number }
  lastfm: { api_key: string; shared_secret: string }
  discord: { client_id: string; serve_art_from_server: boolean }
}

export const settings = {
  get: () => invoke<Settings>('settings_get'),
  set: (next: Settings) => invoke<void>('settings_set', { settings: next }),
}

export interface ThemeFailure {
  fg: string
  bg: string
  ratio: number
}

export interface ThemeSummary {
  id: string
  name: string
  polarity: 'light' | 'dark'
  builtin: boolean
  failures: ThemeFailure[]
}

export const themes = {
  list: () => invoke<ThemeSummary[]>('themes_list'),
}

export interface LyricLine {
  at_ms: number
  text: string
}

export interface LyricsResult {
  lines: LyricLine[]
  plain: string | null
}

export interface PlayerState {
  imported_queue?: boolean
  playback_source?: 'local' | 'navidrome' | 'youtube' | null
  status: Status
  queue: Track[]
  index: number | null
  position_ms: number
  volume: number
  shuffle: boolean
  repeat: Repeat
}

export interface AuthStatus {
  authenticated: boolean
  storefront: string | null
}

export const player = {
  snapshot: () => invoke<PlayerState>('player_snapshot'),
  play: () => invoke<void>('player_play'),
  pause: () => invoke<void>('player_pause'),
  toggle: () => invoke<void>('player_toggle'),
  next: () => invoke<void>('player_next'),
  previous: () => invoke<void>('player_previous'),
  seek: (ms: number) => invoke<void>('player_seek', { ms: Math.max(0, Math.round(ms)) }),
  setVolume: (percent: number) =>
    invoke<void>('player_set_volume', {
      percent: Math.min(100, Math.max(0, Math.round(percent))),
    }),
  toggleShuffle: () => invoke<void>('player_toggle_shuffle'),
  cycleRepeat: () => invoke<void>('player_cycle_repeat'),
}

export interface SpotifyStatus {
  configured: boolean
  authenticated: boolean
  display_name: string | null
  playback_supported: boolean
}

export const spotify = {
  status: () => invoke<SpotifyStatus>('spotify_status'),
  connect: (clientId: string) => invoke<void>('spotify_connect', { clientId }),
  disconnect: () => invoke<void>('spotify_disconnect'),
}

export interface NavidromeStatus {
  configured: boolean
  url: string
  username: string
  insecure: boolean
}

export const navidrome = {
  status: () => invoke<NavidromeStatus>('navidrome_status'),
  connect: (url: string, username: string, password: string) =>
    invoke<void>('navidrome_connect', { url, username, password }),
}

export interface LastfmStatus {
  configured: boolean
  linked: boolean
  username: string | null
}

export const lastfm = {
  status: () => invoke<LastfmStatus>('lastfm_status'),
  connect: () => invoke<void>('lastfm_connect'),
  disconnect: () => invoke<void>('lastfm_disconnect'),
}

export interface SongRow {
  id: string
  catalog_id: string | null
  name: string
  artist_name: string
  album_name: string
  duration_ms: number
  artwork_url: string | null
}

export interface AlbumRow {
  id: string
  name: string
  artist_name: string
  artwork_url: string | null
  track_count: number
}

export interface PlaylistRow {
  id: string
  name: string
  artwork_url: string | null
}

export interface ArtistRow {
  id?: string
  name: string
  artwork_url?: string | null
  song_count: number
  album_count: number
}

export interface SongQuery {
  sort?: 'title' | 'artist' | 'album' | 'duration'
  direction?: SortDirection
  query?: string
  artistName?: string
}

export interface AlbumQuery {
  sort?: 'title' | 'artist'
  direction?: SortDirection
  artistName?: string
}

export interface LibraryCounts {
  songs: number
  albums: number
  playlists: number
  artists: number
}

export interface SyncProgress {
  stage: string
  songs: number
  albums: number
  playlists: number
  done: boolean
}

export interface SyncFailed {
  reason: string
  needsAuth: boolean
}

export const library = {
  songs: (limit = 500, offset = 0, options: SongQuery = {}) => invoke<SongRow[]>('library_songs', { limit, offset, ...options }),
  albums: (limit = 500, offset = 0, options: AlbumQuery = {}) => invoke<AlbumRow[]>('library_albums', { limit, offset, ...options }),
  artists: (limit = 500, offset = 0, options: { sort?: 'name'; direction?: SortDirection } = {}) => invoke<ArtistRow[]>('library_artists', { limit, offset, ...options }),
  playlists: () => invoke<PlaylistRow[]>('library_playlists'),
  albumSongs: (albumId: string) => invoke<SongRow[]>('library_album_songs', { albumId }),
  playlistSongs: (playlistId: string) =>
    invoke<SongRow[]>('library_playlist_songs', { playlistId }),
  search: (query: string, limit = 100) => invoke<SongRow[]>('library_search', { query, limit }),
  lyrics: (trackId: string) => invoke<LyricsResult>('lyrics_for', { trackId }),
  counts: () => invoke<LibraryCounts>('library_counts'),
  sync: () => invoke<void>('library_sync'),
  play: (songs: SongRow[], startIndex = 0) =>
    invoke<void>('play_songs', { songs, startIndex }),
  playAlbum: (albumId: string) => invoke<void>('play_album', { albumId }),
  playPlaylist: (playlistId: string) => invoke<void>('play_playlist', { playlistId }),
}

export function artworkUrl(id: string, size = 96, revision?: number): string {
  return `${convertFileSrc(id, 'artwork')}?w=${size}${revision === undefined ? '' : `&v=${revision}`}`
}

export const dev = {
  loadRecent: () => invoke<void>('dev_load_recent'),
  diagnostics: () => invoke<string>('dev_diagnostics'),
}

type Events = {
  'player://state': PlayerState
  'spotify://changed': SpotifyStatus
  'spotify://error': string
  'library://progress': SyncProgress
  'library://updated': LibraryCounts
  'library://failed': SyncFailed
  'playback://error': string
  'lastfm://linked': LastfmStatus
  'lastfm://failed': string
  'theme://changed': string
  'appearance://changed': string
}

export function on<K extends keyof Events>(
  event: K,
  handler: (payload: Events[K]) => void,
): Promise<UnlistenFn> {
  return listen<Events[K]>(event, (e) => handler(e.payload))
}

export function formatTime(ms: number): string {
  const total = Math.floor(ms / 1000)
  const m = Math.floor(total / 60)
  const s = total % 60
  return `${m}:${String(s).padStart(2, '0')}`
}
