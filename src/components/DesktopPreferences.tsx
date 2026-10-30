import { useEffect, useState } from 'react'
import { desktop, type DesktopSnapshot } from '../lib/desktop-ipc'
import { Switch } from './fields'

const selectStyle = 'rounded border border-rule bg-ground px-2.5 py-1.5 text-xs text-ink disabled:opacity-50'

export function DesktopPreferences() {
  const [state, setState] = useState<DesktopSnapshot | null>(null)
  const [devices, setDevices] = useState<string[]>([])
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  useEffect(() => {
    let alive = true
    let unlisten: (() => void) | undefined
    let revision = 0
    void desktop.subscribe(next => { revision++; if (alive) setState(next) }).then(async stop => {
      if (!alive) { stop(); return }
      unlisten = stop
      const initialRevision = revision
      const next = await desktop.snapshot()
      if (alive && revision === initialRevision && next) setState(next)
    }).catch(reason => { if (alive) setError(String(reason)) })
    void desktop.devices().then(next => { if (alive) setDevices(next ?? []) }).catch(reason => { if (alive) setError(String(reason)) })
    return () => { alive = false; unlisten?.() }
  }, [])
  const change = async (operation: () => Promise<DesktopSnapshot>) => {
    if (busy) return
    setBusy(true)
    setError(null)
    try { setState(await operation()) } catch (reason) { setError(String(reason)) }
    finally { setBusy(false) }
  }
  if (!state) return error ? <p role="alert" className="text-xs text-muted">{error}</p> : null
  const selectedMissing = state.output_device && !devices.includes(state.output_device)
  return (
    <div className="space-y-4">
      <fieldset disabled={busy} className="space-y-4 disabled:opacity-60">
        <Switch label="Close to tray" note="Keep listening after closing the window. Turn off to quit on close." on={state.close_to_tray} onChange={value => void change(() => desktop.preferences(value))} />
        <div className="flex items-center justify-between gap-4">
          <label htmlFor="desktop-output" className="text-[13px] text-ink">Audio output</label>
          <div className="flex items-center gap-2">
            <select id="desktop-output" className={`${selectStyle} max-w-64`} value={state.output_device ?? ''} onChange={event => void change(() => desktop.output(event.target.value || null))}>
              <option value="">System default</option>
              {selectedMissing && <option value={state.output_device!}>{state.output_device} (disconnected)</option>}
              {devices.map(device => <option key={device} value={device}>{device}</option>)}
            </select>
            <button type="button" className="label text-muted hover:text-ink" onClick={() => { setError(null); void desktop.devices().then(setDevices).catch(reason => setError(String(reason))) }}>Refresh</button>
          </div>
        </div>
        <div className="flex items-center justify-between gap-4">
          <label htmlFor="desktop-sleep" className="text-[13px] text-ink">Sleep timer</label>
          <select id="desktop-sleep" className={selectStyle} value="" onChange={event => void change(() => desktop.sleep(event.target.value ? Number(event.target.value) : null))}>
            <option value="">{state.sleep_deadline_ms ? 'Cancel timer' : 'Off'}</option>
            {[15, 30, 45, 60, 90, 120].map(minutes => <option key={minutes} value={minutes}>{minutes} minutes</option>)}
          </select>
        </div>
        {state.sleep_deadline_ms !== null && <div className="flex items-center justify-between gap-3 text-xs text-muted">
          <span>Playback pauses at {new Date(state.sleep_deadline_ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}.</span>
          <button type="button" className="label hover:text-ink" onClick={() => void change(() => desktop.sleep(null))}>Cancel timer</button>
        </div>}
      </fieldset>
      <p className="text-[11px] leading-5 text-muted">Keyboard and headset media buttons control playback while Capsule is in the tray.</p>
      {error && <p role="alert" className="text-xs text-muted">{error}</p>}
    </div>
  )
}
