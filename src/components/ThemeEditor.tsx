import { useEffect, useRef, useState, type CSSProperties } from 'react'
import { themeFiles, type ThemeDocument } from '../lib/theme-ipc'

const colors = [
  ['ground', 'Background'], ['panel', 'Panel'], ['ink', 'Text'], ['accent', 'Accent'],
  ['rule', 'Borders'], ['muted', 'Secondary text'], ['ok', 'Success'], ['warn', 'Warning'], ['crit', 'Error'],
] as const
const button = 'rounded border border-rule px-2.5 py-1.5 text-xs text-muted hover:border-accent hover:text-ink disabled:opacity-40'
type Draft = { name: string; colors: Record<string, string> }
function encode(draft: Draft) {
  return [`name = ${JSON.stringify(draft.name)}`, ...colors.flatMap(([key]) => draft.colors[key]?.trim() ? [`${key} = ${JSON.stringify(draft.colors[key]!.trim())}`] : [])].join('\n')
}

export function ThemeEditor({ activeId, onSaved }: { activeId: string; onSaved: (id: string) => Promise<void | boolean> | void | boolean }) {
  const [draft, setDraft] = useState<Draft | null>(null)
  const [preview, setPreview] = useState<{ raw: string; document: ThemeDocument } | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [note, setNote] = useState<string | null>(null)
  const operation = useRef(0)
  const applyingId = useRef<string | null>(null)
  const latestSaved = useRef(onSaved)
  latestSaved.current = onSaved
  const raw = draft ? encode(draft) : null

  useEffect(() => {
    if (applyingId.current === activeId) {
      applyingId.current = null
      return
    }
    applyingId.current = null
    ++operation.current
    setDraft(null); setPreview(null); setError(null); setNote(null); setBusy(false)
  }, [activeId])

  useEffect(() => () => { ++operation.current }, [])

  useEffect(() => {
    if (raw === null) return
    let current = true
    const timer = setTimeout(() => {
      void themeFiles.preview(raw).then(document => {
        if (current) { setPreview({ raw, document }); setError(null) }
      }).catch(e => { if (current) { setPreview(null); setError(String(e)) } })
    }, 120)
    return () => { current = false; clearTimeout(timer) }
  }, [raw])

  async function action(run: () => Promise<void>) {
    if (busy) return
    setBusy(true); setError(null); setNote(null)
    const revision = ++operation.current
    try { await run() } catch (e) { if (operation.current === revision) setError(String(e)) }
    finally { if (operation.current === revision) setBusy(false) }
  }
  async function open(importing: boolean) {
    const revision = operation.current
    const doc = importing ? await themeFiles.import() : await themeFiles.document(activeId)
    if (doc && operation.current === revision) { setDraft({ name: doc.name, colors: doc.colors }); setPreview(null) }
  }
  async function save() {
    if (!raw || preview?.raw !== raw) return
    const revision = operation.current
    const id = await themeFiles.save(raw)
    if (operation.current !== revision) return
    applyingId.current = id
    try {
      const applied = await latestSaved.current(id)
      if (applied === false) {
        if (operation.current === revision) {
          applyingId.current = null
          setDraft(null)
          setNote('Theme saved. Your current selection was kept.')
        }
        return
      }
    }
    catch (error) {
      if (operation.current === revision) applyingId.current = null
      throw error
    }
    if (operation.current === revision) { setDraft(null); setNote('New theme saved and applied.') }
  }
  const variables = Object.fromEntries((preview?.document.css ?? '').split(';').filter(Boolean).map(pair => {
    const at = pair.indexOf(':')
    return [pair.slice(0, at), pair.slice(at + 1)]
  })) as CSSProperties

  return <div className="space-y-3">
    <div className="flex flex-wrap gap-2">
      <button className={button} disabled={busy || !!draft} onClick={() => void action(() => open(false))}>Edit theme</button>
      <button className={button} disabled={busy || !!draft} onClick={() => void action(() => open(true))}>Import theme</button>
      <button className={button} disabled={busy || !!draft} onClick={() => void action(async () => {
        const revision = operation.current
        if (await themeFiles.export(activeId) && operation.current === revision) setNote('Theme exported.')
      })}>Export theme</button>
    </div>
    {draft && <div className="space-y-4 rounded border border-rule p-4">
      <label className="block text-xs text-muted">Theme name
        <input className="mt-1 block w-full rounded border border-rule bg-ground px-2 py-1.5 text-ink" value={draft.name} maxLength={120} disabled={busy} onChange={e => setDraft({ ...draft, name: e.target.value })} />
      </label>
      <div className="grid grid-cols-2 gap-x-4 gap-y-3">
        {colors.map(([key, label], index) => <div key={key}>
          <label htmlFor={`theme-${key}`} className="label">{label}{index > 3 ? ' · optional' : ''}</label>
          <div className="mt-1 flex items-center gap-2">
            <input type="color" aria-label={`${label} color picker`} className="h-7 w-7 shrink-0 cursor-pointer rounded border border-rule bg-transparent" disabled={busy} value={/^#[a-f\d]{6}$/i.test(draft.colors[key] ?? '') ? draft.colors[key] : '#000000'} onChange={e => setDraft({ ...draft, colors: { ...draft.colors, [key]: e.target.value } })} />
            <input id={`theme-${key}`} className="data min-w-0 w-full rounded border border-rule bg-ground px-2 py-1.5 text-xs text-ink" placeholder={index > 3 ? 'Automatic' : '#000000'} value={draft.colors[key] ?? ''} disabled={busy} onChange={e => setDraft({ ...draft, colors: { ...draft.colors, [key]: e.target.value } })} />
          </div>
        </div>)}
      </div>
      {preview && <div style={variables} className="rounded border border-rule bg-ground p-4" aria-label="Theme preview">
        <p className="label text-muted">Library preview</p>
        <div className="mt-3 flex items-center justify-between gap-3 rounded bg-panel p-3">
          <div><p className="text-sm text-ink">Your favorite song</p><p className="mt-1 text-xs text-muted">Artist · Album</p></div>
          <span className="data text-xs text-accent">02:38</span>
        </div>
      </div>}
      {preview?.document.failures.length ? <div className="text-[11px] text-warn" role="status">
        <p>{preview.document.failures.length} color pairs below AA (4.5:1). You can still save.</p>
        <ul className="mt-1 space-y-0.5">{preview.document.failures.map(f => <li key={`${f.fg}-${f.bg}`}>{f.fg} on {f.bg}: {f.ratio.toFixed(2)}:1</li>)}</ul>
      </div> : null}
      <div className="flex flex-wrap gap-2">
        <button className={button} disabled={busy || !raw || preview?.raw !== raw} onClick={() => void action(save)}>Save as new theme</button>
        <button className={button} disabled={busy} onClick={() => { ++operation.current; setDraft(null); setPreview(null); setError(null) }}>Cancel editing</button>
      </div>
    </div>}
    {error && <p role="alert" className="text-xs text-crit">{error}</p>}
    {note && <p role="status" className="text-xs text-muted">{note}</p>}
  </div>
}
