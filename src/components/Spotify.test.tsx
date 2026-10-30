// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { useState } from 'react'
import { SpotifySetup } from './SpotifySetup'
import { Onboarding } from './Onboarding'
import { SettingsView } from './SettingsView'
import { Readout } from './Readout'
import type { Settings } from '../lib/ipc'

const boundary = vi.hoisted(() => ({
  invoke: vi.fn(),
  handlers: new Map<string, Set<(event: { payload: unknown }) => void>>(),
  win: { isMaximized: async () => false, onResized: async () => () => {}, minimize: async () => {}, toggleMaximize: async () => {}, close: async () => {} },
}))
vi.mock('@tauri-apps/api/core', () => ({ invoke: boundary.invoke, convertFileSrc: (id: string) => id }))
vi.mock('@tauri-apps/api/event', () => ({ listen: async (event: string, handler: (event: { payload: unknown }) => void) => {
  const listeners = boundary.handlers.get(event) ?? new Set()
  listeners.add(handler)
  boundary.handlers.set(event, listeners)
  return () => listeners.delete(handler)
} }))
vi.mock('@tauri-apps/api/window', () => ({ getCurrentWindow: () => boundary.win }))

const clientId = '0123456789abcdef0123456789abcdef'
let stored: Settings
let session: { configured: boolean; authenticated: boolean; display_name: string | null; playback_supported: boolean }
let connect: () => Promise<void>
let disconnect: () => Promise<void>
const counts = { songs: 0, albums: 0, playlists: 0, artists: 0 }
function emit(name: string, payload: unknown) {
  for (const handler of boundary.handlers.get(name) ?? []) handler({ payload })
}
function deferred() {
  let resolve!: () => void
  let reject!: (reason: string) => void
  const promise = new Promise<void>((yes, no) => { resolve = yes; reject = no })
  return { promise, resolve, reject }
}
const click = (name: string | RegExp) => fireEvent.click(screen.getByRole('button', { name }))
const connected = () => ({ ...session, configured: true, authenticated: true, display_name: 'Deniz' })
function MetadataSetup() {
  const [draft, setDraft] = useState(stored)
  return <SpotifySetup draft={draft} edit={async patch => {
    const next = { ...draft, ...patch }
    setDraft(next)
    await boundary.invoke('settings_set', { settings: next })
  }} />
}

beforeEach(() => {
  stored = {
    source: 'local', onboarded: true, developer: false,
    spotify: { client_id: clientId }, appearance: { glass: 'none', theme: 'dark' },
    navidrome: { url: '', username: '' }, local: { folders: [] }, lyrics: { offset_ms: 0 },
    lastfm: { api_key: '', shared_secret: '' }, discord: { client_id: '', serve_art_from_server: false },
  }
  session = { configured: true, authenticated: false, display_name: null, playback_supported: true }
  connect = async () => { session = connected(); emit('spotify://changed', session) }
  disconnect = async () => { session = { ...session, authenticated: false, display_name: null }; emit('spotify://changed', session) }
  boundary.handlers.clear()
  boundary.invoke.mockReset().mockImplementation(async (command: string, args?: { settings?: Settings; clientId?: string }) => {
    switch (command) {
      case 'settings_get': return structuredClone(stored)
      case 'settings_set': {
        const changed = stored.source !== args!.settings!.source
        stored = structuredClone(args!.settings!)
        if (changed) emit('library://updated', counts)
        return
      }
      case 'spotify_status': return { ...session }
      case 'spotify_connect':
        return connect()
      case 'spotify_disconnect': return disconnect()
      case 'auth_status': return { authenticated: true, storefront: 'us' }
      case 'lastfm_status': return { configured: false, linked: false, username: null }
      case 'navidrome_status': return { configured: false, url: '', username: '', insecure: false }
      case 'themes_list': case 'library_songs': case 'library_albums': case 'library_artists': case 'library_playlists': return []
      case 'library_counts': return counts
      case 'player_snapshot': return { status: 'idle', queue: [], index: null, position_ms: 0, volume: 100, shuffle: false, repeat: 'off' }
      default: return
    }
  })
  vi.stubGlobal('ResizeObserver', class { observe() {} unobserve() {} disconnect() {} })
})
afterEach(() => { cleanup(); vi.useRealTimers(); vi.unstubAllGlobals() })

