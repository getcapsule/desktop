// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { DownloadButton, Downloads } from './Downloads'
import { songDownloadRequests } from '../lib/download-actions'
import type { DownloadJob } from '../lib/downloads-ipc'
import type { SongRow } from '../lib/ipc'
const mock = vi.hoisted(() => ({ invoke: vi.fn(), open: vi.fn(), listener: null as null | ((event: { payload: DownloadJob[] }) => void) }))
vi.mock('@tauri-apps/api/core', () => ({ invoke: mock.invoke }))
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: mock.open }))
vi.mock('@tauri-apps/api/event', () => ({ listen: async (_: string, callback: typeof mock.listener) => { mock.listener = callback; return () => {} } }))
beforeEach(() => {
  const storage = new Map<string, string>()
  vi.stubGlobal('localStorage', { getItem: (key: string) => storage.get(key) ?? null, setItem: (key: string, value: string) => storage.set(key, value) })
  mock.invoke.mockReset(); mock.open.mockReset(); mock.listener = null
  vi.stubGlobal('ResizeObserver', class { observe() {} disconnect() {} })
})
afterEach(() => { cleanup(); vi.unstubAllGlobals() })
const request = [{ kind: 'canonical' as const, track_id: 'one' }]
it('does not start downloading when the folder picker is cancelled', async () => {
  mock.open.mockResolvedValue(null)
  render(<DownloadButton requests={request} />)
  fireEvent.click(screen.getByRole('button', { name: 'Download' }))
  await waitFor(() => expect(mock.open).toHaveBeenCalledOnce())
  await waitFor(() => expect(screen.getByRole('button', { name: 'Download' }).hasAttribute('disabled')).toBe(false))
  expect(mock.invoke).not.toHaveBeenCalled()
})
it('sends source identities and the selected folder to the service', async () => {
  mock.open.mockResolvedValue('C:\\Music'); mock.invoke.mockResolvedValue([])
  render(<DownloadButton requests={request} />)
  fireEvent.click(screen.getByRole('button', { name: 'Download' }))
  await waitFor(() => expect(mock.invoke).toHaveBeenCalledWith('downloads_start', { requests: request, directory: 'C:\\Music' }))
})
it('does not mistake a provider UUID for an imported recording', () => {
  const song = { id: '12345678-1234-1234-1234-123456789012', catalog_id: null } as SongRow
  expect(songDownloadRequests([song], 'navidrome', 'server')[0]).toMatchObject({ kind: 'library', source: 'navidrome', context: 'server' })
  expect(songDownloadRequests([{ ...song, catalog_id: song.id }], 'local')[0]).toEqual({ kind: 'canonical', track_id: song.id })
})
it('keeps live download progress when an older initial snapshot arrives late', async () => {
  let finish!: (jobs: DownloadJob[]) => void
  mock.invoke.mockImplementation((command: string) => command === 'downloads_snapshot' ? new Promise(resolve => { finish = resolve }) : Promise.resolve(null))
  render(<Downloads />)
  await waitFor(() => expect(finish).toBeDefined())
  const job = { id: 'job', title: 'New transfer', artist: 'Artist', source: 'youtube', status: 'downloading', bytes: 500, total_bytes: 1000, format: 'm4a', error: null, path: null, directory: 'C:\\Music' } as DownloadJob
  await act(async () => { mock.listener?.({ payload: [job] }); finish([]) })
  expect(screen.getByText('New transfer')).toBeDefined()
  expect(screen.getByRole('progressbar', { name: 'Downloading New transfer' }).getAttribute('value')).toBe('500')
})
