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
export interface YoutubeCharts { quick: DiscoverTrack[]; trending: DiscoverTrack[] }

function search(kind: SearchKind, query: string, offset = 0, source?: 'youtube') {
  return invoke<SearchResults>('discover_search', { kind, query, limit: 30, offset, source })
}

function artistKey(name: string) {
  return name.trim().toLowerCase().normalize('NFKD').replace(/\p{M}/gu, '').replace(/\u0131/g, 'i')
}

export const discover = {
  search,
  artistPhoto: async (artist: string): Promise<string | null> => {
    const { artists } = await search('artists', artist, 0, 'youtube')
    const wanted = artistKey(artist)
    return artists.find(candidate => artistKey(candidate.name) === wanted && candidate.artwork_url?.startsWith('https://'))?.artwork_url ?? null
  },
  artistTracks: (artistId: string, offset = 0, source?: 'youtube') => invoke<DiscoverTrack[]>('discover_artist_tracks', { artistId, limit: 30, offset, source }),
  albumTracks: (albumId: string, source?: 'youtube') => invoke<AlbumTracks>('discover_album_tracks', { albumId, source }),
  youtubeCharts: () => invoke<YoutubeCharts>('discover_youtube_charts'),
  play: (tracks: DiscoverTrack[], startIndex = 0) => invoke<void>('soundcloud_play', { tracks, startIndex }),
}