it('requires a Client ID and saves the trimmed ID before waiting for browser OAuth', async () => {
  stored.spotify.client_id = ''
  const pending = deferred()
  connect = () => pending.promise
  render(<MetadataSetup />)
  const input = await screen.findByLabelText('Spotify Client ID')
  expect((screen.getByRole('button', { name: 'Connect Spotify' }) as HTMLButtonElement).disabled).toBe(true)
  fireEvent.change(input, { target: { value: `  ${clientId}  ` } })
  click('Connect Spotify')
  await waitFor(() => expect(boundary.invoke).toHaveBeenCalledWith('spotify_connect', { clientId }))
  expect(stored.spotify.client_id).toBe(clientId)
  expect((screen.getByRole('button', { name: /waiting for spotify/i }) as HTMLButtonElement).disabled).toBe(true)
  expect(boundary.invoke).not.toHaveBeenCalledWith('auth_show_login')
  await act(async () => { session = connected(); pending.resolve() })
  expect(await screen.findByText(/connected as Deniz/i)).toBeDefined()
})

it('restores an existing Spotify session and signs it out', async () => {
  session = connected()
  render(<MetadataSetup />)
  expect(await screen.findByText(/connected as Deniz/i)).toBeDefined()
  click('Sign out of Spotify')
  await waitFor(() => expect(screen.queryByText(/connected as Deniz/i)).toBeNull())
  expect(boundary.invoke).toHaveBeenCalledWith('spotify_disconnect')
  expect(await screen.findByRole('button', { name: 'Connect Spotify' })).toBeDefined()
})

it('shows sign-out errors without claiming the session was disconnected', async () => {
  session = connected()
  disconnect = async () => { throw new Error('Could not clear credentials') }
  render(<MetadataSetup />)
  fireEvent.click(await screen.findByRole('button', { name: 'Sign out of Spotify' }))
  expect(await screen.findByRole('alert')).toHaveProperty('textContent', 'Could not clear credentials')
  expect(screen.getByText(/connected as Deniz/i)).toBeDefined()
})

it('allows retry after OAuth cancellation and listens for Spotify errors', async () => {
  connect = async () => { throw 'Spotify sign-in was cancelled' }
  render(<MetadataSetup />)
  await screen.findByLabelText('Spotify Client ID')
  click('Connect Spotify')
  expect(await screen.findByRole('alert')).toHaveProperty('textContent', 'Spotify sign-in was cancelled')
  expect((screen.getByRole('button', { name: 'Connect Spotify' }) as HTMLButtonElement).disabled).toBe(false)
  await act(async () => emit('spotify://error', 'Account is not allowlisted'))
  expect(screen.getByRole('alert')).toHaveProperty('textContent', 'Account is not allowlisted')
})

it('releases pending sign-in after the OAuth timeout', async () => {
  connect = () => new Promise(() => {})
  render(<MetadataSetup />)
  await screen.findByLabelText('Spotify Client ID')
  vi.useFakeTimers()
  await act(async () => click('Connect Spotify'))
  await act(async () => { vi.advanceTimersByTime(200_000) })
  expect(screen.getByRole('alert').textContent).toMatch(/timed out/i)
  expect((screen.getByRole('button', { name: 'Connect Spotify' }) as HTMLButtonElement).disabled).toBe(false)
})

it('keeps metadata OAuth separate from the playback source and sync', async () => {
  stored.source = 'youtube'
  render(<MetadataSetup />)
  click('Connect Spotify')
  await screen.findByText(/connected as Deniz/i)
  expect(stored.source).toBe('youtube')
  expect(boundary.invoke).not.toHaveBeenCalledWith('library_sync')
  expect(screen.queryByText(/Web Playback SDK/)).toBeNull()
})

