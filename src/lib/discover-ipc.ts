import { invoke } from '@tauri-apps/api/core'

export type SearchKind = 'tracks' | 'artists' | 'albums'
export interface DiscoverTrack {
  id: string
  title: string
  artist: string
  artist_id: string | null
  album: string | null
  duration_ms: number
  artwork_url: string | null
  permalink_url: string | null
}
export interface DiscoverArtist { id: string; name: string; artwork_url: string | null; permalink_url: string | null }
export interface DiscoverAlbum { id: string; title: string; artist: string; artist_id: string | null; artwork_url: string | null; permalink_url: string | null; track_count: number }
export interface SearchResults { tracks: DiscoverTrack[]; artists: DiscoverArtist[]; albums: DiscoverAlbum[] }
export interface AlbumTracks { album: DiscoverAlbum; tracks: DiscoverTrack[]; unavailable_count: number; truncated: boolean }

export const discover = {
  search: (kind: SearchKind, query: string, offset = 0) => invoke<SearchResults>('discover_search', { kind, query, limit: 30, offset }),
  artistTracks: (artistId: string, offset = 0) => invoke<DiscoverTrack[]>('discover_artist_tracks', { artistId, limit: 30, offset }),
  albumTracks: (albumId: string) => invoke<AlbumTracks>('discover_album_tracks', { albumId }),
  play: (tracks: DiscoverTrack[], startIndex = 0) => invoke<void>('soundcloud_play', { tracks, startIndex }),
}
