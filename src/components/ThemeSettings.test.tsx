// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { SettingsView } from './SettingsView'
import type { Settings, ThemeSummary } from '../lib/ipc'

const boundary = vi.hoisted(() => ({ invoke: vi.fn() }))
vi.mock('@tauri-apps/api/core', () => ({ invoke: boundary.invoke }))
vi.mock('@tauri-apps/api/event', () => ({ listen: async () => () => {} }))
const original: ThemeSummary = { id: 'capsule', name: 'Capsule', polarity: 'dark', builtin: true, failures: [] }
const custom: ThemeSummary = { ...original, id: 'custom-new', name: 'My palette', builtin: false }
const doc = { raw: '', name: 'Original', colors: { ground: '#101010', panel: '#202020', ink: '#eeeeee', accent: '#ccccff' }, css: '--color-ground:#101010;', failures: [] }
let stored: Settings
let list: () => Promise<ThemeSummary[]>
let persist: (next: Settings) => Promise<void>
function deferred<T>() {
  let resolve!: (value: T) => void
  let reject!: (error: string) => void
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no })
  return { promise, resolve, reject }
}
const click = (name: string | RegExp) => fireEvent.click(screen.getByRole('button', { name }))
const saveButton = () => screen.getByRole('button', { name: 'Save as new theme' }) as HTMLButtonElement
async function editTheme() {
  render(<SettingsView />)
  fireEvent.click(await screen.findByRole('button', { name: 'Edit theme' }))
  fireEvent.change(await screen.findByLabelText('Theme name'), { target: { value: 'My palette' } })
  await waitFor(() => expect(saveButton().disabled).toBe(false))
}

beforeEach(() => {
  delete document.documentElement.dataset.glass
  stored = { source: 'local', onboarded: true, developer: false,
    appearance: { glass: 'none', theme: 'capsule' }, spotify: { client_id: '' },
    navidrome: { url: '', username: '' }, local: { folders: [] }, lyrics: { offset_ms: 0 },
    lastfm: { api_key: '', shared_secret: '' }, discord: { client_id: '', serve_art_from_server: false } }
  list = async () => [original, custom]
  persist = async next => { stored = structuredClone(next) }
  boundary.invoke.mockReset().mockImplementation(async (command: string, args: { settings?: Settings } = {}) => {
    switch (command) {
      case 'settings_get': return structuredClone(stored)
      case 'settings_set': return persist(args.settings!)
      case 'themes_list': return list()
      case 'themes_document': case 'themes_preview': return doc
      case 'themes_save': return 'custom-new'
      case 'lastfm_status': return { configured: false, linked: false, username: null }
    }
  })
})
afterEach(() => { cleanup(); delete document.documentElement.dataset.glass; document.documentElement.style.removeProperty('--color-accent') })

it.each([
  ['Solid', 'none'], ['Acrylic\\s*Blur', 'acrylic-chrome'], ['Acrylic \\(all\\)', 'acrylic-all'], ['Mica', 'mica'],
])('applies %s to the open UI only after a successful save and keeps custom colors', async (label, glass) => {
  stored.appearance.glass = glass === 'none' ? 'mica' : 'none'
  document.documentElement.dataset.glass = stored.appearance.glass
  document.documentElement.style.setProperty('--color-accent', '#aabbcc')
  const previous = stored.appearance.glass
  const pending = deferred<void>()
  persist = async next => { await pending.promise; stored = structuredClone(next) }
  render(<SettingsView />)
  await screen.findByRole('button', { name: /^Solid/ })
  click(new RegExp(`^${label}`))
  expect(document.documentElement.dataset.glass).toBe(previous)
  await act(async () => pending.resolve())
  await waitFor(() => expect(document.documentElement.dataset.glass).toBe(glass))
  expect(document.documentElement.style.getPropertyValue('--color-accent')).toBe('#aabbcc')
})

it('reports a material failure beside the controls and restores the applied selection', async () => {
  document.documentElement.dataset.glass = 'none'
  persist = async () => { throw 'Mica is unavailable on this version of Windows.' }
  render(<SettingsView />)
  await screen.findByRole('button', { name: /^Solid/ })
  click(/^Mica/)
  expect(await screen.findByRole('alert')).toHaveProperty('textContent', expect.stringContaining('Mica is unavailable'))
  expect(screen.getByRole('button', { name: /^Solid/ }).getAttribute('aria-pressed')).toBe('true')
  expect(document.documentElement.dataset.glass).toBe('none')
})

it('does not carry a failed material into the next queued source save', async () => {
  document.documentElement.dataset.glass = 'none'
  persist = async next => {
    if (next.appearance.glass === 'mica') throw 'Material failed'
    stored = structuredClone(next)
  }
  render(<SettingsView />)
  await screen.findByRole('button', { name: /^Solid/ })
  click(/^Mica/)
  click(/^YouTube Music/)
  await waitFor(() => expect(stored.source).toBe('youtube'))
  expect(stored.appearance.glass).toBe('none')
  expect(screen.getByRole('button', { name: /^Solid/ }).getAttribute('aria-pressed')).toBe('true')
  expect(document.documentElement.dataset.glass).toBe('none')
})

