import { artworkUrl, formatTime, player, type PlayerState } from '../lib/ipc'
import { NavigationIcon, Next, Pause, Play, Prev, Repeat, RepeatOne, Shuffle, Volume } from './icons'

export function Transport({ state, artworkRevision }: {
  state: PlayerState | null
  artworkRevision?: number
}) {
  const track = state && state.index !== null ? state.queue[state.index] : undefined
  const cover = track ? artworkUrl(track.id, 88, artworkRevision) : undefined
  const busy = state?.status === 'loading' || state?.status === 'stalled'
  const playing = state?.status === 'playing'
  const duration = track?.duration_ms ?? 0
  const repeat = state?.repeat ?? 'off'

  return (
    <footer className="transport chrome" aria-label="Playback controls">
      <div className="transport-track">
        <div className="transport-art">
          {track ? <img key={cover} src={cover} alt="" width={48} height={48} decoding="async"
            onError={e => (e.currentTarget.style.visibility = 'hidden')} /> : <NavigationIcon name="songs" size={22} />}
        </div>
        <div className="min-w-0">
          <p className="truncate font-semibold" title={track?.title}>{track?.title ?? 'Nothing queued'}</p>
          <p className="mt-1 truncate text-xs text-muted" title={track?.artist}>{track?.artist || (track ? 'Unknown artist' : 'Find something to listen to')}</p>
        </div>
      </div>
      <div className="transport-center">
        <div className="transport-buttons">
          <Toggle on={state?.shuffle ?? false}
            onClick={() => void player.toggleShuffle()} label="Shuffle"><Shuffle size={16} /></Toggle>
          <Ctl onClick={() => void player.previous()} disabled={!track} label="Previous"><Prev size={19} /></Ctl>
          <button onClick={() => void player.toggle()} disabled={!track}
            aria-label={playing ? 'Pause' : 'Play'} aria-busy={busy} className="transport-play">
            {busy ? <span className="text-lg" aria-hidden="true">···</span> : playing ? <Pause size={18} /> : <Play size={18} style={{ marginLeft: 2 }} />}
          </button>
          <Ctl onClick={() => void player.next()} disabled={!track} label="Next"><Next size={19} /></Ctl>
          <Toggle on={repeat !== 'off'}
            onClick={() => void player.cycleRepeat()} label={`Repeat: ${repeat}`}>
            {repeat === 'one' ? <RepeatOne size={16} /> : <Repeat size={16} />}
          </Toggle>
        </div>
        <div className="transport-progress">
          <span className="data text-muted">{formatTime(state?.position_ms ?? 0)}</span>
          <input type="range" min={0} max={Math.max(duration, 1)} value={Math.min(state?.position_ms ?? 0, duration)}
            onChange={e => void player.seek(Number(e.target.value))} disabled={!track} aria-label="Seek"
            style={{ '--fill': duration > 0 ? ((state?.position_ms ?? 0) / duration) * 100 : 0 } as React.CSSProperties} />
          <span className="data text-muted">{formatTime(duration)}</span>
        </div>
      </div>
      <div className="transport-volume text-muted">
        <Volume size={17} />
        <input type="range" min={0} max={100} value={state?.volume ?? 100}
          onChange={e => void player.setVolume(Number(e.target.value))} aria-label="Volume"
          style={{ '--fill': state?.volume ?? 100 } as React.CSSProperties} />
      </div>
    </footer>
  )
}

function Ctl({ onClick, disabled, label, children }: { onClick: () => void; disabled?: boolean; label: string; children: React.ReactNode }) {
  return <button onClick={onClick} disabled={disabled} aria-label={label} title={label} className="transport-control">{children}</button>
}

function Toggle({ on, onClick, label, children, disabled }: { on: boolean; onClick: () => void; label: string; children: React.ReactNode; disabled?: boolean }) {
  return <button onClick={onClick} disabled={disabled} aria-label={label} title={label} aria-pressed={on}
    className="transport-control transport-toggle">{children}</button>
}
