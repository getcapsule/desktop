import { useEffect, useRef, useState } from 'react'

import { on, settings, spotify, type Settings, type SpotifyStatus } from '../lib/ipc'

const OAUTH_TIMEOUT_MS = 195_000
const buttonClass = 'rounded border border-rule px-2.5 py-1.5 text-[11px] text-muted transition-colors hover:border-accent hover:text-ink disabled:cursor-not-allowed disabled:opacity-40'

/** A public Client ID identifies the user's developer app; OAuth stays in Rust. */
export function SpotifySetup({
  draft,
  edit,
  onConnected,
  onContinue,
}: {
  draft: Settings
  edit: (patch: Partial<Settings>) => void | Promise<void>
  onConnected?: () => void
  onContinue?: () => void
}) {
  const [status, setStatus] = useState<SpotifyStatus | null>(null)
  const [busy, setBusy] = useState<'connect' | 'disconnect' | null>(null)
  const [error, setError] = useState<string | null>(null)
  const mounted = useRef(false)
  const attempt = useRef(0)
  const pending = useRef<'connect' | 'disconnect' | null>(null)
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined)
  const didConnect = useRef(false)
  const oauthStarted = useRef(false)
  const listenersReady = useRef<Promise<unknown>>(Promise.resolve())
  const connectedCallback = useRef(onConnected)
  connectedCallback.current = onConnected

  const acceptStatus = (next: SpotifyStatus) => {
    setStatus(next)
    if (next.authenticated && pending.current === 'connect' && oauthStarted.current && !didConnect.current) {
      didConnect.current = true
      connectedCallback.current?.()
    }
  }

  useEffect(() => {
    mounted.current = true
    let revision = 0
    const subs = [
      on('spotify://changed', (next) => {
        if (!mounted.current) return
        revision++
        acceptStatus(next)
        if (next.authenticated) setError(null)
      }),
      on('spotify://error', (reason) => {
        if (mounted.current) setError(reason)
      }),
    ]
    // Subscribe before reading so a startup snapshot cannot overwrite a newer event.
    listenersReady.current = Promise.all(subs)
    void listenersReady.current.then(async () => {
      const seen = revision
      const next = await spotify.status()
      if (mounted.current && revision === seen) setStatus(next)
    }).catch((reason) => {
      if (mounted.current) setError(String(reason))
    })
    return () => {
      mounted.current = false
      attempt.current++
      clearTimeout(timer.current)
      for (const sub of subs) void sub.then((unlisten) => unlisten()).catch(() => {})
    }
  }, [])

  const connect = async () => {
    const clientId = draft.spotify.client_id.trim()
    if (pending.current || !clientId) return
    const current = ++attempt.current
    const active = () => mounted.current && current === attempt.current
    pending.current = 'connect'
    didConnect.current = false
    oauthStarted.current = false
    setBusy('connect')
    setError(null)
    try {
      await listenersReady.current
      if (!active()) return
      const patch = { spotify: { client_id: clientId } }
      // Wait for settings-view writes as well as persisting the onboarding draft.
      const saved = edit(patch)
      if (saved) await saved
      else await settings.set({ ...draft, ...patch })
      if (!active()) return
      timer.current = setTimeout(() => {
        if (!active()) return
        attempt.current++
        pending.current = null
        setBusy(null)
        setError('Spotify sign-in timed out. Close the browser tab and try again.')
      }, OAUTH_TIMEOUT_MS)
      oauthStarted.current = true
      await spotify.connect(clientId)
      if (!active()) return
      const next = await spotify.status()
      if (!active()) return
      acceptStatus(next)
      if (!next.authenticated) setError('Spotify sign-in did not complete. Try again.')
    } catch (reason) {
      if (active()) setError(reason instanceof Error ? reason.message : String(reason))
    } finally {
      if (active()) {
        clearTimeout(timer.current)
        pending.current = null
        setBusy(null)
      }
    }
  }

  const disconnect = async () => {
    if (pending.current) return
    const current = ++attempt.current
    const active = () => mounted.current && current === attempt.current
    pending.current = 'disconnect'
    setBusy('disconnect')
    setError(null)
    try {
      await spotify.disconnect()
      const next = await spotify.status()
      if (active()) setStatus(next)
    } catch (reason) {
      if (active()) setError(reason instanceof Error ? reason.message : String(reason))
    } finally {
      if (active()) {
        pending.current = null
        setBusy(null)
      }
    }
  }

  return (
    <div className="space-y-3">
      <p className="text-[12px] leading-5 text-muted">
        Public Spotify imports need no account or Premium. This optional API fallback uses browser sign-in and refreshes access automatically.
        Spotify requires the developer-app owner to have Premium for development-mode API access. Playback uses your selected playback source.
      </p>
      <ol className="list-decimal space-y-1.5 pl-4 text-[11px] leading-5 text-muted">
        <li>
          Create an app in the{' '}
          <a href="https://developer.spotify.com/dashboard" target="_blank" rel="noopener noreferrer" className="text-accent underline underline-offset-2">
            Spotify developer dashboard
          </a>{' '}and select Web API.
        </li>
        <li>
          Register this redirect URI exactly:
          <code className="mt-1 block select-all break-all rounded border border-rule bg-ground px-2 py-1 text-ink">http://127.0.0.1:43821/callback</code>
        </li>
        <li>Add your own Spotify account to the app&apos;s allowlist under User Management.</li>
        <li>Copy the app&apos;s Client ID below. No client secret is needed.</li>
      </ol>
      <label className="block">
        <span className="label">Spotify Client ID</span>
        <input
          value={draft.spotify.client_id}
          disabled={busy !== null}
          spellCheck={false}
          autoComplete="off"
          onChange={(event) => {
            setError(null)
            void edit({ spotify: { client_id: event.target.value } })
          }}
          onKeyDown={(event) => { if (event.key === 'Enter') void connect() }}
          className="mt-1 w-full rounded border border-rule bg-ground px-2.5 py-1.5 text-xs text-ink placeholder:text-muted disabled:opacity-40"
        />
      </label>
      <div className="flex flex-wrap items-center gap-3 pt-1">
        <button onClick={() => void connect()} disabled={busy !== null || !draft.spotify.client_id.trim()} className={buttonClass}>
          {busy === 'connect' ? 'Waiting for Spotify…' : status?.authenticated ? 'Reconnect Spotify' : 'Connect Spotify'}
        </button>
        {status?.authenticated && (
          <button onClick={() => void disconnect()} disabled={busy !== null} className={buttonClass}>
            {busy === 'disconnect' ? 'Signing out…' : 'Sign out of Spotify'}
          </button>
        )}
        {status?.authenticated && onContinue && (
          <button onClick={onContinue} disabled={busy !== null} className={buttonClass}>Continue</button>
        )}
      </div>
      <div aria-live="polite" className="text-[11px] leading-5 text-muted">
        {busy === 'connect'
          ? 'Finish signing in to Spotify in your browser. If you close it or cancel, this request will time out after about three minutes.'
          : status?.authenticated
            ? `Connected${status.display_name ? ` as ${status.display_name}` : ' to Spotify'}.`
            : 'Sign-in opens in your browser. The Client ID is saved in settings.'}
      </div>
      {error && <p role="alert" className="text-[11px] leading-5 text-crit">{error}</p>}
    </div>
  )
}
