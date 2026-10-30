import { useEffect, useMemo, useRef, useState } from 'react'

import { library, player, settings, type LyricLine, type PlayerState } from '../lib/ipc'
import { activeItem, gapProgress, timeline } from '../lib/lyrics'

export function Lyrics({ state }: { state: PlayerState | null }) {
  const track = state?.index != null ? state.queue[state.index] : undefined
  const [lines, setLines] = useState<LyricLine[]>([])
  const [plain, setPlain] = useState<string | null>(null)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState(false)
  const [attempt, retry] = useState(0)
  const lookupTrack = useRef<string | null>(null)
  const scroller = useRef<HTMLDivElement>(null)
  const [offset, setOffset] = useOffset()

  useEffect(() => {
    setLines([])
    setPlain(null)
    setError(false)
    setLoading(false)
    if (!track) return
    let cancelled = false
    setLoading(true)
    const refresh = lookupTrack.current === track.id && attempt > 0
    lookupTrack.current = track.id
    void (refresh ? library.refreshLyrics(track.id) : library.lyrics(track.id))
      .then((r) => {
        if (cancelled) return
        setLines(r.lines)
        setPlain(r.plain)
      })
      .catch(() => { if (!cancelled) setError(true) })
      .finally(() => {
        if (!cancelled) setLoading(false)
      })
    return () => {
      cancelled = true
    }
  }, [track?.id, attempt])

  const items = useMemo(
    () => timeline(lines, track?.duration_ms ?? 0),
    [lines, track?.duration_ms],
  )

  const position = useSmoothPosition(state) - offset
  const active = activeItem(items, position)

  useEffect(() => {
    if (active == null) return
    scroller.current
      ?.querySelector(`[data-item="${active}"]`)
      ?.scrollIntoView({ block: 'center', behavior: 'smooth' })
  }, [active])

  const tap = () => {
    const starts = items.filter((i) => i.kind === 'line').map((i) => i.at)
    if (starts.length === 0) return

    const nearest = starts.reduce((best, at) =>
      Math.abs(at - position) < Math.abs(best - position) ? at : best,
    )
    const error = position - nearest
    if (Math.abs(error) > TAP_MAX_MS) return
    setOffset(offset + error)
  }

  if (!track) return <Centered>Nothing playing.</Centered>
  if (loading && items.length === 0 && !plain) return <Centered>Looking for lyrics…</Centered>
  if (error || (items.length === 0 && !plain)) return <Centered><span>{error ? 'Could not load lyrics.' : 'No lyrics for this track.'}</span><button className="ml-3 underline hover:text-ink" onClick={() => retry(n => n + 1)}>Retry</button></Centered>

  if (items.length === 0 && plain) {
    return (
      <div className="absolute inset-0 overflow-y-auto px-8 py-10">
        <p className="mx-auto max-w-xl whitespace-pre-wrap text-[15px] leading-8 text-muted">
          {plain}
        </p>
      </div>
    )
  }

  return (
    <>
      <div ref={scroller} className="absolute inset-0 overflow-y-auto px-8 py-[40vh]">
        <div className="mx-auto max-w-xl">
          {items.map((item, i) => (
            <div key={`${item.kind}-${item.at}`} data-item={i} className="py-1.5">
              {item.kind === 'gap' ? (
                <Dots
                  progress={i === active ? gapProgress(item, position) : 0}
                  on={i === active}
                />
              ) : (
                <button
                  onClick={() => { void player.seek(Math.min(track.duration_ms || Infinity, Math.max(0, item.at + offset))).catch(() => setError(true)) }}
                  className={`block w-full text-left text-[17px] leading-8 transition-colors duration-200 ${
                    i === active ? 'font-semibold text-ink' : 'text-muted/55'
                  }`}
                >
                  {item.text}
                </button>
              )}
            </div>
          ))}
        </div>
      </div>
      <Calibration offset={offset} onTap={tap} onReset={() => setOffset(0)} />
    </>
  )
}

function Dots({ progress, on }: { progress: number; on: boolean }) {
  return (
    <div className="flex items-center gap-2 py-1" aria-hidden>
      {[0, 1, 2].map((i) => {
        const share = Math.min(1, Math.max(0, progress * 3 - i))
        return (
          <span
            key={i}
            className="size-2 rounded-full transition-all duration-300"
            style={{
              background: on ? 'var(--color-ink)' : 'var(--color-muted)',
              opacity: on ? 0.25 + share * 0.75 : 0.25,
              transform: on ? `scale(${0.85 + share * 0.35})` : 'scale(0.85)',
            }}
          />
        )
      })}
    </div>
  )
}

const OFFSET_LIMIT = 5_000
const TAP_MAX_MS = 2_000

function useOffset(): [number, (next: number) => void] {
  const [offset, set] = useState(0)
  const edited = useRef(false)
  const writes = useRef(Promise.resolve())

  useEffect(() => {
    let cancelled = false
    void settings.get().then((s) => {
      if (!cancelled && !edited.current) set(s.lyrics.offset_ms)
    }).catch(() => {})
    return () => { cancelled = true }
  }, [])

  const update = (next: number) => {
    const clamped = Math.round(Math.max(-OFFSET_LIMIT, Math.min(OFFSET_LIMIT, next)))
    edited.current = true
    set(clamped)
    writes.current = writes.current.then(() => settings.setLyricsOffset(clamped)).then(() => {}, () => {})
  }

  return [offset, update]
}

function Calibration({
  offset,
  onTap,
  onReset,
}: {
  offset: number
  onTap: () => void
  onReset: () => void
}) {
  return (
    <div className="absolute right-4 bottom-3 z-10 flex items-center gap-2 opacity-45 transition-opacity hover:opacity-100">
      <button
        onClick={onTap}
        className="label rounded border border-rule px-2 py-1 hover:text-ink"
        title="Press exactly as a line starts, and the offset corrects itself"
      >
        Tap to sync
      </button>
      <button
        onClick={onReset}
        className="data text-[10px] text-muted hover:text-ink"
        title="Current offset - click to reset"
      >
        {offset === 0 ? 'sync' : `${offset > 0 ? '+' : ''}${(offset / 1000).toFixed(2)}s`}
      </button>
    </div>
  )
}

function useSmoothPosition(state: PlayerState | null): number {
  const anchor = useRef({ ms: 0, at: 0 })
  const [, tick] = useState(0)
  const playing = state?.status === 'playing'
  const reported = state?.position_ms ?? 0

  useEffect(() => {
    anchor.current = { ms: reported, at: performance.now() }
    tick((n) => n + 1)
  }, [reported, playing])

  useEffect(() => {
    if (!playing) return
    const id = window.setInterval(() => tick((n) => n + 1), 100)
    return () => window.clearInterval(id)
  }, [playing])

  if (!playing) return reported
  return anchor.current.ms + (performance.now() - anchor.current.at)
}

function Centered({ children }: { children: React.ReactNode }) {
  return (
    <div className="absolute inset-0 flex items-center justify-center">
      <span className="text-muted">{children}</span>
    </div>
  )
}
