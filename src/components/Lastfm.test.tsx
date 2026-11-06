// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { useState } from 'react'
import { afterEach, expect, it, vi } from 'vitest'
import type { Settings } from '../lib/ipc'
import { LastfmFields } from './fields'

const invoke = vi.hoisted(() => vi.fn())
vi.mock('@tauri-apps/api/core', () => ({ invoke }))
vi.mock('@tauri-apps/api/event', () => ({ listen: async () => () => {} }))
afterEach(() => { cleanup(); invoke.mockReset() })

it('enables Last.fm connection after saving both credentials in the open settings view', async () => {
  let finishSave!: () => void
  const pending = new Promise<void>(resolve => { finishSave = resolve })
  let configured = false
  invoke.mockImplementation(async (command: string) => {
    if (command === 'lastfm_status') return { configured, linked: false, username: null }
  })

  function SettingsForm() {
    const [lastfm, setLastfm] = useState({ api_key: '', shared_secret: '' })
    return <LastfmFields draft={{ lastfm } as Settings} edit={async patch => {
      if (!patch.lastfm) return
      setLastfm(patch.lastfm)
      if (patch.lastfm.api_key && patch.lastfm.shared_secret) {
        await pending
        configured = true
      }
    }} />
  }

  render(<SettingsForm />)
  const connect = await screen.findByRole<HTMLButtonElement>('button', { name: 'Connect account' })
  expect(connect.disabled).toBe(true)
  fireEvent.change(screen.getByLabelText('API key'), { target: { value: 'test-key' } })
  fireEvent.change(screen.getByLabelText('Shared secret'), { target: { value: 'test-secret' } })
  expect(connect.disabled).toBe(true)
  await act(async () => finishSave())
  await waitFor(() => expect(connect.disabled).toBe(false))
  fireEvent.click(connect)
  expect(invoke).toHaveBeenCalledWith('lastfm_connect')
})
