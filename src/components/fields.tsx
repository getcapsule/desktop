import { useEffect, useRef, useState } from 'react'
import { YoutubeSetup } from './YoutubeSetup'
import { ThemeEditor } from './ThemeEditor'
import { open } from '@tauri-apps/plugin-dialog'

import {
  lastfm as lastfmIpc,
  navidrome as navidromeIpc,
  on,
  settings as settingsIpc,
  themes as themesIpc,
  type LastfmStatus,
  type NavidromeStatus,
  type Settings,
  type Source,
  type ThemeSummary,
} from '../lib/ipc'
import { isInsecureUrl } from '../lib/navidrome'
import { credentialStore } from '../lib/platform'

interface SourceInfo {
  id: Source
  name: string
  blurb: string
}

export const SOURCES: SourceInfo[] = [
  { id: 'local', name: 'Local files', blurb: 'Point at a folder on this machine' },
  { id: 'navidrome', name: 'Navidrome', blurb: 'Connect to your own server' },
  { id: 'youtube', name: 'YouTube Music', blurb: 'Play public tracks and playlists' },
  { id: 'soundcloud', name: 'SoundCloud', blurb: 'Search and play public tracks' },
]

export const GLASS = [
  { id: 'none', name: 'Solid', blurb: 'No transparency' },
  { id: 'acrylic-chrome', name: 'Acrylic', blurb: 'Blur behind the chrome' },
  { id: 'acrylic-all', name: 'Acrylic (all)', blurb: 'Blur behind everything' },
  { id: 'mica', name: 'Mica', blurb: 'Tinted from your wallpaper' },
]

export function Field({
  label,
  value,
  placeholder,
  onChange,
  type = 'text',
  onEnter,
}: {
  label: string
  value: string
  placeholder?: string
  onChange: (v: string) => void
  type?: 'text' | 'password'
  onEnter?: () => void
}) {
  return (
    <label className="block">
      <span className="label">{label}</span>
      <input
        value={value}
        type={type}
        placeholder={placeholder}
        onChange={(e) => onChange(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter' && onEnter) onEnter()
        }}
        className="mt-1 w-full rounded border border-rule bg-ground px-2.5 py-1.5 text-xs text-ink outline-none placeholder:text-muted focus:border-muted"
      />
    </label>
  )
}

interface NavidromeConnectResult {
  ok: boolean
  settings: Settings | null
}

export function useNavidromeConnect() {
  const [password, setPassword] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const connect = async (url: string, username: string): Promise<NavidromeConnectResult> => {
    if (busy) return { ok: false, settings: null }
    setBusy(true)
    setError(null)
    try {
      await navidromeIpc.connect(url.trim(), username.trim(), password)
      setPassword('')
    } catch (e) {
      setError(typeof e === 'string' ? e : 'Could not connect')
      return { ok: false, settings: null }
    } finally {
      setBusy(false)
    }

    try {
      return { ok: true, settings: await settingsIpc.get() }
    } catch {
      return { ok: true, settings: null }
    }
  }

  return { password, setPassword, busy, error, setError, connect }
}

export function NavidromeFields({
  url,
  username,
  password,
  onUrl,
  onUsername,
  onPassword,
  onSubmit,
  passwordPlaceholder,
}: {
  url: string
  username: string
  password: string
  onUrl: (v: string) => void
  onUsername: (v: string) => void
  onPassword: (v: string) => void
  onSubmit: () => void
  passwordPlaceholder?: string
}) {
  return (
    <div className="space-y-3">
      <Field
        label="Server URL"
        placeholder="https://music.example.com"
        value={url}
        onChange={onUrl}
        onEnter={onSubmit}
      />
      <Field label="Username" value={username} onChange={onUsername} onEnter={onSubmit} />
      <Field
        label="Password"
        type="password"
        placeholder={passwordPlaceholder}
        value={password}
        onChange={onPassword}
        onEnter={onSubmit}
      />
      {isInsecureUrl(url) && (
        <p className="text-[11px] leading-5 text-warn">
          This connection is not encrypted. Anyone on the network can read your login.
        </p>
      )}
      <p className="text-[11px] leading-5 text-muted">
        The password is checked against your server, then kept in {credentialStore()} - never in
        the settings file.
      </p>
    </div>
  )
}

