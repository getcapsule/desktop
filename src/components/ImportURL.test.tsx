// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { ImportURL } from './ImportURL'
import type { ImportJob, ImportedCollection, Metadata, PlaylistEntry, PlaybackCandidate } from '../lib/music-import'

const boundary = vi.hoisted(() => ({ invoke: vi.fn() }))
vi.mock('@tauri-apps/api/core', () => ({ invoke: boundary.invoke, convertFileSrc: (id: string) => `http://artwork.localhost/${id}` }))
const url = 'https://open.spotify.com/playlist/abc'
const metadata: Metadata = { title: '<img src=x onerror=alert(1)>', artists: ['A & B'], album: 'Album', duration_ms: 1000, artwork_url: null, isrc: null, explicit: null }
const collection: ImportedCollection = { source_url: url, provider: 'spotify', kind: 'playlist', title: 'My mix', artwork_url: 'https://untrusted.example/art', tracks: [{ origin: { platform: 'spotify', external_id: 'abc', url }, metadata }], warnings: ['One unavailable entry was skipped.'] }
const makeJob = (patch: Partial<ImportJob> = {}): ImportJob => ({ id: 'job-1', source_url: url, provider: 'spotify', collection_type: 'playlist', collection_title: 'My mix', collection_artwork: null, total_tracks: 1, processed: 0, matched: 0, needs_review: 0, unavailable: 0, error: null, status: 'ready', created_at: '', updated_at: '', playlist_id: null, ...patch })
const entry: PlaylistEntry = { entry_id: 'entry-1', position: 0, track: { id: 'track-1', ...metadata, origins: [] }, resolution: 'needs_review' }
const candidate: PlaybackCandidate = { ...metadata, title: 'Song (live)', provider: 'youtube', external_id: 'video-1', compatible: false, confidence: 0.72 }
let job: ImportJob
let recent: ImportJob[]
let candidates: PlaybackCandidate[]
let playbackSettings: { youtube_enabled: boolean; priority: ('local' | 'navidrome' | 'youtube')[] }
const click = (name: string | RegExp) => fireEvent.click(screen.getByRole('button', { name }))
const input = () => screen.getByLabelText('Music URL')
function deferred<T>() { let resolve!: (value: T) => void; const promise = new Promise<T>(yes => { resolve = yes }); return { promise, resolve } }
async function preview() {
  fireEvent.change(input(), { target: { value: url } })
  await waitFor(() => expect((screen.getByRole('button', { name: 'Preview' }) as HTMLButtonElement).disabled).toBe(false))
  click('Preview')
  await screen.findByRole('button', { name: 'Save playlist' })
}

beforeEach(() => {
  job = makeJob(); recent = []; candidates = [candidate]
  playbackSettings = { youtube_enabled: false, priority: ['local', 'navidrome', 'youtube'] }
  boundary.invoke.mockReset().mockImplementation(async (command: string, args?: { youtubeEnabled: boolean; priority: ('local' | 'navidrome' | 'youtube')[] }) => {
    switch (command) {
      case 'import_jobs': return recent
      case 'import_detect': return { provider: 'spotify', kind: 'playlist', id: 'abc', storefront: null, url }
      case 'import_job': return makeJob({ status: 'fetching' })
      case 'import_status': return job
      case 'import_preview': return collection
      case 'import_confirm': job = makeJob({ status: 'resolving', playlist_id: 'playlist-1' }); return 'playlist-1'
      case 'import_entries': return [entry]
      case 'import_cancel': job = makeJob({ status: 'cancelled' }); return
      case 'import_resume': job = makeJob(); return
      case 'import_candidates': return candidates
      case 'import_choose': return
      case 'import_resolve': return
      case 'import_credential': return
      case 'import_credential_status': return { spotify: true }
      case 'import_playback_settings': return playbackSettings
      case 'import_set_playback_settings': playbackSettings = { youtube_enabled: args!.youtubeEnabled, priority: args!.priority }; return
    }
  })
})
afterEach(cleanup)

