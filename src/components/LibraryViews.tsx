import { useContext, useEffect, useRef, useState } from 'react'
import { artworkUrl, formatTime, library, type SongRow, type SortDirection } from '../lib/ipc'
import { filterSongs, selectionAfterClick } from '../lib/qol'
import { qol, type LocalPlaylist, type SongEntry } from '../lib/qol-ipc'
import { songDownloadRequests } from '../lib/download-actions'
import { DownloadButton } from './Downloads'
import { ListeningContext, SelectionBar } from './ListeningTools'
import { LibrarySort, SONG_SORTS } from './LibrarySort'
import { LIBRARY_ROW_HEIGHT, SongLine } from './LibraryRows'
import { VirtualList } from './VirtualList'
import { playlistButton } from './PlaylistDialog'
import { PlaylistEditor } from './PlaylistEditor'

export type LibraryDetail = {
  kind: 'album' | 'playlist'
  id: string
  title: string
  subtitle?: string
}

export function SavedSongs({ entries, title, query, nowPlayingId, onProblem }: {
  entries: SongEntry[]
  title: string
  query: string
  nowPlayingId: string | null
  onProblem: (message: string) => void
}) {
  const q = query.trim().toLocaleLowerCase()
  const shown = q
    ? entries.filter(e => `${e.song.name} ${e.song.artist_name} ${e.song.album_name}`.toLocaleLowerCase().includes(q))
    : entries
  const emptyMessage = query ? 'No matches.' : title === 'Favorites'
    ? 'Add favorites from a track’s More menu.'
    : 'Your listening history will appear here.'

  return (
    <div className="flex h-full flex-col">
      <h1 className="border-b border-rule px-4 py-3 text-sm font-semibold">
        {title} <span className="text-xs font-normal text-muted">{shown.length} tracks</span>
      </h1>
      <div className="relative min-h-0 flex-1">
        <VirtualList
          items={shown}
          rowHeight={LIBRARY_ROW_HEIGHT}
          empty={<span className="text-xs text-muted">{emptyMessage}</span>}
          render={(entry, index) => (
            <SongLine
              song={entry.song}
              index={index}
              source={entry.source}
              context={entry.context}
              playable
              playing={entry.song.id === nowPlayingId}
              onPlay={() => void qol.play([entry.song], entry.source, 0, entry.context).catch(e => onProblem(String(e)))}
            />
          )}
        />
      </div>
    </div>
  )
}