export function Cards<T extends string>({
  options,
  value,
  onChange,
}: {
  options: { id: T; name: string; blurb: string }[]
  value: T
  onChange: (id: T) => void
}) {
  return (
    <div className="grid grid-cols-2 gap-2">
      {options.map((o) => (
        <button
          key={o.id}
          onClick={() => onChange(o.id)}
          aria-pressed={value === o.id}
          className={`choice-card rounded border px-3.5 py-3 text-left transition-colors ${
            value === o.id
              ? 'card-on border-accent bg-accent/8'
              : 'border-rule hover:border-muted'
          }`}
        >
          <span className="flex items-center gap-2 text-[13px] text-ink">
            {o.name}
          </span>
          <span className="mt-0.5 block text-[11px] text-muted">{o.blurb}</span>
        </button>
      ))}
    </div>
  )
}

export function Folders({
  folders,
  onChange,
}: {
  folders: string[]
  onChange: (folders: string[]) => void
}) {
  const [failed, setFailed] = useState<string | null>(null)

  const add = async () => {
    setFailed(null)
    try {
      const picked = await open({ directory: true, multiple: true, title: 'Choose music folders' })
      if (!picked) return
      const chosen = Array.isArray(picked) ? picked : [picked]
      onChange([...folders, ...chosen.filter((p) => !folders.includes(p))])
    } catch (e) {
      setFailed(e instanceof Error ? e.message : String(e))
    }
  }

  return (
    <div>
      {folders.length > 0 && (
        <ul className="mb-3 space-y-1">
          {folders.map((f) => (
            <li
              key={f}
              className="flex items-center gap-2 rounded border border-rule px-2.5 py-1.5 text-xs text-ink"
            >
              <span className="min-w-0 flex-1 truncate" title={f}>
                {f}
              </span>
              <button
                onClick={() => onChange(folders.filter((x) => x !== f))}
                className="label shrink-0 px-1 text-muted hover:text-ink"
                aria-label={`Remove ${f}`}
              >
                remove
              </button>
            </li>
          ))}
        </ul>
      )}

      <button
        onClick={() => void add()}
        className="rounded border border-rule px-2.5 py-1.5 text-[11px] text-muted transition-colors hover:border-accent hover:text-ink"
      >
        {folders.length ? 'Add another folder' : 'Choose a folder'}
      </button>

      <p className="mt-3 text-[11px] text-muted">
        Scanned for FLAC, ALAC, MP3 and AAC. Subfolders are included.
      </p>
      {failed && <p className="mt-2 text-[11px] text-crit">Could not open the picker: {failed}</p>}
    </div>
  )
}

/**
 * Server details plus the password, which is the one field that cannot live in
 * `draft`: settings are written on every keystroke here, and a password must
 * never reach `config.toml`. It goes straight to the connect command, which
 * verifies it against the server before storing it in the OS credential store.
 */
function NavidromeSetup({
  draft,
  edit,
}: {
  draft: Settings
  edit: (patch: Partial<Settings>) => void
}) {
  const { password, setPassword, busy, error, setError, connect } = useNavidromeConnect()
  const [done, setDone] = useState(false)
  const [status, setStatus] = useState<NavidromeStatus | null>(null)

  useEffect(() => {
    void navidromeIpc.status().then(setStatus)
  }, [])

  const ready =
    !busy && draft.navidrome.url.trim() !== '' && draft.navidrome.username.trim() !== ''

  const submit = async () => {
    if (!ready) return
    setDone(false)
    const { ok, settings } = await connect(draft.navidrome.url, draft.navidrome.username)
    if (!ok) return
    if (settings) edit({ navidrome: settings.navidrome })
    setDone(true)
    void navidromeIpc.status().then(setStatus)
  }

  return (
    <div className="space-y-3">
      <NavidromeFields
        url={draft.navidrome.url}
        username={draft.navidrome.username}
        password={password}
        onUrl={(v) => {
          setError(null)
          setDone(false)
          edit({ navidrome: { ...draft.navidrome, url: v } })
        }}
        onUsername={(v) => {
          setError(null)
          setDone(false)
          edit({ navidrome: { ...draft.navidrome, username: v } })
        }}
        onPassword={setPassword}
        onSubmit={() => void submit()}
        passwordPlaceholder={status?.configured ? 'Stored - enter to change' : ''}
      />

      <div className="flex items-center gap-3 pt-1">
        <button
          onClick={() => void submit()}
          disabled={!ready}
          className="rounded border border-rule px-2.5 py-1.5 text-[11px] text-muted transition-colors hover:border-accent hover:text-ink disabled:cursor-not-allowed disabled:opacity-40 disabled:hover:border-rule disabled:hover:text-muted"
        >
          {busy ? 'Connecting…' : status?.configured ? 'Reconnect' : 'Connect'}
        </button>
        {status?.configured && !done && !error && (
          <span className="label text-muted">connected as {status.username}</span>
        )}
      </div>

      {done && (
        <p className="text-[11px] leading-5" style={{ color: 'var(--color-ok)' }}>
          Connected. Syncing your library…
        </p>
      )}
      {error && (
        <p className="text-[11px] leading-5" style={{ color: 'var(--color-crit)' }}>
          {error}
        </p>
      )}
    </div>
  )
}