it('opens an explicit recording review without requiring an import job and drops stale candidates', async () => {
  const pending = deferred<PlaybackCandidate[]>()
  const original = boundary.invoke.getMockImplementation()!
  boundary.invoke.mockImplementation((name: string, args: { trackId?: string }) => name === 'import_candidates' && args.trackId === 'old' ? pending.promise : original(name, args))
  const view = render(<ImportURL onComplete={() => {}} focusTrackId="old" />)
  view.rerender(<ImportURL onComplete={() => {}} focusTrackId="track-1" />)
  await screen.findByRole('button', { name: 'Use version mismatch: Song (live)' })
  await act(async () => pending.resolve([{ ...candidate, title: 'Stale recording' }]))
  expect(screen.queryByText(/Stale recording/)).toBeNull()
})

it('explains repeated preview occurrences before saving and exposes measurable progress', async () => {
  const original = boundary.invoke.getMockImplementation()!
  boundary.invoke.mockImplementation((name: string, args: unknown) => name === 'import_preview' ? Promise.resolve({ ...collection, tracks: [collection.tracks[0], collection.tracks[0]] }) : original(name, args))
  render(<ImportURL onComplete={() => {}} />)
  await preview()
  expect(screen.getByText(/1 repeated occurrence.*preserved/i)).toBeDefined()
  expect(screen.getByRole('progressbar', { name: 'Import progress' })).toBeDefined()
})

it('explains that cancelling source checks keeps the saved partial playlist', async () => {
  job = makeJob({ status: 'cancelled', playlist_id: 'playlist-1', total_tracks: 3, processed: 1, needs_review: 1 })
  recent = [job]
  render(<ImportURL onComplete={() => {}} />)
  fireEvent.click(await screen.findByRole('button', { name: /Open import My mix/ }))
  expect(await screen.findByText(/Saved tracks are kept.*resume/i)).toBeDefined()
})

it('retains incomplete-fetch warnings after confirmation and warns before importing a URL twice', async () => {
  job = makeJob({ status: 'completed', playlist_id: 'playlist-1' })
  recent = [job]
  render(<ImportURL onComplete={() => {}} />)
  fireEvent.change(input(), { target: { value: url } })
  expect(await screen.findByText(/This URL was already imported/)).toBeDefined()
  fireEvent.click(await screen.findByRole('button', { name: /Open import My mix/ }))
  expect(await screen.findByText('One unavailable entry was skipped.')).toBeDefined()
})

it('previews metadata before saving immediately and shows per-track review states safely', async () => {
  const saved = vi.fn()
  render(<ImportURL onComplete={saved} />)
  await preview()
  expect(boundary.invoke).not.toHaveBeenCalledWith('import_confirm', expect.anything())
  expect(screen.getByText('My mix')).toBeDefined()
  expect(screen.getByText('One unavailable entry was skipped.')).toBeDefined()
  expect(screen.getByRole('img').getAttribute('src')).toBe('http://artwork.localhost/job-1?w=96')
  click('Save playlist')
  expect(await screen.findByText(metadata.title)).toBeDefined()
  expect(await screen.findByText('Needs review')).toBeDefined()
  expect(boundary.invoke).toHaveBeenCalledWith('import_entries', { playlistId: 'playlist-1' })
  expect(saved).toHaveBeenCalledWith(expect.stringContaining('Saved'))
  expect(boundary.invoke).not.toHaveBeenCalledWith('import_choose', expect.anything())
})

it('ignores stale URL detection and never previews an unvalidated edited URL', async () => {
  const pending = deferred<unknown>()
  const original = boundary.invoke.getMockImplementation()!
  boundary.invoke.mockImplementation((name: string, args: unknown) => name === 'import_detect' ? pending.promise : original(name, args))
  render(<ImportURL onComplete={() => {}} />)
  fireEvent.change(input(), { target: { value: url } })
  await waitFor(() => expect(boundary.invoke).toHaveBeenCalledWith('import_detect', { url }))
  fireEvent.change(input(), { target: { value: 'https://open.spotify.com.evil.example/playlist/a' } })
  await act(async () => pending.resolve({ provider: 'spotify', kind: 'playlist', id: 'abc', url }))
  expect((screen.getByRole('button', { name: 'Preview' }) as HTMLButtonElement).disabled).toBe(true)
  expect(boundary.invoke).not.toHaveBeenCalledWith('import_job', expect.anything())
})

