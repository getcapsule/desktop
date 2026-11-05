import { useRef, useState } from 'react'
import { artworkUrl } from '../lib/ipc'
import { qol, type LocalPlaylist } from '../lib/qol-ipc'
import { PlaylistModal, playlistButton, playlistField } from './PlaylistDialog'

const maxCoverBytes = 5 * 1024 * 1024
const maxCoverSide = 512
const coverTypes = ['image/jpeg', 'image/png', 'image/webp']

async function encodeCover(file: File): Promise<number[]> {
  let bitmap: ImageBitmap
  try {
    bitmap = await createImageBitmap(file)
  } catch {
    throw new Error('Could not read this image. Choose a JPG, PNG, or WebP file.')
  }
  const canvas = document.createElement('canvas')
  try {
    const scale = Math.min(1, maxCoverSide / Math.max(bitmap.width, bitmap.height))
    canvas.width = Math.max(1, Math.round(bitmap.width * scale))
    canvas.height = Math.max(1, Math.round(bitmap.height * scale))
    const context = canvas.getContext('2d')
    if (!context) throw new Error('Could not prepare the playlist cover.')
    context.drawImage(bitmap, 0, 0, canvas.width, canvas.height)
  } finally {
    bitmap.close()
  }
  const blob = await new Promise<Blob>((resolve, reject) => {
    canvas.toBlob(image => image ? resolve(image) : reject(new Error('Could not prepare the playlist cover.')), 'image/png')
  })
  return new Promise((resolve, reject) => {
    const reader = new FileReader()
    reader.onload = () => resolve(Array.from(new Uint8Array(reader.result as ArrayBuffer)))
    reader.onerror = () => reject(new Error('Could not read the playlist cover.'))
    reader.readAsArrayBuffer(blob)
  })
}

export function PlaylistEditor({ playlist, onClose }: { playlist: LocalPlaylist; onClose: () => void }) {
  const [name, setName] = useState(playlist.name)
  const [description, setDescription] = useState(playlist.description ?? '')
  const [cover, setCover] = useState<File | null>(null)
  const [removeCover, setRemoveCover] = useState(false)
  const [busy, setBusy] = useState(false)
  const [problem, setProblem] = useState('')
  const coverInput = useRef<HTMLInputElement>(null)

  async function save(close: () => void) {
    if (busy || !name.trim()) return
    setBusy(true)
    setProblem('')
    try {
      const bytes = cover ? await encodeCover(cover) : null
      await qol.playlistEdit(playlist.id, name.trim(), description.trim(), bytes, removeCover)
      close()
    } catch (error) {
      setProblem(error instanceof Error ? error.message : String(error))
      setBusy(false)
    }
  }

  return (
    <PlaylistModal title="Edit playlist" busy={busy} onClose={onClose}>
      {close => (
        <form className="space-y-4" onSubmit={event => { event.preventDefault(); void save(close) }}>
          <label className="block space-y-1 text-xs text-muted">
            <span>Playlist name</span>
            <input className={playlistField} value={name} maxLength={200} disabled={busy} onChange={event => setName(event.target.value)} />
          </label>
          <label className="block space-y-1 text-xs text-muted">
            <span>Description</span>
            <textarea className={playlistField} value={description} rows={3} maxLength={1000} disabled={busy} onChange={event => setDescription(event.target.value)} />
          </label>
          <div className="flex items-center gap-3">
            {playlist.cover_id && !removeCover && <img alt="" src={artworkUrl(playlist.cover_id, 128)} className="size-14 shrink-0 rounded-md border border-rule object-cover" />}
            <label className="block min-w-0 flex-1 space-y-1 text-xs text-muted">
              <span>Cover image</span>
              <input
                ref={coverInput}
                type="file"
                accept="image/jpeg,image/png,image/webp,.jpg,.jpeg,.png,.webp"
                disabled={busy}
                className="block w-full rounded text-xs text-ink file:mr-2 file:cursor-pointer file:rounded-md file:border file:border-rule file:bg-ground file:px-2 file:py-1 focus-visible:outline-2 focus-visible:outline-accent"
                onChange={event => {
                  const file = event.target.files?.[0]
                  if (!file) return
                  if (file.size > maxCoverBytes || (file.type && !coverTypes.includes(file.type))) {
                    event.target.value = ''
                    setCover(null)
                    setProblem('Choose a JPG, PNG, or WebP image up to 5 MB.')
                    return
                  }
                  setCover(file)
                  setRemoveCover(false)
                  setProblem('')
                }}
              />
            </label>
          </div>
          {playlist.cover_id && (
            <button type="button" className={playlistButton} disabled={busy} onClick={() => {
              if (coverInput.current) coverInput.current.value = ''
              setCover(null)
              setRemoveCover(!removeCover)
              setProblem('')
            }}>{removeCover ? 'Keep current cover' : 'Remove cover'}</button>
          )}
          {problem && <p role="alert" className="text-xs text-crit">{problem}</p>}
          <div className="flex justify-end gap-2 border-t border-rule pt-3">
            <button type="button" className={playlistButton} disabled={busy} onClick={close}>Cancel</button>
            <button className={playlistButton} disabled={busy || !name.trim()}>{busy ? 'Saving…' : 'Save changes'}</button>
          </div>
        </form>
      )}
    </PlaylistModal>
  )
}