export function SourceSetup({
  draft,
  edit,
}: {
  draft: Settings
  edit: (patch: SettingsPatch) => Promise<void>
}) {
  if (draft.source === 'youtube') return <YoutubeSetup />

  if (draft.source === 'soundcloud') {
    return (
      <p className="text-[11px] leading-5 text-muted">
        Search and play public SoundCloud tracks. No account, token or API key is needed.
      </p>
    )
  }

  if (draft.source === 'navidrome') {
    return <NavidromeSetup draft={draft} edit={edit} />
  }

  return (
    <Folders folders={draft.local.folders} onChange={(folders) => edit({ local: { folders } })} />
  )
}

export type SettingsPatch = Omit<Partial<Settings>, 'appearance'> & { appearance?: Partial<Settings['appearance']> }

export function ThemePicker({
  draft,
  edit,
}: {
  draft: Settings
  edit: (patch: SettingsPatch) => Promise<void>
}) {
  const [all, setAll] = useState<ThemeSummary[]>([])
  const selection = useRef(0)
  const mounted = useRef(true)

  useEffect(() => {
    mounted.current = true
    void themesIpc.list().then(setAll)
    return () => { mounted.current = false }
  }, [])

  const active = all.find((t) => t.id === draft.appearance.theme)

  if (all.length === 0) return null

  return (
    <div className="space-y-3">
      <Cards
        options={all.map((t) => ({
          id: t.id,
          name: t.name,
          blurb: t.builtin ? t.polarity : `${t.polarity} · yours`,
        }))}
        value={draft.appearance.theme}
        onChange={(theme) => { selection.current++; void edit({ appearance: { theme } }) }}
      />
      {active && active.failures.length > 0 && (
        <div className="space-y-1">
          <p className="text-[11px] leading-5 text-warn">
            {active.failures.length} colour pair
            {active.failures.length === 1 ? '' : 's'} below AA (4.5:1)
          </p>
          <ul className="space-y-0.5">
            {active.failures.map((f) => (
              <li key={`${f.fg}-${f.bg}`} className="label text-muted">
                {f.fg} on {f.bg} — {f.ratio.toFixed(2)}:1
              </li>
            ))}
          </ul>
        </div>
      )}
      <ThemeEditor activeId={draft.appearance.theme} onSaved={async (id) => {
        const revision = selection.current
        const next = await themesIpc.list()
        if (!mounted.current) return false
        setAll(next)
        if (revision !== selection.current) return false
        await edit({ appearance: { theme: id } })
        return true
      }} />
    </div>
  )
}