it('renders string API failures locally and preserves the URL for retry', async () => {
  const original = boundary.invoke.getMockImplementation()!
  boundary.invoke.mockImplementation((name: string, args: unknown) => name === 'import_job' ? Promise.reject('Metadata access token required') : original(name, args))
  render(<ImportURL onComplete={() => {}} />)
  fireEvent.change(input(), { target: { value: url } })
  await waitFor(() => expect((screen.getByRole('button', { name: 'Preview' }) as HTMLButtonElement).disabled).toBe(false))
  click('Preview')
  expect(await screen.findByRole('alert')).toHaveProperty('textContent', 'Metadata access token required')
  expect((input() as HTMLInputElement).value).toBe(url)
})

it('cancels a pending job and discards an older in-flight status response', async () => {
  const pending = deferred<ImportJob>()
  let first = true
  const original = boundary.invoke.getMockImplementation()!
  boundary.invoke.mockImplementation((name: string, args: unknown) => {
    if (name === 'import_status' && first) { first = false; return pending.promise }
    return original(name, args)
  })
  render(<ImportURL onComplete={() => {}} />)
  fireEvent.change(input(), { target: { value: url } })
  await waitFor(() => expect((screen.getByRole('button', { name: 'Preview' }) as HTMLButtonElement).disabled).toBe(false))
  click('Preview')
  fireEvent.click(await screen.findByRole('button', { name: 'Cancel import' }))
  await screen.findByText('Cancelled')
  await act(async () => pending.resolve(makeJob()))
  expect(screen.queryByRole('button', { name: 'Save playlist' })).toBeNull()
  expect(screen.getByText('Cancelled')).toBeDefined()
})

it('loads a persisted interrupted job and resumes it without starting another import', async () => {
  job = makeJob({ status: 'interrupted' })
  recent = [job]
  render(<ImportURL onComplete={() => {}} />)
  fireEvent.click(await screen.findByRole('button', { name: /Open import My mix/ }))
  await screen.findByRole('button', { name: 'Resume import' })
  click('Resume import')
  await screen.findByRole('button', { name: 'Save playlist' })
  expect(boundary.invoke).toHaveBeenCalledWith('import_resume', { jobId: 'job-1' })
  expect(boundary.invoke).not.toHaveBeenCalledWith('import_job', expect.anything())
})

it('shows candidate version mismatches and applies only the explicitly selected candidate', async () => {
  render(<ImportURL onComplete={() => {}} />)
  await preview(); click('Save playlist')
  fireEvent.click(await screen.findByRole('button', { name: /Review track/ }))
  const choose = await screen.findByRole('button', { name: 'Use version mismatch: Song (live)' })
  expect(boundary.invoke).not.toHaveBeenCalledWith('import_choose', expect.anything())
  fireEvent.click(choose)
  await waitFor(() => expect(boundary.invoke).toHaveBeenCalledWith('import_choose', { trackId: 'track-1', provider: 'youtube', externalId: 'video-1' }))
})

it.each(['spotify', 'apple_music', 'youtube', 'soundcloud'] as const)('stores %s metadata credentials through the credential boundary and clears transient input', async provider => {
  render(<ImportURL onComplete={() => {}} />)
  fireEvent.click(screen.getByText('Metadata API access'))
  fireEvent.change(screen.getByLabelText('Metadata provider'), { target: { value: provider } })
  const labels = { spotify: 'Spotify OAuth access token', apple_music: 'Apple Music developer token', youtube: 'YouTube Data API key', soundcloud: 'SoundCloud OAuth access token' }
  fireEvent.click(screen.getByText('Advanced: manual metadata credential'))
  const secret = screen.getByLabelText(labels[provider])
  expect(secret.getAttribute('type')).toBe('password')
  fireEvent.change(secret, { target: { value: 'private-token' } })
  click('Save metadata credential')
  await waitFor(() => expect(boundary.invoke).toHaveBeenCalledWith('import_credential', { provider, secret: 'private-token' }))
  expect((secret as HTMLInputElement).value).toBe('')
  expect(boundary.invoke.mock.calls.some(([name]) => name === 'settings_set')).toBe(false)
})

