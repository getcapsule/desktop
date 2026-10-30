import { useEffect, useRef, useState } from 'react'

import { dev, library, settings, type Settings } from '../lib/ipc'
import { ImportURL } from './ImportURL'
import { DesktopPreferences } from './DesktopPreferences'
import { dataDir } from '../lib/platform'
import {
  Cards,
  DiscordFields,
  GLASS,
  LastfmFields,
  SOURCES,
  SourceSetup,
  Switch,
  ThemePicker,
  type SettingsPatch,
} from './fields'

export function SettingsView({ focusTrackId }: { focusTrackId?: string }) {
  const [draft, setDraft] = useState<Settings | null>(null)
  const [note, setNote] = useState<string | null>(null)
  const writes = useRef(Promise.resolve())
  const latest = useRef<Settings | null>(null)
  const persisted = useRef<Settings | null>(null)
  const themeRevision = useRef(0)
  const materialRevision = useRef(0)
  const [materialError, setMaterialError] = useState<string | null>(null)
  const [materialPending, setMaterialPending] = useState(false)
  const importSection = useRef<HTMLDivElement>(null)
  useEffect(() => {
    if (focusTrackId && draft) importSection.current?.scrollIntoView?.({ block: 'start' })
  }, [focusTrackId, draft !== null])

  useEffect(() => {
    void settings.get().then((saved) => {
      latest.current = saved
      persisted.current = saved
      setDraft(saved)
    })
  }, [])

  if (!draft) return null

  const edit = (patch: SettingsPatch) => {
    const before = latest.current!
    const appearance = patch.appearance ?? {}
    const themeRequest = appearance.theme === undefined ? null : ++themeRevision.current
    const materialRequest = appearance.glass === undefined ? null : ++materialRevision.current
    if (materialRequest !== null) { setMaterialError(null); setMaterialPending(true) }
    const next = { ...before, ...patch, appearance: { ...before.appearance, ...(appearance.glass === undefined ? {} : { glass: appearance.glass }) } }
    latest.current = next
    setDraft(next)
    if (patch.source) setNote(null)
    const write = writes.current.catch(() => {}).then(async () => {
      // Apply each queued patch to the last successful write. A full snapshot
      // captured earlier could undo a theme or carry a failed save forward.
      const base = persisted.current!
      const saved = { ...base, ...patch, appearance: { ...base.appearance, ...appearance } }
      await settings.set(saved)
      document.documentElement.dataset.glass = saved.appearance.glass
      persisted.current = saved
      if (themeRequest !== null && themeRequest === themeRevision.current) {
        const current = latest.current!
        const applied = { ...current, appearance: { ...current.appearance, theme: saved.appearance.theme } }
        latest.current = applied
        setDraft(applied)
      }
    })
    writes.current = write
    void write.catch((error) => {
      if (materialRequest !== null) {
        if (materialRequest === materialRevision.current) {
          setMaterialError(String(error))
          const current = latest.current!
          const restored = { ...current, appearance: { ...current.appearance, glass: persisted.current!.appearance.glass } }
          latest.current = restored
          setDraft(restored)
        }
      } else setNote(String(error))
    }).finally(() => {
      if (materialRequest !== null && materialRequest === materialRevision.current) setMaterialPending(false)
    })
    return write
  }

  return (
    <div className="absolute inset-0 overflow-y-auto px-8 py-8">
      <div className="mx-auto max-w-xl space-y-9">
        <Section
          title="Source"
          sub="One at a time. Switching does not delete what another source already synced."
        >
          <Cards
            options={SOURCES}
            value={draft.source}
            onChange={(source) => edit({ source })}
          />
          <div className="mt-4">
            <SourceSetup draft={draft} edit={edit} />
          </div>
        </Section>

        <Section title="Desktop" sub="Listening preferences for this computer.">
          <DesktopPreferences />
        </Section>

        <Section title="Appearance" sub="Themes and materials apply immediately. Native desktop blur depends on Windows support.">
          <ThemePicker draft={draft} edit={edit} />
          <div className="mt-4" role="group" aria-label="Window material" aria-busy={materialPending}>
            <Cards
              options={GLASS}
              value={draft.appearance.glass}
              onChange={(glass) => edit({ appearance: { glass } })}
            />
            {materialPending && <p role="status" className="mt-2 text-xs text-muted">Applying material…</p>}
            {materialError && <p role="alert" className="mt-2 text-xs text-crit">{materialError}</p>}
          </div>
        </Section>

        <Section
          title="Lyrics timing"
          sub="Compensates the gap between the reported position and what reaches your speakers. Calibrate by ear from the lyrics view."
        >
          <div className="flex items-center gap-3">
            <button
              onClick={() =>
                edit({ lyrics: { offset_ms: Math.max(-5000, draft.lyrics.offset_ms - 100) } })
              }
              className="rounded border border-rule px-2.5 py-1.5 text-xs text-muted hover:border-accent hover:text-ink"
            >
              −100ms
            </button>
            <span className="data min-w-[72px] text-center text-xs text-ink">
              {draft.lyrics.offset_ms === 0
                ? 'in sync'
                : `${draft.lyrics.offset_ms > 0 ? '+' : ''}${(draft.lyrics.offset_ms / 1000).toFixed(2)}s`}
            </span>
            <button
              onClick={() =>
                edit({ lyrics: { offset_ms: Math.min(5000, draft.lyrics.offset_ms + 100) } })
              }
              className="rounded border border-rule px-2.5 py-1.5 text-xs text-muted hover:border-accent hover:text-ink"
            >
              +100ms
            </button>
            {draft.lyrics.offset_ms !== 0 && (
              <button
                onClick={() => edit({ lyrics: { offset_ms: 0 } })}
                className="label px-1 text-muted hover:text-ink"
              >
                reset
              </button>
            )}
          </div>
        </Section>

        <Section title="Last.fm" sub="Optional. Register an application at last.fm/api.">
          <LastfmFields draft={draft} edit={edit} />
        </Section>

        <Section
          title="Discord Rich Presence"
          sub="Optional. Create an application at discord.com/developers."
        >
          <DiscordFields draft={draft} edit={edit} />
        </Section>

        <Section title="Library" sub={`Your data lives in ${dataDir()}.`}>
          <div className="flex flex-wrap gap-2">
            <Action
              label="Sync now"
              onClick={() => {
                void library.sync()
                setNote('Sync started.')
              }}
            />
            <Action
              label="Run setup again"
              onClick={() => {
                edit({ onboarded: false })
                setNote('Setup will run the next time capsule starts.')
              }}
            />
          </div>
          {note && <p className="mt-3 text-[11px] text-muted">{note}</p>}
        </Section>

        <div ref={importSection}><Section title="Import" sub="Import music from a Spotify, Apple Music, YouTube Music, or SoundCloud URL"><ImportURL onComplete={setNote} settingsDraft={draft} editSettings={edit} focusTrackId={focusTrackId} /></Section></div>
        <Section
          title="Developer"
          sub="Diagnostics for filing a bug report. Off by default because it is noise otherwise."
        >
          <Switch
            label="Developer mode"
            on={draft.developer}
            onChange={(developer) => edit({ developer })}
          />
          {draft.developer && <Diagnostics />}
        </Section>
      </div>
    </div>
  )
}

