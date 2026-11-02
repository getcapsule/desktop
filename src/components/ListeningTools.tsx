import { createContext, useContext, useEffect, useRef, useState } from 'react'
import { createPortal } from 'react-dom'
import { formatTime, type PlayerState, type SongRow, type Source } from '../lib/ipc'
import { qol, type QolSnapshot, type SongEntry } from '../lib/qol-ipc'
import { playlistContext } from '../lib/qol'
import { VirtualList } from './VirtualList'
import { queueDownloads, songDownloadRequests } from '../lib/download-actions'

export const ListeningContext = createContext<{
  artworkRevision?: number
  source: Source; context?: string; snapshot: QolSnapshot; problem: (message: string) => void
  artist: (name: string) => void; album: (song: SongRow) => void; replace: (id: string) => void
} | null>(null)
const button = 'rounded border border-rule px-2 py-1 text-[11px] text-muted hover:border-accent hover:text-ink focus-visible:outline focus-visible:outline-accent disabled:opacity-40'

export function TrackTools({ songs, source, context }: { songs: SongRow[]; source?: Source; context?: string }) {
  const listening = useContext(ListeningContext)
  const [anchor, setAnchor] = useState<{ top: number; left: number } | null>(null)
  const [name, setName] = useState('')
  const [playlist, setPlaylist] = useState('')
  const [info, setInfo] = useState(false)
  const trigger = useRef<HTMLButtonElement>(null)
  const menu = useRef<HTMLDivElement>(null)
  useEffect(() => {
    if (!anchor) return
    menu.current?.querySelector<HTMLButtonElement>('[role="menuitem"]')?.focus()
    const outside = (e: PointerEvent) => { if (!menu.current?.contains(e.target as Node) && !trigger.current?.contains(e.target as Node)) setAnchor(null) }
    const key = (e: KeyboardEvent) => {
      if (e.key === 'Tab') setAnchor(null)
      if (e.key === 'Escape') { setAnchor(null); trigger.current?.focus() }
      if ((e.key === 'ArrowDown' || e.key === 'ArrowUp') && !(e.target as HTMLElement)?.closest('input,select,textarea')) {
        const items = [...(menu.current?.querySelectorAll<HTMLButtonElement>('[role="menuitem"]:not(:disabled)') ?? [])]
        const index = items.indexOf(document.activeElement as HTMLButtonElement)
        if (items.length) { e.preventDefault(); items[(index + (e.key === 'ArrowDown' ? 1 : -1) + items.length) % items.length]?.focus() }
      }
    }
    document.addEventListener('pointerdown', outside); document.addEventListener('keydown', key)
    return () => { document.removeEventListener('pointerdown', outside); document.removeEventListener('keydown', key) }
  }, [anchor])
  if (!listening || songs.length === 0) return null
  const selectedSource = source ?? listening.source
  const bindingContext = context ?? (selectedSource === listening.source ? listening.context : undefined)
  const song = songs[0]!
  const favorite = listening.snapshot.favorites.some(e => e.song.id === song.id && e.source === selectedSource && (e.context ?? null) === (bindingContext ?? null))
  const run = (action: () => Promise<unknown> | void) => {
    setAnchor(null); trigger.current?.focus()
    void Promise.resolve().then(action).catch(e => listening.problem(String(e)))
  }
  const action = (label: string, fn: () => Promise<unknown> | void) => <button role="menuitem" className="block w-full rounded px-3 py-2 text-left text-xs hover:bg-ink/10 focus:bg-ink/10 focus:outline-none" onClick={() => run(fn)}>{label}</button>
  return <span className="inline-flex shrink-0" onClick={e => e.stopPropagation()} onDoubleClick={e => e.stopPropagation()}>
    <button ref={trigger} data-track-menu aria-label={`More actions for ${songs.length > 1 ? `${songs.length} selected tracks` : song.name}`} aria-haspopup="menu" aria-expanded={!!anchor} className={button} onClick={() => {
      const r = trigger.current!.getBoundingClientRect()
      setAnchor(anchor ? null : { left: Math.max(8, Math.min(r.right - 264, window.innerWidth - 280)), top: Math.max(8, Math.min(r.bottom + 4, window.innerHeight - 460)) })
    }}>More</button>
    {anchor && createPortal(<div ref={menu} role="menu" aria-label="Track actions" className="fixed z-[70] max-h-[85vh] w-64 overflow-y-auto rounded border border-rule bg-ground p-1 shadow-xl" style={anchor} onClick={e => e.stopPropagation()}>
      {action('Play next', () => qol.add(songs, selectedSource, true, bindingContext))}
      {action('Add to queue', () => qol.add(songs, selectedSource, false, bindingContext))}
      {action(songs.length === 1 ? 'Download track' : `Download ${songs.length} tracks`, () => queueDownloads(songDownloadRequests(songs, selectedSource, bindingContext)))}
      {action(favorite && songs.length === 1 ? 'Remove from favorites' : 'Add to favorites', () => qol.favorites(songs, selectedSource, !(favorite && songs.length === 1), bindingContext))}
      {songs.length === 1 && <>
        {action('View artist', () => listening.artist(song.artist_name))}
        {action('View album', () => listening.album(song))}
        {/^[0-9a-f]{8}-[0-9a-f-]{27}$/i.test(song.id) && action('Choose another recording', () => listening.replace(song.id))}
        {selectedSource === 'local' && !/^[0-9a-f]{8}-[0-9a-f-]{27}$/i.test(song.id) && action('Reveal local file', () => qol.reveal(song, selectedSource))}
        <button role="menuitem" className="block w-full rounded px-3 py-2 text-left text-xs hover:bg-ink/10" onClick={() => setInfo(v => !v)}>Track information</button>
        {info && <dl className="break-words border-y border-rule px-3 py-2 text-[11px] text-muted"><dt>Title</dt><dd className="mb-2 text-ink">{song.name}</dd><dt>Artist / album</dt><dd className="mb-2 text-ink">{song.artist_name} · {song.album_name}</dd><dt>Playback source / duration</dt><dd className="mb-2 text-ink">{selectedSource} · {formatTime(song.duration_ms)}</dd><dt>Track ID</dt><dd>{song.id}</dd></dl>}
      </>}
      <div className="mt-1 space-y-2 border-t border-rule p-2">
        {listening.snapshot.playlists.length > 0 && <div className="flex gap-1"><select aria-label="Add to playlist" value={playlist} onChange={e => setPlaylist(e.target.value)} className="min-w-0 flex-1 rounded border border-rule bg-ground p-1 text-xs"><option value="">Choose playlist</option>{listening.snapshot.playlists.map(p => <option key={p.id} value={p.id}>{p.name}</option>)}</select><button className={button} disabled={!playlist} onClick={() => run(() => qol.playlistAdd(songs, selectedSource, playlist, null, bindingContext))}>Add</button></div>}
        <form className="flex gap-1" onSubmit={e => { e.preventDefault(); if (name.trim()) run(() => qol.playlistAdd(songs, selectedSource, null, name.trim(), bindingContext)) }}><input aria-label="New playlist name" value={name} onChange={e => setName(e.target.value)} placeholder="New playlist name" maxLength={200} className="min-w-0 flex-1 rounded border border-rule bg-ground px-2 py-1 text-xs" /><button className={button} disabled={!name.trim()}>Create</button></form>
      </div>
    </div>, document.body)}
  </span>
}

