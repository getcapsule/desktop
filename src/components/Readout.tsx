import { useEffect, useRef, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { acceptAudioInfo, audioInfoLabel, audioInfoTooltip, type AudioInfo } from '../lib/audio-info'

import type { LibraryCounts, PlayerState, Source, SyncProgress } from '../lib/ipc'
import { Globe, Lock, QueueList, Stack } from './icons'

export function Readout({
  state,
  counts,
  progress,
  source,
}: {
  state: PlayerState | null
  counts: LibraryCounts | null
  progress: SyncProgress | null
  source: Source
}) {
  const native = source === 'local' || source === 'navidrome'
  const syncing = progress !== null && !progress.done
  const synced = progress ? progress.songs + progress.albums + progress.playlists : 0
  const [audioInfo, setAudioInfo] = useState<AudioInfo | null>(null)

  useEffect(() => {
    setAudioInfo(null)
    let disposed = false
    let unlisten: (() => void) | undefined
    const update = (info: AudioInfo) => {
      if (!disposed) setAudioInfo(current => acceptAudioInfo(current, info) ? info : current)
    }
    void listen<AudioInfo>('audio://format', event => update(event.payload)).then(stop => {
      if (disposed) stop()
      else {
        unlisten = stop
        void invoke<AudioInfo>('audio_info').then(update).catch(() => {})
      }
    }).catch(() => {})
    return () => { disposed = true; unlisten?.() }
  }, [source, state?.index != null ? state.queue[state.index]?.id : null])

  return (
    <div className="readout label flex flex-wrap items-center justify-center gap-x-5 gap-y-1">
      {native && (
        <>
          <span className="flex items-center gap-1.5">
            <span className="inline-block size-1.5" style={{ background: 'var(--color-ok)' }} />
            <span style={{ color: 'var(--color-ok)' }}>native decode</span>
          </span>
          <Field icon={<Globe size={11} />} title="Source" value={source} />
          <Field icon={<Lock size={11} />} title="DRM" value="none" />
        </>
      )}
      <Field
        icon={<Stack size={11} />}
        title="Library"
        value={counts ? `${counts.songs} · ${counts.albums} · ${counts.playlists}` : '-'}
      />
      {audioInfo && state && !['idle', 'ended', 'loading'].includes(state.status) && (
        <span className="text-ink" title={audioInfoTooltip(audioInfo)} aria-label={`Audio format: ${audioInfoLabel(audioInfo)}`}>
          {audioInfoLabel(audioInfo)}
        </span>
      )}
      {state && state.queue.length > 0 && (
        <Field
          icon={<QueueList size={11} />}
          title="Queue position"
          value={`${pad((state.index ?? 0) + 1)}/${pad(state.queue.length)}`}
        />
      )}
      {syncing && (
        <span style={{ color: 'var(--color-warn)' }}>
          syncing {progress.stage} · {synced}
        </span>
      )}
    </div>
  )
}

function Field({
  icon,
  title,
  value,
}: {
  icon: React.ReactNode
  title: string
  value: string
}) {
  const [flash, setFlash] = useState(false)
  const previous = useRef(value)

  useEffect(() => {
    if (previous.current !== value) {
      previous.current = value
      setFlash(true)
      const t = window.setTimeout(() => setFlash(false), 200)
      return () => window.clearTimeout(t)
    }
  }, [value])

  return (
    <span className="flex items-center gap-1.5" title={title}>
      <span className="text-muted">{icon}</span>
      <span className={`text-ink ${flash ? 'value-change' : ''}`}>{value}</span>
    </span>
  )
}

function pad(n: number): string {
  return String(n).padStart(2, '0')
}
