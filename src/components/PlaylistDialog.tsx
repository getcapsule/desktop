import { useEffect, useId, useRef, useState, type ReactNode } from 'react'
import { createPortal } from 'react-dom'
import type { LocalPlaylist } from '../lib/qol-ipc'

export const playlistField = 'w-full rounded-md border border-rule bg-ground px-3 py-2 text-sm text-ink focus-visible:outline-2 focus-visible:outline-accent'
export const playlistButton = 'shrink-0 rounded-md border border-rule px-3 py-1.5 text-xs text-ink hover:border-accent focus-visible:outline-2 focus-visible:outline-accent disabled:opacity-40'

export function PlaylistModal({ title, description, busy, onClose, children }: {
  title: string
  description?: string
  busy: boolean
  onClose: () => void
  children: (close: () => void) => ReactNode
}) {
  const dialog = useRef<HTMLDialogElement>(null)
  const heading = useId()
  const descriptionId = useId()
  useEffect(() => { dialog.current?.showModal() }, [])

  function close() {
    dialog.current?.close()
    onClose()
  }

  return createPortal(
    <dialog
      ref={dialog}
      aria-labelledby={heading}
      aria-describedby={description ? descriptionId : undefined}
      onCancel={event => { event.preventDefault(); if (!busy) close() }}
      onClick={event => event.stopPropagation()}
      onDoubleClick={event => event.stopPropagation()}
      onContextMenu={event => event.stopPropagation()}
      className="glass-dialog fixed inset-0 m-auto max-h-[calc(100%-2rem)] w-[420px] max-w-[calc(100%-2rem)] space-y-4 overflow-y-auto p-5 text-ink backdrop:bg-black/45"
    >
      <div>
        <h2 id={heading} className="text-base font-semibold">{title}</h2>
        {description && <p id={descriptionId} className="mt-1 truncate text-xs text-muted">{description}</p>}
      </div>
      {children(close)}
    </dialog>,
    document.body,
  )
}

export function PlaylistDialog({ description, playlists = [], onSave, onClose }: {
  description?: string
  playlists?: LocalPlaylist[]
  onSave: (playlistId: string | null, name: string | null) => Promise<string>
  onClose: () => void
}) {
  const [playlistId, setPlaylistId] = useState('')
  const [name, setName] = useState('')
  const [busy, setBusy] = useState(false)
  const [problem, setProblem] = useState('')
  const adding = description !== undefined

  async function save(id: string | null, newName: string | null, close: () => void) {
    if (busy) return
    setBusy(true)
    setProblem('')
    try {
      await onSave(id, newName)
      close()
    } catch (error) {
      setProblem(String(error))
      setBusy(false)
    }
  }

  return (
    <PlaylistModal title={adding ? 'Add to playlist' : 'New playlist'} description={description} busy={busy} onClose={onClose}>
      {close => (
        <>
          {adding && playlists.length > 0 && (
            <form className="flex items-end gap-2" onSubmit={event => { event.preventDefault(); if (playlistId) void save(playlistId, null, close) }}>
              <label className="min-w-0 flex-1 space-y-1 text-xs text-muted">
                <span>Playlist</span>
                <select value={playlistId} disabled={busy} onChange={event => setPlaylistId(event.target.value)} className={playlistField}>
                  <option value="">Choose a playlist</option>
                  {playlists.map(playlist => <option key={playlist.id} value={playlist.id}>{playlist.name}</option>)}
                </select>
              </label>
              <button className={playlistButton} disabled={busy || !playlistId}>Add to playlist</button>
            </form>
          )}
          <form className="flex items-end gap-2" onSubmit={event => { event.preventDefault(); if (name.trim()) void save(null, name.trim(), close) }}>
            <label className="min-w-0 flex-1 space-y-1 text-xs text-muted">
              <span>Playlist name</span>
              <input value={name} maxLength={200} disabled={busy} onChange={event => setName(event.target.value)} placeholder="Name your playlist" className={playlistField} />
            </label>
            <button className={playlistButton} disabled={busy || !name.trim()}>{adding ? 'Create and add' : 'Create playlist'}</button>
          </form>
          {problem && <p role="alert" className="text-xs text-crit">{problem}</p>}
          <div className="flex justify-end border-t border-rule pt-3">
            <button type="button" className={playlistButton} disabled={busy} onClick={close}>Cancel</button>
          </div>
        </>
      )}
    </PlaylistModal>
  )
}