it('ignores OAuth completion after the metadata form is unmounted', async () => {
  const pending = deferred()
  connect = () => pending.promise
  const view = render(<MetadataSetup />)
  click('Connect Spotify')
  await waitFor(() => expect(boundary.invoke).toHaveBeenCalledWith('spotify_connect', { clientId }))
  view.unmount()
  await act(async () => pending.reject('Stale OAuth failure'))
  expect(screen.queryByText('Stale OAuth failure')).toBeNull()
})

it('does not launch OAuth after a failed settings save', async () => {
  const original = boundary.invoke.getMockImplementation()!
  boundary.invoke.mockImplementation(async (name: string, args: unknown) => {
    if (name === 'settings_set') throw 'Disk full'
    return original(name, args)
  })
  render(<MetadataSetup />)
  click('Connect Spotify')
  expect(await screen.findByRole('alert')).toHaveProperty('textContent', 'Disk full')
  expect(boundary.invoke).not.toHaveBeenCalledWith('spotify_connect', expect.anything())
})

it('does not let a late startup snapshot overwrite a newer metadata sign-out', async () => {
  const pending = deferred()
  const original = boundary.invoke.getMockImplementation()!
  boundary.invoke.mockImplementation(async (name: string, args: unknown) => {
    if (name === 'spotify_status') { await pending.promise; return connected() }
    return original(name, args)
  })
  render(<MetadataSetup />)
  await waitFor(() => expect(boundary.invoke).toHaveBeenCalledWith('spotify_status'))
  await act(async () => emit('spotify://changed', session))
  await act(async () => pending.resolve())
  expect(screen.queryByText(/connected as Deniz/i)).toBeNull()
})

it.each(['youtube', 'local', 'navidrome'] as const)('omits obsolete Apple diagnostics for %s', source => {
  render(<Readout state={null} counts={null} progress={null} source={source} />)
  expect(screen.queryByText('engine ok')).toBeNull()
})

it('offers supported playback sources in onboarding without Apple Music', async () => {
  stored.source = 'local'
  render(<Onboarding onFinished={() => {}} />)
  await act(async () => {})
  click('Get started')
  expect(screen.queryByRole('button', { name: /^Apple Music/ })).toBeNull()
  expect(screen.queryByRole('button', { name: /^Spotify/ })).toBeNull()
  for (const name of [/^Local files/, /^Navidrome/, /^YouTube Music/]) {
    expect(screen.getByRole('button', { name })).toBeDefined()
  }
})

it('removes Apple playback setup while preserving Apple metadata import credentials', async () => {
  render(<SettingsView />)
  await screen.findByLabelText('Metadata provider')
  expect(screen.queryByRole('button', { name: /^Apple Music/ })).toBeNull()
  expect(screen.queryByRole('button', { name: /^Spotify/ })).toBeNull()
  expect(screen.queryByRole('button', { name: /Sign in to Apple Music/ })).toBeNull()
  const provider = screen.getByLabelText('Metadata provider')
  fireEvent.change(provider, { target: { value: 'apple_music' } })
  expect((provider as HTMLSelectElement).value).toBe('apple_music')
  expect(screen.getByRole('option', { name: 'Apple Music' })).toBeDefined()
})

it('offers Spotify browser sign-in only under import metadata access', async () => {
  stored.source = 'youtube'
  render(<SettingsView />)
  await screen.findByText(/Ready to play/)
  expect(screen.queryByRole('button', { name: /^Spotify/ })).toBeNull()
  fireEvent.click(screen.getByText('Metadata API access'))
  const input = await screen.findByLabelText('Spotify Client ID')
  fireEvent.change(input, { target: { value: clientId } })
  click('Connect Spotify')
  await screen.findByText(/connected as Deniz/i)
  expect(stored.source).toBe('youtube')
  expect(boundary.invoke).not.toHaveBeenCalledWith('library_sync')
})