function Diagnostics() {
  const [text, setText] = useState<string | null>(null)
  const [copied, setCopied] = useState(false)

  useEffect(() => {
    void dev.diagnostics().then(setText)
  }, [])

  const refresh = async () => {
    setCopied(false)
    setText(await dev.diagnostics())
  }

  const copy = async () => {
    const latest = await dev.diagnostics()
    setText(latest)
    await navigator.clipboard.writeText(latest)
    setCopied(true)
  }

  return (
    <div className="mt-4">
      <pre className="data max-h-56 overflow-auto rounded-md border border-rule bg-ground p-3 text-[10px] leading-5 text-muted">
        {text ?? 'Reading…'}
      </pre>
      <div className="mt-2 flex items-center gap-2">
        <Action label={copied ? 'Copied' : 'Copy diagnostics'} onClick={() => void copy()} />
        <Action label="Refresh" onClick={() => void refresh()} />
      </div>
      <p className="mt-3 text-[11px] leading-5 text-muted">
        Paste this into a bug report. Your server address and password are not included.
      </p>
    </div>
  )
}

function Section({
  title,
  sub,
  children,
}: {
  title: string
  sub?: string
  children: React.ReactNode
}) {
  return (
    <section>
      <h2 className="text-[15px] font-semibold tracking-tight">{title}</h2>
      {sub && <p className="mt-1 mb-4 text-[11px] leading-5 text-muted">{sub}</p>}
      {!sub && <div className="mb-4" />}
      {children}
    </section>
  )
}

function Action({ label, onClick, disabled }: { label: string; onClick: () => void; disabled?: boolean }) {
  return (
    <button
      onClick={onClick}
      disabled={disabled}
      className="rounded border border-rule px-2.5 py-1.5 text-[11px] text-muted transition-colors hover:border-accent hover:text-ink"
    >
      {label}
    </button>
  )
}