it('applies a saved theme to the latest settings after refreshing the theme list', async () => {
  await editTheme()
  const pending = deferred<ThemeSummary[]>()
  list = () => pending.promise
  click('Save as new theme')
  await waitFor(() => expect(boundary.invoke.mock.calls.filter(([name]) => name === 'themes_list')).toHaveLength(2))
  click(/^YouTube Music/)
  click(/^Mica/)
  await waitFor(() => expect(stored.appearance.glass).toBe('mica'))
  await act(async () => pending.resolve([original, custom]))
  await waitFor(() => expect(stored.appearance.theme).toBe('custom-new'))
  expect(stored.source).toBe('youtube')
  expect(stored.appearance.glass).toBe('mica')
})

it('keeps failed theme edits retryable without reverting unrelated queued edits', async () => {
  await editTheme()
  const pending = deferred<void>()
  persist = async next => {
    if (next.appearance.theme === 'custom-new') await pending.promise
    stored = structuredClone(next)
  }
  click('Save as new theme')
  await waitFor(() => expect(boundary.invoke).toHaveBeenCalledWith('settings_set', expect.objectContaining({ settings: expect.objectContaining({ appearance: { theme: 'custom-new', glass: 'none' } }) })))
  expect((screen.getByLabelText('Theme name') as HTMLInputElement).value).toBe('My palette')
  click(/^YouTube Music/)
  click(/^Mica/)
  await act(async () => pending.reject('Config write failed'))
  expect(await screen.findByRole('alert')).toHaveProperty('textContent', 'Config write failed')
  expect((screen.getByLabelText('Theme name') as HTMLInputElement).value).toBe('My palette')
  await waitFor(() => expect(stored.source).toBe('youtube'))
  expect(stored.appearance).toEqual({ theme: 'capsule', glass: 'mica' })
  expect(screen.getByRole('button', { name: /^Capsule/ }).className).toContain('card-on')
  persist = async next => { stored = structuredClone(next) }
  click('Save as new theme')
  await screen.findByText('New theme saved and applied.')
  expect(stored.source).toBe('youtube')
  expect(stored.appearance).toEqual({ theme: 'custom-new', glass: 'mica' })
  expect(screen.queryByLabelText('Theme name')).toBeNull()
})

it.each([['Mica', 'mica'], ['Solid', 'none']])('does not let a queued %s selection and source edit undo a successful theme application', async (label, glass) => {
  await editTheme()
  const pending = deferred<void>()
  persist = async next => {
    if (next.appearance.theme === 'custom-new' && next.appearance.glass === 'none') await pending.promise
    stored = structuredClone(next)
  }
  click('Save as new theme')
  await waitFor(() => expect(boundary.invoke).toHaveBeenCalledWith('settings_set', expect.anything()))
  click(/^YouTube Music/)
  click(new RegExp(`^${label}`))
  await act(async () => pending.resolve())
  await screen.findByText('New theme saved and applied.')
  await waitFor(() => expect(stored.source).toBe('youtube'))
  expect(stored.appearance.glass).toBe(glass)
  expect(stored.source).toBe('youtube')
  expect(stored.appearance.theme).toBe('custom-new')
  expect(screen.getByRole('button', { name: /^My palette/ }).className).toContain('card-on')
})

it('respects an explicit re-selection while the saved theme list is pending', async () => {
  await editTheme()
  const pending = deferred<ThemeSummary[]>()
  list = () => pending.promise
  click('Save as new theme')
  await waitFor(() => expect(boundary.invoke.mock.calls.filter(([name]) => name === 'themes_list')).toHaveLength(2))
  click(/^Capsule/)
  await act(async () => pending.resolve([original, custom]))
  await screen.findByText('Theme saved. Your current selection was kept.')
  expect(stored.appearance.theme).toBe('capsule')
  expect(boundary.invoke.mock.calls.filter(([name, args]) => name === 'settings_set' && args.settings.appearance.theme === 'custom-new')).toHaveLength(0)
})

it('does not apply a saved theme after leaving settings during its list refresh', async () => {
  const view = render(<SettingsView />)
  fireEvent.click(await screen.findByRole('button', { name: 'Edit theme' }))
  await waitFor(() => expect(saveButton().disabled).toBe(false))
  const pending = deferred<ThemeSummary[]>()
  list = () => pending.promise
  click('Save as new theme')
  await waitFor(() => expect(boundary.invoke.mock.calls.filter(([name]) => name === 'themes_list')).toHaveLength(2))
  view.unmount()
  await act(async () => pending.resolve([original, custom]))
  expect(boundary.invoke.mock.calls.filter(([name]) => name === 'settings_set')).toHaveLength(0)
})