export function SelectionBar({ songs, entries, onClear }: { songs: SongRow[]; entries?: SongEntry[]; onClear: () => void }) {
  const ctx = useContext(ListeningContext)
  if (!songs.length || !ctx) return null
  let binding: { source: Source; context?: string } | null = { source: ctx.source, context: ctx.context }
  try { if (entries) binding = playlistContext(entries) } catch { binding = null }
  const add = (next: boolean) => { if (binding) void qol.add(songs, binding.source, next, binding.context).catch(e => ctx.problem(String(e))) }
  return <div className="flex shrink-0 items-center gap-3 border-b border-rule px-4 py-2 text-xs"><span className="mr-auto">{songs.length} selected{!binding && <span className="ml-2 text-muted">Different playback sources</span>}</span><button className={button} disabled={!binding} onClick={() => add(true)}>Play next</button><button className={button} disabled={!binding} onClick={() => add(false)}>Add to queue</button>{binding && <TrackTools songs={songs} source={binding.source} context={binding.context} />}<button className={button} onClick={onClear}>Clear selection</button></div>
}

export function QueueView({ state, onProblem }: { state: PlayerState | null; onProblem: (s: string) => void }) {
  const [dragging, setDragging] = useState<number | null>(null)
  const run = (promise: Promise<unknown>) => void promise.catch(e => onProblem(String(e)))
  const start = Math.max(0, state?.index ?? 0)
  const rows = state?.queue.slice(start) ?? []
  const firstUpcoming = state?.index == null ? 0 : 1
  return <div className="flex h-full flex-col">
    <div className="flex shrink-0 items-center gap-3 border-b border-rule px-4 py-3"><h1 className="mr-auto text-sm font-semibold">Queue <span className="text-xs font-normal text-muted">{rows.length} tracks</span></h1><button className={button} onClick={() => run(qol.undo())}>Undo queue change</button><button className={button} disabled={!rows.length} onClick={() => run(qol.clear())}>Clear queue</button></div>
    <div className="relative min-h-0 flex-1"><VirtualList key={start} items={rows} rowHeight={52} empty={<span className="text-xs text-muted">Your queue is empty.</span>} render={(track, index) => {
      const queueIndex = start + index
      const canReorder = queueIndex > (state?.index ?? -1)
      return <div draggable={canReorder} onDragStart={e => {
        if (!canReorder) { e.preventDefault(); return }
        setDragging(queueIndex); e.dataTransfer.setData('text/plain', String(queueIndex)); e.dataTransfer.effectAllowed = 'move'
      }} onDragOver={e => { e.preventDefault(); e.dataTransfer.dropEffect = canReorder ? 'move' : 'none' }} onDragEnd={() => setDragging(null)} onDrop={e => {
        e.preventDefault()
        if (canReorder && dragging !== null && dragging > (state?.index ?? -1) && dragging !== queueIndex) run(qol.move(dragging, queueIndex))
        setDragging(null)
      }} className="row mx-2 flex h-[52px] items-center gap-3 px-2" data-playing={state?.index === queueIndex}>
        <button aria-label={`Play queue item ${index + 1}: ${track.title}`} className="min-w-0 flex-1 truncate text-left text-xs" onClick={() => run(qol.jump(queueIndex))}><span className="mr-3 data text-muted">{index + 1}</span>{track.title}<span className="ml-3 text-muted">{track.artist}</span></button>
        <span className="data text-[10px] text-muted">{formatTime(track.duration_ms)}</span>
        <button className={button} aria-label={`Move queue item ${index + 1} up`} disabled={!canReorder || index <= firstUpcoming} onClick={() => run(qol.move(queueIndex, queueIndex - 1))}>Up</button>
        <button className={button} aria-label={`Move queue item ${index + 1} down`} disabled={!canReorder || index === rows.length - 1} onClick={() => run(qol.move(queueIndex, queueIndex + 1))}>Down</button>
        <button className={button} aria-label={`Remove queue item ${index + 1}`} onClick={() => run(qol.remove(queueIndex))}>Remove</button>
      </div>
    }} /></div>
  </div>
}
