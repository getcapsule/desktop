import { open } from '@tauri-apps/plugin-dialog'
import { downloads, type DownloadRequest } from './downloads-ipc'
import type { SongRow, Source } from './ipc'

const DIRECTORY_KEY = 'capsule.download-directory'
let choosing: Promise<string | null> | null = null
export function downloadDirectory(): string {
  try { return localStorage.getItem(DIRECTORY_KEY) ?? '' } catch { return '' }
}
export async function chooseDownloadDirectory(change = false): Promise<string | null> {
  const current = downloadDirectory()
  if (current && !change) return current
  if (choosing) return choosing
  choosing = (async () => {
    const selected = await open({ directory: true, multiple: false, title: 'Save music to', ...(current ? { defaultPath: current } : {}) })
    if (typeof selected !== 'string') return null
    try { localStorage.setItem(DIRECTORY_KEY, selected) } catch { /* Folder selection still works without browser storage. */ }
    return selected
  })()
  try { return await choosing } finally { choosing = null }
}
export function songDownloadRequests(songs: SongRow[], source: Source, context?: string): DownloadRequest[] {
  return songs.map(song => song.catalog_id === song.id && /^[0-9a-f]{8}-[0-9a-f-]{27}$/i.test(song.id)
    ? { kind: 'canonical', track_id: song.id }
    : { kind: 'library', song, source, ...(context ? { context } : {}) })
}
export async function queueDownloads(requests: DownloadRequest[]) {
  if (!requests.length) return null
  const directory = await chooseDownloadDirectory()
  return directory ? downloads.start(requests, directory) : null
}
