import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import type { DiscoverTrack } from './discover-ipc'
import type { SongRow, Source } from './ipc'
export type DownloadRequest =
  | { kind: 'canonical'; track_id: string }
  | { kind: 'soundcloud'; track: DiscoverTrack }
  | { kind: 'library'; song: SongRow; source: Source; context?: string }
export interface DownloadJob {
  id: string
  title: string
  artist: string
  source: string
  status: 'queued' | 'resolving' | 'downloading' | 'completed' | 'cancelled' | 'failed'
  bytes: number
  total_bytes: number | null
  error: string | null
  path: string | null
  format: string | null
  directory: string
}
export const downloads = {
  start: (requests: DownloadRequest[], directory: string) => invoke<DownloadJob[]>('downloads_start', { requests, directory }),
  snapshot: () => invoke<DownloadJob[]>('downloads_snapshot'),
  directory: () => invoke<string | null>('downloads_directory'),
  cancel: (id: string) => invoke<void>('downloads_cancel', { id }),
  retry: (id: string) => invoke<void>('downloads_retry', { id }),
  reveal: (id: string) => invoke<void>('downloads_reveal', { id }),
  onChanged: (fn: (jobs: DownloadJob[]) => void) => listen<DownloadJob[]>('downloads://changed', event => fn(event.payload)),
}

