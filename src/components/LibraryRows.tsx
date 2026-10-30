import { useContext } from 'react'
import { artworkUrl, formatTime, type AlbumRow, type SongRow, type Source } from '../lib/ipc'
import { ListeningContext, TrackTools } from './ListeningTools'

export const LIBRARY_ROW_HEIGHT = 40

export function SongLine({
  song,
  index,
  playable,
  playing,
  onPlay,
  selected = false,
  onSelect,
  source,
  context,
  showCheckbox = true,
}: {
  song: SongRow
  index: number
  playable: boolean
  playing: boolean
  onPlay: () => void
  selected?: boolean
  onSelect?: (shift: boolean, toggle: boolean) => void
  source?: Source
  context?: string
  showCheckbox?: boolean
}) {
  return (
    <div
      onClick={e => onSelect?.(e.shiftKey, e.ctrlKey || e.metaKey)}
      onContextMenu={e => { e.preventDefault(); e.currentTarget.querySelector<HTMLButtonElement>('[data-track-menu]')?.click() }}
      onDoubleClick={playable ? onPlay : undefined}
      onKeyDown={(e) => {
        if (e.target !== e.currentTarget) return
        if (playable && (e.key === 'Enter' || e.key === ' ')) {
          e.preventDefault()
          onPlay()
        }
      }}

      role="button"
      aria-label={`${song.name}${song.artist_name}${formatTime(song.duration_ms)}`}
      tabIndex={playable ? 0 : -1}
      aria-disabled={!playable}
      aria-pressed={selected}
      data-playing={playing}
      className={`row mx-2 grid h-10 grid-cols-[28px_36px_1fr_1fr_52px_auto] items-center gap-3 px-2 outline-none focus-visible:ring-1 focus-visible:ring-accent ${selected ? 'bg-ink/10' : ''} ${
        playable ? '' : 'opacity-40'
      }`}
      title={playable ? 'Play' : 'Not playable from this source'}
    >
      <span className="data text-[10px] text-muted">
        {onSelect && showCheckbox ? (
          <input
            type="checkbox"
            aria-label={`Select ${song.name}`}
            checked={selected}
            onClick={e => e.stopPropagation()}
            onChange={() => onSelect(false, true)}
          />
        ) : String(index + 1).padStart(2, '0')}
      </span>
      <Art id={song.id} />
      <span className="truncate">{song.name}</span>
      <span className="truncate text-muted">{song.artist_name}</span>
      <span className="text-right data text-[10px] text-muted">
        {formatTime(song.duration_ms)}
      </span>
      <TrackTools songs={[song]} source={source} context={context} />
    </div>
  )
}

export function AlbumLine({
  album,
  onOpen,
  onPlay,
}: {
  album: AlbumRow
  onOpen: () => void
  onPlay: () => void
}) {
  return (
    <div
      onClick={onOpen}
      onKeyDown={(e) => {
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault()
          onOpen()
        }
      }}
      role="button"
      tabIndex={0}
      title="Open, or double-click to play"
      onDoubleClick={onPlay}
      className="row mx-2 grid h-10 grid-cols-[36px_1fr_1fr_60px] items-center gap-3 px-2 outline-none focus-visible:ring-1 focus-visible:ring-accent"
    >
      <Art id={album.id} />
      <span className="truncate">{album.name}</span>
      <span className="truncate text-muted">{album.artist_name}</span>
      <span className="text-right data text-[10px] text-muted">
        {album.track_count} trk
      </span>
    </div>
  )
}

export function Art({ id }: { id: string }) {
  const revision = useContext(ListeningContext)?.artworkRevision
  const src = artworkUrl(id, 56, revision)
  return (
    <span className="track-art block size-7 overflow-hidden rounded-md border border-rule bg-panel">
      <img
        key={src}
        src={src}
        alt=""
        loading="lazy"
        className="size-full object-cover"
        onError={(e) => (e.currentTarget.style.visibility = 'hidden')}
      />
    </span>
  )
}