it('polls resolution progress after saving and replaces pending entry states', async () => {
  const original = boundary.invoke.getMockImplementation()!
  boundary.invoke.mockImplementation((name: string, args: unknown) => name === 'import_entries'
    ? Promise.resolve([{ ...entry, resolution: job.status === 'completed' ? 'matched' : 'pending' }])
    : original(name, args))
  render(<ImportURL onComplete={() => {}} />)
  await preview(); click('Save playlist')
  await screen.findByText('Pending')
  job = makeJob({ playlist_id: 'playlist-1', status: 'completed', processed: 1, matched: 1 })
  expect(await screen.findByText('Matched', {}, { timeout: 2000 })).toBeDefined()
  expect(screen.queryByRole('button', { name: 'Cancel import' })).toBeNull()
})

it('honors a saved disabled YouTube playback setting until enabled and restores persisted source priority', async () => {
  const view = render(<ImportURL onComplete={() => {}} />)
  fireEvent.click(screen.getByText('Playback sources'))
  const checkbox = screen.getByRole('checkbox', { name: 'Enable YouTube playback' }) as HTMLInputElement
  await waitFor(() => expect(checkbox.disabled).toBe(false))
  expect(checkbox.checked).toBe(false)
  expect(boundary.invoke).not.toHaveBeenCalledWith('import_set_playback_settings', expect.anything())
  expect(screen.getByText(/No account or cookies needed/)).toBeDefined()
  fireEvent.click(checkbox)
  await waitFor(() => expect(checkbox.checked).toBe(true))
  expect(boundary.invoke).toHaveBeenCalledWith('import_set_playback_settings', { youtubeEnabled: true, priority: ['local', 'navidrome', 'youtube'] })
  fireEvent.change(screen.getByLabelText('Preferred playback source'), { target: { value: 'navidrome' } })
  await waitFor(() => expect(playbackSettings.priority[0]).toBe('navidrome'))
  view.unmount()
  render(<ImportURL onComplete={() => {}} />)
  await waitFor(() => expect((screen.getByRole('checkbox', { name: 'Enable YouTube playback' }) as HTMLInputElement).checked).toBe(true))
  expect((screen.getByLabelText('Preferred playback source') as HTMLSelectElement).value).toBe('navidrome')
})

it('keeps failed YouTube opt-in off and reports the persistence error beside playback controls', async () => {
  const original = boundary.invoke.getMockImplementation()!
  boundary.invoke.mockImplementation((name: string, args: unknown) => name === 'import_set_playback_settings'
    ? Promise.reject('Playback settings could not be saved') : original(name, args))
  render(<ImportURL onComplete={() => {}} />)
  const checkbox = screen.getByRole('checkbox', { name: 'Enable YouTube playback' }) as HTMLInputElement
  await waitFor(() => expect(checkbox.disabled).toBe(false))
  fireEvent.click(checkbox)
  expect(await screen.findByRole('alert')).toHaveProperty('textContent', 'Playback settings could not be saved')
  expect(checkbox.checked).toBe(false)
  expect(checkbox.disabled).toBe(false)
  expect((screen.getByLabelText('Preferred playback source').querySelector('option[value="youtube"]') as HTMLOptionElement).disabled).toBe(true)
})

