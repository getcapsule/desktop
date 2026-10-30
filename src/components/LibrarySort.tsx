import type { SortDirection } from '../lib/ipc'

export const SONG_SORTS = [
  { value: 'title', label: 'Title' }, { value: 'artist', label: 'Artist' },
  { value: 'album', label: 'Album' }, { value: 'duration', label: 'Duration' },
] as const
export const ALBUM_SORTS = [{ value: 'title', label: 'Title' }, { value: 'artist', label: 'Artist' }] as const
export const ARTIST_SORTS = [{ value: 'name', label: 'Name' }] as const

export function LibrarySort<T extends string>({ label, value, options, onChange, direction, onDirection, count, loading }: {
  label: string
  value: T
  options: readonly { value: T; label: string }[]
  onChange: (value: T) => void
  direction: SortDirection
  onDirection: (direction: SortDirection) => void
  count: number
  loading: boolean
}) {
  const style = 'rounded border border-rule bg-ground px-2 py-1 text-[11px] text-ink outline-none focus:border-accent'
  return (
    <div className="library-sort flex shrink-0 flex-wrap items-center gap-2">
      <span className="mr-auto text-[11px] text-muted" role="status">{loading ? 'Reading library…' : `${count} ${label}`}</span>
      <label className="flex items-center gap-2 text-[11px] text-muted">
        Sort
        <select aria-label={`Sort ${label}`} value={value} onChange={(e) => onChange(e.target.value as T)} className={style}>
          {options.map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}
        </select>
      </label>
      <select aria-label="Sort direction" value={direction} onChange={(e) => onDirection(e.target.value as SortDirection)} className={style}>
        <option value="asc">Ascending</option><option value="desc">Descending</option>
      </select>
    </div>
  )
}