export function DetailView({
  detail,
  query,
  localPlaylist,
  sort,
  onSort,
  direction,
  onDirection,
  canPlay,
  nowPlayingId,
  trackIdOf,
  onBack,
  onProblem,
}: {
  detail: LibraryDetail
  query: string
  localPlaylist?: LocalPlaylist
  sort: string
  onSort: (value: string) => void
  direction: SortDirection
  onDirection: (value: SortDirection) => void
  canPlay: (s: SongRow) => boolean
  nowPlayingId: string | null
  trackIdOf: (s: SongRow) => string | null
  onBack: () => void
  onProblem: (msg: string) => void
}) {
  const [songs, setSongs] = useState<SongRow[] | null>(null)
  const [editingPlaylistId, setEditingPlaylistId] = useState<string | null>(null)
  const listening = useContext(ListeningContext)
  const [selected, setSelected] = useState<Set<number>>(new Set())
  const anchor = useRef(0)
  const localEntries = localPlaylist?.songs

  useEffect(() => {
    setSongs(null)
    let active = true
    if (localEntries) {
      setSongs(localEntries.map(e => e.song))
      return () => { active = false }
    }
    const load =
      detail.kind === 'album'
        ? library.albumSongs(detail.id)
        : library.playlistSongs(detail.id)
    void load.then(rows => { if (active) setSongs(rows) }).catch((e) => {
      if (!active) return
      onProblem(String(e))
      setSongs([])
    })
    return () => { active = false }
  }, [detail.kind, detail.id, localEntries, onProblem])
  useEffect(() => {
    setSelected(new Set())
    anchor.current = 0
  }, [query, sort, direction, detail.id, songs, localEntries])
  const shown = filterSongs(localEntries ? localEntries.map(e => e.song) : songs ?? [], query, sort, direction)
  const displayedEntries = localEntries ? shown.map(row => localEntries.find(e => e.song === row)!) : undefined
  const play = (index: number) => {
    if (!localEntries) return library.play(shown, index)
    return qol.playEntries(displayedEntries ?? [], index)
  }

  const total = songs?.reduce((n, s) => n + s.duration_ms, 0) ?? 0
  const selectedIndices = [...selected].sort((a, b) => a - b)
  const selectedSongs = selectedIndices.map(i => shown[i]).filter((song): song is SongRow => !!song)
  const selectedEntries = displayedEntries
    ? selectedIndices.map(i => displayedEntries[i]).filter((entry): entry is SongEntry => !!entry)
    : undefined
  const selectSong = (index: number, shift: boolean, toggle: boolean) => {
    setSelected(previous => selectionAfterClick(previous, index, anchor.current, shift, toggle))
    if (!shift) anchor.current = index
  }

  return (
    <div className="flex h-full flex-col">
      <div className="flex items-center gap-3 border-b border-rule/70 px-4 py-3">
        <button
          onClick={onBack}
          className="rounded-md border border-rule px-2 py-1 text-[11px] text-muted transition-colors hover:border-muted hover:text-ink"
        >
          Back
        </button>
        {localPlaylist?.cover_id && <img alt="Playlist cover" src={artworkUrl(localPlaylist.cover_id, 128)} className="size-12 shrink-0 rounded-md border border-rule object-cover" />}
        <div className="min-w-0 flex-1">
          <h1 className="truncate text-[13px] text-ink">{localPlaylist?.name ?? detail.title}</h1>
          {detail.subtitle && (
            <div className="truncate text-[11px] text-muted">{detail.subtitle}</div>
          )}
        </div>
        <span className="label shrink-0">
          {songs ? `${songs.length} · ${formatTime(total)}` : ''}
        </span>
        {localPlaylist && <button className={playlistButton} aria-haspopup="dialog" onClick={() => setEditingPlaylistId(localPlaylist.id)}>Edit playlist</button>}
        <button
          onClick={() => {
            if (shown.length > 0) void play(0).catch(e => onProblem(String(e)))
          }}
          disabled={!songs || songs.length === 0}
          className="rounded-md border border-rule px-2.5 py-1 text-[11px] text-muted transition-colors hover:border-accent hover:text-ink disabled:opacity-40"
        >
          Play
        </button>
        {listening && <DownloadButton disabled={!songs?.length} label="Download all" onProblem={onProblem} requests={shown.flatMap((song, index) => songDownloadRequests([song], displayedEntries?.[index]?.source ?? listening.source, displayedEntries?.[index]?.context ?? listening.context))} />}
      </div>
      {localPlaylist?.description && <p className="max-h-24 shrink-0 overflow-auto whitespace-pre-wrap break-words px-4 py-2 text-xs text-muted">{localPlaylist.description}</p>}
      {localPlaylist && editingPlaylistId === localPlaylist.id && <PlaylistEditor playlist={localPlaylist} onClose={() => setEditingPlaylistId(null)} />}
      <LibrarySort label={`${detail.kind} tracks`} options={[{ value: 'original', label: 'Original order' }, ...SONG_SORTS]} value={sort} onChange={onSort} direction={direction} onDirection={onDirection} count={shown.length} loading={songs === null} />
      <SelectionBar songs={selectedSongs} entries={selectedEntries} onClear={() => setSelected(new Set())} />

      <div className="relative min-h-0 flex-1">
        {songs === null ? (
          <div className="flex h-full items-center justify-center text-[11px] text-muted">
            Reading…
          </div>
        ) : (
          <VirtualList
            items={shown}
            rowHeight={LIBRARY_ROW_HEIGHT}
            empty={<span className="text-muted">{localEntries?.length === 0 ? 'Add songs from Discover or a track’s More menu.' : 'Nothing in here'}</span>}
            render={(s, i) => {
              const entry = displayedEntries?.[i]
              const entryId = entry?.entry_id
              return <SongLine
                song={s}
                index={i}
                playable={canPlay(s)}
                playing={nowPlayingId !== null && trackIdOf(s) === nowPlayingId}
                selected={selected.has(i)}
                showCheckbox={detail.kind !== 'playlist'}
                onSelect={(shift, toggle) => selectSong(i, shift, toggle)}
                source={entry?.source}
                context={entry?.context}
                onRemoveFromPlaylist={entryId ? () => qol.playlistRemoveSong(detail.id, entryId) : undefined}
                onPlay={() => void play(i).catch(e => onProblem(String(e)))}
              />
            }}
          />
        )}
      </div>
    </div>
  )
}