it('edits a local playlist with stable duplicate entry IDs and keeps source metadata untouched', async () => {
  const original = boundary.invoke.getMockImplementation()!
  const duplicate = { ...entry, entry_id: 'entry-2', position: 1 }
  let rows = [entry, duplicate]
  boundary.invoke.mockImplementation((name: string, args: any) => {
    if (name === 'import_entries') return rows
    if (name === 'import_reorder') { rows = args.entryIds.map((id: string, position: number) => ({ ...rows.find(row => row.entry_id === id)!, position })); return }
    if (name === 'import_rename') return
    if (name === 'import_remove') { rows = rows.filter(row => row.entry_id !== args.entryId); return }
    return original(name, args)
  })
  render(<ImportURL onComplete={() => {}} />)
  await preview(); click('Save playlist')
  fireEvent.click(await screen.findByText('Edit local playlist'))
  fireEvent.click(await screen.findByRole('button', { name: 'Move entry 2 up' }))
  await waitFor(() => expect(boundary.invoke).toHaveBeenCalledWith('import_reorder', { playlistId: 'playlist-1', entryIds: ['entry-2', 'entry-1'] }))
  await waitFor(() => expect(rows[0]?.entry_id).toBe('entry-2'))
  fireEvent.change(screen.getByLabelText('New local playlist name'), { target: { value: 'My local name' } })
  await waitFor(() => expect((screen.getByRole('button', { name: 'Rename playlist' }) as HTMLButtonElement).disabled).toBe(false))
  click('Rename playlist')
  await waitFor(() => expect(boundary.invoke).toHaveBeenCalledWith('import_rename', { playlistId: 'playlist-1', name: 'My local name' }))
  await waitFor(() => expect((screen.getByRole('button', { name: 'Remove entry 1' }) as HTMLButtonElement).disabled).toBe(false))
  click('Remove entry 1')
  await waitFor(() => expect(boundary.invoke).toHaveBeenCalledWith('import_remove', { playlistId: 'playlist-1', entryId: 'entry-2' }))
  expect(collection.title).toBe('My mix')
})

it('reports local playlist editing failures without removing the entry', async () => {
  const original = boundary.invoke.getMockImplementation()!
  boundary.invoke.mockImplementation((name: string, args: unknown) => name === 'import_remove' ? Promise.reject('Local playlist could not be saved') : original(name, args))
  render(<ImportURL onComplete={() => {}} />)
  await preview(); click('Save playlist')
  fireEvent.click(await screen.findByText('Edit local playlist'))
  await waitFor(() => expect((screen.getByRole('button', { name: 'Remove entry 1' }) as HTMLButtonElement).disabled).toBe(false))
  click('Remove entry 1')
  expect(await screen.findByText('Local playlist could not be saved')).toBeDefined()
  expect(screen.getByRole('button', { name: 'Remove entry 1' })).toBeDefined()
})

it('removes a saved YouTube API key so public imports can run without a credential', async () => {
  render(<ImportURL onComplete={() => {}} />)
  fireEvent.click(screen.getByText('Metadata API access'))
  fireEvent.change(screen.getByLabelText('Metadata provider'), { target: { value: 'youtube' } })
  fireEvent.click(screen.getByText('Advanced: manual metadata credential'))
  const input = screen.getByLabelText('YouTube Data API key') as HTMLInputElement
  fireEvent.change(input, { target: { value: 'stale-key' } })
  click('Remove saved credential')
  await waitFor(() => expect(boundary.invoke).toHaveBeenCalledWith('import_credential', { provider: 'youtube', secret: '' }))
  expect(input.value).toBe('')
  expect(await screen.findByText('YouTube Music manual metadata credential removed.')).toBeDefined()
})

it('reports credential-removal failures without claiming the credential was removed', async () => {
  const original = boundary.invoke.getMockImplementation()!
  boundary.invoke.mockImplementation((name: string, args: unknown) => name === 'import_credential' ? Promise.reject('Credential store unavailable') : original(name, args))
  render(<ImportURL onComplete={() => {}} />)
  fireEvent.click(screen.getByText('Metadata API access'))
  fireEvent.click(screen.getByText('Advanced: manual metadata credential'))
  click('Remove saved credential')
  expect(await screen.findByRole('alert')).toHaveProperty('textContent', 'Credential store unavailable')
  expect(screen.queryByText(/manual metadata credential removed/)).toBeNull()
})