export function LastfmFields({
  draft,
  edit,
}: {
  draft: Settings
  edit: (patch: Partial<Settings>) => Promise<void>
}) {
  const [status, setStatus] = useState<LastfmStatus | null>(null)
  const [note, setNote] = useState<string | null>(null)
  const [waiting, setWaiting] = useState(false)
  const [credentialSave, setCredentialSave] = useState<'ready' | 'saving' | 'failed'>('ready')
  const request = useRef(0)

  useEffect(() => {
    const initial = ++request.current
    void lastfmIpc.status().then(result => {
      if (request.current === initial) setStatus(result)
    }).catch(error => {
      if (request.current === initial) setNote(String(error))
    })
    const subs = [
      on('lastfm://linked', (s) => {
        setStatus(s)
        setWaiting(false)
        setNote(null)
      }),
      on('lastfm://failed', (reason) => {
        setWaiting(false)
        setNote(reason)
      }),
    ]
    return () => {
      ++request.current
      for (const s of subs) void s.then((un) => un())
    }
  }, [])

  const saveCredentials = async (credentials: Settings['lastfm']) => {
    const current = ++request.current
    setCredentialSave('saving')
    setNote(null)
    try {
      await edit({ lastfm: credentials })
      if (request.current !== current) return
      const result = await lastfmIpc.status()
      if (request.current === current) {
        setStatus(result)
        setCredentialSave('ready')
      }
    } catch (error) {
      if (request.current === current) {
        setCredentialSave('failed')
        setNote(String(error))
      }
    }
  }

  const link = async () => {
    setNote(null)
    setWaiting(true)
    try {
      await lastfmIpc.connect()
    } catch (e) {
      setWaiting(false)
      setNote(typeof e === 'string' ? e : 'Could not reach Last.fm')
    }
  }

  const unlink = async () => {
    setNote(null)
    try {
      await lastfmIpc.disconnect()
    } catch (e) {
      setNote(typeof e === 'string' ? e : 'Could not sign out')
    }
  }

  const saving = credentialSave === 'saving'
  const canConnect = credentialSave === 'ready' && status?.configured
  let hint = 'Add a key and secret above, then connect your account.'
  if (saving) hint = 'Saving Last.fm credentials…'
  else if (credentialSave === 'failed') hint = 'Edit the credentials to try again.'
  else if (status?.linked) hint = 'Plays are sent straight to Last.fm, so local files count too.'
  else if (waiting) hint = 'Approve Capsule in the window that just opened.'
  else if (canConnect) hint = 'Connect your account to send plays to Last.fm.'

  return (
    <div className="space-y-3">
      <Field
        label="API key"
        value={draft.lastfm.api_key}
        onChange={value => void saveCredentials({ ...draft.lastfm, api_key: value })}
      />
      <Field
        label="Shared secret"
        type="password"
        value={draft.lastfm.shared_secret}
        onChange={value => void saveCredentials({ ...draft.lastfm, shared_secret: value })}
      />
      <div className="space-y-2 pt-1">
        <div className="flex items-center gap-3">
          <button
            onClick={() => void (status?.linked ? unlink() : link())}
            disabled={saving || waiting || !(status?.linked || canConnect)}
            className="rounded border border-rule px-2.5 py-1.5 text-[11px] text-muted transition-colors hover:border-accent hover:text-ink disabled:cursor-not-allowed disabled:opacity-40 disabled:hover:border-rule disabled:hover:text-muted"
          >
            {status?.linked ? 'Sign out' : waiting ? 'Waiting…' : 'Connect account'}
          </button>
          {status?.linked && status.username && (
            <span className="label text-muted">scrobbling as {status.username}</span>
          )}
        </div>
        <p className="text-[11px] leading-5 text-muted">{hint}</p>
        {note && <p role="alert" className="text-[11px] leading-5 text-crit">{note}</p>}
      </div>
    </div>
  )
}

export function DiscordFields({
  draft,
  edit,
}: {
  draft: Settings
  edit: (patch: Partial<Settings>) => void
}) {
  return (
    <div className="space-y-4">
      <Field
        label="Client ID"
        value={draft.discord.client_id}
        onChange={(v) => edit({ discord: { ...draft.discord, client_id: v } })}
      />
      <Switch
        label="Serve cover art from your server"
        note="Discord fetches the image itself, so your server must be reachable from the internet. The link it receives also works for the rest of your library - leave this off unless you are comfortable with that."
        on={draft.discord.serve_art_from_server}
        onChange={(v) => edit({ discord: { ...draft.discord, serve_art_from_server: v } })}
      />
      {!draft.discord.serve_art_from_server && (
        <p className="text-[11px] leading-5 text-muted">
          Cover art comes from Last.fm instead, which needs the API key above and only finds
          albums Last.fm knows.
        </p>
      )}
    </div>
  )
}

export function Switch({
  label,
  note,
  on,
  onChange,
}: {
  label: string
  note?: string
  on: boolean
  onChange: (v: boolean) => void
}) {
  return (
    <div className="flex items-start justify-between gap-4">
      <div className="min-w-0">
        <div className="text-[13px] text-ink">{label}</div>
        {note && <p className="mt-0.5 text-[11px] leading-5 text-muted">{note}</p>}
      </div>
      <button
        role="switch"
        aria-checked={on}
        aria-label={label}
        onClick={() => onChange(!on)}
        className={`mt-0.5 h-5 w-9 shrink-0 rounded-full border transition-colors ${
          on ? 'border-accent bg-accent/30' : 'border-rule bg-ground'
        }`}
      >
        <span
          className={`block size-3.5 rounded-full transition-transform ${
            on ? 'translate-x-4 bg-accent' : 'translate-x-0.5 bg-muted'
          }`}
        />
      </button>
    </div>
  )
}
