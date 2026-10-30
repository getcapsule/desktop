import { invoke } from '@tauri-apps/api/core'

export type MusicPlatform = 'spotify' | 'apple_music' | 'youtube' | 'soundcloud' | 'navidrome' | 'local'
export type ContentKind = 'track' | 'album' | 'playlist' | 'artist' | 'profile' | 'likes'
export interface TrackOrigin { platform: MusicPlatform; external_id: string; url: string | null }
export interface Metadata {
  title: string; artists: string[]; album: string | null; duration_ms: number | null
  artwork_url: string | null; isrc: string | null; explicit: boolean | null
}
export interface CapsuleTrack extends Metadata { id: string; origins: TrackOrigin[] }
export interface ImportUrl { provider: MusicPlatform; kind: ContentKind; id: string; storefront: string | null; url: string }
export interface ImportedCollection {
  source_url: string; provider: MusicPlatform; kind: ContentKind; title: string
  artwork_url: string | null; tracks: { origin: TrackOrigin; metadata: Metadata }[]; warnings: string[]
}
export interface ImportJob {
  id: string,
  playlist_id: string | null,
  source_url: string,
  provider: string,
  collection_type: string,
  collection_title: string | null,
  collection_artwork: string | null,
  total_tracks: number,
  processed: number,
  matched: number,
  needs_review: number,
  unavailable: number,
  error: string | null,
  status: string,
  created_at: string,
  updated_at: string,
}
export interface PlaylistEntry { entry_id: string; position: number; track: CapsuleTrack; source_preference?: MusicPlatform | null; resolution: 'pending' | 'searching' | 'matched' | 'needs_review' | 'unavailable' | 'error' }
export interface PlaybackCandidate extends Metadata { provider: MusicPlatform; external_id: string; confidence: number; compatible: boolean }
export interface ImportPlaybackSettings { youtube_enabled: boolean; priority: MusicPlatform[] }

export const musicImport = {
  detect: (url: string) => invoke<ImportUrl>('import_detect', { url }),
  start: (url: string) => invoke<ImportJob>('import_job', { url }),
  jobs: () => invoke<ImportJob[]>('import_jobs'),
  job: (jobId: string) => invoke<ImportJob>('import_status', { jobId }),
  preview: (jobId: string) => invoke<ImportedCollection | null>('import_preview', { jobId }),
  confirm: (jobId: string) => invoke<string>('import_confirm', { jobId }),
  cancel: (jobId: string) => invoke<void>('import_cancel', { jobId }),
  resume: (jobId: string) => invoke<void>('import_resume', { jobId }),
  entries: (playlistId: string) => invoke<PlaylistEntry[]>('import_entries', { playlistId }),
  candidates: (trackId: string) => invoke<PlaybackCandidate[]>('import_candidates', { trackId }),
  resolve: (trackId: string) => invoke<void>('import_resolve', { trackId }),
  choose: (trackId: string, provider: MusicPlatform, externalId: string) => invoke<void>('import_choose', { trackId, provider, externalId }),
  invalidate: (trackId: string, provider: MusicPlatform) => invoke<void>('import_invalidate', { trackId, provider }),
  retryPlayback: (trackId: string) => invoke<void>('import_retry_playback', { trackId }),
  rename: (playlistId: string, name: string) => invoke<void>('import_rename', { playlistId, name }),
  reorder: (playlistId: string, entryIds: string[]) => invoke<void>('import_reorder', { playlistId, entryIds }),
  remove: (playlistId: string, entryId: string) => invoke<void>('import_remove', { playlistId, entryId }),
  saveCredential: (provider: MusicPlatform, secret: string) => invoke<void>('import_credential', { provider, secret }),
  credentialStatus: () => invoke<Record<string, boolean>>('import_credential_status'),
  playbackSettings: () => invoke<ImportPlaybackSettings>('import_playback_settings'),
  setPlaybackSettings: (youtubeEnabled: boolean, priority: MusicPlatform[]) => invoke<void>('import_set_playback_settings', { youtubeEnabled, priority }),
  sourcePreference: (trackId: string, provider: MusicPlatform | null) => invoke<void>('import_source_preference', { trackId, provider }),
  linkSource: (trackId: string, provider: MusicPlatform, externalId: string) => invoke<void>('import_link_source', { trackId, provider, externalId }),
}
